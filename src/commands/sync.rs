use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use base64::Engine;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use walkdir::WalkDir;
use zeroize::Zeroizing;

use crate::api::ApiClient;
use crate::config::load_config;
use crate::owned_keys::{CUSTODY_UNAVAILABLE, OwnedKeyResolver, ResolvedCliKey};
use crate::ui;
use beebeeb_core::owned_keys::KeyOrigin;

const STATE_FILE: &str = ".bb-sync.json";
// Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
// See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

struct SyncDashboard {
    local_path: String,
    remote_path: String,
    file_count: AtomicU64,
    total_bytes: AtomicU64,
    is_synced: AtomicBool,
}

fn print_dashboard(dash: &SyncDashboard) {
    let w = 50;
    let status = if dash.is_synced.load(Ordering::Relaxed) {
        format!(
            "{} synced \u{00b7} watching",
            "\u{25CF}".custom_color(crate::colors::GREEN_OK)
        )
    } else {
        format!("{} syncing", "\u{25CF}".custom_color(crate::colors::AMBER))
    };
    let files = dash.file_count.load(Ordering::Relaxed);
    let bytes = dash.total_bytes.load(Ordering::Relaxed);

    println!("{}", crate::ui::box_header("SYNC", w));
    println!(
        "{}",
        crate::ui::box_line(
            &format!(
                "  {:<10}{} \u{2194} {}",
                "folder".custom_color(crate::colors::INK_DIM),
                dash.local_path,
                dash.remote_path
            ),
            w,
        )
    );
    println!(
        "{}",
        crate::ui::box_line(
            &format!("  {:<10}{}", "status".custom_color(crate::colors::INK_DIM), status),
            w,
        )
    );
    println!(
        "{}",
        crate::ui::box_line(
            &format!(
                "  {:<10}{} tracked \u{00b7} {}",
                "files".custom_color(crate::colors::INK_DIM),
                files,
                crate::ui::human_size(bytes)
            ),
            w,
        )
    );
    println!("{}", crate::ui::box_footer(w));
    println!();
}

fn is_ignored_path(path: &Path) -> bool {
    // Files `bb sync` moved aside (task 1915) are not sync input.
    if path
        .components()
        .any(|c| c.as_os_str() == crate::sync_safety::RECOVERY_DIR)
    {
        return true;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == ".DS_Store"
        || name.starts_with("._")
        || name == ".bb-sync.json"
        || name == "Thumbs.db"
        || name.starts_with(".Spotlight-")
        || name == ".Trashes"
        || name == ".fseventsd"
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct SyncState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    remote_path: Option<String>,
    remote_folder_id: Option<Uuid>,
    last_sync: Option<DateTime<Utc>>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    files: HashMap<String, FileEntry>,
    #[serde(default)]
    owned_keys: HashMap<String, OpaqueOwnedKey>,
    #[serde(default)]
    dirty_shared_roots: BTreeSet<String>,
    #[serde(default)]
    owner_index_dirty: bool,
    #[serde(default)]
    removed_index_ids: BTreeSet<String>,
}

/// Ciphertext and envelopes only. Account and root checks precede all reads.
#[derive(Serialize, Deserialize, Clone)]
struct OpaqueOwnedKey {
    canonical: serde_json::Value,
    pending_owner_wrap: Option<Vec<u8>>,
    shared_root_id: Option<String>,
}
fn sync_keys(state: &SyncState) -> Result<OwnedKeyResolver, String> {
    let mut keys = OwnedKeyResolver::new();
    if state.account_id.is_some() {
        for (id, record) in &state.owned_keys {
            keys.seed(std::slice::from_ref(&record.canonical));
            if let Some(frame) = &record.pending_owner_wrap {
                keys.restore_pending(id, frame.clone())?;
            }
        }
    }
    Ok(keys)
}
#[derive(Serialize, Deserialize, Clone)]
struct FileEntry {
    remote_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_version: Option<i64>,
    last_mtime: i64,
    last_size: u64,
    last_sync: DateTime<Utc>,
    #[serde(default)]
    content_hash: Option<String>,
}

#[derive(Clone)]
struct LocalFile {
    mtime: i64,
    size: u64,
    content_hash: String,
}

fn compute_file_hash(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Is the on-disk file byte-identical to what the manifest recorded at the last
/// sync? Prefer the content hash (catches same-size edits); fall back to
/// `(mtime, size)` for manifest entries written before content hashing existed.
///
/// This is the data-safety gate for remote-delete reconciliation: we only ever
/// remove a local mirror file when this returns `true`, so a file the user
/// edited offline is never destroyed by a remote deletion.
fn local_matches_entry(local: &LocalFile, entry: &FileEntry) -> bool {
    match &entry.content_hash {
        Some(h) => *h == local.content_hash,
        None => local.mtime == entry.last_mtime && local.size == entry.last_size,
    }
}

/// Has the on-disk file changed relative to the manifest entry recorded at the
/// last sync? This is the strict inverse of [`local_matches_entry`]: it prefers
/// the content hash (catches a same-size/same-mtime edit) and only falls back to
/// `(mtime, size)` for manifest entries written before content hashing existed.
///
/// ALL upload paths — the one-shot classifier and both continuous-watch resync
/// loops — call this so their change detection can never drift apart again. An
/// earlier divergence (the watch loops compared only `(mtime, size)`) meant a
/// same-size offline edit that was remote-deleted got correctly kept but then
/// never re-uploaded; routing every site through this helper closes that gap.
fn local_changed_vs_manifest(local: &LocalFile, entry: &FileEntry) -> bool {
    !local_matches_entry(local, entry)
}

/// What to do with a prior-synced manifest key whose remote counterpart is
/// missing from the current remote listing.
///
/// Absence is NOT deletion (deletion-safety spec R5, task 1915): a file is only
/// removed locally when the server positively reports that exact file trashed.
#[derive(Debug, PartialEq, Eq)]
enum RemoteAbsent {
    /// Manifest entry exists, the local file is byte-identical to it AND the
    /// server's own record for that id says it is trashed (a tombstone): a
    /// genuine remote delete. The local file is moved to the recoverable folder
    /// (never unlinked) and the manifest entry dropped.
    DeleteLocal,
    /// Manifest entry exists but the local file DIFFERS (the user edited it
    /// offline while it was deleted remotely). Never destroy unsynced local
    /// edits - surface it and re-upload the local copy as a new file.
    ReAddLocal,
    /// Manifest entry exists, local file untouched, but the server gave NO
    /// deletion record (still alive elsewhere - moved or renamed - or unknown to
    /// it, or the listing is not trustworthy). The listing alone proves nothing:
    /// keep the file and the manifest entry, delete and upload nothing.
    KeepUnconfirmed,
    /// Manifest entry exists but the local file is already gone: nothing on disk
    /// to touch, just drop the stale entry.
    ForgetStale,
    /// No manifest entry: the local file is genuinely new (never synced), so
    /// this is not a remote delete at all - upload it as new (caller's default).
    NotADelete,
}

/// Classify a manifest key that is absent from the current remote listing.
///
/// `local` is the on-disk file (if it still exists), `prior` the manifest entry
/// from `SyncState.files`, `tombstoned` whether the server positively reported
/// that entry's remote id trashed. This is the ONE place the remote-delete vs
/// offline-edit vs unconfirmed decision lives; the one-shot classifier and both
/// continuous-watch resync loops call it so they stay consistent.
fn classify_remote_absent(local: Option<&LocalFile>, prior: Option<&FileEntry>, tombstoned: bool) -> RemoteAbsent {
    match (local, prior) {
        // Synced before + still on disk -> compare against the manifest.
        (Some(l), Some(p)) => {
            if !local_matches_entry(l, p) {
                RemoteAbsent::ReAddLocal
            } else if tombstoned {
                RemoteAbsent::DeleteLocal
            } else {
                RemoteAbsent::KeepUnconfirmed
            }
        }
        // Synced before, already gone locally: nothing to delete, drop the entry.
        (None, Some(_)) => RemoteAbsent::ForgetStale,
        // No prior manifest entry -> genuinely new local file, not a delete.
        (Some(_), None) => RemoteAbsent::NotADelete,
        (None, None) => RemoteAbsent::NotADelete,
    }
}

const ACCOUNT_REFUSAL: &str =
    "This folder was synced by a different account. Sign in as that account, or sync into a new folder.";

/// D-L1: resolve session identity before any manifest-driven work. A legacy
/// root must be readable by that session; email is never an account identity.
async fn check_sync_account(api: &ApiClient, state: &SyncState) -> Result<String, String> {
    let me = api.get_me().await?;
    let id = me["id"].as_str().ok_or("session response missing user id")?;
    uuid::Uuid::parse_str(id).map_err(|_| "session response has invalid user id")?;
    if let Some(bound) = &state.account_id {
        if bound != id {
            return Err(ACCOUNT_REFUSAL.into());
        }
    } else if let Some(root) = state.remote_folder_id {
        let row = api.get_file(&root.to_string()).await.map_err(|_| ACCOUNT_REFUSAL)?;
        if row["id"].as_str() != Some(root.to_string().as_str())
            || row["is_folder"].as_bool() != Some(true)
            || row["is_trashed"].as_bool() == Some(true)
        {
            return Err(ACCOUNT_REFUSAL.into());
        }
    }
    Ok(id.to_string())
}

/// Is this directory bound to a different remote folder than the one resolved
/// now? Then its manifest describes another tree and no pass may remove local
/// files on its account until `bb sync --rebind`. One-shot and both watch loops
/// ask this, so a root change blocks removals everywhere.
fn root_unreconciled(state: &SyncState, resolved: Uuid) -> bool {
    state.remote_folder_id.is_some_and(|bound| bound != resolved)
}

/// The manifest entry this pass may trust for `rel`. While the root is
/// unreconciled, an entry for a file that ALSO exists in the current remote
/// folder describes a different file of the old tree, so it counts as absent:
/// the two copies become a both-sides conflict (skipped unless `--force`),
/// never a trash, replace-upload or download-over-local on the old entry's
/// account (#83 review).
fn trusted_prior(state: &SyncState, rel: &str, remote_present: bool, root_changed: bool) -> Option<FileEntry> {
    if root_changed && remote_present {
        return None;
    }
    state.files.get(rel).cloned()
}

/// Everything one pass knows about whether remote absence may mean "deleted":
/// the ids the server positively reports trashed, and - if the listing itself
/// is not fit to conclude anything from - why.
struct AbsencePlan {
    tombstones: HashSet<Uuid>,
    /// `Some(reason)`: the remote listing is empty / partial / unreadable / from
    /// another folder, so NO local file may be removed on its account.
    untrusted: Option<String>,
}

impl AbsencePlan {
    fn tombstoned(&self, entry: &FileEntry) -> bool {
        self.tombstones.contains(&entry.remote_id)
    }
}

/// Decide whether this pass may remove anything, and ask the server about the
/// only files it could remove (manifest entries whose local copy is untouched
/// and which are absent remotely). Skipped entirely for an untrusted listing.
async fn plan_absence(
    api: &ApiClient,
    state: &SyncState,
    local_files: &HashMap<String, LocalFile>,
    remote_files: &HashMap<String, RemoteFile>,
    health: &crate::sync_safety::ListingHealth,
    root_changed: bool,
    allow_mass_delete: bool,
) -> AbsencePlan {
    use crate::sync_safety::{breaker_limit, confirm_tombstones, untrusted_reason};
    let untrusted = untrusted_reason(health, remote_files.len(), state.files.len(), root_changed);
    if untrusted.is_some() {
        return AbsencePlan {
            tombstones: HashSet::new(),
            untrusted,
        };
    }
    let candidates: Vec<Uuid> = state
        .files
        .iter()
        .filter(|(rel, _)| !remote_files.contains_key(*rel))
        .filter(|(rel, entry)| local_files.get(*rel).is_some_and(|l| local_matches_entry(l, entry)))
        .map(|(_, entry)| entry.remote_id)
        .collect();
    // Once the breaker is certain to trip there is no point asking about the rest.
    let stop_after = if allow_mass_delete {
        None
    } else {
        Some(breaker_limit(state.files.len()))
    };
    AbsencePlan {
        tombstones: confirm_tombstones(api, &candidates, stop_after).await,
        untrusted: None,
    }
}

/// Why files that are missing remotely were left alone, when the listing is
/// not fit to conclude deletions from - and only if something was missing.
fn untrusted_notice(
    plan: &AbsencePlan,
    state: &SyncState,
    remote_files: &HashMap<String, RemoteFile>,
) -> Option<String> {
    let why = plan.untrusted.as_ref()?;
    let missing = state.files.keys().filter(|k| !remote_files.contains_key(*k)).count();
    (missing > 0).then(|| {
        format!(
            "kept {missing} local file{} missing from the remote listing: {why}. Nothing was deleted.",
            if missing == 1 { "" } else { "s" },
        )
    })
}

/// One line for the files that are missing remotely, untouched locally, with no
/// server deletion record.
fn kept_unconfirmed_notice(rels: &[String]) -> Option<String> {
    if rels.is_empty() {
        return None;
    }
    let mut sorted: Vec<&str> = rels.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let shown: Vec<&str> = sorted.iter().copied().take(3).collect();
    let more = sorted.len() - shown.len();
    Some(format!(
        "kept {} local file{} missing remotely: the server has no deletion record for {} (moved or renamed remotely?): {}{}",
        sorted.len(),
        if sorted.len() == 1 { "" } else { "s" },
        if sorted.len() == 1 { "it" } else { "them" },
        shown.join(", "),
        if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        },
    ))
}

fn breaker_notice(count: usize, tracked: usize) -> String {
    format!(
        "mass delete held back: {count} local file(s) reported deleted remotely in one pass - more than the limit of {} \
         (max of {} and 10% of {tracked} tracked). Nothing was removed. Check the Trash in the web app; if this is intended, \
         re-run once with --allow-mass-delete (files then move to {}/ inside the sync folder, not a plain delete).",
        crate::sync_safety::breaker_limit(tracked),
        crate::sync_safety::BREAKER_FLOOR,
        crate::sync_safety::RECOVERY_DIR,
    )
}

/// Print one of the notices above (one-shot path; the TUI routes them as events).
fn print_notice(msg: &str) {
    if !ui::is_quiet() {
        eprintln!("  {} {msg}", "!".custom_color(crate::colors::AMBER));
    }
}

/// Stat + hash a file right now (not from the scan taken at the start of the
/// pass). Used to re-check a file immediately before it is moved aside.
fn fresh_local_file(path: &Path) -> Result<LocalFile, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok(LocalFile {
        mtime,
        size: meta.len(),
        content_hash: compute_file_hash(path)?,
    })
}

/// Move one local file aside because the server reports it deleted. Returns
/// `Ok(true)` if it was moved (or was already gone), `Ok(false)` if it was left
/// in place on purpose (it changed since the scan), `Err` if the move failed.
fn remove_local_recoverably(local_dir: &Path, rel: &str, entry: &FileEntry) -> Result<bool, String> {
    // Same containment rule as downloads: never touch anything outside the sync root.
    let path = crate::safe_path::safe_rel_join(local_dir, rel, crate::safe_path::Rules::Host)
        .map_err(|e| format!("could not remove {rel:?}: {e}"))?;
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(true); // already gone
    }
    // The scan may be hours old (large upload phase): re-check the file now.
    let fresh = fresh_local_file(&path)?;
    if !local_matches_entry(&fresh, entry) {
        return Ok(false);
    }
    crate::sync_safety::move_to_recovery(local_dir, &path, rel)?;
    Ok(true)
}

/// What a reconcile pass did.
struct Reconciled {
    /// Rels whose local copy was moved aside (or was already gone): drop them
    /// from the pre-reconcile snapshot.
    removed: Vec<String>,
    /// Lines for the user about what was left alone and why. The caller shows
    /// them (stderr for the plain loop, an event for the TUI) so nothing here
    /// scribbles over a raw-mode screen.
    notices: Vec<String>,
}

/// Apply remote-delete reconciliation for every manifest key that is absent
/// from `remote_files`. Used by both continuous-watch resync loops (the one-shot
/// path threads the same decision through its classifier instead).
///
/// For each prior-synced key no longer present remotely:
/// - local byte-identical AND the server reports the id trashed (tombstone) ->
///   move the local file to `.bb-trash/` + drop the manifest entry;
/// - local byte-identical, no tombstone -> keep everything (absence is not deletion);
/// - locally modified offline -> leave the file, keep the manifest entry, and the
///   caller's normal upload pass re-adds it (never silently destroy local edits).
///
/// More removals than the breaker allows in one pass remove nothing.
fn reconcile_remote_deletions(
    local_dir: &Path,
    state: &mut SyncState,
    local_files: &HashMap<String, LocalFile>,
    remote_files: &HashMap<String, RemoteFile>,
    plan: &AbsencePlan,
    allow_mass_delete: bool,
    dry_run: bool,
) -> Reconciled {
    let mut to_remove: Vec<String> = Vec::new();
    let mut to_forget: Vec<String> = Vec::new();
    let mut kept_unconfirmed: Vec<String> = Vec::new();
    let mut notices: Vec<String> = Vec::new();
    // Snapshot the keys up front - we mutate state.files below.
    let manifest_keys: Vec<String> = state.files.keys().cloned().collect();
    for rel in manifest_keys {
        if remote_files.contains_key(&rel) {
            continue; // still present remotely - not a delete
        }
        let prior = state.files.get(&rel).cloned();
        let tombstoned = prior.as_ref().is_some_and(|p| plan.tombstoned(p));
        match classify_remote_absent(local_files.get(&rel), prior.as_ref(), tombstoned) {
            RemoteAbsent::DeleteLocal => to_remove.push(rel),
            RemoteAbsent::ForgetStale => to_forget.push(rel),
            RemoteAbsent::KeepUnconfirmed => kept_unconfirmed.push(rel),
            RemoteAbsent::ReAddLocal => {
                // Local diverged from the manifest while the remote was deleted.
                // Do NOT delete. Surface it; the normal upload pass re-adds it as
                // a new file (the manifest entry stays so it is treated as a
                // local change, not a fresh upload conflict).
                if !ui::is_json() && !ui::is_quiet() {
                    eprintln!(
                        "  {} {} {}",
                        "\u{26A1}".custom_color(crate::colors::AMBER),
                        "deleted remotely, kept (edited locally)".custom_color(crate::colors::AMBER),
                        rel.custom_color(crate::colors::INK),
                    );
                }
            }
            RemoteAbsent::NotADelete => {}
        }
    }

    if !allow_mass_delete && crate::sync_safety::breaker_trips(to_remove.len(), state.files.len()) {
        notices.push(breaker_notice(to_remove.len(), state.files.len()));
        to_remove.clear();
    }
    notices.extend(kept_unconfirmed_notice(&kept_unconfirmed));

    let mut removed = Vec::new();
    for rel in to_remove {
        if !dry_run {
            let Some(entry) = state.files.get(&rel).cloned() else {
                continue;
            };
            match remove_local_recoverably(local_dir, &rel, &entry) {
                Ok(true) => {
                    state.files.remove(&rel);
                    state.removed_index_ids.insert(entry.remote_id.to_string());
                }
                Ok(false) => {
                    notices.push(format!("{rel} changed since the scan, kept"));
                    continue;
                }
                Err(e) => {
                    // Keep the manifest entry so we retry next pass.
                    notices.push(e);
                    continue;
                }
            }
        }
        removed.push(rel);
    }
    for rel in to_forget {
        if !dry_run {
            state.files.remove(&rel);
        }
        removed.push(rel);
    }
    Reconciled { removed, notices }
}

