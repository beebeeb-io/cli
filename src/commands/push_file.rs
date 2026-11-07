//! One file of `bb push`, in the two steps a folder push runs apart (`--parallel` spec §7): [`prepare`] decides
//! everything that comes before the upload (the conflict check and its question, the id, the "already uploaded"
//! skip) and takes no lock; [`upload`] sends it. A folder push prepares every file first, in order, so every
//! question is asked before the first byte; then, as each file is admitted, it takes the file's upload lock
//! ([`lock`]) and re-checks under it what [`prepare`] saw ([`recheck`]), as task 1950 requires of every resume
//! decision. A single-file push takes the lock first and runs both steps under it, as before.
//!
//! Owns: `Prepared`, `Ready`, `UploadResult`. Must not: schedule files (`push_schedule`) or print the summary
//! (`push`).

use colored::Colorize;
use uuid::Uuid;

use super::push::{
    ConflictResolution, ConflictStrategy, PushCtl, Pushed, find_conflict, prompt_conflict, split_stem_ext,
};
use crate::commands::push_repeat::{self, Existing};
use crate::commands::push_report;
use crate::ui;
use crate::upload::{ChunkProgress, Stream};

/// What stage 1 decided for one file.
pub(super) enum Prepared {
    /// Upload it.
    Upload(Box<Ready>),
    /// Nothing to send: answered "skip", or this machine's finished upload of it is already there.
    Done(Pushed),
}

/// A file ready to upload, with what stage 1 saw for the re-check under its lock.
pub(super) struct Ready {
    pub(super) path: std::path::PathBuf,
    /// The local name: what the conflict rule looked up.
    file_name: String,
    /// The name it is stored under (a `--keep-both` copy is suffixed).
    upload_name: String,
    parent_id: Option<String>,
    file_id: Uuid,
    mint_tag: Option<[u8; 32]>,
    /// The row a replace versions.
    replace_file_id: Option<Uuid>,
    read_row: Option<crate::upload::ReadRow>,
    /// The same-name row the conflict rule acted on, as stage 1 found it.
    seen_conflict: Option<Uuid>,
    /// The resume record's identity (file id, session id) for this path, as stage 1 read it.
    seen_record: Option<(Uuid, Uuid)>,
}

/// Result data from a single file upload, used for summary + JSON output.
pub(super) struct UploadResult {
    pub(super) name: String,
    pub(super) id: String,
    pub(super) size_bytes: u64,
    pub(super) encrypted_bytes: u64,
    /// `"direct"` or `"api"` (additive `--json` key `transport`, as for `bb pull`).
    pub(super) transport: &'static str,
    pub(super) duration: std::time::Duration,
    /// The bytes of its chunks confirmed sent, over every upload attempt of the file (a chunk re-sent by a later
    /// attempt, after a swept session, counts each time; one HTTP chunk retried counts once, when confirmed;
    /// spec §7 `bytes_transferred`): its end line's speed.
    pub(super) transferred_bytes: u64,
}

impl UploadResult {
    pub(super) fn speed_bps(&self) -> f64 {
        let secs = self.duration.as_secs_f64();
        if secs > 0.0 {
            self.encrypted_bytes as f64 / secs
        } else {
            0.0
        }
    }
}

/// The resume record's identity for `path` now (`None`: no record).
fn record_identity(path: &std::path::Path) -> Option<(Uuid, Uuid)> {
    crate::resume::resumable_upload_now(path).map(|p| (p.file_id, p.session_id))
}

