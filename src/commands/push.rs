//! `bb push`: upload files, encrypting on the fly, with the conflict policy.

use base64::Engine;
use beebeeb_types::quota::{Plan, effective_quota, format_storage_si};
use colored::Colorize;
use std::io::{self, Write};
use std::path::PathBuf;
use zeroize::Zeroizing;

use crate::api::ApiClient;
use crate::backup_guard::BackupDepths;
use crate::commands::push_file::{self, Prepared, UploadResult};
use crate::commands::push_report::{self, FilePlan};
use crate::commands::push_schedule;
use crate::config::load_config;
use crate::transfer_stats::FileEnd;
use crate::transfer_view::LiveProgress;
use crate::ui;

use crate::interrupt::Interrupt;
use crate::transfer_stats::Summary;
use crate::upload::limits::ChunkLimits;

/// What one `bb push` run shares across its files (spec §5.1, §8): the `--parallel` asked, the one 640 MiB
/// buffer budget every chunk-sized buffer is charged to, and the one interrupt a Ctrl-C sets.
pub(super) struct PushCtl {
    pub(super) parallel: u32,
    pub(super) limits: std::sync::Arc<ChunkLimits>,
    pub(super) interrupt: Interrupt,
}

impl PushCtl {
    /// One per run: the interrupt is bound to this API's pause gate (so a Ctrl-C ends a rate-limit pause at
    /// once) and its handler installed, `bb sync`'s pattern: the first Ctrl-C sets it (no new chunk or file
    /// starts, the chunks in flight are dropped, the record stays); a second exits at once.
    fn new(parallel: u32, api: &ApiClient) -> Self {
        let interrupt = Interrupt::new();
        crate::api::pause::bind_interrupt(api.pause_origin(), interrupt.clone());
        {
            let interrupt = interrupt.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    interrupt.set();
                }
                if tokio::signal::ctrl_c().await.is_ok() {
                    std::process::exit(crate::exit::INTERRUPTED);
                }
            });
        }
        Self {
            parallel,
            limits: ChunkLimits::new(parallel),
            interrupt,
        }
    }

    /// The `transfer` object of `--json` (2333's keys, every one kept) plus, additive: `parallel_asked` (the
    /// flag's value) and `pauses` (how many pauses the shared gate had this run, spec §6.1).
    fn transfer_json(&self, transfer: &Summary, api: &ApiClient) -> serde_json::Value {
        let mut json = transfer.to_json();
        json["parallel_asked"] = serde_json::json!(self.parallel);
        json["pauses"] = serde_json::json!(crate::api::pause::pauses(api.pause_origin()));
        json
    }

    /// After a Ctrl-C: the one interrupt line (spec §5.1), unless quiet, then exit 130. `k` counts the chunks
    /// this run saw confirmed (stored or `skipped`) of `name`, so it never overstates what is on the server.
    fn exit_interrupted(&self, transfer: &Summary, name: &str) -> ! {
        use std::io::Write;
        if !ui::is_quiet() {
            eprintln!(
                "Interrupted: {} of {} chunks of {name} confirmed in this run. Run the same command to continue.",
                transfer.chunks_done, transfer.chunks_total
            );
        }
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(crate::exit::INTERRUPTED)
    }
}

/// `true` for the error an interrupted upload returns (`upload::INTERRUPTED` reaching the string boundary).
pub(super) fn was_interrupted(e: &str) -> bool {
    e == crate::upload::INTERRUPTED
}

// Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
// See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

pub(crate) fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
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
    crate::crypto::master_key_from_bytes(&mk_bytes)
}

/// How to handle a filename conflict when uploading.
#[derive(Clone, Copy, PartialEq)]
pub enum ConflictStrategy {
    /// Ask interactively (default when a TTY is attached).
    Prompt,
    /// Replace the existing file by versioning over it.
    Replace,
    /// Keep both — upload under a suffixed name.
    KeepBoth,
}

/// What a push did with one local file.
pub(super) enum Pushed {
    Uploaded(UploadResult),
    /// Skipped by the conflict rule (answered "skip").
    Skipped,
    /// This machine's own finished upload of it is already there, unchanged on both sides (task 2334).
    AlreadyUploaded,
}

/// Outcome of conflict resolution for a single file.
pub(super) enum ConflictResolution {
    /// Upload, version over the existing file with this server ID.
    Replace { existing_id: uuid::Uuid },
    /// Upload under this (possibly modified) filename as a new file.
    Upload { filename: String },
    /// Skip this file entirely.
    Skip,
}