#[derive(Clone)]
struct RemoteFile {
    id: Uuid,
    resolved: Option<ResolvedCliKey>,
    chunk_count: u32,
    updated_at: DateTime<Utc>,
    /// The content's read pin from the file's authenticated metadata
    /// (`crypto::content_pin`, task 1943). `Err` when that metadata cannot be
    /// read: the file is still listed, but its content is refused, never guessed.
    pin: Result<beebeeb_core::chunk_stream::ExpectedChunkFormat, String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    local_dir: Option<PathBuf>,
    remote_path_arg: Option<String>,
    dry_run: bool,
    force: bool,
    delete_remote: bool,
    once: bool,
    daemon: bool,
    stop_legacy: bool,
    status: bool,
    stop_session: Option<String>,
    stop_all: bool,
    concurrency: usize,
    rehash: bool,
    allow_mass_delete: bool,
    rebind: bool,
) -> Result<(), String> {
    // ── --status: show all active sync sessions ────────────────────────────
    if status || stop_session.is_some() || stop_all {
        let api = ApiClient::from_config();
        api.require_auth()?;

        if let Some(ref name) = stop_session {
            return stop_session_by_name(&api, name).await;
        }
        if stop_all {
            return stop_all_sessions(&api).await;
        }
        // --status
        return print_session_status(&api).await;
    }

    // If no local_dir and no control flags, show status + hint (Task 4)
    let local_dir = match local_dir {
        Some(d) => d,
        None => {
            let api = ApiClient::from_config();
            api.require_auth()?;
            print_session_status(&api).await?;
            println!();
            println!(
                "  {} {}",
                "hint".custom_color(crate::colors::INK_DIM),
                "Run bb sync <local> <remote> to start a new sync".custom_color(crate::colors::INK_DIM),
            );
            return Ok(());
        }
    };

    if stop_legacy {
        // Legacy --stop: try to uninstall any old single-plist LaunchAgent
        return crate::daemon::uninstall_launchagent("sync").map(|removed| {
            if removed {
                println!(
                    "  {} sync daemon removed",
                    "\u{2713}".custom_color(crate::colors::GREEN_OK),
                );
            } else {
                println!(
                    "  {} no legacy sync daemon installed",
                    "\u{00b7}".custom_color(crate::colors::INK_DIM),
                );
            }
        });
    }
    if daemon {
        return spawn_sync_daemon(&local_dir, remote_path_arg.as_deref());
    }

    let api = ApiClient::from_config();
    api.require_auth()?;

    if !local_dir.exists() {
        return Err(format!("local directory not found: {}", local_dir.display()));
    }
    if !local_dir.is_dir() {
        return Err(format!("not a directory: {}", local_dir.display()));
    }

    let local_dir = std::fs::canonicalize(&local_dir).map_err(|e| format!("cannot resolve path: {e}"))?;

    let state_path = local_dir.join(STATE_FILE);
    let mut state: SyncState = if state_path.exists() {
        let data = std::fs::read(&state_path).map_err(|e| format!("read state: {e}"))?;
        serde_json::from_slice(&data).map_err(|e| format!("invalid state file: {e}"))?
    } else {
        SyncState::default()
    };

    let account_id = check_sync_account(&api, &state).await?;
    if state.account_id.is_none() {
        state.owned_keys.clear();
        state.dirty_shared_roots.clear();
        state.removed_index_ids.clear();
        state.owner_index_dirty = false;
    }

    let remote_path = match remote_path_arg.or_else(|| state.remote_path.clone()) {
        Some(p) => normalize_remote_path(&p),
        None => {
            return Err("remote_path is required (no .bb-sync.json found). \
                        Usage: bb sync <local_dir> <remote_path>"
                .to_string());
        }
    };

    // Multiple owners (each parallel upload future) need the key; `MasterKey`
    // is not `Clone`, so share it behind an `Arc`. The downstream functions
    // that take `&MasterKey` get it via deref coercion.
    let master_key = Arc::new(load_master_key()?);

    // Graceful Ctrl-C: the first interrupt asks the upload phase to stop
    // cleanly (so we can report "interrupted — N done, M remaining" and resume
    // next run); a second interrupt hard-exits (covers the watch loop and a
    // wedged upload).
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let s = Arc::clone(&shutdown);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                s.store(true, Ordering::Relaxed);
            }
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        });
    }

    if !ui::is_json() && !ui::is_quiet() {
        println!(
            "  {} {} {} {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            "syncing".custom_color(crate::colors::GREEN_OK),
            local_dir.display().to_string().custom_color(crate::colors::INK),
            format!("\u{2192} {remote_path}").custom_color(crate::colors::INK_DIM),
        );
        if dry_run {
            println!(
                "  {} {}",
                "mode".custom_color(crate::colors::INK_DIM),
                "dry-run (no file changes; security observations are saved)".custom_color(crate::colors::AMBER),
            );
        }
        println!();
    }

    let sync_start = Instant::now();
    let mut total_bytes: u64 = 0;

    let mut path_keys = OwnedKeyResolver::new();
    let resolved_root = resolve_remote_folder(&api, &master_key, &remote_path, dry_run, &mut path_keys).await;
    let remote_folder_id = resolved_root?;
    // `--rebind`: the user accepts the folder resolved now as this directory's
    // remote root. The old manifest describes the other tree, so it is dropped:
    // nothing that belonged there can be removed on this folder's account, and
    // the local files sync into the new folder as new files.
    if rebind && root_unreconciled(&state, remote_folder_id) {
        state.files.clear();
        state.owned_keys.clear();
        state.dirty_shared_roots.clear();
        state.removed_index_ids.clear();
        state.remote_folder_id = Some(remote_folder_id);
        print_notice("rebound to the new remote folder; the old folder's sync record was dropped");
    }
    // If this directory last synced against a DIFFERENT remote folder (the old
    // one was trashed and re-created, or the remote path changed), the manifest
    // describes files of another tree: nothing may be removed on its account.
    // The old binding is KEPT, so every later pass and the watch loops stay
    // blocked too, until `--rebind` (#83 review: adopting the new folder here
    // made the next pass trust it).
    let root_changed = root_unreconciled(&state, remote_folder_id);
    state.remote_path = Some(remote_path.clone());
    state.remote_folder_id.get_or_insert(remote_folder_id);

    // Scan phase — the dominant "looks frozen" symptom. Show a live spinner so
    // hashing the local tree + fetching the remote tree never looks like a hang.
    let scan_active = ui::is_rich() && std::io::stdout().is_terminal();

    let local_spinner = make_scan_spinner(scan_active, "scanning local", "files");
    let mut local_files = scan_local_files(&local_dir, &state.files, rehash, local_spinner.as_ref())?;
    if let Some(pb) = &local_spinner {
        pb.finish_and_clear();
    }
    let local_folders = walk_local_folders(&local_dir)?;

    let mut keys = sync_keys(&state)?;
    let remote_spinner = make_scan_spinner(scan_active, "scanning remote", "entries");
    let mut remote_files: HashMap<String, RemoteFile> = HashMap::new();
    let mut remote_folders: HashMap<String, Uuid> = HashMap::new();
    let mut listing_health = crate::sync_safety::ListingHealth::default();
    let scan_result = walk_remote(
        &api,
        &master_key,
        remote_folder_id,
        "",
        &mut remote_files,
        &mut remote_folders,
        &mut listing_health,
        &mut keys,
        remote_spinner.as_ref(),
    )
    .await;
    if let Some(pb) = &remote_spinner {
        pb.finish_and_clear();
    }
    scan_result?;

    // Register this device with the server (best-effort, non-fatal)
    let device_info = crate::device::load_or_create();
    let device_resp = if dry_run {
        None
    } else {
        match api
            .register_device(&device_info.hostname, &device_info.platform, env!("CARGO_PKG_VERSION"))
            .await
        {
            Ok(v) => Some(v),
            // A 401 means the server no longer accepts our session: stop now with
            // a distinct exit code so the service manager does not restart us.
            Err(e) if e == crate::api::SESSION_EXPIRED_MESSAGE => return Err(crate::exit::sync_error(e)),
            Err(_) => None,
        }
    };
    let server_device_id = device_resp
        .as_ref()
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    // Create or reuse a client session for this sync pair
    let session_id: Option<String> = if let Some(ref device_id) = server_device_id {
        if let Some(ref existing) = state.session_id {
            Some(existing.clone())
        } else {
            // Check if a session already exists for this remote_path on this device
            let existing_session = api
                .list_client_sessions()
                .await
                .ok()
                .and_then(|resp| resp.get("sessions").and_then(|v| v.as_array()).cloned())
                .and_then(|sessions| {
                    sessions.into_iter().find(|s| {
                        s.get("remote_path").and_then(|v| v.as_str()) == Some(&remote_path)
                            && s.get("device_id").and_then(|v| v.as_str()) == Some(device_id)
                            && s.get("status").and_then(|v| v.as_str()) != Some("stopped")
                    })
                });

            if let Some(existing) = existing_session {
                let id = existing.get("id").and_then(|v| v.as_str()).map(String::from);
                if let Some(ref id) = id {
                    state.session_id = Some(id.clone());
                }
                id
            } else {
                let default_name = format!(
                    "{}/{}",
                    device_info.hostname,
                    remote_path.trim_start_matches('/').replace('/', "-")
                );
                match api
                    .create_client_session(
                        device_id,
                        &default_name,
                        "sync",
                        Some(&local_dir.display().to_string()),
                        &remote_path,
                    )
                    .await
                {
                    Ok(resp) => {
                        let id = resp.get("id").and_then(|v| v.as_str()).map(String::from);
                        if let Some(ref id) = id {
                            state.session_id = Some(id.clone());
                        }
                        id
                    }
                    Err(e) => {
                        if !ui::is_quiet() {
                            eprintln!("  {} session registration: {e}", "!".custom_color(crate::colors::AMBER));
                        }
                        None
                    }
                }
            }
        }
    } else {
        None
    };

    let moved_conflicts = if root_changed {
        HashSet::new()
    } else {
        reconcile_moves(&local_dir, &mut state, &local_files, &remote_files, dry_run)?
    };
    if !dry_run {
        local_files = walk_local_files(&local_dir)?;
    }
    // Which absent files may be treated as deleted? Only those the server itself
    // reports trashed, from a listing that is not empty/partial/unreadable.
    let absence = plan_absence(
        &api,
        &state,
        &local_files,
        &remote_files,
        &listing_health,
        root_changed,
        allow_mass_delete,
    )
    .await;
    if let Some(msg) = untrusted_notice(&absence, &state, &remote_files) {
        print_notice(&msg);
    }

    let mut all_folders: HashSet<String> = HashSet::new();
    all_folders.extend(local_folders.iter().cloned());
    all_folders.extend(remote_folders.keys().cloned());
    let mut sorted_folders: Vec<String> = all_folders.into_iter().collect();
    sorted_folders.sort_by_key(|s| s.matches('/').count());

    for folder_rel in &sorted_folders {
        if !remote_folders.contains_key(folder_rel) && listing_healthy(&listing_health) {
            let parent_id = parent_remote_id(folder_rel, &remote_folders, remote_folder_id);
            let folder_name = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
            if dry_run {
                println!(
                    "  {} {} {}",
                    "+".custom_color(crate::colors::GREEN_OK),
                    "would create remote folder".custom_color(crate::colors::INK_DIM),
                    folder_rel.custom_color(crate::colors::INK),
                );
            } else {
                let new_id = create_folder(&api, &master_key, folder_name, parent_id).await?;
                remote_folders.insert(folder_rel.clone(), new_id);
                println!(
                    "  {} {} {}",
                    "+".custom_color(crate::colors::GREEN_OK),
                    "remote folder".custom_color(crate::colors::INK_DIM),
                    folder_rel.custom_color(crate::colors::INK),
                );
            }
        }
        if !local_folders.contains(folder_rel) {
            let local_path =
                match crate::safe_path::safe_rel_join(&local_dir, folder_rel, crate::safe_path::Rules::Host) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!(
                            "  {} skipped local folder {folder_rel:?}: {e}",
                            "!".custom_color(crate::colors::AMBER)
                        );
                        continue;
                    }
                };
            if !dry_run {
                std::fs::create_dir_all(&local_path).map_err(|e| format!("mkdir {}: {e}", local_path.display()))?;
            }
            println!(
                "  {} {} {}",
                "+".custom_color(crate::colors::GREEN_OK),
                "local folder".custom_color(crate::colors::INK_DIM),
                folder_rel.custom_color(crate::colors::INK),
            );
        }
    }

    let mut all_files: HashSet<String> = HashSet::new();
    all_files.extend(local_files.keys().cloned());
    all_files.extend(remote_files.keys().cloned());
    // Include manifest keys so a prior-synced file gone from BOTH local and
    // remote (deleted on both sides) is still visited — otherwise its stale
    // entry persists and is silently resurrected on a later round-trip. This
    // makes the `(None, None, prior)` RemoteDelete arm below reachable in the
    // one-shot path (the watch loops reach it via reconcile_remote_deletions).
    all_files.extend(state.files.keys().cloned());
    let mut sorted_files: Vec<String> = all_files.into_iter().collect();
    sorted_files.sort();

    let mut up_count = 0u32;
    let mut down_count = 0u32;
    let mut conflicts = 0u32;
    let mut deletes = 0u32;
    let mut skipped = 0u32;
    // Uploads that errored (not Ctrl-C). Together with `conflicts` this decides
    // whether a one-shot pass may report "synced" and exit 0.
    let mut failed = listing_health.unreadable + u32::from(listing_health.truncated);

    // ── Phase 1: classify each file into an action ─────────────────────────
    enum SyncAction {
        Upload {
            rel: String,
            local: LocalFile,
            replace_id: Option<Uuid>,
        },
        Download {
            rel: String,
            remote: RemoteFile,
        },
        Conflict {
            msg: String,
        },
        Delete {
            rel: String,
            remote: RemoteFile,
        },
        MissingLocal {
            rel: String,
        },
        /// A prior-synced file the server reports TRASHED (tombstone). The local
        /// mirror is byte-identical to the manifest, so it is moved to the
        /// recoverable folder + the manifest entry dropped.
        RemoteDelete {
            rel: String,
        },
        /// A prior-synced file gone from BOTH sides: just drop the stale entry.
        ForgetStale {
            rel: String,
        },
        /// Missing remotely, untouched locally, no deletion record: keep it.
        KeepUnconfirmed {
            rel: String,
        },
        Skip,
    }

    let mut actions: Vec<SyncAction> = Vec::with_capacity(sorted_files.len());

    for rel in &sorted_files {
        if moved_conflicts.contains(rel) {
            actions.push(SyncAction::Conflict {
                msg: format!("{rel} moved remotely; local edits kept"),
            });
            continue;
        }
        let local = local_files.get(rel).cloned();
        let remote = remote_files.get(rel).cloned();
        let prior = trusted_prior(&state, rel, remote.is_some(), root_changed);

        match (local, remote, prior) {
            (Some(l), Some(r), Some(p)) => {
                let local_changed = local_changed_vs_manifest(&l, &p);
                let remote_changed = remote_changed(&r, &p);
                match (local_changed, remote_changed) {
                    (false, false) => {
                        actions.push(SyncAction::Skip);
                    }
                    (true, false) => {
                        actions.push(SyncAction::Upload {
                            rel: rel.clone(),
                            local: l,
                            replace_id: Some(r.id),
                        });
                    }
                    (false, true) => {
                        actions.push(SyncAction::Download {
                            rel: rel.clone(),
                            remote: r,
                        });
                    }
                    (true, true) => {
                        if force {
                            actions.push(SyncAction::Upload {
                                rel: rel.clone(),
                                local: l,
                                replace_id: Some(r.id),
                            });
                        } else {
                            actions.push(SyncAction::Conflict {
                                msg: format!("{rel} (skipped \u{2014} use --force to overwrite)"),
                            });
                        }
                    }
                }
            }
            (Some(l), Some(r), None) => {
                if force {
                    actions.push(SyncAction::Upload {
                        rel: rel.clone(),
                        local: l,
                        replace_id: Some(r.id),
                    });
                } else {
                    actions.push(SyncAction::Conflict {
                        msg: format!("{rel} (exists both sides, no prior sync \u{2014} skipped)"),
                    });
                }
            }
            (Some(l), None, prior) => {
                // Local file present, remote absent. The prior manifest entry
                // distinguishes "user just created this" from "this was synced
                // before and has since been deleted on the server".
                let tombstoned = prior.as_ref().is_some_and(|p| absence.tombstoned(p));
                match classify_remote_absent(Some(&l), prior.as_ref(), tombstoned) {
                    RemoteAbsent::DeleteLocal => {
                        // Synced before + byte-identical to the manifest + the
                        // server reports it trashed: a genuine remote delete.
                        actions.push(SyncAction::RemoteDelete { rel: rel.clone() });
                    }
                    RemoteAbsent::KeepUnconfirmed => {
                        actions.push(SyncAction::KeepUnconfirmed { rel: rel.clone() });
                    }
                    RemoteAbsent::ForgetStale => {
                        actions.push(SyncAction::ForgetStale { rel: rel.clone() });
                    }
                    RemoteAbsent::ReAddLocal => {
                        // Synced before but edited offline → never destroy the
                        // local edit; re-upload it as a new file.
                        actions.push(SyncAction::Upload {
                            rel: rel.clone(),
                            local: l,
                            replace_id: None,
                        });
                    }
                    RemoteAbsent::NotADelete => {
                        // No prior entry → genuinely new local file: upload.
                        actions.push(SyncAction::Upload {
                            rel: rel.clone(),
                            local: l,
                            replace_id: None,
                        });
                    }
                }
            }
            (None, Some(r), prior) => {
                if prior.is_some() {
                    if delete_remote {
                        actions.push(SyncAction::Delete {
                            rel: rel.clone(),
                            remote: r,
                        });
                    } else {
                        actions.push(SyncAction::MissingLocal { rel: rel.clone() });
                    }
                } else {
                    actions.push(SyncAction::Download {
                        rel: rel.clone(),
                        remote: r,
                    });
                }
            }
            (None, None, prior) => {
                if prior.is_some() {
                    // Synced before, now gone from BOTH sides (user deleted it
                    // locally and it's also gone remotely). The local file is
                    // already absent - nothing to touch - but the stale
                    // manifest entry must be dropped so it isn't resurrected.
                    actions.push(SyncAction::ForgetStale { rel: rel.clone() });
                } else {
                    actions.push(SyncAction::Skip);
                }
            }
        }
    }

    // ── Phase 2: print conflicts/skips, collect uploads, execute downloads ─
    // Handle conflicts, missing-local, and skips immediately (no I/O)
    let mut pending_uploads: Vec<(String, LocalFile, Option<Uuid>)> = Vec::new();
    let mut pending_downloads: Vec<(String, RemoteFile)> = Vec::new();
    let mut pending_deletes: Vec<(String, RemoteFile)> = Vec::new();
    // Prior-synced files the server reports trashed: move the local mirror aside.
    let mut pending_remote_deletes: Vec<String> = Vec::new();
    // Prior-synced files gone from both sides: drop the stale manifest entry.
    let mut pending_forget: Vec<String> = Vec::new();
    let mut kept_unconfirmed: Vec<String> = Vec::new();

    for action in actions {
        match action {
            SyncAction::Skip => {
                skipped += 1;
            }
            SyncAction::Conflict { msg } => {
                if !ui::is_json() && !ui::is_quiet() {
                    println!(
                        "  {} {} {}",
                        "\u{26A1}".custom_color(crate::colors::AMBER),
                        "conflict".custom_color(crate::colors::AMBER),
                        msg.custom_color(crate::colors::INK),
                    );
                }
                conflicts += 1;
            }
            SyncAction::MissingLocal { rel } => {
                if !ui::is_json() && !ui::is_quiet() {
                    println!(
                        "  {} {} {}",
                        "?".custom_color(crate::colors::INK_DIM),
                        "missing locally".custom_color(crate::colors::INK_DIM),
                        format!("{rel} (use --delete to trash from vault)").custom_color(crate::colors::INK_DIM),
                    );
                }
            }
            SyncAction::Upload { rel, local, replace_id } => {
                if !listing_healthy(&listing_health) || foreign_missing(&state, &rel, &remote_files) {
                    skipped += 1;
                    continue;
                }
                pending_uploads.push((rel, local, replace_id));
            }
            SyncAction::Download { rel, remote } => {
                pending_downloads.push((rel, remote));
            }
            SyncAction::Delete { rel, remote } => {
                pending_deletes.push((rel, remote));
            }
            SyncAction::RemoteDelete { rel } => {
                pending_remote_deletes.push(rel);
            }
            SyncAction::ForgetStale { rel } => {
                pending_forget.push(rel);
            }
            SyncAction::KeepUnconfirmed { rel } => {
                kept_unconfirmed.push(rel);
                skipped += 1;
            }
        }
    }
    if let Some(msg) = kept_unconfirmed_notice(&kept_unconfirmed) {
        print_notice(&msg);
    }

    // Mass-delete breaker (spec R4): more than max(50, 10 %) removals in one
    // pass is a stop-and-ask, not a sync.
    let mut held_back = 0u32;
    if !allow_mass_delete && crate::sync_safety::breaker_trips(pending_remote_deletes.len(), state.files.len()) {
        print_notice(&breaker_notice(pending_remote_deletes.len(), state.files.len()));
        held_back = pending_remote_deletes.len() as u32;
        pending_remote_deletes.clear();
    }

    // `--delete` against an unreconciled root (#83 review): the manifest's
    // "synced before, now missing locally" entries belong to the other tree, so
    // they say nothing about this folder's same-named files. Trash nothing and
    // fail the pass until `bb sync --rebind`.
    let delete_refused = delete_remote && root_changed;
    if delete_refused {
        print_notice(
            "--delete held back: the remote folder is not the one this directory last synced with, so nothing \
             is trashed. Run `bb sync --rebind` once to accept the new folder, then re-run.",
        );
        // `trusted_prior` already keeps old-tree entries from queuing a Delete;
        // this is the backstop.
        held_back += pending_deletes.len() as u32;
        pending_deletes.clear();
    }

    // ── Phase 3: parallel streaming uploads ────────────────────────────────
    let pending_count = pending_uploads.len();
    let mut interrupted_count = 0u32;
    {
        use futures_util::stream::{self, StreamExt};

        // Rich + TTY → byte-accurate bars (one overall + transient per-file).
        // Otherwise a no-op sink, and the plain per-file lines below stay.
        let show_bars = scan_active && !dry_run && pending_count > 0;
        let progress: Box<dyn crate::upload::ChunkProgress> = if show_bars {
            let total_expected: u64 = pending_uploads
                .iter()
                .map(|(_, l, _)| crate::upload::expected_ciphertext_for(l.size, concurrency as u32))
                .sum();
            Box::new(crate::upload::BarProgress::new(pending_count as u64, total_expected))
        } else {
            Box::new(crate::upload::NoopProgress)
        };

        let upload_results: Vec<Result<(String, FileEntry, u64), String>> =
            stream::iter(pending_uploads.into_iter().map(|(rel, local, replace_id)| {
                let api = &api;
                let master_key = Arc::clone(&master_key);
                let local_dir = &local_dir;
                let remote_folders = &remote_folders;
                let resolved = remote_files.get(&rel).and_then(|r| r.resolved.clone());
                let progress = progress.as_ref();
                let shutdown = Arc::clone(&shutdown);
                async move {
                    if shutdown.load(Ordering::Relaxed) {
                        return Err(crate::upload::INTERRUPTED.to_string());
                    }

                    // Plain status line only when bars are not showing them.
                    if !show_bars && !ui::is_json() && !ui::is_quiet() {
                        let size_str = format_size(local.size);
                        println!(
                            "  {} {} {}",
                            "\u{2191}".custom_color(crate::colors::GREEN_OK),
                            rel.custom_color(crate::colors::INK),
                            format!("({size_str})").custom_color(crate::colors::INK_DIM),
                        );
                    }

                    if dry_run {
                        return Ok((
                            rel,
                            FileEntry {
                                remote_id: Uuid::nil(),
                                remote_version: None,
                                last_mtime: local.mtime,
                                last_size: local.size,
                                last_sync: Utc::now(),
                                content_hash: Some(local.content_hash.clone()),
                            },
                            local.size,
                        ));
                    }

                    // SF-0002 review: a version replace needs the CURRENT
                    // version's content writable too, not just its name. An
                    // unreadable check is treated as read-only: the edit then
                    // becomes a new file instead of rewriting an object we
                    // cannot prove writable.
                    let content_read_only = match resolved
                        .as_ref()
                        .filter(|r| r.origin != KeyOrigin::Derived && !r.read_only)
                    {
                        Some(r) => {
                            let id = r.canonical["id"].as_str().unwrap_or_default();
                            crate::owned_keys::content_is_upper_repair(
                                api,
                                &master_key,
                                id,
                                r.canonical["size_bytes"].as_u64(),
                                r.canonical["chunk_count"].as_u64().unwrap_or(1) as u32,
                            )
                            .await
                            .unwrap_or(true)
                        }
                        None => false,
                    };
                    let replacement = resolved
                        .filter(|r| r.origin != KeyOrigin::Derived && !r.read_only && !content_read_only);
                    if let Some(old_id) = replace_id.filter(|_| replacement.is_none()) {
                        let _ = api.trash_file(&old_id.to_string()).await;
                    }

                    let parent_id = match rel.rsplit_once('/') {
                        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
                        None => Some(remote_folder_id),
                    }
                    .or(Some(remote_folder_id));

                    let file_name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                    let spec = crate::upload::UploadSpec {
                        path: local_dir.join(&rel),
                        file_name,
                        file_id: if replacement.is_some() {
                            replace_id.ok_or("missing replacement id")?
                        } else {
                            Uuid::new_v4()
                        },
                        // Sync trashes the old file (above) and uploads a fresh
                        // id, so this is always a new file — no version replace.
                        base_version_number: replacement
                            .as_ref()
                            .map(|r| replacement_version(&r.canonical))
                            .transpose()?,
                        replace_pin: replacement
                            .as_ref()
                            .map(|r| {
                                crate::crypto::content_pin_with_key(
                                    &r.key,
                                    r.canonical["id"].as_str().unwrap_or(""),
                                    r.canonical["name_encrypted"].as_str().unwrap_or(""),
                                )
                            })
                            .transpose()?,
                        replace_file_key: replacement.as_ref().map(|r| r.key.clone()),
                        parent_id,
                        concurrency: concurrency as u32,
                        shutdown,
                    };
                    // Name the file in the error: "upload failed: <server
                    // message>" alone doesn't say which file to look at (flow-6).
                    let outcome = crate::upload::stream_encrypt_upload(api, master_key, spec, progress)
                        .await
                        .map_err(|e| {
                            if e == crate::upload::INTERRUPTED {
                                e
                            } else {
                                format!("{rel}: {e}")
                            }
                        })?;

                    let entry = FileEntry {
                        remote_id: outcome.server_id,
                        remote_version: replacement
                            .as_ref()
                            .and_then(|r| crate::api::current_version_number(&r.canonical))
                            .and_then(|v| v.checked_add(1)),
                        last_mtime: local.mtime,
                        last_size: local.size,
                        last_sync: Utc::now(),
                        content_hash: Some(local.content_hash.clone()),
                    };
                    Ok((rel, entry, local.size))
                }
            }))
            .buffer_unordered(concurrency)
            .collect()
            .await;

        // Clear the bars before printing the summary / any errors.
        progress.finish_all();

        for result in upload_results {
            match result {
                Ok((rel, entry, size)) => {
                    if !dry_run {
                        state.files.insert(rel, entry);
                    }
                    total_bytes += size;
                    up_count += 1;
                }
                Err(e) if e == crate::upload::INTERRUPTED => {
                    interrupted_count += 1;
                }
                Err(e) => {
                    eprintln!("  {} upload failed: {}", "!".custom_color(crate::colors::RED_ERR), e);
                    failed += 1;
                }
            }
        }
    }

    // If Ctrl-C interrupted the upload phase, persist what we confirmed and
    // stop cleanly — don't start new downloads/deletes or enter watch mode.
    // A re-run resumes from the saved state.
    if shutdown.load(Ordering::Relaxed) {
        if !dry_run {
            state.last_sync = Some(Utc::now());
            let data = serde_json::to_vec_pretty(&state).map_err(|e| format!("serialize state: {e}"))?;
            std::fs::write(&state_path, data).map_err(|e| format!("write state file: {e}"))?;
        }
        let remaining = (pending_count as u32).saturating_sub(up_count);
        if !ui::is_json() {
            println!();
            println!(
                "  {} {} \u{00b7} {} done, {} remaining \u{00b7} re-run to finish",
                "\u{25CF}".custom_color(crate::colors::AMBER),
                "interrupted".custom_color(crate::colors::AMBER),
                up_count,
                remaining.max(interrupted_count),
            );
        }
        // Ctrl-C during the upload phase: the process is exiting without ever
        // entering watch mode, so stop the session this run owns (best-effort)
        // — otherwise the server's heartbeat checker keeps nagging the user.
        if let Some(ref sid) = session_id {
            stop_session_best_effort(&api, sid).await;
        }
        return Ok(());
    }

    // ── Phase 4: sequential downloads ──────────────────────────────────────
    for (rel, remote) in &pending_downloads {
        match do_download(&api, &master_key, &local_dir, rel, remote, &mut state, dry_run).await? {
            DownloadOutcome::Downloaded => down_count += 1,
            // Still uploading elsewhere (or unsafe destination); nothing written.
            DownloadOutcome::Deferred => skipped += 1,
            // The remote copy is untouched; the next pass tries again (task 1916).
            DownloadOutcome::Unreadable => failed += 1,
        }
    }

    // ── Phase 5: sequential deletes ────────────────────────────────────────
    for (rel, remote) in &pending_deletes {
        if !dry_run {
            api.trash_file(&remote.id.to_string()).await?;
            state.files.remove(rel);
            state.removed_index_ids.insert(remote.id.to_string());
        }
        if !ui::is_json() && !ui::is_quiet() {
            println!(
                "  {} {} {}",
                "\u{2717}".custom_color(crate::colors::RED_ERR),
                "trashing remote".custom_color(crate::colors::RED_ERR),
                rel.custom_color(crate::colors::INK),
            );
        }
        deletes += 1;
    }

    // ── Phase 6: remote-delete reconciliation ──────────────────────────────
    // Absence is not deletion (task 1915). A local mirror is removed only when
    // the server itself reports that file's id trashed (a tombstone, checked in
    // `plan_absence`), the file is still byte-identical to the manifest (re-checked
    // right now, not from the scan), and the breaker above did not trip. The
    // file is MOVED to `.bb-trash/` inside the sync folder - never unlinked - so
    // the only live copy can always be put back.
    for rel in &pending_remote_deletes {
        if !dry_run {
            let Some(entry) = state.files.get(rel).cloned() else {
                continue;
            };
            match remove_local_recoverably(&local_dir, rel, &entry) {
                Ok(true) => {
                    state.files.remove(rel);
                    state.removed_index_ids.insert(entry.remote_id.to_string());
                }
                Ok(false) => {
                    print_notice(&format!("{rel} changed since the scan, kept"));
                    continue;
                }
                Err(e) => {
                    print_notice(&e);
                    // Keep the manifest entry so we retry on the next pass.
                    continue;
                }
            }
        }
        if !ui::is_json() && !ui::is_quiet() {
            println!(
                "  {} {} {}",
                "\u{2717}".custom_color(crate::colors::RED_ERR),
                format!("moved to {}/ (deleted remotely)", crate::sync_safety::RECOVERY_DIR)
                    .custom_color(crate::colors::INK_DIM),
                rel.custom_color(crate::colors::INK),
            );
        }
        deletes += 1;
    }
    // Gone from both sides: nothing on disk to touch, just drop the stale entry.
    for rel in &pending_forget {
        if !dry_run {
            state.files.remove(rel);
        }
    }

    state.last_sync = Some(Utc::now());
    if !dry_run && failed == 0 && conflicts == 0 && held_back == 0 && !delete_refused {
        state.account_id = Some(account_id);
    }
    let index_report = if !dry_run {
        let report = finish_owned_sync(&api, &master_key, &mut keys, &mut state, remote_folder_id, root_changed).await;
        let data = serde_json::to_vec_pretty(&state).map_err(|e| format!("serialize state: {e}"))?;
        std::fs::write(&state_path, data).map_err(|e| format!("write state file: {e}"))?;
        report
    } else {
        crate::shared_folder_index::IndexRepairReport::default()
    };

    // Send a final heartbeat for --once mode
    if once {
        if let Some(ref sid) = session_id {
            let file_count = local_files.len() as u64;
            let _ = api
                .send_heartbeat(sid, "idle", Some(file_count), None, Some(total_bytes), None, None, None)
                .await;
        }
    }

    // Whether this invocation will enter the long-lived watch loop below.
    // Everything else — `--once`, `--dry-run`, a Ctrl-C'd pass, or a single
    // json/quiet pass — is about to exit, so its session must be stopped now;
    // the continuous watch path stops its session after the loop unwinds.
    // Computed once and reused for the watch guard so the two stay consistent
    // (a fresh re-check could leave a Ctrl-C'd run as a zombie).
    let will_enter_watch = !once && !dry_run && !shutdown.load(Ordering::Relaxed) && !ui::is_json() && !ui::is_quiet();
    if !will_enter_watch {
        if let Some(ref sid) = session_id {
            stop_session_best_effort(&api, sid).await;
        }
    }

    let elapsed = sync_start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();

    // A pass that left uploads failed or conflicts unresolved is not "synced".
    // A one-shot run (`--once`, json/quiet, …) exits 3 so scripts and cron jobs
    // notice (flow-6: it used to print "✓ synced" and exit 0). A dry run only
    // previews, and the watch loop keeps retrying, so neither fails here.
    let incomplete = failed > 0 || conflicts > 0 || held_back > 0 || delete_refused;
    let incomplete_err = || {
        crate::exit::with_code(
            crate::exit::INCOMPLETE,
            format!(
                "sync incomplete: {} \u{2014} fix the cause and re-run{}",
                match incomplete_summary(failed, conflicts, held_back) {
                    s if s.is_empty() && delete_refused => "--delete refused on a changed remote folder".to_string(),
                    s => s,
                },
                if delete_refused {
                    " (`bb sync --rebind` accepts the new remote folder before --delete may trash anything)"
                } else if held_back > 0 {
                    " (`--allow-mass-delete` confirms the held-back removals)"
                } else if conflicts > 0 {
                    " (`--force` overwrites conflicts with the local copy)"
                } else {
                    ""
                }
            ),
        )
    };
    let fail_one_shot = incomplete && !dry_run && !will_enter_watch;

    if ui::is_json() {
        let json_out = serde_json::json!({
            "ok": !incomplete,
            "uploaded": up_count,
            "downloaded": down_count,
            "failed": failed,
            "conflicts": conflicts,
            "deleted": deletes,
            "held_back": held_back,
            "skipped": skipped,
            "index_written": index_report.written,
            "index_deferred": index_report.deferred,
            "index_partial": index_report.partial,
            "bytes": total_bytes,
            "elapsed_secs": elapsed_secs,
        });
        println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
        return if fail_one_shot { Err(incomplete_err()) } else { Ok(()) };
    }

    if ui::is_quiet() {
        println!(
            "{} up {} down {} conflict{}{}",
            up_count,
            down_count,
            conflicts,
            if failed > 0 {
                format!(" {failed} failed")
            } else {
                String::new()
            },
            if deletes > 0 {
                format!(" {deletes} del")
            } else {
                String::new()
            },
        );
        return if fail_one_shot { Err(incomplete_err()) } else { Ok(()) };
    }

    println!();
    // Rich summary: ✓ synced · 2 ↑ 1 ↓ 1 ⚡ · 3.3 MB · avg 45 MB/s · 1.8s
    let avg_speed = if elapsed_secs > 0.0 {
        total_bytes as f64 / elapsed_secs
    } else {
        0.0
    };

    let mut parts: Vec<String> = Vec::new();
    if up_count > 0 {
        parts.push(format!(
            "{}",
            format!("{up_count} \u{2191}").custom_color(crate::colors::GREEN_OK)
        ));
    }
    if down_count > 0 {
        parts.push(format!(
            "{}",
            format!("{down_count} \u{2193}").custom_color(crate::colors::CYAN)
        ));
    }
    if conflicts > 0 {
        parts.push(format!(
            "{}",
            format!("{conflicts} \u{26A1}").custom_color(crate::colors::AMBER)
        ));
    }
    if failed > 0 {
        parts.push(format!(
            "{}",
            format!("{failed} failed").custom_color(crate::colors::RED_ERR)
        ));
    }
    if held_back > 0 {
        parts.push(format!(
            "{}",
            format!("{held_back} held back").custom_color(crate::colors::AMBER)
        ));
    }
    if deletes > 0 {
        parts.push(format!(
            "{}",
            format!("{deletes} \u{2717}").custom_color(crate::colors::RED_ERR)
        ));
    }

    let counts_str = if parts.is_empty() {
        "no changes".to_string()
    } else {
        parts.join(" ")
    };

    let mut meta_parts: Vec<String> = Vec::new();
    if total_bytes > 0 {
        meta_parts.push(ui::human_size(total_bytes));
    }
    if avg_speed > 0.0 && total_bytes > 0 {
        meta_parts.push(format!("avg {}", ui::human_speed(avg_speed)));
    }
    meta_parts.push(format!("{:.1}s", elapsed_secs));

    let (mark, verdict) = if incomplete {
        (
            "!".custom_color(crate::colors::AMBER),
            "incomplete".custom_color(crate::colors::AMBER),
        )
    } else {
        (
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            "synced".custom_color(crate::colors::GREEN_OK),
        )
    };
    println!(
        "  {} {} {} {} {} {}",
        mark,
        verdict,
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        counts_str,
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        meta_parts.join(" \u{00b7} ").custom_color(crate::colors::INK_DIM),
    );

    if fail_one_shot {
        return Err(incomplete_err());
    }

    // ── Continuous watch mode ───────────────────────────────────────────────
    if will_enter_watch {
        let use_tui = ui::is_rich() && std::io::stdout().is_terminal();

        let daemonized = if use_tui {
            run_tui_watch(
                &local_dir,
                &remote_path,
                remote_folder_id,
                &mut state,
                &state_path,
                session_id.as_deref(),
                local_files.len() as u64,
                total_bytes,
            )
            .await?
        } else {
            run_plain_watch(
                &local_dir,
                &remote_path,
                remote_folder_id,
                &mut state,
                &state_path,
                session_id.as_deref(),
                local_files.len() as u64,
                total_bytes,
                &shutdown,
            )
            .await?;
            false
        };

        // The watch loop has exited (Ctrl-C, or the user quit the TUI). Stop the
        // session this run owns so the heartbeat checker stops nagging — unless
        // the user daemonized, in which case the background daemon now owns it.
        if !daemonized {
            if let Some(ref sid) = session_id {
                stop_session_best_effort(&api, sid).await;
            }
        }
    }

    Ok(())
}

