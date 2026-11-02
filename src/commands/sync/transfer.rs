//! `transfer` items split out of `sync.rs`.

use super::*;

/// Read failures never make index/adoption failures fatal to file access.
pub(super) async fn finish_owned_sync(
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

pub(super) fn parent_remote_id(rel: &str, folders: &HashMap<String, Uuid>, root_id: Uuid) -> Option<Uuid> {
    match rel.rsplit_once('/') {
        Some((parent, _)) => folders.get(parent).copied().or(Some(root_id)),
        None => Some(root_id),
    }
}

/// "1 failed · 2 conflicts" — the counts a non-clean sync pass reports.
pub(super) fn incomplete_summary(failed: u32, conflicts: u32, held_back: u32, read_only_kept: u32) -> String {
    let mut parts = Vec::new();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if read_only_kept > 0 {
        let s = if read_only_kept == 1 { "" } else { "s" };
        parts.push(format!("{read_only_kept} read-only edit{s} kept"));
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

pub(super) async fn resolve_remote_folder(
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
                let new_id = crate::item_id::create_owned_folder(api, master_key, seg, current_parent).await?;
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn walk_remote(
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
                    pin: pin.map_err(String::from),
                    resolved: Some(resolved),
                },
            );
        }
    }

    Ok(())
}

/// Marks a pass upload that left a read-only row's edit local (SF-0002 r3 K6):
/// the upload result is `Err(READ_ONLY_KEPT + rel)`, reported as a notice.
pub(super) const READ_ONLY_KEPT: &str = "\u{0}sf-0002-read-only-kept\u{0}";

/// What sync says when it keeps a local edit of a read-only (SF-0002) row: a
/// notice, not a failure, with what the user can do. Every sync path (the pass,
/// the plain watch loop, the TUI) says exactly this.
pub(super) fn read_only_notice(rel: &str) -> String {
    format!(
        "{rel}: {} Your edit stays on this computer: save it under a new name and sync uploads that as a new file.",
        crate::crypto::OWNER_REPAIR_READ_ONLY
    )
}