/// One push per local file across processes (task 1950), with today's refusal when another push holds it.
pub(super) fn lock(path: &std::path::Path) -> Result<crate::upload_lock::UploadLock, String> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    match crate::upload_lock::try_acquire(path) {
        Ok(lock) => Ok(lock),
        Err(crate::upload_lock::LockError::Held { pid }) => {
            let holder = pid.map(|p| format!(" (pid {p})")).unwrap_or_default();
            Err(crate::exit::with_code(
                crate::exit::BUSY,
                format!(
                    "another bb push of {file_name} is still running{holder}; \
                     wait for it to finish, or stop it and run this again to resume"
                ),
            ))
        }
        Err(crate::upload_lock::LockError::Io(e)) => Err(format!("{file_name}: upload lock: {e}")),
    }
}

/// Stage 1 for one file: the conflict check and its question, the id, the "already uploaded" skip. `in_folder`:
/// part of a folder push, which prints no `resuming` line (as before).
pub(super) async fn prepare(
    api: &crate::api::ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    path: &std::path::Path,
    parent_id: Option<String>,
    strategy: ConflictStrategy,
    in_folder: bool,
    progress: &dyn ChunkProgress,
) -> Result<Prepared, String> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
    let parent_uuid: Option<Uuid> = parent_id.as_deref().and_then(|s| s.parse().ok());
    let seen_record = record_identity(path);

    // ── Conflict detection ────────────────────────────────────────────────────
    // Decrypt existing filenames in the folder and check for a match.
    // If found, ask the user (or apply the strategy flag) how to proceed.
    let mut mint_tag: Option<[u8; 32]> = None;
    let mut read_row = None;
    let existing = find_conflict(api, master_key, &file_name, parent_uuid).await?;
    // Our own upload of this very file is not a conflict (`push_repeat`): an interrupted one resumes (task 1950,
    // with no explicit --replace / --keep-both), and a finished one, unchanged on both sides, is skipped (task
    // 2334). --replace always uploads.
    let found = match existing {
        Some(id) if strategy != ConflictStrategy::Replace => {
            let resume = strategy == ConflictStrategy::Prompt;
            push_repeat::existing_upload(api, path, id, parent_uuid, resume).await
        }
        _ => Existing::Other,
    };
    let resumes = match found {
        Existing::AlreadyUploaded => return Ok(Prepared::Done(Pushed::AlreadyUploaded)),
        Existing::Interrupted(row) => {
            read_row = row;
            true
        }
        Existing::Other => false,
    };

    let (file_id, upload_name, replace_file_id) = match existing {
        None => {
            // L13/N4: a new item's id comes from core's mint (it carries the resume tag), never from a local
            // random number.
            let minted = crate::item_id::mint_owner(master_key)?;
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
            // The same id as a new file: the driver finds the resume record and continues (or, under core's
            // rule, restarts) that upload.
            (existing_id, file_name.clone(), None)
        }
        Some(existing_id) => {
            let resolution = match strategy {
                ConflictStrategy::Replace => ConflictResolution::Replace { existing_id },
                ConflictStrategy::KeepBoth => ConflictResolution::Upload {
                    filename: kept_both(&file_name),
                },
                ConflictStrategy::Prompt => {
                    // No one can answer the prompt: fail loudly instead of reading EOF as "skip" and exiting 0
                    // (flow-6).
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
                    match r {
                        ConflictResolution::Replace { .. } => ConflictResolution::Replace { existing_id },
                        ConflictResolution::Upload { .. } => ConflictResolution::Upload {
                            filename: kept_both(&file_name),
                        },
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
                            return Ok(Prepared::Done(Pushed::Skipped));
                        }
                    }
                }
            };
            match resolution {
                // Re-use the existing UUID so the server auto-versions. Key is derived from the existing ID
                // (same as when the file was first uploaded).
                ConflictResolution::Replace { existing_id } => (existing_id, file_name.clone(), Some(existing_id)),
                // L13: a fresh item's id comes from core's minting type.
                ConflictResolution::Upload { filename } => {
                    let minted = crate::item_id::mint_owner(master_key)?;
                    mint_tag = Some(*minted.tag());
                    (minted.id(), filename, None)
                }
                ConflictResolution::Skip => return Ok(Prepared::Done(Pushed::Skipped)),
            }
        }
    };
    Ok(Prepared::Upload(Box::new(Ready {
        path: path.to_path_buf(),
        file_name,
        upload_name,
        parent_id,
        file_id,
        mint_tag,
        replace_file_id,
        read_row,
        seen_conflict: existing,
        seen_record,
    })))
}