// ── Plain-text watch loop (non-TTY / json / quiet fallback) ───────────────

#[allow(clippy::too_many_arguments)]
async fn run_plain_watch(
    local_dir: &Path,
    remote_path: &str,
    remote_folder_id: Uuid,
    state: &mut SyncState,
    state_path: &Path,
    session_id: Option<&str>,
    file_count: u64,
    total_bytes: u64,
    shutdown: &Arc<AtomicBool>,
) -> Result<(), String> {
    let local_display = local_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| local_dir.display().to_string());

    let dashboard = Arc::new(SyncDashboard {
        local_path: local_display,
        remote_path: remote_path.to_string(),
        file_count: AtomicU64::new(file_count),
        total_bytes: AtomicU64::new(total_bytes),
        is_synced: AtomicBool::new(true),
    });

    println!();
    print_dashboard(&dashboard);

    // Spawn a background heartbeat every 60s while watching
    if let Some(sid) = session_id {
        let hb_api = ApiClient::from_config();
        let hb_sid = sid.to_string();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let _ = hb_api
                    .send_heartbeat(&hb_sid, "watching", None, None, None, None, None, None)
                    .await;
            }
        });
    }

    use notify::RecursiveMode;
    use notify_debouncer_full::new_debouncer;

    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer =
        new_debouncer(std::time::Duration::from_millis(500), None, tx).map_err(|e| format!("watcher: {e}"))?;

    debouncer
        .watch(local_dir, RecursiveMode::Recursive)
        .map_err(|e| format!("watch: {e}"))?;

    eprintln!(
        "  {} watching for changes \u{00b7} press Ctrl+C to stop",
        "\u{00b7}".custom_color(crate::colors::INK_DIM)
    );

    loop {
        // Poll the watch channel with a timeout so a Ctrl-C (which sets the
        // shutdown flag) breaks the loop promptly instead of blocking forever.
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Ok(events)) => {
                let changed: Vec<_> = events
                    .iter()
                    .filter_map(|e| e.paths.first().cloned())
                    .filter(|p| !is_ignored_path(p))
                    .collect();

                if !changed.is_empty() {
                    dashboard.is_synced.store(false, Ordering::Relaxed);
                    let now = chrono::Local::now().format("%H:%M:%S");
                    for path in &changed {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        eprintln!(
                            "  {} {} {}",
                            now.to_string().custom_color(crate::colors::INK_DIM),
                            "\u{2191}".custom_color(crate::colors::GREEN_OK),
                            name.custom_color(crate::colors::INK),
                        );
                    }

                    // Re-run the sync to upload changes
                    let resync_api = ApiClient::from_config();
                    check_sync_account(&resync_api, state).await?;
                    let resync_master = Arc::new(load_master_key()?);
                    let mut resync_local = walk_local_files(local_dir)?;
                    let resync_folders = walk_local_folders(local_dir)?;
                    let mut resync_remote_files: HashMap<String, RemoteFile> = HashMap::new();
                    let mut resync_remote_folders: HashMap<String, Uuid> = HashMap::new();
                    let mut resync_health = crate::sync_safety::ListingHealth::default();
                    let mut keys = sync_keys(state)?;
                    let scan_result = walk_remote(
                        &resync_api,
                        &resync_master,
                        remote_folder_id,
                        "",
                        &mut resync_remote_files,
                        &mut resync_remote_folders,
                        &mut resync_health,
                        &mut keys,
                        None,
                    )
                    .await;
                    scan_result?;

                    let blocked = refresh_watch_reads(
                        &resync_api,
                        &resync_master,
                        local_dir,
                        state,
                        &mut resync_local,
                        &resync_remote_files,
                        remote_folder_id,
                    )
                    .await?;
                    // Ensure remote folders exist
                    let mut all_dirs: HashSet<String> = HashSet::new();
                    all_dirs.extend(resync_folders.iter().cloned());
                    all_dirs.extend(resync_remote_folders.keys().cloned());
                    let mut sorted_dirs: Vec<String> = all_dirs.into_iter().collect();
                    sorted_dirs.sort_by_key(|s| s.matches('/').count());
                    for folder_rel in &sorted_dirs {
                        if !resync_remote_folders.contains_key(folder_rel) && listing_healthy(&resync_health) {
                            let pid = parent_remote_id(folder_rel, &resync_remote_folders, remote_folder_id);
                            let fname = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
                            let new_id = create_folder(&resync_api, &resync_master, fname, pid).await?;
                            resync_remote_folders.insert(folder_rel.clone(), new_id);
                        }
                    }

                    // Reconcile remote deletions FIRST: any prior-synced file no
                    // longer present remotely + byte-identical locally is removed
                    // (shared helper — same decision as the one-shot path). This
                    // must run before the upload loop so a remotely-deleted file
                    // is not re-uploaded (resurrected) by the change scan below.
                    // The watch loop never mass-deletes: the breaker has no
                    // confirmation channel here, so `allow_mass_delete` is false.
                    // A root change the first pass saw is still unreconciled here.
                    let absence = plan_absence(
                        &resync_api,
                        state,
                        &resync_local,
                        &resync_remote_files,
                        &resync_health,
                        root_unreconciled(state, remote_folder_id),
                        false,
                    )
                    .await;
                    if let Some(msg) = untrusted_notice(&absence, state, &resync_remote_files) {
                        print_notice(&msg);
                    }
                    let reconciled = reconcile_remote_deletions(
                        local_dir,
                        state,
                        &resync_local,
                        &resync_remote_files,
                        &absence,
                        false,
                        false,
                    );
                    for msg in &reconciled.notices {
                        print_notice(msg);
                    }
                    let removed = reconciled.removed;
                    let now_del = chrono::Local::now().format("%H:%M:%S");
                    for rel in &removed {
                        eprintln!(
                            "  {} {} {}",
                            now_del.to_string().custom_color(crate::colors::INK_DIM),
                            "\u{2717}".custom_color(crate::colors::RED_ERR),
                            rel.custom_color(crate::colors::INK),
                        );
                    }
                    // Drop the just-removed keys from the pre-reconcile snapshot.
                    // The upload loop below iterates this snapshot; without this,
                    // a removed key (local=Some in the stale snapshot, remote=None,
                    // prior=None after the manifest drop) would look like a fresh
                    // change and re-upload the file we just unlinked (resurrecting
                    // a remote delete, and erroring on the missing file).
                    for rel in &removed {
                        resync_local.remove(rel);
                    }

                    // Upload changed files
                    let mut resync_bytes: u64 = 0;
                    let resync_keys: Vec<String> = resync_local.keys().cloned().collect();
                    for rel in &resync_keys {
                        if !listing_healthy(&resync_health)
                            || blocked.contains(rel)
                            || foreign_missing(state, rel, &resync_remote_files)
                        {
                            continue;
                        }
                        let local = resync_local.get(rel).cloned();
                        let remote = resync_remote_files.get(rel).cloned();
                        let prior =
                            trusted_prior(state, rel, remote.is_some(), root_unreconciled(state, remote_folder_id));

                        if let Some(l) = local {
                            // Use the SAME content-hash-aware change detection as
                            // the one-shot path (local_changed_vs_manifest). A
                            // same-size/same-mtime offline edit that was remote-
                            // deleted is kept by the reconcile above (ReAddLocal)
                            // and MUST still be re-uploaded here — a bare
                            // (mtime,size) compare would miss it.
                            let local_changed = match &prior {
                                Some(p) => local_changed_vs_manifest(&l, p),
                                None => remote.is_none(),
                            };
                            if local_changed {
                                do_upload(
                                    &resync_api,
                                    Arc::clone(&resync_master),
                                    local_dir,
                                    rel,
                                    &l,
                                    remote,
                                    remote_folder_id,
                                    &resync_remote_folders,
                                    state,
                                    false,
                                )
                                .await?;
                                resync_bytes += l.size;
                            }
                        }
                    }

                    finish_owned_sync(
                        &resync_api,
                        &resync_master,
                        &mut keys,
                        state,
                        remote_folder_id,
                        root_unreconciled(state, remote_folder_id),
                    )
                    .await;
                    // Save state
                    state.last_sync = Some(Utc::now());
                    let data = serde_json::to_vec_pretty(&state).map_err(|e| format!("serialize state: {e}"))?;
                    std::fs::write(state_path, data).map_err(|e| format!("write state file: {e}"))?;

                    // Update dashboard
                    let new_local = walk_local_files(local_dir)?;
                    dashboard.file_count.store(new_local.len() as u64, Ordering::Relaxed);
                    dashboard.total_bytes.fetch_add(resync_bytes, Ordering::Relaxed);
                    dashboard.is_synced.store(true, Ordering::Relaxed);
                }
            }
            Ok(Err(errs)) => {
                for e in errs {
                    eprintln!("  {} watch error: {}", "!".custom_color(crate::colors::RED_ERR), e);
                }
            }
        }
    }
    Ok(())
}