/// SF-0002 round 2 (C2): what a sync upload may do with the remote row it
/// would replace. One decision for the pass's upload phase and the watch-mode
/// resync, from the one write check ([`crate::owned_keys::write_check`]).
pub(super) enum Replace<'a> {
    /// Version-replace the row in place under its own key (a shared-folder,
    /// owner-wrap or file-request row).
    InPlace(&'a ResolvedCliKey),
    /// Trash the old row and upload the edit under a fresh id: an ordinary owner
    /// row, as sync always did, or a path with no remote row at all.
    NewFile,
    /// Leave the remote row untouched (never trashed) and upload nothing this
    /// pass: it is read-only, or the check could not read it. The text says
    /// which; the next pass asks again.
    Skip(&'static str),
}

pub(super) async fn replace_decision<'a>(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    resolved: Option<&'a ResolvedCliKey>,
    has_remote: bool,
) -> Replace<'a> {
    use crate::owned_keys::{WRITE_CHECK_UNKNOWN, WriteCheck, WriteTarget};
    if !has_remote {
        return Replace::NewFile;
    }
    // A remote row this pass did not resolve cannot be checked.
    let Some(r) = resolved else {
        return Replace::Skip(WRITE_CHECK_UNKNOWN);
    };
    let target = WriteTarget {
        file_id: r.canonical["id"].as_str().unwrap_or_default(),
        is_folder: false,
        name_read_only: r.read_only,
        row_key: Some(r.key.as_ref()),
        size_bytes: r.canonical["size_bytes"].as_u64(),
        chunk_count: r.canonical["chunk_count"].as_u64().unwrap_or(1) as u32,
    };
    match crate::owned_keys::write_check(api, master_key, &target).await {
        WriteCheck::Writable if r.origin != KeyOrigin::Derived => Replace::InPlace(r),
        WriteCheck::Writable => Replace::NewFile,
        WriteCheck::UpperRepair => Replace::Skip(crate::crypto::OWNER_REPAIR_READ_ONLY),
        WriteCheck::Unknown => Replace::Skip(WRITE_CHECK_UNKNOWN),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn do_upload(
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

    // SF-0002: the same write check as the pass above (one helper). A skipped
    // file is the bare refusal text, so the callers can tell it from a failure.
    let candidate = remote.as_ref().and_then(|r| r.resolved.as_ref());
    let replacement = match replace_decision(api, &master_key, candidate, remote.is_some()).await {
        Replace::Skip(why) => return Err(why.to_string()),
        Replace::InPlace(r) => Some(r),
        Replace::NewFile => None,
    };
    // Not a version of the old remote: uploaded as a new file, and the old one
    // trashed only AFTER that (L5).
    let replaced = remote.as_ref().filter(|_| replacement.is_none()).map(|r| r.id);

    let parent_id = match rel.rsplit_once('/') {
        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
        None => Some(root_remote_id),
    }
    .or(Some(root_remote_id));

    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    let local_path = local_dir.join(rel);
    let spec = match replacement {
        Some(r) => crate::upload::UploadSpec {
            path: local_path,
            file_name: file_name.into(),
            file_id: remote.as_ref().unwrap().id,
            base_version_number: Some(replacement_version(&r.canonical)?),
            replace_pin: Some(remote.as_ref().unwrap().pin.clone()?),
            replace_file_key: Some(r.key.clone()),
            parent_id,
            concurrency: 1,
            shutdown: Arc::new(AtomicBool::new(false)),
        },
        None => new_file_spec(&local_path, file_name, parent_id),
    };
    let new_id = upload_then_trash_replaced(api, master_key, spec, &crate::upload::NoopProgress, replaced)
        .await?
        .server_id;

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
pub(super) enum DownloadOutcome {
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
    pub(super) fn of_failure(e: &crate::download::DownloadError) -> Option<Self> {
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
pub(super) fn is_still_uploading(e: &str) -> bool {
    e.contains("still in progress") || e.starts_with("download failed (409")
}

/// Does this download error mean "could not decrypt" (as opposed to a network
/// or server failure)? Both are reported and retried; neither trashes anything.
pub(super) fn is_decrypt_failure(e: &str) -> bool {
    e.contains("key derivation") || e.contains("decrypt")
}

pub(super) async fn do_download(
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

/// The upload of a local file as a NEW remote file: a fresh id of its own, no
/// version replace. Used by the watch loops (`do_upload`); the main sync path
/// builds its spec inline so it can attach progress bars. Uploads here are not
/// tied to the main Ctrl-C flag (watch-mode Ctrl-C hard-exits), so a fresh,
/// never-tripped shutdown flag is used.
pub(super) fn new_file_spec(file_path: &Path, file_name: &str, parent_id: Option<Uuid>) -> crate::upload::UploadSpec {
    crate::upload::UploadSpec {
        path: file_path.to_path_buf(),
        file_name: file_name.to_string(),
        file_id: crate::item_id::mint(),
        // Watch-loop uploads are always new files (fresh id) → no replace.
        base_version_number: None,
        replace_pin: None,
        replace_file_key: None,
        parent_id,
        // Single in-flight file in this watch-loop wrapper → full Cli cap.
        concurrency: 1,
        shutdown: Arc::new(AtomicBool::new(false)),
    }
}

/// Upload `spec`, then trash `replaced` (the old remote this upload replaces as
/// a new file, when it is not a version of it). L13 round 2 (L5): the trash
/// runs only once the upload returned, and the upload returns only after its
/// init and completion both echoed this client's id (`stream_encrypt_upload`
/// checks both). An L13 abort, or any other failure, leaves the old remote
/// untouched: never in the trash with nothing in its place. Best effort, as
/// before: a failed trash leaves both copies listed.
pub(super) async fn upload_then_trash_replaced(
    api: &ApiClient,
    master_key: Arc<beebeeb_core::kdf::MasterKey>,
    spec: crate::upload::UploadSpec,
    progress: &dyn crate::upload::ChunkProgress,
    replaced: Option<Uuid>,
) -> Result<crate::upload::UploadOutcome, String> {
    let outcome = crate::upload::stream_encrypt_upload(api, master_key, spec, progress).await?;
    if let Some(old_id) = replaced {
        let _ = api.trash_file(&old_id.to_string()).await;
    }
    Ok(outcome)
}

pub(super) async fn download_to(
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

pub(super) fn normalize_remote_path(raw: &str) -> String {
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

pub(super) fn format_size(bytes: u64) -> String {
    beebeeb_types::format_binary(bytes as i64)
}
