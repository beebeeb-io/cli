//! `bb push`: upload files, encrypting on the fly, with the conflict policy.

use base64::Engine;
use beebeeb_types::quota::{Plan, effective_quota, format_storage_si};
use colored::Colorize;
use std::io::{self, Write};
use std::path::PathBuf;
use zeroize::Zeroizing;

use crate::api::ApiClient;
use crate::commands::push_report::{self, FilePlan};
use crate::config::load_config;
use crate::transfer_stats::FileEnd;
use crate::transfer_view::LiveProgress;
use crate::ui;
use crate::upload::{ChunkProgress, Stream};

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
enum Pushed {
    Uploaded(UploadResult),
    /// Skipped by the conflict rule (answered "skip").
    Skipped,
    /// This machine's own finished upload of it is already there, unchanged on both sides (task 2334).
    AlreadyUploaded,
}

/// Outcome of conflict resolution for a single file.
enum ConflictResolution {
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
    let resp = api
        .check_conflict(parent_uuid)
        .await
        .map_err(|e| unreadable(e.message))?;
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
            Err(crate::owned_keys::Unresolved::NotNow(e)) => {
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
fn prompt_conflict(filename: &str) -> ConflictResolution {
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
) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }

    let api = ApiClient::from_config();
    api.require_auth()?;

    // ── Quota pre-check ────────────────────────────────────────────────────
    // Compute total size of the upload, then check against the user's quota.
    let upload_size = if path.is_dir() {
        dir_total_size(&path)
    } else {
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    };
    check_quota(&api, upload_size).await?;

    let parent_id = match (parent_id, folder) {
        (Some(parent_id), None) => Some(parent_id),
        (None, Some(folder)) => Some(resolve_upload_folder(&api, &folder).await?),
        (None, None) => None,
        (Some(_), Some(_)) => return Err("use either --parent or --folder, not both".to_string()),
    };
    // LD-0113: nothing new goes into the Backups tree (core's app rule).
    crate::backup_guard::check_create_in(&api, parent_id.as_deref()).await?;

    if path.is_dir() {
        return push_directory(&api, &path, parent_id, strategy, shared, verbose).await;
    }

    // ── Resolve line (single file) ──────────────────────────────────────────
    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if ui::is_rich() {
        push_report::print_resolve_line(1, file_size);
    }

    let plan = FilePlan::of_size(file_size);
    let progress = push_report::start([plan], verbose);
    let total_start = std::time::Instant::now();
    // For the owner-only sentence after the upload: the key loaded and the parent id normalised now, as the upload
    // itself does (an invalid id fails the upload first), so a push that went up cannot fail afterwards.
    let notice_key = load_master_key()?;
    let notice_parent = parent_id
        .as_deref()
        .and_then(|p| p.parse::<uuid::Uuid>().ok())
        .map(|p| p.to_string());
    let result = push_single_file(&api, &path, parent_id, strategy, false, &progress, plan).await?;
    let total_elapsed = total_start.elapsed();
    // The live lines are cleared here: everything below prints under them.
    let transfer = progress.summary();

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
                    "transfer": transfer.to_json(),
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
                    "transfer": transfer.to_json(),
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            } else if already > 0 && ui::is_rich() {
                say_already_uploaded(already);
            }
        }
    }

    Ok(())
}

async fn resolve_upload_folder(api: &ApiClient, folder: &str) -> Result<String, String> {
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
    );
    Ok(folders.named(folder).await?.to_string())
}

/// Result data from a single file upload, used for summary + JSON output.
struct UploadResult {
    name: String,
    id: String,
    size_bytes: u64,
    encrypted_bytes: u64,
    /// `"direct"` or `"api"` (additive `--json` key `transport`, as for `bb pull`).
    transport: &'static str,
    duration: std::time::Duration,
    /// The bytes of its chunks confirmed sent, over every upload attempt of the file (a chunk re-sent by a later
    /// attempt, after a swept session, counts each time; one HTTP chunk retried counts once, when confirmed;
    /// spec §7 `bytes_transferred`): its end line's speed.
    transferred_bytes: u64,
}

impl UploadResult {
    fn speed_bps(&self) -> f64 {
        let secs = self.duration.as_secs_f64();
        if secs > 0.0 {
            self.encrypted_bytes as f64 / secs
        } else {
            0.0
        }
    }
}

