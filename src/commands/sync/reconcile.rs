//! `reconcile` items split out of `sync.rs`.

use super::*;

pub(super) struct SyncDashboard {
    pub(super) local_path: String,
    pub(super) remote_path: String,
    pub(super) file_count: AtomicU64,
    pub(super) total_bytes: AtomicU64,
    pub(super) is_synced: AtomicBool,
}

pub(super) fn print_dashboard(dash: &SyncDashboard) {
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

pub(super) fn is_ignored_path(path: &Path) -> bool {
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
pub(super) struct SyncState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) account_id: Option<String>,
    pub(super) remote_path: Option<String>,
    pub(super) remote_folder_id: Option<Uuid>,
    pub(super) last_sync: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(super) session_id: Option<String>,
    /// The session's client-generated record key (task 1922). Persisted next to
    /// `session_id` so reuse finds the server row by key, never by plaintext.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) session_client_key: Option<Uuid>,
    #[serde(default)]
    pub(super) files: HashMap<String, FileEntry>,
    #[serde(default)]
    pub(super) owned_keys: HashMap<String, OpaqueOwnedKey>,
    #[serde(default)]
    pub(super) dirty_shared_roots: BTreeSet<String>,
    #[serde(default)]
    pub(super) owner_index_dirty: bool,
    #[serde(default)]
    pub(super) removed_index_ids: BTreeSet<String>,
}

/// Ciphertext and envelopes only. Account and root checks precede all reads.
#[derive(Serialize, Deserialize, Clone)]
pub(super) struct OpaqueOwnedKey {
    pub(super) canonical: serde_json::Value,
    pub(super) pending_owner_wrap: Option<Vec<u8>>,
    pub(super) shared_root_id: Option<String>,
}

pub(super) fn sync_keys(state: &SyncState) -> Result<OwnedKeyResolver, String> {
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
pub(super) struct FileEntry {
    pub(super) remote_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) remote_version: Option<i64>,
    pub(super) last_mtime: i64,
    pub(super) last_size: u64,
    pub(super) last_sync: DateTime<Utc>,
    #[serde(default)]
    pub(super) content_hash: Option<String>,
}

#[derive(Clone)]
pub(super) struct LocalFile {
    pub(super) mtime: i64,
    pub(super) size: u64,
    pub(super) content_hash: String,
}

pub(super) fn compute_file_hash(path: &Path) -> Result<String, String> {
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
pub(super) fn local_matches_entry(local: &LocalFile, entry: &FileEntry) -> bool {
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
pub(super) fn local_changed_vs_manifest(local: &LocalFile, entry: &FileEntry) -> bool {
    !local_matches_entry(local, entry)
}

/// What to do with a prior-synced manifest key whose remote counterpart is
/// missing from the current remote listing.
///
/// Absence is NOT deletion (deletion-safety spec R5, task 1915): a file is only
/// removed locally when the server positively reports that exact file trashed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RemoteAbsent {
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
pub(super) fn classify_remote_absent(
    local: Option<&LocalFile>,
    prior: Option<&FileEntry>,
    tombstoned: bool,
) -> RemoteAbsent {
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

pub(super) const ACCOUNT_REFUSAL: &str =
    "This folder was synced by a different account. Sign in as that account, or sync into a new folder.";

/// D-L1: resolve session identity before any manifest-driven work. A legacy
/// root must be readable by that session; email is never an account identity.
pub(super) async fn check_sync_account(api: &ApiClient, state: &SyncState) -> Result<String, String> {
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
pub(super) fn root_unreconciled(state: &SyncState, resolved: Uuid) -> bool {
    state.remote_folder_id.is_some_and(|bound| bound != resolved)
}

/// The manifest entry this pass may trust for `rel`. While the root is
/// unreconciled, an entry for a file that ALSO exists in the current remote
/// folder describes a different file of the old tree, so it counts as absent:
/// the two copies become a both-sides conflict (skipped unless `--force`),
/// never a trash, replace-upload or download-over-local on the old entry's
/// account (#83 review).
pub(super) fn trusted_prior(
    state: &SyncState,
    rel: &str,
    remote_present: bool,
    root_changed: bool,
) -> Option<FileEntry> {
    if root_changed && remote_present {
        return None;
    }
    state.files.get(rel).cloned()
}

/// Everything one pass knows about whether remote absence may mean "deleted":
/// the ids the server positively reports trashed, and - if the listing itself
/// is not fit to conclude anything from - why.
pub(super) struct AbsencePlan {
    pub(super) tombstones: HashSet<Uuid>,
    /// `Some(reason)`: the remote listing is empty / partial / unreadable / from
    /// another folder, so NO local file may be removed on its account.
    pub(super) untrusted: Option<String>,
}

impl AbsencePlan {
    pub(super) fn tombstoned(&self, entry: &FileEntry) -> bool {
        self.tombstones.contains(&entry.remote_id)
    }
}

/// Decide whether this pass may remove anything, and ask the server about the
/// only files it could remove (manifest entries whose local copy is untouched
/// and which are absent remotely). Skipped entirely for an untrusted listing.
pub(super) async fn plan_absence(
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
pub(super) fn untrusted_notice(
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
pub(super) fn kept_unconfirmed_notice(rels: &[String]) -> Option<String> {
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

pub(super) fn breaker_notice(count: usize, tracked: usize) -> String {
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
pub(super) fn print_notice(msg: &str) {
    if !ui::is_quiet() {
        eprintln!("  {} {msg}", "!".custom_color(crate::colors::AMBER));
    }
}

/// One `+ <what> <name>` progress line for a folder the pass creates. Under `--json` stdout holds exactly one
/// JSON document, so the line goes to stderr there (LD-0135); otherwise it stays on stdout.
pub(super) fn print_folder_line(what: &str, name: &str) {
    let line = format!(
        "  {} {} {}",
        "+".custom_color(crate::colors::GREEN_OK),
        what.custom_color(crate::colors::INK_DIM),
        name.custom_color(crate::colors::INK),
    );
    if ui::is_json() {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// Stat + hash a file right now (not from the scan taken at the start of the
/// pass). Used to re-check a file immediately before it is moved aside.
pub(super) fn fresh_local_file(path: &Path) -> Result<LocalFile, String> {
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
pub(super) fn remove_local_recoverably(local_dir: &Path, rel: &str, entry: &FileEntry) -> Result<bool, String> {
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
pub(super) struct Reconciled {
    /// Rels whose local copy was moved aside (or was already gone): drop them
    /// from the pre-reconcile snapshot.
    pub(super) removed: Vec<String>,
    /// Lines for the user about what was left alone and why. The caller shows
    /// them (stderr for the plain loop, an event for the TUI) so nothing here
    /// scribbles over a raw-mode screen.
    pub(super) notices: Vec<String>,
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
pub(super) fn reconcile_remote_deletions(
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
pub(super) struct RemoteFile {
    pub(super) id: Uuid,
    pub(super) resolved: Option<ResolvedCliKey>,
    pub(super) chunk_count: u32,
    pub(super) updated_at: DateTime<Utc>,
    /// The content's read pin from the file's authenticated metadata
    /// (`crypto::content_pin`, task 1943). `Err` when that metadata cannot be
    /// read: the file is still listed, but its content is refused, never guessed.
    pub(super) pin: Result<beebeeb_core::chunk_stream::ExpectedChunkFormat, String>,
}