/// `--keep-both`'s name: "report.pdf" → "report (1).pdf".
fn kept_both(file_name: &str) -> String {
    let (stem, ext) = split_stem_ext(file_name);
    if ext.is_empty() {
        format!("{stem} (1)")
    } else {
        format!("{stem} (1).{ext}")
    }
}

/// Stage 2, under the file's upload lock (spec §7): what stage 1 decided without the lock must still hold. The
/// resume record is the one it read, and the conflict rule meets the same same-name row (a row that appeared,
/// went, or another one: a push or a device wrote meanwhile). The "already uploaded" skip needs no check of its
/// own: a skipped file never reaches stage 2, and one that became uploaded since shows as a new same-name row.
pub(super) async fn recheck(
    api: &crate::api::ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    ready: &Ready,
) -> Result<(), String> {
    let parent_uuid: Option<Uuid> = ready.parent_id.as_deref().and_then(|s| s.parse().ok());
    let now = find_conflict(api, master_key, &ready.file_name, parent_uuid).await?;
    if now != ready.seen_conflict || record_identity(&ready.path) != ready.seen_record {
        return Err("changed while this push was preparing; run the command again".to_string());
    }
    Ok(())
}

/// Send a prepared file: a replace's version, pin and key, then the streaming upload, the finished record, and
/// its end line (rich) or id (quiet), printed through `progress` as it finishes. JSON: the caller collects the
/// results. The owner-only sentence (L15) is the caller's, once the lines are cleared.
pub(super) async fn upload(
    api: &crate::api::ApiClient,
    master_key: std::sync::Arc<beebeeb_core::kdf::MasterKey>,
    ready: Ready,
    progress: &dyn ChunkProgress,
    ctl: &PushCtl,
) -> Result<Pushed, String> {
    let Ready {
        path,
        upload_name,
        parent_id,
        file_id,
        mut mint_tag,
        replace_file_id,
        read_row,
        ..
    } = ready;
    // ── Streaming, constant-memory upload ─────────────────────────────────────
    // For a replace, `file_id` already equals the existing id, so the file key, the encrypted name, and the
    // server-side version all derive from one id. The v2 upload driver versions by `file_id` +
    // `base_version_number`, so on a replace we fetch the existing file's current `version_number` and pass it as
    // the optimistic-concurrency base (the server returns a stale-version 409 if it no longer matches). On a fresh
    // push, `base_version_number` stays `None` and the absent-`file_id`/new-`file_id` decides "new file".
    //
    // A replace also carries the existing file's read pin (task 1943), so a v2-pinned file is written as v2
    // again: the server keeps its v2 metadata, and a v1 replace would complete and never open.
    let (base_version_number, replace_pin, replace_file_key, fresh_id) = match replace_file_id {
        Some(existing_id) => {
            let existing = existing_id.to_string();
            let meta = api.get_file(&existing).await?;
            // The file's current version (task 1951: it sits in `download_version`); default 1 if the server
            // reports none — the v2 init still proceeds and only a genuine mismatch 409s.
            let version = crate::api::current_version_number(&meta).unwrap_or(1) as i32;
            let name_encrypted = meta
                .get("name_encrypted")
                .and_then(|v| v.as_str())
                .ok_or("server response missing name_encrypted")?;
            let resolved = crate::owned_keys::OwnedKeyResolver::new()
                .resolve(api, &master_key, &meta)
                .await?;
            // L13: a request row's content is read-only. An edit is saved as a new owner item — never sealed
            // under `C`, never published.
            if resolved.origin == beebeeb_core::owned_keys::KeyOrigin::RequestContent {
                // L13: the edit is a NEW owner item, minted by core.
                let minted = crate::item_id::mint_owner(&master_key)?;
                mint_tag = Some(*minted.tag());
                (None, None, None, Some(minted.id()))
            } else {
                // R4-B: request fields whose request list could not load stay read-only with their own reason.
                if let Some(reason) = resolved.read_only_reason {
                    return Err(reason.to_string());
                }
                // P2-10: a plaintext name proves nothing; say so, not "older iOS app".
                if resolved.read_only && crate::crypto::name_is_plaintext(&resolved.canonical) {
                    return Err(crate::owned_keys::PLAINTEXT_NAME_READ_ONLY.to_string());
                }
                // SF-0002: a row sealed by the old iOS app (its name or its CURRENT version) is read-only, and a
                // check that could not decide refuses too: only a proven-writable row gets a new version.
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
                // L13: the replacement takes its chunk-format pin from the resolver's write target, not from a
                // local re-read.
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
    let display_name = upload_name.clone();

    let parent_uuid: Option<Uuid> = match &parent_id {
        Some(pid) => Some(pid.parse().map_err(|e| format!("invalid parent ID: {e}"))?),
        None => None,
    };

    let file_start = std::time::Instant::now();
    let spec = crate::upload::UploadSpec {
        path: path.clone(),
        file_name: upload_name,
        file_id,
        resume_tag: mint_tag,
        base_version_number,
        replace_pin,
        replace_file_key,
        parent_id: parent_uuid,
        // `--parallel` chunks of this file at once, planned at that concurrency (spec §4.2); a record from before
        // `--parallel` resumes as the push planned then, at 1 (spec §4.3). The run's one budget and one
        // interrupt, whose flag is the shutdown the producer and the direct route read.
        concurrency: ctl.parallel,
        unrecorded_resume_concurrency: 1,
        chunks_in_flight: ctl.parallel,
        limits: std::sync::Arc::clone(&ctl.limits),
        interrupt: Some(ctl.interrupt.clone()),
        shutdown: std::sync::Arc::clone(ctl.interrupt.flag()),
        read_row,
    };
    let signature = crate::resume::local_signature(&path);
    let outcome = crate::upload::stream_encrypt_upload(api, master_key, spec, progress).await?;
    // Task 2334: remember this upload, so a re-run of the push skips it while the file stays unchanged. Only when
    // the file did not change while it was read (same signature before and after, the size uploaded) and the
    // server named the version it completed.
    let unchanged = signature
        .filter(|before| crate::resume::local_signature(&path) == Some(*before) && before.0 == outcome.plaintext_bytes);
    if let (Some((size, mtime_ns)), Some(version)) = (unchanged, outcome.version_number) {
        crate::resume::record_finished(
            &path,
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

    // Mark this upload so a `bb watch` remote-event handler in the same process doesn't redundantly download the
    // file we just sent.
    crate::loopback::mark_uploaded(&server_id);

    let upload_result = UploadResult {
        name: display_name.clone(),
        id: server_id.clone(),
        size_bytes: outcome.plaintext_bytes,
        encrypted_bytes: outcome.ciphertext_bytes,
        transport: outcome.transport,
        duration: file_elapsed,
        transferred_bytes: outcome.transferred_bytes,
    };

    // ── Per-file output ─────────────────────────────────────────────────────
    // Quiet prints the id (no live lines are drawn then); rich prints the end line around the live lines, in the
    // order the files finish.
    if ui::is_quiet() {
        println!("{}", server_id);
    } else if ui::is_rich() {
        let line = push_report::file_line(
            &display_name,
            upload_result.encrypted_bytes,
            upload_result.transferred_bytes,
            file_elapsed,
        );
        progress.note(&line, Stream::Stdout);
    }

    Ok(Pushed::Uploaded(upload_result))
}