/// Ask the server for all encrypted names in `parent_id`, decrypt them
/// locally, and return the existing file's UUID if `filename` matches.
///
/// Uses the shared `crypto::decrypt_name` which handles all server formats
/// (Rust EncryptedBlob, web-app base64 blob, plaintext) and both UUID key
/// derivations (binary and string).
///
/// A listing that cannot be read is an error, never "no conflict" (task 2334:
/// a re-pushed folder is reused, so a missed conflict would put a second file
/// of the same name beside the first). So is a row whose name could not be
/// read NOW (a read its key needs did not answer) when no other row matches: it
/// may be the same name. A row this device can never open is passed over, as
/// before: no run could tell its name. A row without a readable id is skipped.
pub(crate) async fn find_conflict(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    filename: &str,
    parent_uuid: Option<uuid::Uuid>,
) -> Result<Option<uuid::Uuid>, String> {
    let unreadable = |why: String| format!("could not check whether {filename} already exists: {why}");
    // A Ctrl-C that ended a rate-limit pause is the push's interrupt, not this file's failure: the bare sentinel
    // goes up, so the folder push counts the file as left undone (review #141).
    let resp = api.check_conflict(parent_uuid).await.map_err(|e| {
        if e.message == crate::upload::INTERRUPTED {
            e.message
        } else {
            unreadable(e.message)
        }
    })?;
    let files = resp
        .get("files")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| unreadable("the answer has no file list".to_string()))?;

    let mut keys = crate::owned_keys::OwnedKeyResolver::new();
    keys.seed(files);
    let mut cannot_read: Option<String> = None;
    for file in files {
        let Some(file_uuid) = file
            .get("id")
            .and_then(serde_json::Value::as_str)
            .and_then(|id| id.parse::<uuid::Uuid>().ok())
        else {
            continue;
        };
        match keys.resolve_or_why(api, master_key, file).await {
            Ok(resolved) if resolved.name == filename => return Ok(Some(file_uuid)),
            Ok(_) | Err(crate::owned_keys::Unresolved::Never(_)) => {}
            Err(crate::owned_keys::Unresolved::NotNow(e)) if e.contains(crate::upload::INTERRUPTED) => {
                return Err(crate::upload::INTERRUPTED.to_string());
            }
            Err(crate::owned_keys::Unresolved::NotNow(e)) => {
                // The rate-limit sentinel is for retry loops, never for the person.
                let e = e.replace(crate::api::RATE_LIMITED_MESSAGE, "the server is rate limiting requests");
                cannot_read.get_or_insert_with(|| format!("a file here could not be read now ({e}); run it again"));
            }
        }
    }
    match cannot_read {
        Some(why) => Err(unreadable(why)),
        None => Ok(None),
    }
}

/// The count of files skipped because this machine had already uploaded them, unchanged (task 2334).
fn say_already_uploaded(count: usize) {
    println!(
        "  {}",
        format!("skipped {count} already uploaded").custom_color(crate::colors::INK_DIM)
    );
}

/// Prompt the user interactively for conflict resolution.
pub(super) fn prompt_conflict(filename: &str) -> ConflictResolution {
    eprint!(
        "  {} {} [R]eplace / [K]eep both / [S]kip: ",
        filename.custom_color(crate::colors::AMBER),
        "already exists.".custom_color(crate::colors::INK_DIM),
    );
    let _ = io::stderr().flush();

    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return ConflictResolution::Skip;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "r" | "replace" => ConflictResolution::Replace {
            existing_id: uuid::Uuid::nil(),
        }, // filled by caller
        "k" | "keep" | "keep both" => ConflictResolution::Upload {
            filename: filename.to_string(),
        },
        _ => ConflictResolution::Skip,
    }
}