/// Upload a single file, counted in `progress` under `plan` (what the run planned for it).
///
/// The file is announced to the run as the one sent next, and its end settles its share (`TransferStats::next_file`,
/// `end_file`): its totals follow the size the driver reads when the file begins (after the conflict lookup and any
/// prompt), so done never passes total (spec §4.6, I1). A file that is not uploaded (a skip, or already uploaded)
/// takes its share back out of the run. A failed one keeps it, so the run ends short of its total, and gives up what
/// it did not send, so the ETA does not wait for it. `in_folder`: part of a folder push, which prints no `resuming`
/// line (as before).
async fn push_single_file(
    api: &ApiClient,
    path: &std::path::Path,
    parent_id: Option<String>,
    strategy: ConflictStrategy,
    in_folder: bool,
    progress: &LiveProgress,
    plan: FilePlan,
) -> Result<Pushed, String> {
    plan.announce(progress);
    let pushed = upload_one(api, path, parent_id, strategy, in_folder, progress).await;
    progress.end_file(match &pushed {
        Ok(Pushed::Uploaded(_)) => FileEnd::Uploaded,
        Ok(Pushed::AlreadyUploaded | Pushed::Skipped) => FileEnd::NotSent,
        Err(_) => FileEnd::Failed,
    });
    pushed
}