// ── TUI watch loop (interactive terminal) ─────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_tui_watch(
    local_dir: &Path,
    remote_path: &str,
    remote_folder_id: Uuid,
    state: &mut SyncState,
    state_path: &Path,
    session_id: Option<&str>,
    file_count: u64,
    total_bytes: u64,
) -> Result<bool, String> {
    use crate::tui::events::{TuiAction, TuiEvent};
    use crate::tui::state::{FileEventStatus, SyncFileEvent};

    let local_display = local_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| local_dir.display().to_string());

    let session_name = format!("{local_display} \u{2192} {remote_path}");

    let (tui_tx, tui_rx) = tokio::sync::mpsc::unbounded_channel::<TuiEvent>();

    // Spawn heartbeat task.
    if let Some(sid) = session_id {
        let hb_api = ApiClient::from_config();
        let hb_sid = sid.to_string();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let _ = hb_api
                    .send_heartbeat(&hb_sid, "watching", None, None, None, None, None, None)
                    .await;
            }
        });
    }

    // Spawn session refresh task (every 30s, sends SessionsRefresh events).
    {
        let tx = tui_tx.clone();
        tokio::spawn(async move {
            let refresh_api = ApiClient::from_config();
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                if let Ok(resp) = refresh_api.list_client_sessions().await {
                    let sessions = parse_sessions_from_json(&resp);
                    let _ = tx.send(TuiEvent::SessionsRefresh(sessions));
                }
            }
        });
    }

    // Do an immediate session refresh so the dashboard has data.
    {
        let tx = tui_tx.clone();
        tokio::spawn(async move {
            let api = ApiClient::from_config();
            if let Ok(resp) = api.list_client_sessions().await {
                let sessions = parse_sessions_from_json(&resp);
                let _ = tx.send(TuiEvent::SessionsRefresh(sessions));
            }
        });
    }

    // Set up the file watcher.
    use notify::RecursiveMode;
    use notify_debouncer_full::new_debouncer;

    let (watch_tx, watch_rx) = std::sync::mpsc::channel();
    let mut debouncer =
        new_debouncer(std::time::Duration::from_millis(500), None, watch_tx).map_err(|e| format!("watcher: {e}"))?;
    debouncer
        .watch(local_dir, RecursiveMode::Recursive)
        .map_err(|e| format!("watch: {e}"))?;

    // Spawn the sync worker that processes file system events and sends TUI updates.
    let sync_tx = tui_tx.clone();
    let sync_local_dir = local_dir.to_path_buf();
    let _sync_remote_path = remote_path.to_string();
    let sync_state_path = state_path.to_path_buf();
    // We need to share SyncState between the sync task and the caller.
    // Use a mutex for simplicity since contention is minimal.
    let shared_state = Arc::new(tokio::sync::Mutex::new(state.clone()));
    let sync_shared_state = Arc::clone(&shared_state);

    tokio::spawn(async move {
        loop {
            // Block on the watcher channel (this is in a tokio task, but the
            // recv is blocking — that's fine because the TUI runs on
            // spawn_blocking and the sync worker is on a separate tokio task).
            let events = match watch_rx.recv() {
                Ok(Ok(events)) => events,
                Ok(Err(_)) => continue,
                Err(_) => break,
            };

            let changed: Vec<_> = events
                .iter()
                .filter_map(|e| e.paths.first().cloned())
                .filter(|p| !is_ignored_path(p))
                .collect();

            if changed.is_empty() {
                continue;
            }

            let now = chrono::Local::now().format("%H:%M:%S").to_string();

            // Notify TUI that sync started.
            let _ = sync_tx.send(TuiEvent::SyncStarted(changed.len() as u32));

            // Send an event per changed file.
            for path in &changed {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                    status: FileEventStatus::Uploading,
                    filename: name,
                    size,
                    timestamp: now.clone(),
                }));
            }

            // Re-run the sync logic.
            let resync_api = ApiClient::from_config();
            let binding_state = sync_shared_state.lock().await.clone();
            if let Err(e) = check_sync_account(&resync_api, &binding_state).await {
                let _ = sync_tx.send(TuiEvent::SyncError(e));
                break;
            }
            let resync_master = match load_master_key() {
                Ok(k) => Arc::new(k),
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };
            let mut resync_local = match walk_local_files(&sync_local_dir) {
                Ok(f) => f,
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };
            let resync_folders = match walk_local_folders(&sync_local_dir) {
                Ok(f) => f,
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };

            let mut resync_remote_files: HashMap<String, RemoteFile> = HashMap::new();
            let mut resync_remote_folders: HashMap<String, Uuid> = HashMap::new();
            let mut resync_health = crate::sync_safety::ListingHealth::default();
            let mut keys = match sync_keys(&binding_state) {
                Ok(k) => k,
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };
            let scan_result = walk_remote(
                &resync_api,
                &resync_master,
                remote_folder_id,
                "",
                &mut resync_remote_files,
                &mut resync_remote_folders,
                &mut resync_health,
                &mut keys,
                None,
            )
            .await;
            if let Err(e) = scan_result {
                let _ = sync_tx.send(TuiEvent::SyncError(e));
                continue;
            }

            // Ensure remote folders exist.
            let mut all_dirs: HashSet<String> = HashSet::new();
            all_dirs.extend(resync_folders.iter().cloned());
            all_dirs.extend(resync_remote_folders.keys().cloned());
            let mut sorted_dirs: Vec<String> = all_dirs.into_iter().collect();
            sorted_dirs.sort_by_key(|s| s.matches('/').count());
            for folder_rel in &sorted_dirs {
                if !resync_remote_folders.contains_key(folder_rel) && listing_healthy(&resync_health) {
                    let pid = parent_remote_id(folder_rel, &resync_remote_folders, remote_folder_id);
                    let fname = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
                    match create_folder(&resync_api, &resync_master, fname, pid).await {
                        Ok(new_id) => {
                            resync_remote_folders.insert(folder_rel.clone(), new_id);
                        }
                        Err(e) => {
                            let _ = sync_tx.send(TuiEvent::SyncError(e));
                        }
                    }
                }
            }

            // Upload changed files.
            let mut sync_state = sync_shared_state.lock().await;
            let blocked = match refresh_watch_reads(
                &resync_api,
                &resync_master,
                &sync_local_dir,
                &mut sync_state,
                &mut resync_local,
                &resync_remote_files,
                remote_folder_id,
            )
            .await
            {
                Ok(b) => b,
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };

            // Reconcile remote deletions FIRST (shared helper — identical
            // decision to the one-shot path and the plain-watch loop): a
            // prior-synced file gone from the remote + byte-identical locally is
            // removed, so the upload loop below cannot resurrect it. Offline-
            // edited files are left for the upload pass to re-add.
            let absence = plan_absence(
                &resync_api,
                &sync_state,
                &resync_local,
                &resync_remote_files,
                &resync_health,
                root_unreconciled(&sync_state, remote_folder_id),
                false,
            )
            .await;
            if let Some(msg) = untrusted_notice(&absence, &sync_state, &resync_remote_files) {
                let _ = sync_tx.send(TuiEvent::SyncError(msg));
            }
            let reconciled = reconcile_remote_deletions(
                &sync_local_dir,
                &mut sync_state,
                &resync_local,
                &resync_remote_files,
                &absence,
                false,
                false,
            );
            for msg in &reconciled.notices {
                let _ = sync_tx.send(TuiEvent::SyncError(msg.clone()));
            }
            let removed = reconciled.removed;
            for rel in &removed {
                let file_name = rel.rsplit('/').next().unwrap_or(rel).to_string();
                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                    status: FileEventStatus::Done,
                    filename: format!("removed {file_name}"),
                    size: 0,
                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                }));
            }
            // Drop the just-removed keys from the pre-reconcile snapshot so the
            // upload loop below cannot re-upload (resurrect) a file we just
            // unlinked — otherwise that key (stale local=Some, remote=None,
            // prior=None) looks like a fresh change and emits a spurious error
            // event when do_upload fails on the missing file.
            for rel in &removed {
                resync_local.remove(rel);
            }

            let resync_keys: Vec<String> = resync_local.keys().cloned().collect();
            for rel in &resync_keys {
                if !listing_healthy(&resync_health)
                    || blocked.contains(rel)
                    || foreign_missing(&sync_state, rel, &resync_remote_files)
                {
                    continue;
                }
                let local = resync_local.get(rel).cloned();
                let remote = resync_remote_files.get(rel).cloned();
                let prior = trusted_prior(
                    &sync_state,
                    rel,
                    remote.is_some(),
                    root_unreconciled(&sync_state, remote_folder_id),
                );

                if let Some(l) = local {
                    // Same content-hash-aware detection as the one-shot + plain-
                    // watch paths (local_changed_vs_manifest): a same-size offline
                    // edit that survived the reconcile (ReAddLocal) is re-uploaded.
                    let local_changed = match &prior {
                        Some(p) => local_changed_vs_manifest(&l, p),
                        None => remote.is_none(),
                    };
                    if local_changed {
                        let file_name = rel.rsplit('/').next().unwrap_or(rel).to_string();
                        match do_upload(
                            &resync_api,
                            Arc::clone(&resync_master),
                            &sync_local_dir,
                            rel,
                            &l,
                            remote,
                            remote_folder_id,
                            &resync_remote_folders,
                            &mut sync_state,
                            false,
                        )
                        .await
                        {
                            Ok(()) => {
                                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                                    status: FileEventStatus::Done,
                                    filename: file_name,
                                    size: l.size,
                                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                }));
                            }
                            Err(e) => {
                                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                                    status: FileEventStatus::Error,
                                    filename: file_name,
                                    size: l.size,
                                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                }));
                                let _ = sync_tx.send(TuiEvent::SyncError(e));
                            }
                        }
                    }
                }
            }

            // Save state.
            let root_changed = root_unreconciled(&sync_state, remote_folder_id);
            finish_owned_sync(
                &resync_api,
                &resync_master,
                &mut keys,
                &mut sync_state,
                remote_folder_id,
                root_changed,
            )
            .await;
            sync_state.last_sync = Some(Utc::now());
            if let Ok(data) = serde_json::to_vec_pretty(&*sync_state) {
                let _ = std::fs::write(&sync_state_path, data);
            }

            // Compute updated totals.
            let new_local = walk_local_files(&sync_local_dir).unwrap_or_default();
            let new_count = new_local.len() as u64;
            let new_bytes: u64 = new_local.values().map(|f| f.size).sum();
            drop(sync_state);

            let _ = sync_tx.send(TuiEvent::SyncFinished {
                file_count: new_count,
                total_bytes: new_bytes,
            });
        }
    });

    // Run the TUI on a blocking thread (it does terminal I/O).
    let tui_session_name = session_name.clone();
    let tui_result =
        tokio::task::spawn_blocking(move || crate::tui::app::run(tui_rx, tui_session_name, file_count, total_bytes))
            .await
            .map_err(|e| format!("TUI task panicked: {e}"))??;

    // Copy shared state back to caller's state.
    let final_state = shared_state.lock().await;
    *state = final_state.clone();

    let daemonized = match tui_result {
        TuiAction::Quit => {
            // Normal exit — the caller should stop the session.
            false
        }
        TuiAction::Daemonize => {
            // User pressed 'd' — spawn daemon and exit. The daemon process now
            // owns the session, so the caller must NOT stop it.
            let remote_str = remote_path.to_string();
            spawn_sync_daemon(local_dir, Some(&remote_str))?;
            true
        }
    };

    Ok(daemonized)
}