pub async fn run(
    path: PathBuf,
    parent_id: Option<String>,
    folder: Option<String>,
    strategy: ConflictStrategy,
    shared: crate::commands::push_folders::SharedReuse,
    verbose: bool,
    parallel: u32,
) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }

    let api = ApiClient::from_config();
    api.require_auth()?;
    let ctl = PushCtl::new(parallel, &api);

    // ── Quota pre-check ────────────────────────────────────────────────────
    // Compute total size of the upload, then check against the user's quota.
    let upload_size = if path.is_dir() {
        dir_total_size(&path)
    } else {
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    };
    check_quota(&api, upload_size).await?;

    // LD-0113: where each folder this command meets is relative to the Backups tree, read at most once.
    let mut depths = BackupDepths::default();
    let parent_id = match (parent_id, folder) {
        (Some(parent_id), None) => Some(parent_id),
        (None, Some(folder)) => Some(resolve_upload_folder(&api, &folder, &mut depths).await?),
        (None, None) => None,
        (Some(_), Some(_)) => return Err("use either --parent or --folder, not both".to_string()),
    };
    // LD-0113: nothing new goes into the Backups tree (core's app rule).
    depths.check_create_in(&api, parent_id.as_deref()).await?;

    if path.is_dir() {
        return push_directory(&api, &path, parent_id, depths, strategy, shared, verbose, &ctl).await;
    }

    // ── Resolve line (single file) ──────────────────────────────────────────
    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if ui::is_rich() {
        push_report::print_resolve_line(1, file_size);
    }

    let plan = FilePlan::of_path(&path, ctl.parallel);
    let progress = push_report::start([plan], verbose, &ctl.limits);
    let total_start = std::time::Instant::now();
    // For the owner-only sentence after the upload: the key loaded and the parent id normalised now, as the upload
    // itself does (an invalid id fails the upload first), so a push that went up cannot fail afterwards.
    let notice_key = load_master_key()?;
    let notice_parent = parent_id
        .as_deref()
        .and_then(|p| p.parse::<uuid::Uuid>().ok())
        .map(|p| p.to_string());
    let result = push_single_file(&api, &path, parent_id, strategy, &progress, plan, &ctl).await;
    let total_elapsed = total_start.elapsed();
    // The live lines are cleared here: everything below prints under them.
    let transfer = progress.summary();
    // Ctrl-C (spec §5.1): the file was left undone; say how far it got and exit 130. A file that finished in
    // spite of it (its `complete` was in flight and waited for) takes the normal path below.
    let result = match result {
        Err(e) if was_interrupted(&e) && ctl.interrupt.is_set() => {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
            ctl.exit_interrupted(&transfer, name);
        }
        other => other?,
    };

    match result {
        Pushed::Uploaded(ur) => {
            // L15 = A: the CLI never publishes an owner key. A write into a shared
            // folder stays owner-only until the owner shares it from web or mobile.
            if !ui::is_json() {
                crate::owned_keys::announce_owner_only(&api, &notice_key, notice_parent.as_deref()).await;
            }
            if ui::is_json() {
                let speed = ur.speed_bps();
                let json = serde_json::json!({
                    "files": [{
                        "name": ur.name,
                        "id": ur.id,
                        "size_bytes": ur.size_bytes,
                        "speed_bps": speed as u64,
                        "duration_ms": ur.duration.as_millis() as u64,
                        "transport": ur.transport,
                    }],
                    "total_files": 1,
                    "total_bytes": ur.encrypted_bytes,
                    "avg_speed_bps": speed as u64,
                    "total_duration_ms": total_elapsed.as_millis() as u64,
                    "transfer": ctl.transfer_json(&transfer, &api),
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            } else if ui::is_rich() {
                push_report::print_summary(&transfer, 1, 0, ur.encrypted_bytes, verbose);
            }
        }
        skipped => {
            // File was skipped (conflict resolution, or already uploaded: task 2334)
            let already = usize::from(matches!(skipped, Pushed::AlreadyUploaded));
            if ui::is_json() {
                let json = serde_json::json!({
                    "files": [],
                    "total_files": 0,
                    "total_bytes": 0,
                    "avg_speed_bps": 0,
                    "total_duration_ms": total_elapsed.as_millis() as u64,
                    "total_already_uploaded": already,
                    "transfer": ctl.transfer_json(&transfer, &api),
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            } else if already > 0 && ui::is_rich() {
                say_already_uploaded(already);
            }
        }
    }

    Ok(())
}

/// The folder `--folder` names; `depths` learns where the folders it lists or makes are (LD-0113).
async fn resolve_upload_folder(api: &ApiClient, folder: &str, depths: &mut BackupDepths) -> Result<String, String> {
    if folder.trim().is_empty() {
        return Err("--folder must not be empty".to_string());
    }

    if let Ok(folder_id) = folder.parse::<uuid::Uuid>() {
        return Ok(folder_id.to_string());
    }

    // Task 2334: the same find-or-create as a pushed folder's own name, so the same one of several same-name
    // folders is chosen (the oldest unshared one; a shared one only when every one is shared). The user named
    // it, so it is reused even when shared.
    let master_key = load_master_key()?;
    let mut folders = crate::commands::push_folders::PushFolders::new(
        api,
        &master_key,
        crate::commands::push_folders::SharedReuse::Allow,
        std::mem::take(depths),
    );
    let found = folders.named(folder).await?;
    *depths = folders.into_backup_depths();
    Ok(found.to_string())
}

/// Upload a single file (`bb push <file>`), counted in `progress` under `plan` (what the run planned for it).
///
/// The file is announced to the run as the one sent next, and its end settles its share (`TransferStats::next_file`,
/// `end_file`): its totals follow the size the driver reads when the file begins (after the conflict lookup and any
/// prompt), so done never passes total (spec §4.6, I1). A file that is not uploaded (a skip, or already uploaded)
/// takes its share back out of the run. A failed one keeps it, so the run ends short of its total, and gives up what
/// it did not send, so the ETA does not wait for it.
async fn push_single_file(
    api: &ApiClient,
    path: &std::path::Path,
    parent_id: Option<String>,
    strategy: ConflictStrategy,
    progress: &LiveProgress,
    plan: FilePlan,
    ctl: &PushCtl,
) -> Result<Pushed, String> {
    plan.announce(progress);
    let pushed = upload_one(api, path, parent_id, strategy, progress, ctl).await;
    progress.end_file(match &pushed {
        Ok(Pushed::Uploaded(_)) => FileEnd::Uploaded,
        Ok(Pushed::AlreadyUploaded | Pushed::Skipped) => FileEnd::NotSent,
        Err(_) => FileEnd::Failed,
    });
    pushed
}

/// One file, both steps of `push_file` under its upload lock, taken first: one push per local file across
/// processes, held until this function returns (task 1950 review), so a second `bb push` of a file whose first
/// push is still uploading never "resumes" the live upload.
async fn upload_one(
    api: &ApiClient,
    path: &std::path::Path,
    parent_id: Option<String>,
    strategy: ConflictStrategy,
    progress: &LiveProgress,
    ctl: &PushCtl,
) -> Result<Pushed, String> {
    // The streaming uploader stats + reads the file itself in bounded chunks; no full read here.
    let master_key = load_master_key()?;
    let _upload_lock = push_file::lock(path)?;
    match push_file::prepare(api, &master_key, path, parent_id, strategy, false, progress).await? {
        Prepared::Done(pushed) => Ok(pushed),
        Prepared::Upload(ready) => push_file::upload(api, std::sync::Arc::new(master_key), *ready, progress, ctl).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn push_directory(
    api: &ApiClient,
    dir_path: &std::path::Path,
    parent_id: Option<String>,
    depths: BackupDepths,
    strategy: ConflictStrategy,
    shared: crate::commands::push_folders::SharedReuse,
    verbose: bool,
    ctl: &PushCtl,
) -> Result<(), String> {
    let master_key = load_master_key()?;
    let dir_name = dir_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("folder")
        .to_string();

    let mut entries: Vec<(PathBuf, bool)> = Vec::new();
    collect_entries(dir_path, &mut entries)?;

    let file_count = entries.iter().filter(|(_, is_dir)| !*is_dir).count();
    let _folder_count = entries.iter().filter(|(_, is_dir)| *is_dir).count();
    let total_file_size: u64 = entries
        .iter()
        .filter(|(_, is_dir)| !*is_dir)
        .filter_map(|(p, _)| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    // ── Resolve line ────────────────────────────────────────────────────────
    if ui::is_rich() {
        push_report::print_resolve_line(file_count, total_file_size);
    }

    let parent_uuid = parent_id
        .as_deref()
        .map(|p| p.parse::<uuid::Uuid>())
        .transpose()
        .map_err(|e| format!("invalid parent ID: {e}"))?;

    // Task 2334: a re-push writes into the folder of this name already there (at every level below too),
    // never into a second one; one it finds that is shared on its own needs a yes first (`push_folders`).
    let mut folders = crate::commands::push_folders::PushFolders::new(api, &master_key, shared, depths);
    // A local name that is not UTF-8 is stored as "folder" (as before): it gets a new folder and never matches
    // an unrelated existing one.
    let root_id = match dir_path.file_name().and_then(|n| n.to_str()) {
        Some(name) => folders.child(parent_uuid, name, &dir_name).await?,
        None => folders.new_folder(parent_uuid, &dir_name).await?,
    };
    // LD-0113: a folder named like the Backups root at the vault root is reused above; it takes nothing new.
    // Found in the listing just read, or made just now: known, so this reads nothing.
    let root_text = root_id.to_string();
    folders.backup_depths().check_create_in(api, Some(&root_text)).await?;

    let mut folder_map: std::collections::HashMap<PathBuf, uuid::Uuid> = std::collections::HashMap::new();
    folder_map.insert(dir_path.to_path_buf(), root_id);
    // Every folder first, found or made (entries are in pre-order: a folder
    // before what it holds), so a question or a refusal about a shared one
    // comes before any file is uploaded (task 2334).
    for (entry_path, _) in entries.iter().filter(|(_, is_dir)| *is_dir) {
        let entry_parent = entry_path.parent().unwrap_or(dir_path);
        let entry_parent_id = folder_map.get(entry_parent).copied().unwrap_or(root_id);
        // A vault path is shown with `/` on every OS, never an OS path's separator.
        let relative = entry_path.strip_prefix(dir_path).unwrap_or(entry_path);
        let shown = std::iter::once(dir_name.to_string())
            .chain(
                relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned()),
            )
            .collect::<Vec<_>>()
            .join("/");
        let folder_id = match entry_path.file_name().and_then(|n| n.to_str()) {
            Some(name) => folders.child(Some(entry_parent_id), name, &shown).await?,
            None => folders.new_folder(Some(entry_parent_id), "folder").await?,
        };
        folder_map.insert(entry_path.clone(), folder_id);
    }

    // Every file planned before the first one goes up (its size now): the run's totals and ETA. A file that changes
    // later is followed when it begins (`push_single_file`).
    let plans: Vec<FilePlan> = entries
        .iter()
        .filter(|(_, is_dir)| !*is_dir)
        .map(|(p, _)| FilePlan::of_path(p, ctl.parallel))
        .collect();
    let progress = push_report::start(plans.iter().copied(), verbose, &ctl.limits);
    // `folders` borrows `master_key` to the end; the uploads share a key of their own.
    let upload_key = std::sync::Arc::new(load_master_key()?);

    let total_start = std::time::Instant::now();
    // (input index, result) for every file uploaded, and (input index, path relative to the pushed folder, error)
    // for every file that failed: both reported in input order.
    let mut upload_results: Vec<(u32, UploadResult)> = Vec::new();
    let mut failures: Vec<(u32, String, String)> = Vec::new();
    // Files this machine had already uploaded, unchanged (task 2334): skipped, counted.
    let mut already_uploaded = 0usize;

    // ── Stage 1 (spec §7): in order, without a lock, everything before each file's upload: the conflict check
    // and its question, the id, the "already uploaded" skip. Every question is asked before the first byte goes
    // out. One file failing (a server reject, a read error, a refused question) must not abandon the rest of the
    // folder: it is recorded with its path, and the command fails at the end (flow-6).
    let files: Vec<&PathBuf> = entries.iter().filter(|(_, is_dir)| !*is_dir).map(|(p, _)| p).collect();
    let mut admits = Vec::new();
    let mut prepared = 0usize;
    for (index, (entry_path, plan)) in files.iter().zip(plans).enumerate() {
        if ctl.interrupt.is_set() {
            break;
        }
        let index = index as u32;
        let entry_parent = entry_path.parent().unwrap_or(dir_path);
        let parent = Some(folder_map.get(entry_parent).copied().unwrap_or(root_id).to_string());
        let rel = entry_path
            .strip_prefix(dir_path)
            .unwrap_or(entry_path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let ready = push_file::prepare(api, &upload_key, entry_path, parent, strategy, true, &progress).await;
        // A Ctrl-C during its lookup: the file is left undone (not started), not failed.
        if matches!(&ready, Err(e) if was_interrupted(e) && ctl.interrupt.is_set()) {
            break;
        }
        prepared += 1;
        match ready {
            Ok(Prepared::Upload(ready)) => admits.push(push_schedule::Admit {
                index,
                rel,
                plan,
                ready,
            }),
            Ok(Prepared::Done(pushed)) => {
                plan.announce_as(&progress, index);
                progress.end_file_as(index, FileEnd::NotSent);
                already_uploaded += usize::from(matches!(pushed, Pushed::AlreadyUploaded));
            }
            Err(e) => {
                plan.announce_as(&progress, index);
                progress.end_file_as(index, FileEnd::Failed);
                push_report::note_failure(&progress, &rel, &e);
                failures.push((index, rel, e));
            }
        }
    }

    // ── Stage 2: up to `--parallel` files at once, each locked and re-checked on admission (`push_schedule`).
    let mut interrupted: Vec<String> = Vec::new();
    let mut not_started = files.len() - prepared;
    if ctl.interrupt.is_set() {
        not_started += admits.len();
    } else {
        let (ended, never) = push_schedule::upload_all(api, upload_key, admits, ctl, &progress).await;
        not_started += never;
        for push_schedule::Ended { index, rel, result } in ended {
            match result {
                Ok(Pushed::Uploaded(ur)) => upload_results.push((index, ur)),
                Ok(Pushed::AlreadyUploaded) => already_uploaded += 1,
                Ok(Pushed::Skipped) => {}
                // An interrupted file is left undone, not failed: the whole push stops (spec §5.1).
                Err(e) if was_interrupted(&e) && ctl.interrupt.is_set() => interrupted.push(rel),
                Err(e) => failures.push((index, rel, e)),
            }
        }
    }
    upload_results.sort_by_key(|(index, _)| *index);
    failures.sort_by_key(|(index, ..)| *index);
    let upload_results: Vec<UploadResult> = upload_results.into_iter().map(|(_, ur)| ur).collect();
    let failures: Vec<(String, String)> = failures.into_iter().map(|(_, rel, e)| (rel, e)).collect();

    let total_elapsed = total_start.elapsed();
    let total_bytes: u64 = upload_results.iter().map(|r| r.encrypted_bytes).sum();
    let uploaded_count = upload_results.len();
    // The live lines are cleared here: everything below prints under them.
    let transfer = progress.summary();
    // Ctrl-C (spec §5.1, §5.2): one line, exit 130, when a file was left undone. It names the one file that was in
    // flight, or the pushed folder when several were (its counts are the run's).
    if ctl.interrupt.is_set() && (!interrupted.is_empty() || not_started > 0) {
        let name = match interrupted.as_slice() {
            [one] => one.clone(),
            _ => dir_name.clone(),
        };
        ctl.exit_interrupted(&transfer, &name);
    }

    // ── Summary ─────────────────────────────────────────────────────────────
    if ui::is_json() {
        let files_json: Vec<serde_json::Value> = upload_results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "id": r.id,
                    "size_bytes": r.size_bytes,
                    "speed_bps": r.speed_bps() as u64,
                    "duration_ms": r.duration.as_millis() as u64,
                    "transport": r.transport,
                })
            })
            .collect();
        let avg_speed = if total_elapsed.as_secs_f64() > 0.0 {
            total_bytes as f64 / total_elapsed.as_secs_f64()
        } else {
            0.0
        };
        let json = serde_json::json!({
            "files": files_json,
            "total_files": uploaded_count,
            "total_bytes": total_bytes,
            "avg_speed_bps": avg_speed as u64,
            "total_duration_ms": total_elapsed.as_millis() as u64,
            "total_failed": failures.len(),
            "total_already_uploaded": already_uploaded,
            "failed": failures
                .iter()
                .map(|(path, error)| serde_json::json!({ "path": path, "error": error }))
                .collect::<Vec<_>>(),
            "transfer": ctl.transfer_json(&transfer, api),
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    } else if ui::is_rich() {
        push_report::print_summary(&transfer, uploaded_count, failures.len(), total_bytes, verbose);
        if already_uploaded > 0 {
            say_already_uploaded(already_uploaded);
        }
    }

    // L15 = A (P2-11): a directory push into a shared folder says the owner-only
    // sentence ONCE for the command (the per-file line is suppressed here).
    if uploaded_count > 0 && !ui::is_json() {
        // The folder the batch landed IN decides; when the push is the folder
        // itself, its parent is the shared one. Since task 2334 the push may
        // also write into an existing folder shared on its own (after a yes or
        // `--reuse-shared`, at any level): that one decides then.
        let target = folders.shared_reused().or(parent_uuid).unwrap_or(root_id).to_string();
        crate::owned_keys::announce_owner_only(api, &master_key, Some(&target)).await;
    }
    if !failures.is_empty() {
        let names: Vec<&str> = failures.iter().map(|(path, _)| path.as_str()).collect();
        return Err(format!(
            "{} of {file_count} files failed to upload: {}",
            failures.len(),
            names.join(", "),
        ));
    }

    Ok(())
}

fn collect_entries(dir: &std::path::Path, entries: &mut Vec<(PathBuf, bool)>) -> Result<(), String> {
    let read_dir = std::fs::read_dir(dir).map_err(|e| format!("failed to read directory: {e}"))?;
    let mut sorted: Vec<_> = read_dir.filter_map(|e| e.ok()).collect();
    sorted.sort_by_key(|e| e.file_name());

    for entry in sorted {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            entries.push((path.clone(), true));
            collect_entries(&path, entries)?;
        } else {
            entries.push((path, false));
        }
    }
    Ok(())
}

/// Split "report.pdf" → ("report", "pdf"); "notes" → ("notes", "").
pub(super) fn split_stem_ext(filename: &str) -> (&str, &str) {
    match filename.rfind('.') {
        Some(i) if i > 0 => (&filename[..i], &filename[i + 1..]),
        _ => (filename, ""),
    }
}

/// Recursively compute the total size of all files in a directory.
fn dir_total_size(dir: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Fetch the user's subscription and usage, then reject the upload if the
/// account can't upload at all (task 1037: `needs_plan` / `lapsed`) or
/// `used_bytes + upload_size` exceeds the effective quota.
///
/// These two GETs already ran before every `bb push`, so explaining a
/// `needs_plan` / `lapsed` account here costs no extra round-trip. A failed
/// fetch never blocks (`unwrap_or_default`) — the server's upload init stays
/// the enforcer (409 `plan_required` / `account_lapsed`), and its refusal is
/// explained in `ApiClient::upload_init_typed`.
async fn check_quota(api: &ApiClient, upload_size: u64) -> Result<(), String> {
    let (usage_res, sub_res, onboarding_res) =
        tokio::join!(api.get_usage(), api.get_subscription(), api.get_onboarding(true));

    let usage = usage_res.unwrap_or_default();
    let sub = sub_res.unwrap_or_default();

    let used_bytes = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    // Task 1750: the onboarding document's capabilities decide when it is
    // available (best-effort: any failure, a 404 or another schema major
    // leaves the legacy subscription logic exactly as before).
    let app_base = crate::web_url::web_app_base();
    let doc = onboarding_res
        .ok()
        .flatten()
        .and_then(|body| crate::account_state::OnboardingDoc::parse(&body))
        .and_then(|doc| doc.account_summary(&app_base));
    check_quota_against(&sub, used_bytes, upload_size, &app_base, doc.as_ref())
}

/// Pure half of [`check_quota`]: decide from an already-fetched subscription
/// and usage. `app_base` is the web app's base URL for the account-state
/// notice.
fn check_quota_against(
    sub: &serde_json::Value,
    used_bytes: i64,
    upload_size: u64,
    app_base: &str,
    doc: Option<&crate::account_state::AccountSummary>,
) -> Result<(), String> {
    // Task 1037: a `needs_plan` / `lapsed` account has quota 0, which the
    // `quota > 0` guard below would wave through to a bare server refusal.
    // Say what's actually wrong and where to fix it.
    //
    // Task 1750: the onboarding document, when available, is authoritative
    // for "can this account upload" (an `allowance` account is `needs_plan`
    // in the legacy view yet may upload); the legacy notice only decides when
    // there is no document.
    let legacy_notice = crate::account_state::AccountState::from_subscription(sub).notice(app_base);
    if let Some(notice) = crate::account_state::upload_refusal(legacy_notice, doc) {
        return Err(notice);
    }

    let plan_slug = sub.get("plan").and_then(|v| v.as_str()).unwrap_or("free");
    let plan = Plan::from_slug(plan_slug);
    let extra_tb = sub.get("extra_storage_tb").and_then(|v| v.as_i64()).unwrap_or(0);
    // Task 1547 finding 2 (Codex review, PR #33): read the server's own
    // `quota_bytes` (already folds in extra_storage_tb + any referral bonus
    // — see `beebeeb-api/src/quota.rs::get_user_quota`) instead of a
    // `bonus_bytes` key `GET /billing/subscription` never actually returns
    // (always read back 0) and recomputing via `effective_quota(plan,
    // extra_tb, bonus_bytes)` client-side — the same bug fixed in
    // `commands::{quota,whoami}::build_plan_label`, found while sweeping
    // every `bonus_bytes` call site. Only falls back to the client-side
    // compute (bonus-blind) if the server ever omits the field.
    let quota = sub
        .get("quota_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| effective_quota(plan, extra_tb, 0));

    if quota > 0 && used_bytes + upload_size as i64 > quota {
        let used_str = format_storage_si(used_bytes);
        let quota_str = format_storage_si(quota);
        let extra_hint = if plan.can_add_storage() {
            " Add more storage at app.beebeeb.io/billing"
        } else {
            ""
        };
        return Err(format!("Storage full ({used_str} / {quota_str}).{extra_hint}"));
    }

    Ok(())
}

#[cfg(test)]
mod check_quota_tests {
    use super::*;
    use serde_json::json;

    const APP: &str = "https://app.beebeeb.io";

    #[test]
    fn needs_plan_account_is_refused_with_the_choose_plan_notice() {
        let sub = json!({
            "plan": "free", "effective_plan": "none", "quota_bytes": 0,
            "account_state": "needs_plan", "data_deletion_at": null,
        });
        assert_eq!(
            check_quota_against(&sub, 0, 10, APP, None),
            Err("Your account has no plan yet \u{2014} choose one at https://app.beebeeb.io/choose-plan".to_string())
        );
    }

    #[test]
    fn lapsed_account_is_refused_with_the_deletion_date_and_billing_link() {
        let sub = json!({
            "plan": "pro", "effective_plan": "none", "quota_bytes": 0,
            "account_state": "lapsed", "data_deletion_at": "2026-11-27T10:00:00Z",
        });
        let err = check_quota_against(&sub, 5, 10, APP, None).unwrap_err();
        assert!(
            err.starts_with("Your trial has ended; your vault is read-only"),
            "{err}"
        );
        assert!(err.contains("November 27, 2026"), "{err}");
        assert!(err.ends_with("https://app.beebeeb.io/billing?view=change"), "{err}");
    }

    #[test]
    fn ok_account_within_quota_passes() {
        let sub = json!({ "plan": "pro", "quota_bytes": 1_000, "account_state": "ok" });
        assert_eq!(check_quota_against(&sub, 100, 10, APP, None), Ok(()));
    }

    #[test]
    fn pre_1037_server_without_account_state_keeps_the_old_behaviour() {
        // Missing fields ⇒ ok: within quota passes, over quota is "Storage full".
        let sub = json!({ "plan": "pro", "quota_bytes": 1_000 });
        assert_eq!(check_quota_against(&sub, 100, 10, APP, None), Ok(()));
        let err = check_quota_against(&sub, 995, 10, APP, None).unwrap_err();
        assert!(err.starts_with("Storage full"), "{err}");
    }

    #[test]
    fn a_failed_subscription_fetch_never_blocks() {
        // `check_quota` passes `Value::Null` (unwrap_or_default) when the
        // GET failed — the server stays the enforcer.
        assert_eq!(check_quota_against(&serde_json::Value::Null, 0, 10, APP, None), Ok(()));
    }

    fn doc(name: &str) -> crate::account_state::AccountSummary {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("contracts/onboarding/fixtures")
            .join(format!("{name}.json"));
        let body: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        crate::account_state::OnboardingDoc::parse(&body)
            .unwrap()
            .account_summary(APP)
            .unwrap()
    }

    #[test]
    fn document_allowing_upload_overrides_the_legacy_needs_plan_refusal() {
        // `allowance`: legacy view says needs_plan, document says upload allowed.
        let sub = json!({
            "plan": "free", "effective_plan": "none", "quota_bytes": 0,
            "account_state": "needs_plan", "data_deletion_at": null,
        });
        let allowance = doc("account.allowance.web");
        assert_eq!(check_quota_against(&sub, 0, 10, APP, Some(&allowance)), Ok(()));
        // No document: the legacy refusal is unchanged.
        assert!(check_quota_against(&sub, 0, 10, APP, None).is_err());
    }

    #[test]
    fn document_denying_upload_refuses_with_its_own_notice() {
        let sub = json!({ "plan": "pro", "quota_bytes": 1_000, "account_state": "ok" });
        let ended = doc("account.trial_ended.ios");
        let err = check_quota_against(&sub, 0, 10, APP, Some(&ended)).unwrap_err();
        assert!(err.starts_with("Your trial has ended"), "{err}");
    }
}