async fn upload_one(
    api: &ApiClient,
    path: &std::path::Path,
    parent_id: Option<String>,
    strategy: ConflictStrategy,
    in_folder: bool,
    progress: &LiveProgress,
) -> Result<Pushed, String> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();

    // The streaming uploader stats + reads the file itself in bounded chunks;
    // no full read here.
    let master_key = load_master_key()?;

    let parent_uuid: Option<uuid::Uuid> = parent_id.as_deref().and_then(|s| s.parse().ok());

    // One push per local file across processes, held until this function
    // returns (task 1950 review): a second `bb push` of a file whose first push
    // is still uploading would otherwise "resume" the live upload below.
    let _upload_lock = match crate::upload_lock::try_acquire(path) {
        Ok(lock) => lock,
        Err(crate::upload_lock::LockError::Held { pid }) => {
            let holder = pid.map(|p| format!(" (pid {p})")).unwrap_or_default();
            return Err(crate::exit::with_code(
                crate::exit::BUSY,
                format!(
                    "another bb push of {file_name} is still running{holder}; \
                     wait for it to finish, or stop it and run this again to resume"
                ),
            ));
        }
        Err(crate::upload_lock::LockError::Io(e)) => return Err(format!("{file_name}: upload lock: {e}")),
    };

    // ── Conflict detection ────────────────────────────────────────────────────
    // Decrypt existing filenames in the folder and check for a match.
    // If found, ask the user (or apply the strategy flag) how to proceed.
    let mut mint_tag: Option<[u8; 32]> = None;
    let mut read_row = None;
    let (file_id, upload_name, replace_file_id) = {
        let existing = find_conflict(api, &master_key, &file_name, parent_uuid).await?;
        // Our own upload of this very file is not a conflict (`push_repeat`): an interrupted one resumes
        // (task 1950, with no explicit --replace / --keep-both), and a finished one, unchanged on both sides, is
        // skipped (task 2334). --replace always uploads.
        let found = match existing {
            Some(id) if strategy != ConflictStrategy::Replace => {
                let resume = strategy == ConflictStrategy::Prompt;
                crate::commands::push_repeat::existing_upload(api, path, id, parent_uuid, resume).await
            }
            _ => crate::commands::push_repeat::Existing::Other,
        };
        let resumes = match found {
            crate::commands::push_repeat::Existing::AlreadyUploaded => return Ok(Pushed::AlreadyUploaded),
            crate::commands::push_repeat::Existing::Interrupted(row) => {
                read_row = row;
                true
            }
            crate::commands::push_repeat::Existing::Other => false,
        };

        match existing {
            None => {
                // L13/N4: a new item's id comes from core's mint (it carries
                // the resume tag), never from a local random number.
                let minted = crate::item_id::mint_owner(&master_key)?;
                mint_tag = Some(*minted.tag());
                (minted.id(), file_name.clone(), None)
            }
            Some(existing_id) if resumes => {
                if !in_folder && !ui::is_quiet() && !ui::is_json() {
                    progress.note(
                        &format!(
                            "  {} {}",
                            "resuming".custom_color(crate::colors::INK_DIM),
                            format!("{file_name} (the interrupted upload)").custom_color(crate::colors::INK),
                        ),
                        Stream::Stdout,
                    );
                }
                // The same id as a new file: the driver finds the resume record
                // and continues (or, under core's rule, restarts) that upload.
                (existing_id, file_name.clone(), None)
            }
            Some(existing_id) => {
                let resolution = match strategy {
                    ConflictStrategy::Replace => ConflictResolution::Replace { existing_id },
                    ConflictStrategy::KeepBoth => {
                        let (stem, ext) = split_stem_ext(&file_name);
                        let new_name = if ext.is_empty() {
                            format!("{stem} (1)")
                        } else {
                            format!("{stem} (1).{ext}")
                        };
                        ConflictResolution::Upload { filename: new_name }
                    }
                    ConflictStrategy::Prompt => {
                        // No one can answer the prompt: fail loudly instead of
                        // reading EOF as "skip" and exiting 0 (flow-6).
                        if !crate::exit::stdin_is_interactive() {
                            return Err(crate::exit::with_code(
                                crate::exit::USAGE,
                                format!(
                                    "{file_name} already exists \u{2014} pass --replace or --keep-both \
                                     (stdin is not a terminal, so there is no one to ask)"
                                ),
                            ));
                        }
                        // The prompt reads stdin with the live lines hidden (spec §6.7).
                        let mut r = ConflictResolution::Skip;
                        progress.suspend(&mut || r = prompt_conflict(&file_name));
                        // If user chose Replace, inject the actual existing_id
                        match r {
                            ConflictResolution::Replace { .. } => ConflictResolution::Replace { existing_id },
                            ConflictResolution::Upload { .. } => {
                                let (stem, ext) = split_stem_ext(&file_name);
                                let new_name = if ext.is_empty() {
                                    format!("{stem} (1)")
                                } else {
                                    format!("{stem} (1).{ext}")
                                };
                                ConflictResolution::Upload { filename: new_name }
                            }
                            ConflictResolution::Skip => {
                                if ui::is_rich() {
                                    progress.note(
                                        &format!(
                                            "  {} {}",
                                            "skip".custom_color(crate::colors::INK_DIM),
                                            file_name.custom_color(crate::colors::INK_DIM),
                                        ),
                                        Stream::Stdout,
                                    );
                                }
                                return Ok(Pushed::Skipped);
                            }
                        }
                    }
                };

                match resolution {
                    ConflictResolution::Replace { existing_id } => {
                        // Re-use the existing UUID so the server auto-versions.
                        // Key is derived from the existing ID (same as when the
                        // file was first uploaded).
                        (existing_id, file_name.clone(), Some(existing_id))
                    }
                    // L13: a fresh item's id comes from core's minting type.
                    ConflictResolution::Upload { filename } => {
                        let minted = crate::item_id::mint_owner(&master_key)?;
                        mint_tag = Some(*minted.tag());
                        (minted.id(), filename, None)
                    }
                    ConflictResolution::Skip => return Ok(Pushed::Skipped),
                }
            }
        }
    };

    // ── Streaming, constant-memory upload ─────────────────────────────────────
    // For a replace, `file_id` already equals the existing id, so the file key,
    // the encrypted name, and the server-side version all derive from one id.
    // The v2 upload driver versions by `file_id` + `base_version_number`, so on
    // a replace we fetch the existing file's current `version_number` and pass
    // it as the optimistic-concurrency base (the server returns a stale-version
    // 409 if it no longer matches). On a fresh push, `base_version_number` stays
    // `None` and the absent-`file_id`/new-`file_id` decides "new file".
    //
    // A replace also carries the existing file's read pin (task 1943), so a
    // v2-pinned file is written as v2 again: the server keeps its v2 metadata,
    // and a v1 replace would complete and never open.
    let (base_version_number, replace_pin, replace_file_key, fresh_id) = match replace_file_id {
        Some(existing_id) => {
            let existing = existing_id.to_string();
            let meta = api.get_file(&existing).await?;
            // The file's current version (task 1951: it sits in
            // `download_version`); default 1 if the server reports none — the v2
            // init still proceeds and only a genuine mismatch 409s.
            let version = crate::api::current_version_number(&meta).unwrap_or(1) as i32;
            let name_encrypted = meta
                .get("name_encrypted")
                .and_then(|v| v.as_str())
                .ok_or("server response missing name_encrypted")?;
            let resolved = crate::owned_keys::OwnedKeyResolver::new()
                .resolve(api, &master_key, &meta)
                .await?;
            // L13: a request row's content is read-only. An edit is saved as a
            // new owner item — never sealed under `C`, never published.
            if resolved.origin == beebeeb_core::owned_keys::KeyOrigin::RequestContent {
                // L13: the edit is a NEW owner item, minted by core.
                let minted = crate::item_id::mint_owner(&master_key)?;
                mint_tag = Some(*minted.tag());
                (None, None, None, Some(minted.id()))
            } else {
                // R4-B: request fields whose request list could not load stay
                // read-only with their own reason.
                if let Some(reason) = resolved.read_only_reason {
                    return Err(reason.to_string());
                }
                // P2-10: a plaintext name proves nothing; say so, not "older iOS app".
                if resolved.read_only && crate::crypto::name_is_plaintext(&resolved.canonical) {
                    return Err(crate::owned_keys::PLAINTEXT_NAME_READ_ONLY.to_string());
                }
                // SF-0002: a row sealed by the old iOS app (its name or its CURRENT
                // version) is read-only, and a check that could not decide refuses
                // too: only a proven-writable row gets a new version.
                crate::owned_keys::write_check(
                    api,
                    &master_key,
                    &crate::owned_keys::WriteTarget {
                        file_id: &existing,
                        is_folder: false,
                        name_read_only: resolved.read_only,
                        row_key: Some(resolved.key.as_ref()),
                        size_bytes: meta.get("size_bytes").and_then(|v| v.as_u64()),
                        chunk_count: meta.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
                    },
                )
                .await
                .ensure()?;
                // L13: the replacement takes its chunk-format pin from the
                // resolver's write target, not from a local re-read.
                let pin = match &resolved.write_target {
                    Some(target) => target.pin(),
                    None => crate::crypto::content_pin_with_key(&resolved.key, &existing, name_encrypted)?,
                };
                (Some(version), Some(pin), Some(resolved.key), None)
            }
        }
        None => (None, None, None, None),
    };
    let file_id = fresh_id.unwrap_or(file_id);
    let master_key = std::sync::Arc::new(master_key);
    let display_name = upload_name.clone();

    let parent_uuid: Option<uuid::Uuid> = match &parent_id {
        Some(pid) => Some(pid.parse().map_err(|e| format!("invalid parent ID: {e}"))?),
        None => None,
    };

    let file_start = std::time::Instant::now();
    let spec = crate::upload::UploadSpec {
        path: path.to_path_buf(),
        file_name: upload_name,
        file_id,
        resume_tag: mint_tag,
        base_version_number,
        replace_pin,
        replace_file_key,
        parent_id: parent_uuid,
        // bb push uploads one file at a time → full Cli cap (128 MiB).
        concurrency: 1,
        shutdown: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        read_row,
    };
    let signature = crate::resume::local_signature(path);
    let outcome = crate::upload::stream_encrypt_upload(api, std::sync::Arc::clone(&master_key), spec, progress).await?;
    // Task 2334: remember this upload, so a re-run of the push skips it while the file stays unchanged. Only
    // when the file did not change while it was read (same signature before and after, the size uploaded) and
    // the server named the version it completed.
    let unchanged = signature
        .filter(|before| crate::resume::local_signature(path) == Some(*before) && before.0 == outcome.plaintext_bytes);
    if let (Some((size, mtime_ns)), Some(version)) = (unchanged, outcome.version_number) {
        crate::resume::record_finished(
            path,
            crate::resume::FinishedUpload {
                file_id: outcome.server_id,
                parent_id: parent_uuid,
                version,
                size,
                mtime_ns,
            },
        );
    }
    let server_id = outcome.server_id.to_string();
    let file_elapsed = file_start.elapsed();
    let file_name = display_name; // possibly-suffixed name for display

    // Mark this upload so a `bb watch` remote-event handler in the same
    // process doesn't redundantly download the file we just sent.
    crate::loopback::mark_uploaded(&server_id);

    let upload_result = UploadResult {
        name: file_name.clone(),
        id: server_id.clone(),
        size_bytes: outcome.plaintext_bytes,
        encrypted_bytes: outcome.ciphertext_bytes,
        transport: outcome.transport,
        duration: file_elapsed,
        transferred_bytes: outcome.transferred_bytes,
    };

    // ── Per-file output ─────────────────────────────────────────────────────
    // Quiet prints the id (no live lines are drawn then); rich prints the end line around the live lines. JSON:
    // the caller collects the results. The owner-only sentence (L15) is the caller's, once the lines are cleared.
    if ui::is_quiet() {
        println!("{}", server_id);
    } else if ui::is_rich() {
        let line = push_report::file_line(
            &file_name,
            upload_result.encrypted_bytes,
            upload_result.transferred_bytes,
            file_elapsed,
        );
        progress.note(&line, Stream::Stdout);
    }

    Ok(Pushed::Uploaded(upload_result))
}