/// Parse the JSON response from list_client_sessions into SessionInfo structs.
fn parse_sessions_from_json(resp: &serde_json::Value) -> Vec<crate::tui::state::SessionInfo> {
    use crate::tui::state::SessionInfo;
    let sessions = match resp.get("sessions").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    sessions
        .iter()
        .map(|s| {
            let heartbeat_ago = s
                .get("last_heartbeat")
                .and_then(|v| v.as_str())
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| {
                    let elapsed = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
                    if elapsed.num_seconds() < 60 {
                        format!("{}s ago", elapsed.num_seconds())
                    } else if elapsed.num_minutes() < 60 {
                        format!("{}m ago", elapsed.num_minutes())
                    } else {
                        format!("{}h ago", elapsed.num_hours())
                    }
                });

            SessionInfo {
                name: s.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string(),
                device_name: s.get("device_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                remote_path: s.get("remote_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                status: s
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                files_synced: s.get("files_synced").and_then(|v| v.as_u64()).unwrap_or(0),
                bytes_synced: s.get("bytes_synced").and_then(|v| v.as_u64()).unwrap_or(0),
                last_heartbeat: heartbeat_ago,
                current_file: s.get("current_file").and_then(|v| v.as_str()).map(String::from),
                speed_bps: s.get("speed_bps").and_then(|v| v.as_u64()),
            }
        })
        .collect()
}

// ── Session management helpers ─────────────────────────────────────────────

async fn print_session_status(api: &ApiClient) -> Result<(), String> {
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    if sessions.is_empty() {
        println!(
            "  {} {}",
            "\u{00b7}".custom_color(crate::colors::INK_DIM),
            "no active sync sessions".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    }

    let w = 62;
    println!("{}", crate::ui::box_header("SESSIONS", w));

    for session in sessions {
        let name = session.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
        let session_status = session.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
        let remote_path = session.get("remote_path").and_then(|v| v.as_str()).unwrap_or("");
        let device_name = session.get("device_name").and_then(|v| v.as_str()).unwrap_or("");
        let files_synced = session.get("files_synced").and_then(|v| v.as_u64()).unwrap_or(0);
        let bytes_synced = session.get("bytes_synced").and_then(|v| v.as_u64()).unwrap_or(0);
        let current_file = session.get("current_file").and_then(|v| v.as_str());
        let speed_bps = session.get("speed_bps").and_then(|v| v.as_u64());

        // Compute time since last heartbeat
        let heartbeat_ago = session
            .get("last_heartbeat")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| {
                let elapsed = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
                if elapsed.num_seconds() < 60 {
                    format!("{}s ago", elapsed.num_seconds())
                } else if elapsed.num_minutes() < 60 {
                    format!("{}m ago", elapsed.num_minutes())
                } else {
                    format!("{}h ago", elapsed.num_hours())
                }
            });

        // Status indicator and color
        let (indicator, status_text, status_color) = match session_status {
            "watching" | "idle" => ("\u{25CF}", "watching", crate::colors::GREEN_OK),
            "syncing" | "uploading" | "downloading" => ("\u{25C6}", session_status, crate::colors::AMBER),
            "stopped" => ("\u{25CB}", "stopped", crate::colors::INK_DIM),
            "error" => ("\u{25CF}", "error", crate::colors::RED_ERR),
            _ => ("\u{25CF}", session_status, crate::colors::INK_DIM),
        };

        // Line 1: indicator + name + device → remote + status
        let path_display = remote_path.to_string();

        let line1 = if !device_name.is_empty() {
            format!(
                "  {} {}  {} \u{2192} {}  {}",
                indicator.custom_color(status_color),
                name.custom_color(crate::colors::INK),
                device_name.custom_color(crate::colors::INK_DIM),
                path_display.custom_color(crate::colors::INK_DIM),
                status_text.custom_color(status_color),
            )
        } else {
            format!(
                "  {} {}  \u{2192} {}  {}",
                indicator.custom_color(status_color),
                name.custom_color(crate::colors::INK),
                path_display.custom_color(crate::colors::INK_DIM),
                status_text.custom_color(status_color),
            )
        };
        println!("{}", crate::ui::box_line(&line1, w));

        // Line 2: stats
        let mut detail_parts: Vec<String> = Vec::new();
        if files_synced > 0 {
            detail_parts.push(format!("{} files", format_count(files_synced)));
        }
        if bytes_synced > 0 {
            detail_parts.push(crate::ui::human_size(bytes_synced));
        }
        if let Some(ref ago) = heartbeat_ago {
            detail_parts.push(format!("last heartbeat {ago}"));
        }

        // If syncing, show current file + speed
        if let Some(file) = current_file {
            let speed_str = speed_bps
                .map(|s| format!(" \u{00b7} {}", crate::ui::human_speed(s as f64)))
                .unwrap_or_default();
            let line2 = format!(
                "    {} {file}{speed_str}",
                "uploading".custom_color(crate::colors::INK_DIM),
            );
            println!(
                "{}",
                crate::ui::box_line(&line2.custom_color(crate::colors::INK_DIM).to_string(), w,)
            );
        } else if !detail_parts.is_empty() {
            let line2 = format!("    {}", detail_parts.join(" \u{00b7} "));
            println!(
                "{}",
                crate::ui::box_line(&line2.custom_color(crate::colors::INK_DIM).to_string(), w,)
            );
        }
    }

    println!("{}", crate::ui::box_footer(w));
    Ok(())
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        format!("{n}")
    }
}

/// Best-effort: mark the session this invocation owns as `stopped` so the
/// server's heartbeat checker stops sending the user "no sync activity"
/// notifications after the agent exits. Never fails or blocks the caller — a
/// stop error is logged and swallowed (the request is bounded by the
/// `ApiClient`'s existing per-request timeout). Only ever called with the
/// current invocation's own `session_id`, never another user's session.
async fn stop_session_best_effort(api: &ApiClient, session_id: &str) {
    if let Err(e) = api.stop_client_session(session_id).await {
        if !ui::is_quiet() {
            eprintln!(
                "  {} could not stop sync session on exit: {e}",
                "!".custom_color(crate::colors::AMBER),
            );
        }
    }
}

async fn stop_session_by_name(api: &ApiClient, name: &str) -> Result<(), String> {
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    let name_lower = name.to_lowercase();
    let matching: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|s| {
            s.get("name")
                .and_then(|v| v.as_str())
                .map(|n| {
                    let n_lower = n.to_lowercase();
                    n_lower == name_lower
                        || n_lower.ends_with(&format!("/{name_lower}"))
                        || n_lower.contains(&name_lower)
                })
                .unwrap_or(false)
        })
        .collect();

    if matching.is_empty() {
        return Err(format!("no session found with name \"{name}\""));
    }

    for session in matching {
        let id = session.get("id").and_then(|v| v.as_str()).ok_or("session missing id")?;
        let session_name = session.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");

        // Stop the server-side session
        api.stop_client_session(id).await?;

        // Kill the local daemon process + remove auto-start
        let slug = crate::daemon::slugify(session_name);
        let _ = crate::daemon::stop_daemon(&slug);

        println!(
            "  {} stopped {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            session_name.custom_color(crate::colors::INK),
        );
    }

    Ok(())
}

async fn stop_all_sessions(api: &ApiClient) -> Result<(), String> {
    let device_info = crate::device::load_or_create();
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    // Filter to sessions on this device (match by device_name == hostname)
    let device_sessions: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|s| {
            s.get("device_name")
                .and_then(|v| v.as_str())
                .map(|n| n == device_info.hostname)
                .unwrap_or(false)
        })
        .collect();

    if device_sessions.is_empty() {
        println!(
            "  {} {}",
            "\u{00b7}".custom_color(crate::colors::INK_DIM),
            "no active sessions on this device".custom_color(crate::colors::INK_DIM),
        );
        // Still stop any orphaned local daemons
        let _ = crate::daemon::stop_all_daemons();
        return Ok(());
    }

    let mut stopped = 0u32;
    for session in &device_sessions {
        let id = session.get("id").and_then(|v| v.as_str()).ok_or("session missing id")?;
        let name = session.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");

        // Stop server-side session
        api.stop_client_session(id).await?;

        // Kill local daemon + remove auto-start
        let slug = crate::daemon::slugify(name);
        let _ = crate::daemon::stop_daemon(&slug);

        println!(
            "  {} stopped {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            name.custom_color(crate::colors::INK),
        );
        stopped += 1;
    }

    // Also stop any orphaned daemons (PID files with no matching server session)
    let _ = crate::daemon::stop_all_daemons();

    println!(
        "\n  {} {} session{} stopped on {}",
        "\u{2713}".custom_color(crate::colors::GREEN_OK),
        stopped,
        if stopped == 1 { "" } else { "s" },
        device_info.hostname.custom_color(crate::colors::INK),
    );

    Ok(())
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
    let config = load_config();
    let mk_b64 = config.master_key.ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(
        b64()
            .decode(&mk_b64)
            .map_err(|e| format!("invalid master key in config: {e}"))?,
    );
    if mk_bytes.len() != 32 {
        return Err(format!("master key must be 32 bytes, got {}", mk_bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&mk_bytes);
    Ok(beebeeb_core::kdf::MasterKey::from_bytes(arr))
}

fn rel_path_str(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .collect::<Vec<_>>()
        .join("/")
}

/// Build an amber-accented scan spinner with a live `{pos}`-driven counter,
/// or `None` when progress is suppressed (non-TTY / json / quiet). Draws to
/// stderr so it never corrupts stdout output.
fn make_scan_spinner(active: bool, label: &str, unit: &str) -> Option<indicatif::ProgressBar> {
    if !active {
        return None;
    }
    let pb = indicatif::ProgressBar::new_spinner();
    let template = format!("  {{spinner:.214}} {label} · {{pos}} {unit}");
    pb.set_style(
        indicatif::ProgressStyle::with_template(&template)
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner()),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    Some(pb)
}

/// Walk the local tree like [`walk_local_files`], but with two scan-phase
/// optimisations: a live spinner counter, and skip-hashing.
///
/// Skip-hashing: hashing every file with SHA-256 is the bulk of the scan cost.
/// When `rehash` is false and a file's `(mtime, size)` match the prior sync
/// state, the stored `content_hash` is reused and the read+hash is skipped.
/// `--rehash` forces a full hash of every file (catches same-size edits).
fn scan_local_files(
    root: &Path,
    prior: &HashMap<String, FileEntry>,
    rehash: bool,
    scan: Option<&indicatif::ProgressBar>,
) -> Result<HashMap<String, LocalFile>, String> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size = meta.len();
        let rel_str = rel_path_str(rel);

        // Reuse the stored hash when unchanged (unless --rehash forces a read).
        let reused = if rehash {
            None
        } else {
            prior.get(&rel_str).and_then(|p| {
                if p.last_mtime == mtime && p.last_size == size {
                    p.content_hash.clone()
                } else {
                    None
                }
            })
        };
        let content_hash = match reused {
            Some(h) => h,
            None => compute_file_hash(path)?,
        };

        if let Some(pb) = scan {
            pb.inc(1);
        }
        out.insert(
            rel_str,
            LocalFile {
                mtime,
                size,
                content_hash,
            },
        );
    }
    Ok(out)
}

fn walk_local_files(root: &Path) -> Result<HashMap<String, LocalFile>, String> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let content_hash = compute_file_hash(path)?;
        out.insert(
            rel_path_str(rel),
            LocalFile {
                mtime,
                size: meta.len(),
                content_hash,
            },
        );
    }
    Ok(out)
}

fn walk_local_folders(root: &Path) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() || path == root {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        out.insert(rel_path_str(rel));
    }
    Ok(out)
}

fn remote_version(remote: &RemoteFile) -> Option<i64> {
    remote
        .resolved
        .as_ref()
        .filter(|r| r.origin != KeyOrigin::Derived)
        .and_then(|r| crate::api::current_version_number(&r.canonical))
}
fn remote_changed(remote: &RemoteFile, prior: &FileEntry) -> bool {
    if remote.id != prior.remote_id {
        return true;
    }
    match (remote_version(remote), prior.remote_version) {
        (Some(current), Some(previous)) => current > previous,
        _ => remote.updated_at > prior.last_sync + chrono::Duration::seconds(1),
    }
}
fn replacement_version(row: &serde_json::Value) -> Result<i32, String> {
    crate::api::current_version_number(row)
        .and_then(|v| i32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| "missing or invalid replacement version".into())
}
fn listing_healthy(health: &crate::sync_safety::ListingHealth) -> bool {
    health.unreadable == 0 && !health.truncated
}
fn foreign_missing(state: &SyncState, rel: &str, remote: &HashMap<String, RemoteFile>) -> bool {
    !remote.contains_key(rel)
        && state
            .files
            .get(rel)
            .is_some_and(|p| state.owned_keys.contains_key(&p.remote_id.to_string()))
}
/// Follow identity across remote moves. Only an unchanged local mirror may be
/// renamed; dirty paths and occupied destinations are held for conflict handling.
fn reconcile_moves(
    root: &Path,
    state: &mut SyncState,
    local: &HashMap<String, LocalFile>,
    remote: &HashMap<String, RemoteFile>,
    dry_run: bool,
) -> Result<HashSet<String>, String> {
    let mut blocked = HashSet::new();
    for (new, r) in remote {
        let old = state
            .files
            .iter()
            .find(|(path, e)| *path != new && e.remote_id == r.id)
            .map(|(p, e)| (p.clone(), e.clone()));
        let Some((old, entry)) = old else {
            continue;
        };
        if local.contains_key(new) || local.get(&old).is_some_and(|l| !local_matches_entry(l, &entry)) {
            blocked.insert(old);
            blocked.insert(new.clone());
            continue;
        }
        if dry_run {
            continue;
        }
        let from =
            crate::safe_path::safe_rel_join(root, &old, crate::safe_path::Rules::Host).map_err(|e| e.to_string())?;
        let to =
            crate::safe_path::safe_rel_join(root, new, crate::safe_path::Rules::Host).map_err(|e| e.to_string())?;
        if from.exists() {
            // Recheck bytes immediately before a local move, as for tombstones.
            if compute_file_hash(&from).ok().as_ref() != entry.content_hash.as_ref() {
                blocked.insert(old);
                blocked.insert(new.clone());
                continue;
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // Never overwrite a destination which appeared after the scan.
            if to.exists() {
                blocked.insert(old);
                blocked.insert(new.clone());
                continue;
            }
            std::fs::rename(from, to).map_err(|e| e.to_string())?;
        }
        state.files.remove(&old);
        state.files.insert(new.clone(), entry);
    }
    Ok(blocked)
}
/// Apply incoming files through the same downloader during an existing watch
/// resync. Local edits keep precedence until the existing conflict flow resolves.
async fn refresh_watch_reads(
    api: &ApiClient,
    master: &beebeeb_core::kdf::MasterKey,
    root: &Path,
    state: &mut SyncState,
    local: &mut HashMap<String, LocalFile>,
    remote: &HashMap<String, RemoteFile>,
    remote_root: Uuid,
) -> Result<HashSet<String>, String> {
    if root_unreconciled(state, remote_root) {
        return Ok(state.files.keys().cloned().collect());
    }
    let mut blocked = reconcile_moves(root, state, local, remote, false)?;
    *local = walk_local_files(root)?;
    for (rel, r) in remote {
        if blocked.contains(rel) {
            continue;
        }
        let prior = state.files.get(rel);
        let changed = prior.is_none_or(|p| remote_changed(r, p));
        let dirty = local
            .get(rel)
            .is_some_and(|l| prior.is_none_or(|p| local_changed_vs_manifest(l, p)));
        if changed && dirty {
            blocked.insert(rel.clone());
            continue;
        }
        if changed || !local.contains_key(rel) && prior.is_none() {
            if let Err(e) = do_download(api, master, root, rel, r, state, false).await {
                print_notice(&format!("{rel}: {e}"));
            }
        }
    }
    *local = walk_local_files(root)?;
    Ok(blocked)
}
/// Read failures never make index/adoption failures fatal to file access.
async fn finish_owned_sync(
    api: &ApiClient,
    master: &beebeeb_core::kdf::MasterKey,
    keys: &mut OwnedKeyResolver,
    state: &mut SyncState,
    root: Uuid,
    root_changed: bool,
) -> crate::shared_folder_index::IndexRepairReport {
    if state.account_id.is_none() {
        state.owned_keys.clear();
        state.dirty_shared_roots.clear();
        state.removed_index_ids.clear();
        return crate::shared_folder_index::IndexRepairReport::default();
    }
    if root_changed {
        return crate::shared_folder_index::IndexRepairReport::default();
    }
    let resolved = keys.resolved();
    for r in &resolved {
        let id = r.canonical["id"].as_str().unwrap_or("");
        let retained = r.canonical["is_folder"] == true || state.files.values().any(|e| e.remote_id.to_string() == id);
        if retained && r.pending_owner_wrap.is_some() {
            if let Err(e) = keys.adopt(api, master, r).await {
                print_notice(&format!("{id}: adoption pending ({e})"));
            }
        }
    }
    for r in keys.resolved() {
        let id = r.canonical["id"].as_str().unwrap_or("").to_string();
        if r.origin != KeyOrigin::Derived {
            let shared_root_id = r
                .shared_root_id
                .or_else(|| state.owned_keys.get(&id).and_then(|r| r.shared_root_id.clone()));
            if let Some(root) = &shared_root_id {
                state.dirty_shared_roots.insert(root.clone());
            }
            state.owned_keys.insert(
                id.clone(),
                OpaqueOwnedKey {
                    canonical: r.canonical,
                    pending_owner_wrap: keys.pending(&id),
                    shared_root_id,
                },
            );
        }
    }
    let entries: Vec<_> = resolved
        .iter()
        .map(|r| (r.canonical["id"].as_str().unwrap_or("").to_string(), r.name.clone()))
        .collect();
    let removed: Vec<_> = state.removed_index_ids.iter().cloned().collect();
    let mut report = match crate::shared_folder_index::update_owner_index(api, master, &entries, &removed).await {
        Ok(r) => {
            state.owner_index_dirty = r.deferred > 0 || r.partial;
            if !state.owner_index_dirty {
                state.removed_index_ids.clear();
            }
            r
        }
        Err(e) => {
            state.owner_index_dirty = true;
            print_notice(&format!("search index deferred: {e}"));
            crate::shared_folder_index::IndexRepairReport {
                deferred: 1,
                ..Default::default()
            }
        }
    };
    state.dirty_shared_roots.insert(root.to_string());
    for id in state.dirty_shared_roots.clone() {
        match crate::shared_folder_index::repair(api, master, keys, &id).await {
            Ok(r) => {
                if r.deferred == 0 && !r.partial {
                    state.dirty_shared_roots.remove(&id);
                }
                report.written += r.written;
                report.deferred += r.deferred;
                report.partial |= r.partial;
            }
            Err(e) => {
                report.deferred += 1;
                print_notice(&format!("shared search index deferred: {e}"));
            }
        }
    }
    if report.partial || report.deferred > 0 {
        let status = if report.partial { "partial" } else { "deferred" };
        print_notice(&format!(
            "search index {status}: {} written; {} deferred",
            report.written, report.deferred
        ));
    }
    report
}

fn parent_remote_id(rel: &str, folders: &HashMap<String, Uuid>, root_id: Uuid) -> Option<Uuid> {
    match rel.rsplit_once('/') {
        Some((parent, _)) => folders.get(parent).copied().or(Some(root_id)),
        None => Some(root_id),
    }
}

/// "1 failed · 2 conflicts" — the counts a non-clean sync pass reports.
fn incomplete_summary(failed: u32, conflicts: u32, held_back: u32) -> String {
    let mut parts = Vec::new();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if held_back > 0 {
        parts.push(format!(
            "{held_back} deletion{} held back",
            if held_back == 1 { "" } else { "s" }
        ));
    }
    if conflicts > 0 {
        let s = if conflicts == 1 { "" } else { "s" };
        parts.push(format!("{conflicts} conflict{s} unresolved"));
    }
    parts.join(" \u{00b7} ")
}

async fn resolve_remote_folder(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    remote_path: &str,
    dry_run: bool,
    keys: &mut OwnedKeyResolver,
) -> Result<Uuid, String> {
    let segments: Vec<&str> = remote_path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err("remote_path cannot be empty or root".to_string());
    }

    let mut current_parent: Option<Uuid> = None;

    for seg in segments {
        let listing = api.list_files(current_parent.map(|u| u.to_string()).as_deref()).await?;
        let items = listing
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or("invalid file listing")?;

        keys.seed(items);
        let mut unreadable = false;
        let mut found: Option<Uuid> = None;
        for item in items {
            let is_folder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
            if !is_folder {
                continue;
            }
            let id_str = match item.get("id").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let id: Uuid = match id_str.parse() {
                Ok(u) => u,
                Err(_) => continue,
            };
            let name = match keys.resolve(api, master_key, item).await {
                Ok(r) => Some(r.name),
                Err(e) if e == CUSTODY_UNAVAILABLE => return Err(e),
                Err(_) => {
                    unreadable = true;
                    None
                }
            };
            if name.as_deref() == Some(seg) {
                found = Some(id);
                break;
            }
        }

        if found.is_none() && (unreadable || crate::sync_safety::listing_truncated(&listing)) {
            return Err("remote folder listing incomplete; cannot create a folder".into());
        }
        match found {
            Some(id) => current_parent = Some(id),
            None => {
                if dry_run {
                    return Err(format!("remote folder '{seg}' does not exist (dry-run, cannot create)"));
                }
                let new_id = create_folder(api, master_key, seg, current_parent).await?;
                println!(
                    "  {} {} {}",
                    "+".custom_color(crate::colors::GREEN_OK),
                    "remote folder".custom_color(crate::colors::INK_DIM),
                    seg.custom_color(crate::colors::INK),
                );
                current_parent = Some(new_id);
            }
        }
    }

    current_parent.ok_or_else(|| "path resolution failed".to_string())
}