async fn push_directory(
    api: &ApiClient,
    dir_path: &std::path::Path,
    parent_id: Option<String>,
    strategy: ConflictStrategy,
    shared: crate::commands::push_folders::SharedReuse,
    verbose: bool,
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
    let mut folders = crate::commands::push_folders::PushFolders::new(api, &master_key, shared);
    // A local name that is not UTF-8 is stored as "folder" (as before): it gets a new folder and never matches
    // an unrelated existing one.
    let root_id = match dir_path.file_name().and_then(|n| n.to_str()) {
        Some(name) => folders.child(parent_uuid, name, &dir_name).await?,
        None => folders.new_folder(parent_uuid, &dir_name).await?,
    };
    // LD-0113: a folder named like the Backups root at the vault root is reused above; it takes nothing new.
    crate::backup_guard::check_create_in(api, Some(&root_id.to_string())).await?;

    let mut folder_map: std::collections::HashMap<PathBuf, uuid::Uuid> = std::collections::HashMap::new();
    folder_map.insert(dir_path.to_path_buf(), root_id);
    // Every folder first, found or made (entries are in pre-order: a folder
    // before what it holds), so a question or a refusal about a shared one
    // comes before any file is uploaded (task 2334).
    for (entry_path, _) in entries.iter().filter(|(_, is_dir)| *is_dir) {
        let entry_parent = entry_path.parent().unwrap_or(dir_path);
        let entry_parent_id = folder_map.get(entry_parent).copied().unwrap_or(root_id);
        let shown = std::path::Path::new(&dir_name)
            .join(entry_path.strip_prefix(dir_path).unwrap_or(entry_path))
            .display()
            .to_string();
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
        .map(|(p, _)| FilePlan::of_path(p))
        .collect();
    let progress = push_report::start(plans.iter().copied(), verbose);
    let mut plans = plans.into_iter();

    let total_start = std::time::Instant::now();
    let mut upload_results: Vec<UploadResult> = Vec::new();
    // (path relative to the pushed folder, error) for every file that failed.
    let mut failures: Vec<(String, String)> = Vec::new();
    // Files this machine had already uploaded, unchanged (task 2334): skipped, counted.
    let mut already_uploaded = 0usize;

    for (entry_path, is_dir) in &entries {
        let entry_parent = entry_path.parent().unwrap_or(dir_path);
        let entry_parent_id = folder_map.get(entry_parent).copied().unwrap_or(root_id);

        if *is_dir {
            continue;
        } else {
            // One file failing (a server reject, a read error, a network blip)
            // must not abandon the rest of the folder: record it with its path,
            // carry on, and fail the command at the end (flow-6: a 0-byte file
            // used to stop the push, so later siblings were never uploaded).
            let plan = plans.next().unwrap_or(FilePlan::of_size(0));
            let parent = Some(entry_parent_id.to_string());
            let ur = match push_single_file(api, entry_path, parent, strategy, true, &progress, plan).await {
                Ok(ur) => ur,
                Err(e) => {
                    let rel = entry_path
                        .strip_prefix(dir_path)
                        .unwrap_or(entry_path)
                        .display()
                        .to_string();
                    if !ui::is_json() {
                        progress.note(
                            &format!(
                                "  {} {}: {}",
                                "!".custom_color(crate::colors::RED_ERR),
                                rel.custom_color(crate::colors::INK),
                                e,
                            ),
                            Stream::Stderr,
                        );
                    }
                    failures.push((rel, e));
                    continue;
                }
            };

            let ur = match ur {
                Pushed::Uploaded(ur) => Some(ur),
                Pushed::AlreadyUploaded => {
                    already_uploaded += 1;
                    None
                }
                Pushed::Skipped => None,
            };
            // Its end line (rich) or id (quiet) was printed by push_single_file.
            if let Some(ur) = ur {
                upload_results.push(ur);
            }
        }
    }

    let total_elapsed = total_start.elapsed();
    let total_bytes: u64 = upload_results.iter().map(|r| r.encrypted_bytes).sum();
    let uploaded_count = upload_results.len();
    // The live lines are cleared here: everything below prints under them.
    let transfer = progress.summary();

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
            "transfer": transfer.to_json(),
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
fn split_stem_ext(filename: &str) -> (&str, &str) {
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