async fn create_folder(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    name: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, String> {
    let new_id = Uuid::new_v4();
    let name_enc = beebeeb_core::encrypt::encrypt_name(master_key, &new_id.to_string(), name, None)
        .map_err(|e| format!("encrypt folder name: {e}"))?;
    let result = api.create_folder(&name_enc, parent_id, Some(new_id)).await?;
    let id_str = result.get("id").and_then(|v| v.as_str()).ok_or("missing folder id")?;
    id_str.parse().map_err(|e| format!("invalid folder id: {e}"))
}

#[allow(clippy::too_many_arguments)]
async fn walk_remote(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    folder_id: Uuid,
    prefix: &str,
    files: &mut HashMap<String, RemoteFile>,
    folders: &mut HashMap<String, Uuid>,
    health: &mut crate::sync_safety::ListingHealth,
    keys: &mut OwnedKeyResolver,
    scan: Option<&indicatif::ProgressBar>,
) -> Result<(), String> {
    let listing = api.list_files(Some(&folder_id.to_string())).await?;
    if crate::sync_safety::listing_truncated(&listing) {
        health.truncated = true;
    }
    let items = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("invalid file listing")?;

    keys.seed(items);
    for item in items {
        let Some(id_str) = item["id"].as_str() else {
            health.unreadable += 1;
            continue;
        };
        let Ok(id) = Uuid::parse_str(id_str) else {
            health.unreadable += 1;
            continue;
        };
        let is_folder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let resolved = match keys.resolve(api, master_key, item).await {
            Ok(r) => r,
            Err(e) if e == CUSTODY_UNAVAILABLE => return Err(e),
            Err(e) => {
                health.unreadable += 1;
                eprintln!("cannot open item {id_str}: {e}");
                continue;
            }
        };
        let is_folder = resolved.canonical["is_folder"].as_bool().unwrap_or(is_folder);
        let name = resolved.name.clone();
        // A remote name becomes a local path segment (and part of every `rel`
        // below), so a name that could climb out of the sync root or smuggle a
        // separator is skipped with a warning, never joined (task 1733).
        if let Err(e) = crate::safe_path::check_component(&name, crate::safe_path::Rules::Host) {
            eprintln!(
                "  {} skipped remote {} {name:?}: unsafe name ({e})",
                "!".custom_color(crate::colors::AMBER),
                if is_folder { "folder" } else { "file" },
            );
            health.unreadable += 1;
            continue;
        }

        if let Some(pb) = scan {
            pb.inc(1);
        }

        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };

        if is_folder {
            if folders.values().any(|previous| *previous == id)
                || files.contains_key(&rel)
                || folders.contains_key(&rel)
            {
                health.unreadable += 1;
                continue;
            }
            folders.insert(rel.clone(), id);
            Box::pin(walk_remote(
                api, master_key, id, &rel, files, folders, health, keys, scan,
            ))
            .await?;
        } else {
            let updated_at_str = resolved
                .canonical
                .get("updated_at")
                .or_else(|| resolved.canonical.get("created_at"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let updated_at = DateTime::parse_from_rfc3339(updated_at_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let chunk_count = resolved
                .canonical
                .get("chunk_count")
                .and_then(|v| v.as_u64())
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(1);
            if files.contains_key(&rel) || folders.contains_key(&rel) {
                health.unreadable += 1;
                continue;
            }
            let pin = if resolved.origin == KeyOrigin::Derived {
                crate::crypto::content_pin(
                    master_key,
                    id_str,
                    resolved.canonical["name_encrypted"].as_str().unwrap_or(""),
                )
            } else {
                crate::crypto::content_pin_with_key(
                    &resolved.key,
                    id_str,
                    resolved.canonical["name_encrypted"].as_str().unwrap_or(""),
                )
            };
            files.insert(
                rel,
                RemoteFile {
                    id,
                    chunk_count,
                    updated_at,
                    pin,
                    resolved: Some(resolved),
                },
            );
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn do_upload(
    api: &ApiClient,
    master_key: Arc<beebeeb_core::kdf::MasterKey>,
    local_dir: &Path,
    rel: &str,
    local: &LocalFile,
    remote: Option<RemoteFile>,
    root_remote_id: Uuid,
    remote_folders: &HashMap<String, Uuid>,
    state: &mut SyncState,
    dry_run: bool,
) -> Result<(), String> {
    let size_str = format_size(local.size);
    if !ui::is_json() && !ui::is_quiet() {
        println!(
            "  {} {} {}",
            "\u{2191}".custom_color(crate::colors::GREEN_OK),
            rel.custom_color(crate::colors::INK),
            format!("({size_str})").custom_color(crate::colors::INK_DIM),
        );
    }

    if dry_run {
        return Ok(());
    }

    // SF-0002 review: the name may read normally while the CURRENT version's
    // content is still uppercase-sealed; such an edit becomes a new file.
    let candidate = remote.as_ref().and_then(|r| r.resolved.as_ref());
    let content_read_only = match candidate.filter(|r| r.origin != KeyOrigin::Derived && !r.read_only) {
        Some(r) => {
            let id = r.canonical["id"].as_str().unwrap_or_default();
            crate::owned_keys::content_is_upper_repair(
                api,
                &master_key,
                id,
                r.canonical["size_bytes"].as_u64(),
                r.canonical["chunk_count"].as_u64().unwrap_or(1) as u32,
            )
            .await
            .unwrap_or(true)
        }
        None => false,
    };
    let replacement =
        candidate.filter(|r| r.origin != KeyOrigin::Derived && !r.read_only && !content_read_only);
    if let Some(old_id) = remote.as_ref().filter(|_| replacement.is_none()).map(|r| r.id) {
        let _ = api.trash_file(&old_id.to_string()).await;
    }

    let parent_id = match rel.rsplit_once('/') {
        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
        None => Some(root_remote_id),
    }
    .or(Some(root_remote_id));

    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    let local_path = local_dir.join(rel);
    let new_id = if let Some(r) = replacement {
        let spec = crate::upload::UploadSpec {
            path: local_path,
            file_name: file_name.into(),
            file_id: remote.as_ref().unwrap().id,
            base_version_number: Some(replacement_version(&r.canonical)?),
            replace_pin: Some(remote.as_ref().unwrap().pin.clone()?),
            replace_file_key: Some(r.key.clone()),
            parent_id,
            concurrency: 1,
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        crate::upload::stream_encrypt_upload(api, master_key, spec, &crate::upload::NoopProgress)
            .await?
            .server_id
    } else {
        upload_file_to(api, master_key, &local_path, file_name, parent_id).await?
    };

    state.files.insert(
        rel.to_string(),
        FileEntry {
            remote_id: new_id,
            remote_version: remote.as_ref().and_then(remote_version).and_then(|v| v.checked_add(1)),
            last_mtime: local.mtime,
            last_size: local.size,
            last_sync: Utc::now(),
            content_hash: Some(local.content_hash.clone()),
        },
    );

    Ok(())
}

/// How one sync download ended.
#[derive(Debug, PartialEq, Eq)]
enum DownloadOutcome {
    Downloaded,
    /// Not downloaded this pass and nothing written: another device is still
    /// uploading it, or the destination failed the containment check.
    Deferred,
    /// The bytes could not be decrypted. The remote copy was NOT touched and no
    /// local file or manifest entry was written; the next pass tries again.
    Unreadable,
}

impl DownloadOutcome {
    /// How a typed download failure ends one file's turn in a pass. `None`: not a decision for the
    /// pass, the caller handles the failure as it always did (task 2173: a daemon must not die of a
    /// file it cannot read, or of one that changed under it).
    fn of_failure(e: &crate::download::DownloadError) -> Option<Self> {
        use crate::download::DownloadError;
        match e {
            DownloadError::Unreadable(_) => Some(Self::Unreadable),
            DownloadError::Changed(_) => Some(Self::Deferred),
            DownloadError::Other(_) => None,
        }
    }
}

/// Is this download error the API's answer that the file is still being uploaded (a 409 from
/// `/download`, rendered by the API client as `download failed (409 Conflict): …`)? Matched on that
/// exact prefix, never on a bare "409": error texts carry random ids, and about one run in a hundred a
/// decrypt failure containing `…409…` in a uuid was reported as "still uploading" and the pass exited 0.
fn is_still_uploading(e: &str) -> bool {
    e.contains("still in progress") || e.starts_with("download failed (409")
}

/// Does this download error mean "could not decrypt" (as opposed to a network
/// or server failure)? Both are reported and retried; neither trashes anything.
fn is_decrypt_failure(e: &str) -> bool {
    e.contains("key derivation") || e.contains("decrypt")
}

async fn do_download(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    local_dir: &Path,
    rel: &str,
    remote: &RemoteFile,
    state: &mut SyncState,
    dry_run: bool,
) -> Result<DownloadOutcome, String> {
    if !ui::is_json() && !ui::is_quiet() {
        println!(
            "  {} {}",
            "\u{2193}".custom_color(crate::colors::CYAN),
            rel.custom_color(crate::colors::INK),
        );
    }

    if dry_run {
        return Ok(DownloadOutcome::Downloaded);
    }

    // Task 1943: no read pin, no read. The file's metadata could not be read, so
    // its chunk format is unknown. Like a download that fails to decrypt (1916)
    // it is reported, the remote copy is left alone, and a later pass retries.
    if let Err(e) = &remote.pin {
        eprintln!(
            "  {} {} \u{2014} {e}. The remote copy is untouched; will retry next pass.",
            "!".custom_color(crate::colors::RED_ERR),
            rel.custom_color(crate::colors::INK),
        );
        return Ok(DownloadOutcome::Unreadable);
    }

    // Resolve the destination through the containment check: a symlinked
    // subdirectory inside the sync root must not redirect a download elsewhere.
    let out_path = match crate::safe_path::safe_rel_join(local_dir, rel, crate::safe_path::Rules::Host) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "  {} skipped download of {rel:?}: {e}",
                "!".custom_color(crate::colors::AMBER)
            );
            return Ok(DownloadOutcome::Deferred);
        }
    };
    let downloaded = download_to(api, master_key, remote, &out_path).await;
    // Typed failures first: no message text decides whether a daemon survives (task 2173).
    if let Err(e) = &downloaded {
        if let Some(outcome) = DownloadOutcome::of_failure(e) {
            let then = match outcome {
                DownloadOutcome::Deferred => "will pick up the new version next pass",
                _ => "the remote copy is untouched; will retry next pass",
            };
            eprintln!(
                "  {} {} \u{2014} {e}; {then}.",
                "!".custom_color(crate::colors::RED_ERR),
                rel.custom_color(crate::colors::INK),
            );
            return Ok(outcome);
        }
    }
    match downloaded.map_err(String::from) {
        Ok(()) => {}
        Err(e) if is_still_uploading(&e) => {
            // A partially-uploaded remote (this or another device still pushing).
            // Do NOT trash it - that throws away resumable progress and forces a
            // restart from byte 0. Leave the partial: the upload side resumes via
            // /upload/status (skipping present chunks), and a later sync sees the
            // completed file.
            if !ui::is_quiet() {
                use colored::Colorize;
                eprintln!(
                    "  {} {} \u{2014} still uploading, leaving partial to resume",
                    "!".custom_color(crate::colors::AMBER),
                    rel.custom_color(crate::colors::INK_DIM),
                );
            }
            return Ok(DownloadOutcome::Deferred);
        }
        Err(e) if is_decrypt_failure(&e) => {
            // Task 1916 (deletion-safety spec C3): a download that fails to
            // decrypt says something about THIS client (an older bb meeting a
            // newer chunk format, a wrong key), not about the file. The remote
            // copy may be the only copy there is: leave it alone, report it, and
            // try again on a later pass.
            eprintln!(
                "  {} {} \u{2014} could not decrypt ({e}). The remote copy is untouched; will retry next pass.",
                "!".custom_color(crate::colors::RED_ERR),
                rel.custom_color(crate::colors::INK),
            );
            return Ok(DownloadOutcome::Unreadable);
        }
        Err(e) => return Err(e),
    }

    let meta = std::fs::metadata(&out_path).map_err(|e| format!("stat {}: {e}", out_path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let content_hash = compute_file_hash(&out_path).ok();
    state.files.insert(
        rel.to_string(),
        FileEntry {
            remote_id: remote.id,
            remote_version: remote_version(remote),
            last_mtime: mtime,
            last_size: meta.len(),
            last_sync: Utc::now(),
            content_hash,
        },
    );

    Ok(DownloadOutcome::Downloaded)
}

/// Streaming, constant-memory upload of a single file. Thin wrapper over
/// [`crate::upload::stream_encrypt_upload`] used by the watch loops; the main
/// sync path calls the driver directly so it can attach progress bars. Uploads
/// here are not tied to the main Ctrl-C flag (watch-mode Ctrl-C hard-exits), so
/// a fresh, never-tripped shutdown flag is used.
async fn upload_file_to(
    api: &ApiClient,
    master_key: Arc<beebeeb_core::kdf::MasterKey>,
    file_path: &Path,
    file_name: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, String> {
    let spec = crate::upload::UploadSpec {
        path: file_path.to_path_buf(),
        file_name: file_name.to_string(),
        file_id: Uuid::new_v4(),
        // Watch-loop uploads are always new files (fresh id) → no replace.
        base_version_number: None,
        replace_pin: None,
        replace_file_key: None,
        parent_id,
        // Single in-flight file in this watch-loop wrapper → full Cli cap.
        concurrency: 1,
        shutdown: Arc::new(AtomicBool::new(false)),
    };
    let outcome = crate::upload::stream_encrypt_upload(api, master_key, spec, &crate::upload::NoopProgress).await?;
    Ok(outcome.server_id)
}

async fn download_to(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    remote: &RemoteFile,
    out_path: &Path,
) -> Result<(), crate::download::DownloadError> {
    let pin = remote.pin.clone()?;
    let file_id_str = remote.id.to_string();
    let name = out_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&file_id_str)
        .to_string();
    // Streaming, constant-memory decrypt. The "↓ rel" line is already printed by
    // the caller, so use a no-op progress sink here.
    if let Some(r) = remote.resolved.as_ref().filter(|r| r.origin != KeyOrigin::Derived) {
        crate::download::stream_download_decrypt_with_file_key(
            api,
            master_key,
            r.key.clone(),
            &file_id_str,
            pin,
            remote.chunk_count,
            out_path,
            &crate::upload::NoopProgress,
            &name,
        )
        .await?;
        return Ok(());
    }
    crate::download::stream_download_decrypt(
        api,
        master_key,
        &file_id_str,
        pin,
        remote.chunk_count,
        out_path,
        &crate::upload::NoopProgress,
        &name,
    )
    .await?;

    Ok(())
}

fn normalize_remote_path(raw: &str) -> String {
    let path = raw.trim_end_matches('/');
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
            let rest = rest.trim_start_matches('/');
            return if rest.is_empty() {
                "/".to_string()
            } else {
                format!("/{rest}")
            };
        }
    }
    if path.is_empty() || path == "/" {
        return "/".to_string();
    }
    if !path.starts_with('/') {
        format!("/{path}")
    } else {
        path.to_string()
    }
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} kB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ── Daemon spawning ────────────────────────────────────────────────────────

/// Spawn a sync daemon and optionally install platform auto-start.
///
/// Builds a session name from the local directory and remote path, computes
/// a filesystem slug, spawns `bb sync <local> <remote>` as a detached
/// process, writes a PID file, and installs a LaunchAgent (macOS) or
/// systemd user unit (Linux) for auto-start on login.
fn spawn_sync_daemon(local_dir: &Path, remote_path: Option<&str>) -> Result<(), String> {
    let local_dir = std::fs::canonicalize(local_dir).map_err(|e| format!("cannot resolve path: {e}"))?;

    let remote = remote_path.unwrap_or("/");
    let remote_normalized = normalize_remote_path(remote);

    // Build a session-like name for the slug
    let device_info = crate::device::load_or_create();
    let session_name = format!(
        "{}/{}",
        device_info.hostname,
        remote_normalized.trim_start_matches('/').replace('/', "-")
    );
    let slug = crate::daemon::slugify(&session_name);

    // Check if already running
    if let Some(pid) = crate::daemon::is_daemon_running(&slug)? {
        println!(
            "  {} daemon already running (PID {})",
            "\u{25CF}".custom_color(crate::colors::GREEN_OK),
            pid,
        );
        println!("  {} bb sync --status", "status".custom_color(crate::colors::INK_DIM),);
        return Ok(());
    }

    // Spawn the daemon process
    let pid = crate::daemon::spawn_daemon(&local_dir, &remote_normalized, &slug)?;

    // Install platform auto-start (best-effort, non-fatal)
    if let Err(e) = crate::daemon::install_launchagent(&local_dir, &remote_normalized, &slug) {
        if !ui::is_quiet() {
            eprintln!("  {} LaunchAgent install: {e}", "!".custom_color(crate::colors::AMBER),);
        }
    }
    if let Err(e) = crate::daemon::install_systemd_unit(&local_dir, &remote_normalized, &slug) {
        if !ui::is_quiet() {
            eprintln!("  {} systemd unit install: {e}", "!".custom_color(crate::colors::AMBER),);
        }
    }

    // Success output
    println!(
        "  {} {} running as daemon (PID {})",
        "\u{25CF}".custom_color(crate::colors::GREEN_OK),
        session_name.custom_color(crate::colors::INK),
        pid,
    );
    println!(
        "  {} {} \u{2192} {}",
        "folder".custom_color(crate::colors::INK_DIM),
        local_dir.display().to_string().custom_color(crate::colors::INK),
        remote_normalized.custom_color(crate::colors::INK_DIM),
    );

    let log_dir = crate::daemon::log_dir()?;
    println!(
        "  {} {}",
        "logs".custom_color(crate::colors::INK_DIM),
        log_dir
            .join(format!("{slug}.log"))
            .display()
            .to_string()
            .custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {} bb sync --status to check, bb sync --stop-session \"{}\" to stop",
        "hint".custom_color(crate::colors::INK_DIM),
        session_name,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Make a unique scratch directory under the system temp dir (no extra
    /// test-only dependency). Caller removes it.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bb-sync-test-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(dir: &Path, rel: &str, contents: &[u8]) -> LocalFile {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        LocalFile {
            mtime,
            size: meta.len(),
            content_hash: compute_file_hash(&path).unwrap(),
        }
    }

    /// A manifest entry matching the given local file (the post-sync state).
    fn entry_for(local: &LocalFile) -> FileEntry {
        FileEntry {
            remote_id: Uuid::new_v4(),
            remote_version: None,
            last_mtime: local.mtime,
            last_size: local.size,
            last_sync: Utc::now(),
            content_hash: Some(local.content_hash.clone()),
        }
    }

    fn remote_file() -> RemoteFile {
        RemoteFile {
            id: Uuid::new_v4(),
            resolved: None,
            chunk_count: 1,
            updated_at: Utc::now(),
            pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
        }
    }

    /// Listing trusted, and the server reports EVERY manifest entry trashed.
    fn plan_all(state: &SyncState) -> AbsencePlan {
        AbsencePlan {
            tombstones: state.files.values().map(|e| e.remote_id).collect(),
            untrusted: None,
        }
    }

    /// Listing trusted, but the server reports nothing trashed.
    fn plan_none() -> AbsencePlan {
        AbsencePlan {
            tombstones: HashSet::new(),
            untrusted: None,
        }
    }

    /// `n` previously-synced files `f0.txt`.. on disk and in the manifest.
    fn synced_files(dir: &Path, n: usize) -> (SyncState, HashMap<String, LocalFile>) {
        let mut state = SyncState::default();
        let mut local_files = HashMap::new();
        for i in 0..n {
            let rel = format!("f{i}.txt");
            let local = write_file(dir, &rel, format!("content {i}").as_bytes());
            state.files.insert(rel.clone(), entry_for(&local));
            local_files.insert(rel, local);
        }
        (state, local_files)
    }

    fn recovered_names(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.join(crate::sync_safety::RECOVERY_DIR)];
        while let Some(p) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&p) else { continue };
            for e in rd.flatten() {
                if e.path().is_dir() {
                    stack.push(e.path());
                } else {
                    out.push(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
        out.sort();
        out
    }

    // ── pure decision function ─────────────────────────────────────────────

    #[test]
    fn classify_prior_synced_unchanged_with_tombstone_is_remote_delete() {
        let dir = scratch_dir("classify-del");
        let local = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&local);
        assert_eq!(
            classify_remote_absent(Some(&local), Some(&entry), true),
            RemoteAbsent::DeleteLocal,
            "a prior-synced, byte-identical local file the server reports trashed is a remote delete"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_prior_synced_modified_is_readd() {
        let dir = scratch_dir("classify-readd");
        let original = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&original);
        // Simulate an offline edit: same path, different bytes → different hash.
        let edited = write_file(&dir, "a.txt", b"hello, edited offline");
        assert_ne!(edited.content_hash, entry.content_hash.clone().unwrap());
        assert_eq!(
            classify_remote_absent(Some(&edited), Some(&entry), true),
            RemoteAbsent::ReAddLocal,
            "a locally-modified file must NOT be deleted on remote-absence"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_new_local_is_not_a_delete() {
        let dir = scratch_dir("classify-new");
        let local = write_file(&dir, "fresh.txt", b"brand new");
        assert_eq!(
            classify_remote_absent(Some(&local), None, true),
            RemoteAbsent::NotADelete,
            "a genuinely-new local file (no manifest entry) is an upload, not a delete"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_matches_entry_uses_hash_then_size_mtime() {
        let dir = scratch_dir("match");
        let local = write_file(&dir, "a.txt", b"data");
        let mut entry = entry_for(&local);
        assert!(local_matches_entry(&local, &entry));
        // Hash mismatch → not a match (even if size/mtime line up).
        entry.content_hash = Some("deadbeef".into());
        assert!(!local_matches_entry(&local, &entry));
        // No stored hash → fall back to (mtime, size).
        entry.content_hash = None;
        assert!(local_matches_entry(&local, &entry));
        entry.last_size += 1;
        assert!(!local_matches_entry(&local, &entry));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── reconcile helper (used by both watch loops) ────────────────────────

    /// (a) prior-synced + remote gone + local byte-identical
    ///     ⇒ local removed, manifest entry removed, nothing re-uploaded.
    #[test]
    fn reconcile_removes_byte_identical_remote_delete() {
        let dir = scratch_dir("reconcile-del");
        let local = write_file(&dir, "a.txt", b"hello");
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry_for(&local));

        let mut local_files = HashMap::new();
        local_files.insert("a.txt".to_string(), local);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert_eq!(removed, vec!["a.txt".to_string()]);
        assert!(!dir.join("a.txt").exists(), "local mirror must leave the sync folder");
        assert_eq!(recovered_names(&dir), vec!["a.txt".to_string()], "and be recoverable");
        assert!(!state.files.contains_key("a.txt"), "manifest entry must be dropped");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// (b) prior-synced + remote gone but local MODIFIED offline
    ///     ⇒ NOT deleted, manifest entry retained (re-add path).
    #[test]
    fn reconcile_keeps_offline_modified_file() {
        let dir = scratch_dir("reconcile-keep");
        let original = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&original);
        // Edit offline AFTER the manifest snapshot.
        let edited = write_file(&dir, "a.txt", b"hello, locally edited");

        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry);

        let mut local_files = HashMap::new();
        local_files.insert("a.txt".to_string(), edited);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert!(removed.is_empty(), "an offline-edited file must NOT be removed");
        assert!(dir.join("a.txt").exists(), "local edit must survive reconciliation");
        assert!(
            state.files.contains_key("a.txt"),
            "manifest entry retained so the upload pass re-adds it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// (c) genuinely-new local file (no manifest entry)
    ///     ⇒ untouched by reconciliation (it is uploaded by the normal pass).
    #[test]
    fn reconcile_ignores_new_local_file() {
        let dir = scratch_dir("reconcile-new");
        let local = write_file(&dir, "new.txt", b"fresh content");

        let mut state = SyncState::default(); // no manifest entry for new.txt
        let mut local_files = HashMap::new();
        local_files.insert("new.txt".to_string(), local);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert!(removed.is_empty(), "a new local file is never a remote delete");
        assert!(dir.join("new.txt").exists(), "new local file must remain on disk");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file still present remotely is never touched by reconciliation.
    #[test]
    fn reconcile_leaves_present_remote_alone() {
        let dir = scratch_dir("reconcile-present");
        let local = write_file(&dir, "a.txt", b"hello");
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry_for(&local));

        let mut local_files = HashMap::new();
        local_files.insert("a.txt".to_string(), local);
        let mut remote_files = HashMap::new();
        remote_files.insert("a.txt".to_string(), remote_file()); // still on server

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert!(removed.is_empty());
        assert!(dir.join("a.txt").exists());
        assert!(state.files.contains_key("a.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Manifest entry whose local file is already gone (deleted both sides):
    /// no local unlink needed, but the stale manifest entry is dropped.
    #[test]
    fn reconcile_drops_manifest_when_local_already_gone() {
        let dir = scratch_dir("reconcile-gone");
        // Build an entry for a file that does NOT exist on disk.
        let phantom = LocalFile {
            mtime: 0,
            size: 5,
            content_hash: "00".into(),
        };
        let mut state = SyncState::default();
        state.files.insert("gone.txt".to_string(), entry_for(&phantom));

        let local_files: HashMap<String, LocalFile> = HashMap::new(); // not on disk
        let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // not on server

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert_eq!(removed, vec!["gone.txt".to_string()]);
        assert!(!state.files.contains_key("gone.txt"), "stale manifest entry dropped");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// dry_run must be side-effect-free: no unlink, no manifest mutation.
    #[test]
    fn reconcile_dry_run_is_side_effect_free() {
        let dir = scratch_dir("reconcile-dry");
        let local = write_file(&dir, "a.txt", b"hello");
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry_for(&local));

        let mut local_files = HashMap::new();
        local_files.insert("a.txt".to_string(), local);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, true).removed;

        // It still reports the rel, but touches nothing.
        assert_eq!(removed, vec!["a.txt".to_string()]);
        assert!(dir.join("a.txt").exists(), "dry-run must not unlink");
        assert!(state.files.contains_key("a.txt"), "dry-run must not mutate manifest");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── reconcile → upload-snapshot prune (the watch-loop integration) ─────

    /// Models exactly what both watch loops do after a remote delete: capture the
    /// `removed` Vec, prune those keys from the pre-reconcile `resync_local`
    /// snapshot, then run the change-detection that drives the upload loop. The
    /// deleted file must be ABSENT from the pruned snapshot, so no upload is even
    /// considered for it (the BLOCKER: a stale snapshot re-uploaded the unlinked
    /// file every cycle, resurrecting the remote delete and erroring on the
    /// now-missing path).
    #[test]
    fn reconcile_then_prune_drops_deleted_key_from_upload_snapshot() {
        let dir = scratch_dir("prune-snapshot");
        let local = write_file(&dir, "a.txt", b"hello");
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry_for(&local));

        // The pre-reconcile snapshot the upload loop iterates.
        let mut resync_local = HashMap::new();
        resync_local.insert("a.txt".to_string(), local);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &resync_local, &remote_files, &plan, false, false).removed;
        assert_eq!(removed, vec!["a.txt".to_string()]);
        // Local mirror + manifest entry are gone after reconcile.
        assert!(!dir.join("a.txt").exists());
        assert!(!state.files.contains_key("a.txt"));

        // The fix: prune removed keys from the snapshot BEFORE the upload loop.
        for rel in &removed {
            resync_local.remove(rel);
        }
        assert!(
            !resync_local.contains_key("a.txt"),
            "deleted file must be absent from the pruned upload snapshot"
        );

        // Replay the watch upload loop's decision over the pruned snapshot: it
        // must consider ZERO uploads (nothing left to resurrect).
        let mut would_upload: Vec<String> = Vec::new();
        for (rel, l) in &resync_local {
            let remote_present = remote_files.contains_key(rel);
            let changed = match state.files.get(rel) {
                Some(p) => local_changed_vs_manifest(l, p),
                None => !remote_present,
            };
            if changed {
                would_upload.push(rel.clone());
            }
        }
        assert!(
            would_upload.is_empty(),
            "no upload may be attempted for the just-deleted file; got {would_upload:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Data-safety invariant + the HIGH change-detection fix together: a
    /// SAME-SIZE offline edit (identical byte length, different content → different
    /// hash) of a remote-deleted file must (a) survive reconciliation (ReAddLocal,
    /// never deleted) and (b) be detected as changed by `local_changed_vs_manifest`
    /// so the watch upload loop re-uploads it. A bare `(mtime,size)` compare —
    /// the old watch-loop bug — would have missed it and silently lost the edit.
    #[test]
    fn same_size_offline_edit_survives_and_is_re_uploaded() {
        let dir = scratch_dir("same-size-edit");
        // 5-byte original, manifest snapshot taken against it.
        let original = write_file(&dir, "a.txt", b"aaaaa");
        let entry = entry_for(&original);
        // Offline edit: SAME 5-byte size, different content → different hash.
        // Force the mtime to match the manifest so a (mtime,size) compare can't
        // tell them apart — only the content hash distinguishes them.
        let mut edited = write_file(&dir, "a.txt", b"bbbbb");
        edited.mtime = entry.last_mtime;
        assert_eq!(edited.size, original.size, "test premise: identical size");
        assert_eq!(edited.mtime, entry.last_mtime, "test premise: identical mtime");
        assert_ne!(edited.content_hash, original.content_hash, "content differs");

        // (a) reconcile must KEEP it (ReAddLocal), not delete it.
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry.clone());
        let mut resync_local = HashMap::new();
        resync_local.insert("a.txt".to_string(), edited.clone());
        let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &resync_local, &remote_files, &plan, false, false).removed;
        assert!(removed.is_empty(), "an offline edit must never be deleted");
        assert!(dir.join("a.txt").exists(), "the local edit must survive on disk");
        assert!(state.files.contains_key("a.txt"), "manifest entry retained for re-add");

        // Pruning removes nothing (it wasn't deleted), so the key stays.
        for rel in &removed {
            resync_local.remove(rel);
        }
        assert!(resync_local.contains_key("a.txt"));

        // (b) the change detector that gates the upload loop must flag it.
        assert!(
            local_changed_vs_manifest(&edited, &entry),
            "a same-size/same-mtime content edit MUST be detected as changed (hash compare)"
        );
        // And the bare (mtime,size) compare the old watch loop used would NOT
        // have — proving the bug this fix closes.
        let old_buggy_compare = edited.mtime != entry.last_mtime || edited.size != entry.last_size;
        assert!(
            !old_buggy_compare,
            "the old (mtime,size)-only watch compare misses this edit — that was the bug"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── tasks 1915/1916: absence is not deletion ───────────────────────────

    #[test]
    fn classify_identical_without_tombstone_is_kept_not_deleted() {
        let dir = scratch_dir("classify-unconfirmed");
        let local = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&local);
        assert_eq!(
            classify_remote_absent(Some(&local), Some(&entry), false),
            RemoteAbsent::KeepUnconfirmed,
            "absent from the listing but no deletion record: keep"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_edited_file_is_readded_whether_or_not_tombstoned() {
        let dir = scratch_dir("classify-edit-both");
        let original = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&original);
        let edited = write_file(&dir, "a.txt", b"hello again, different");
        for tomb in [true, false] {
            assert_eq!(
                classify_remote_absent(Some(&edited), Some(&entry), tomb),
                RemoteAbsent::ReAddLocal
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_missing_local_only_forgets_the_manifest_entry() {
        let dir = scratch_dir("classify-forget");
        let phantom = write_file(&dir, "p.txt", b"x");
        let entry = entry_for(&phantom);
        for tomb in [true, false] {
            assert_eq!(
                classify_remote_absent(None, Some(&entry), tomb),
                RemoteAbsent::ForgetStale
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// No tombstone => nothing is touched, however absent the file looks.
    #[test]
    fn reconcile_keeps_a_file_without_a_tombstone() {
        let dir = scratch_dir("reconcile-no-tomb");
        let (mut state, local_files) = synced_files(&dir, 3);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(
            &dir,
            &mut state,
            &local_files,
            &remote_files,
            &plan_none(),
            false,
            false,
        );

        assert!(r.removed.is_empty());
        assert_eq!(state.files.len(), 3, "manifest untouched");
        for i in 0..3 {
            assert!(dir.join(format!("f{i}.txt")).exists());
        }
        assert!(recovered_names(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A tombstone removes THAT file only, and into the recoverable folder.
    #[test]
    fn reconcile_moves_only_the_tombstoned_file_to_recovery() {
        let dir = scratch_dir("reconcile-one-tomb");
        let (mut state, local_files) = synced_files(&dir, 3);
        let plan = AbsencePlan {
            tombstones: [state.files["f1.txt"].remote_id].into_iter().collect(),
            untrusted: None,
        };
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

        assert_eq!(r.removed, vec!["f1.txt".to_string()]);
        assert!(!dir.join("f1.txt").exists());
        assert!(dir.join("f0.txt").exists() && dir.join("f2.txt").exists());
        assert!(!state.files.contains_key("f1.txt") && state.files.len() == 2);
        assert_eq!(
            recovered_names(&dir),
            vec!["f1.txt".to_string()],
            "moved aside, not unlinked"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 60 tombstones of 60 tracked is over max(50, 10 %): remove nothing.
    #[test]
    fn reconcile_breaker_holds_back_a_mass_delete() {
        let dir = scratch_dir("reconcile-breaker");
        let (mut state, local_files) = synced_files(&dir, 60);
        let plan = plan_all(&state);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

        assert!(
            r.notices
                .iter()
                .any(|n| n.contains("mass delete held back") && n.contains("--allow-mass-delete")),
            "the trip must be explained: {:?}",
            r.notices
        );
        assert!(r.removed.is_empty());
        assert_eq!(state.files.len(), 60);
        for i in 0..60 {
            assert!(dir.join(format!("f{i}.txt")).exists(), "f{i}.txt must survive");
        }

        // Explicit confirmation lets it through - still recoverably.
        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, true, false);
        assert_eq!(r.removed.len(), 60);
        assert!(!r.notices.iter().any(|n| n.contains("mass delete held back")));
        assert_eq!(recovered_names(&dir).len(), 60);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Exactly the limit (50) is allowed; the breaker trips strictly above it.
    #[test]
    fn reconcile_breaker_allows_exactly_the_limit() {
        let dir = scratch_dir("reconcile-breaker-edge");
        let (mut state, local_files) = synced_files(&dir, 60);
        let plan = AbsencePlan {
            tombstones: (0..50).map(|i| state.files[&format!("f{i}.txt")].remote_id).collect(),
            untrusted: None,
        };
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

        assert_eq!(
            r.removed.len(),
            50,
            "50 tombstones of 60 tracked is within max(50, 10%)"
        );
        assert!(!r.notices.iter().any(|n| n.contains("mass delete held back")));
        assert_eq!(state.files.len(), 10);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The scan can be hours old: a file edited since must be kept, not moved.
    #[test]
    fn reconcile_keeps_a_file_edited_after_the_scan() {
        let dir = scratch_dir("reconcile-edited-since");
        let (mut state, local_files) = synced_files(&dir, 1);
        // `local_files` (the scan) still says identical; the disk has moved on.
        std::fs::write(dir.join("f0.txt"), b"typed after the scan began").unwrap();
        let plan = plan_all(&state);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

        assert!(r.removed.is_empty());
        assert_eq!(
            std::fs::read(dir.join("f0.txt")).unwrap(),
            b"typed after the scan began"
        );
        assert!(state.files.contains_key("f0.txt"));
        assert!(recovered_names(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recovery_folder_is_never_sync_input() {
        assert!(is_ignored_path(Path::new(
            "/home/u/Docs/.bb-trash/20261017-120000/a.txt"
        )));
        assert!(is_ignored_path(Path::new("/home/u/Docs/.bb-trash")));
        assert!(!is_ignored_path(Path::new("/home/u/Docs/a.txt")));
    }

    #[test]
    fn root_is_unreconciled_only_when_bound_to_another_folder() {
        let bound = Uuid::new_v4();
        let mut state = SyncState::default();
        assert!(
            !root_unreconciled(&state, bound),
            "an unbound directory binds on its first sync"
        );
        state.remote_folder_id = Some(bound);
        assert!(!root_unreconciled(&state, bound));
        assert!(
            root_unreconciled(&state, Uuid::new_v4()),
            "bound to another folder: removals stay blocked"
        );
    }

    #[test]
    fn decrypt_failures_are_told_apart_from_other_errors() {
        assert!(is_decrypt_failure("decrypt chunk 0: aead error"));
        assert!(is_decrypt_failure("key derivation failed"));
        assert!(!is_decrypt_failure("download failed (500): oops"));
        assert!(!is_decrypt_failure("download truncated: chunk 1/3"));
    }

    #[test]
    fn a_409_is_a_file_still_being_uploaded_and_nothing_else_is() {
        assert!(is_still_uploading(
            "download failed (409 Conflict): replace_in_progress"
        ));
        assert!(is_still_uploading("upload is still in progress"));
        // Random ids in an error text must not decide it (a uuid may contain "409").
        assert!(!is_still_uploading(
            "decrypt chunk 0 of 7b0c1a52-3f4e-4409-9c61-2f8e5a1d0b37: aead error"
        ));
        assert!(!is_still_uploading(
            "download failed (500 Internal Server Error): 409 widgets"
        ));
        assert!(!is_still_uploading("download failed (403 Forbidden)"));
    }

    #[test]
    fn a_typed_download_failure_ends_one_files_turn_not_the_pass() {
        use crate::download::DownloadError;
        // A chunk that never authenticates: the same outcome as a decrypt failure on the API path.
        assert_eq!(
            DownloadOutcome::of_failure(&DownloadError::Unreadable("x".into())),
            Some(DownloadOutcome::Unreadable)
        );
        // The version changed under the download: not this pass; the next one reads the new version.
        assert_eq!(
            DownloadOutcome::of_failure(&DownloadError::Changed("x".into())),
            Some(DownloadOutcome::Deferred)
        );
        // Everything else is the caller's, as before (an expired session must still stop the pass).
        assert_eq!(DownloadOutcome::of_failure(&DownloadError::Other("x".into())), None);
    }

    // ── walk_remote: what the listing could not show us ────────────────────

    /// In-process `GET /api/v1/files` that answers every request with `entries`.
    async fn mock_listing(entries: Vec<serde_json::Value>) -> ApiClient {
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/api/v1/files",
            get(move || {
                let entries = entries.clone();
                async move { axum::Json(serde_json::json!({ "files": entries })) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        ApiClient::new_for_test(format!("http://{addr}"))
    }

    fn entry(name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": Uuid::new_v4().to_string(),
            "name_encrypted": name,
            "is_folder": false,
            "chunk_count": 1,
            "updated_at": "2020-01-01T00:00:00+00:00",
        })
    }

    async fn walk(api: &ApiClient) -> (HashMap<String, RemoteFile>, crate::sync_safety::ListingHealth) {
        let mk = beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32]);
        let mut files = HashMap::new();
        let mut folders = HashMap::new();
        let mut health = crate::sync_safety::ListingHealth::default();
        walk_remote(
            api,
            &mk,
            Uuid::new_v4(),
            "",
            &mut files,
            &mut folders,
            &mut health,
            &mut OwnedKeyResolver::new(),
            None,
        )
        .await
        .unwrap();
        (files, health)
    }

    #[tokio::test]
    async fn walk_remote_reports_a_complete_listing_as_healthy() {
        let api = mock_listing(vec![entry("a.txt"), entry("b.txt")]).await;
        let (files, health) = walk(&api).await;
        assert_eq!(files.len(), 2);
        assert_eq!(health, crate::sync_safety::ListingHealth::default());
    }

    #[tokio::test]
    async fn walk_remote_counts_names_it_cannot_read() {
        // `{...` is not a plaintext name and not a valid blob: undecryptable.
        // `../x` and `a/b` are unsafe as local path segments.
        let api = mock_listing(vec![
            entry("ok.txt"),
            entry("{not-a-blob"),
            entry("../escape.txt"),
            entry("a/b.txt"),
        ])
        .await;
        let (files, health) = walk(&api).await;
        assert_eq!(files.keys().cloned().collect::<Vec<_>>(), vec!["ok.txt".to_string()]);
        assert_eq!(health.unreadable, 3, "three entries were skipped");
        assert!(!health.truncated);
    }

    #[tokio::test]
    async fn walk_remote_flags_a_listing_cut_short_by_the_cap() {
        // 50,001 entries in one page: the client's 50,000 safety cap cuts it.
        let entries: Vec<_> = (0..50_001).map(|i| entry(&format!("f{i}.txt"))).collect();
        let api = mock_listing(entries).await;
        let (files, health) = walk(&api).await;
        assert_eq!(files.len(), 50_000);
        assert!(health.truncated, "a capped listing must be marked incomplete");
    }
}
