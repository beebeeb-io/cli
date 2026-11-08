//! Walking a remote folder and writing its files locally: create directories,
//! skip what the user declined, and stream each file through the download
//! driver.
//!
//! Owns: `pull_folder`, `pull_folder_inner`, `pull_single_file`,
//! `warn_skipped`.
//! Must not: parse the CLI argument (see `pull::resolve`) or build archives
//! (see `pull::zip`).

use super::*;
use crate::transfer_stats::FileEnd;
use crate::transfer_view::LiveProgress;
use crate::upload::{ChunkProgress, FileProgress, InFlight, Route, Stream, Tally};

pub(super) async fn pull_folder(
    api: &ApiClient,
    folder_id: &str,
    out_dir: &std::path::Path,
    force: bool,
    verbose: bool,
) -> Result<(), String> {
    // A single resolver, shared across the whole folder tree, so request-uploaded
    // files only trigger one `GET /file-requests` and each R_priv is unwrapped once.
    let mut request_keys: Option<crate::commands::request::RequestKeyResolver> = None;
    let mut keys = crate::owned_keys::OwnedKeyResolver::new();
    let mut run = report::FolderRun {
        progress: report::start(verbose),
        pulled_bytes: 0,
    };
    let skipped = pull_folder_inner(api, folder_id, out_dir, force, &mut request_keys, &mut keys, &mut run).await?;
    // The walk is over: every file is planned (spec §4.2).
    run.progress.seal_plan();
    let transfer = run.progress.summary();
    if crate::ui::is_rich() {
        report::print_summary(&transfer, skipped as u32, run.pulled_bytes, verbose);
    }
    if skipped > 0 {
        // Everything safe was pulled; the run is not a clean success, though.
        return Err(crate::exit::with_code(
            crate::exit::INCOMPLETE,
            format!("{skipped} item(s) were skipped (see the warnings above); the rest was pulled"),
        ));
    }
    Ok(())
}

/// Warn (stderr, always) that an item was skipped instead of written.
/// A skipped item, on stderr, around any live lines (task 2333); `!` in the error colour, never amber.
pub(super) fn warn_skipped(what: &str) {
    crate::transfer_view::notice::stderr(&format!(
        "  {} skipped {what}",
        "!".custom_color(crate::colors::RED_ERR)
    ));
}

/// Pull one folder level into `out_dir`. Returns how many items were skipped
/// (unsafe name, symlink, refused overwrite) across this level and below.
pub(super) async fn pull_folder_inner(
    api: &ApiClient,
    folder_id: &str,
    out_dir: &std::path::Path,
    force: bool,
    request_keys: &mut Option<crate::commands::request::RequestKeyResolver>,
    keys: &mut crate::owned_keys::OwnedKeyResolver,
    run: &mut report::FolderRun,
) -> Result<usize, String> {
    let master_key = load_master_key()?;

    std::fs::create_dir_all(out_dir).map_err(|e| format!("failed to create directory: {e}"))?;

    let listing = api.list_files(Some(folder_id)).await?;
    let files = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("invalid file listing response")?;

    run.progress.note(
        &format!(
            "  {} {} ({})",
            "pulling".custom_color(crate::colors::GREEN_OK),
            out_dir.display().to_string().custom_color(crate::colors::INK),
            crate::transfer_view::line::count(files.len() as u32, "item", "items").custom_color(crate::colors::INK_DIM),
        ),
        Stream::Stdout,
    );

    let mut skipped = 0usize;
    keys.seed(files);
    for item in files {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let is_subfolder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let name_enc = item.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");

        // Request-uploaded files decrypt their name via the request-key path.
        let content_key = resolve_request_key(api, &master_key, item, request_keys).await;
        let resolved = match keys.resolve(api, &master_key, item).await {
            Ok(r) => r,
            Err(e) => {
                warn_skipped(&e);
                skipped += 1;
                continue;
            }
        };
        let decrypted_name = resolved.name;
        let explicit_key = if !crate::owned_keys::is_derived_origin(resolved.origin) && content_key.is_none() {
            Some(resolved.key.clone())
        } else {
            None
        };

        // The name is server-provided (for file-request uploads: chosen by an
        // anonymous stranger). It becomes exactly ONE path component under
        // `out_dir`, never a path, and the result must resolve inside `out_dir`
        // (task 1733). An unsafe item is skipped, not fatal: the rest still pulls.
        let target = match crate::safe_path::safe_child(out_dir, &decrypted_name, crate::safe_path::Rules::Portable) {
            Ok(p) => p,
            Err(e) => {
                warn_skipped(&format!("{decrypted_name:?} in {}: {e}", out_dir.display()));
                skipped += 1;
                continue;
            }
        };

        if is_subfolder {
            skipped += Box::pin(pull_folder_inner(api, item_id, &target, force, request_keys, keys, run)).await?;
        } else {
            // A request upload's name is attacker-chosen, so it must not silently
            // replace a local file of the same name (same rule as a single-file
            // `bb pull`): refuse without --force.
            if content_key.is_some() {
                if let Err(e) = guard_existing_output(&target, force) {
                    warn_skipped(&format!("{}: {e}", target.display()));
                    skipped += 1;
                    continue;
                }
            }
            // No read pin (task 1943: plaintext or unreadable metadata), no
            // read: skipped like an unsafe name, and the rest still pulls.
            let pin_key = resolved.name_key.as_deref().unwrap_or(&resolved.key);
            if let Err(e) = crate::crypto::content_pin_with_key(pin_key, item_id, name_enc) {
                warn_skipped(&format!("{}: {e}", target.display()));
                skipped += 1;
                continue;
            }
            // A new version published while this file downloaded: nothing was written, so read
            // THIS file again from fresh metadata, once, and carry on with the rest of the folder.
            // (This used to abort the whole folder pull.)
            match pull_single_file(
                api,
                item_id,
                &target,
                content_key.as_ref(),
                resolved.name_key.as_deref(),
                explicit_key.clone(),
                run,
            )
            .await
            {
                Ok(()) => {}
                Err(PullError::Changed(_)) => {
                    // Nothing of it was kept: its share leaves the plan, and the next read plans it again.
                    run.progress.end_file(FileEnd::NotSent);
                    crate::transfer_view::notice::stderr(&format!(
                        "  {} {}: changed while it downloaded; reading it again",
                        "!".custom_color(crate::colors::RED_ERR),
                        target.display()
                    ));
                    pull_single_file(
                        api,
                        item_id,
                        &target,
                        content_key.as_ref(),
                        resolved.name_key.as_deref(),
                        explicit_key.clone(),
                        run,
                    )
                    .await
                    .map_err(|e| format!("{}: {}", target.display(), String::from(e)))?;
                }
                // Every other failure (an expired session, a server error, a full disk, a file that does
                // not decrypt) stops the folder pull and names the file, as it always did: a pull that
                // skipped it silently and exited 0 would look complete.
                Err(other) => return Err(format!("{}: {}", target.display(), String::from(other))),
            }
        }
    }

    Ok(skipped)
}

pub(super) async fn pull_single_file(
    api: &ApiClient,
    file_id: &str,
    out_path: &std::path::Path,
    request_content_key: Option<&beebeeb_core::kdf::FileKey>,
    name_key: Option<&beebeeb_core::kdf::FileKey>,
    explicit_key: Option<std::sync::Arc<beebeeb_core::kdf::FileKey>>,
    run: &mut report::FolderRun,
) -> Result<(), PullError> {
    let master_key = load_master_key()?;

    let file_meta = api.get_file(file_id).await?;
    let chunk_count = file_meta.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;
    let name_encrypted = file_meta
        .get("name_encrypted")
        .and_then(|v| v.as_str())
        .ok_or("server response missing name_encrypted")?;
    // Read pin from the file's authenticated metadata (task 1943).
    let pin = match (&explicit_key, name_key) {
        (_, Some(key)) => crate::crypto::content_pin_with_key(key, file_id, name_encrypted)?,
        (Some(key), None) => crate::crypto::content_pin_with_key(key, file_id, name_encrypted)?,
        (None, None) => crate::crypto::file_row_pin(&master_key, file_id, name_encrypted, request_content_key)?,
    };

    let name = out_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    // The walk reached this file: it joins the run's plan now (spec §4.2), with its storage country.
    let plan = report::FilePlan::of_row(&file_meta);
    plan.add_next(&run.progress);
    let pool = report::pool_of(&file_meta);
    let progress = &run.progress;

    // File-request uploads are sealed under a per-file content key C, which the
    // streaming path (master-derived key) can't decrypt — use the buffered
    // request-key path and return early.
    if let Some(c) = request_content_key {
        progress.set_limit(1);
        let file = progress.begin_buffered(&name, plan.bytes, plan.chunks);
        if let Some(label) = &pool {
            file.pool(label.clone());
        }
        let flight = InFlight::start(file.as_ref(), 0, Route::Api);
        let dl_start = std::time::Instant::now();
        let mut counted = None;
        let pulled = async {
            let encrypted_bytes = api.download_file(file_id).await?;
            let dl_elapsed = dl_start.elapsed();
            let plaintext = crate::crypto::try_decrypt_all_chunks(c, file_id, pin, &encrypted_bytes, chunk_count)?;
            // The body counts once it decrypted (spec §4.3): all its chunks at once.
            let body = Tally {
                bytes: encrypted_bytes.len() as u64,
                chunks: chunk_count.max(1),
                ..Tally::default()
            };
            file.body_done(body.chunks, body.bytes, Route::Api);
            counted = Some(body);
            drop(flight);

            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("failed to create dir: {e}"))?;
            }
            // Through a temp and a publish, like the streamed path (task 2408).
            crate::download::AtomicFile::write_whole(out_path, &plaintext)
                .map_err(|e| format!("failed to write: {e}"))?;
            Ok::<_, PullError>((encrypted_bytes, plaintext, dl_elapsed))
        }
        .await;
        close_buffered(progress, file, counted, pulled.is_ok());
        let (encrypted_bytes, plaintext, dl_elapsed) = pulled?;
        run.pulled_bytes += plaintext.len() as u64;

        if crate::ui::is_quiet() {
            progress.note(&out_path.display().to_string(), Stream::Stdout);
        } else if crate::ui::is_rich() {
            let ms = dl_elapsed.as_millis() as u64;
            report::print_file(
                progress,
                "    ",
                (&name, plaintext.len() as u64, encrypted_bytes.len() as u64, ms),
                &report::time_text(ms, "request"),
            );
        }
        return Ok(());
    }

    // Streaming, constant-memory decrypt, counted on the folder's one sink.
    let start = std::time::Instant::now();
    let stats = if let Some(key) = explicit_key {
        crate::download::stream_download_decrypt_with_file_key(
            api,
            key,
            file_id,
            pin,
            chunk_count,
            out_path,
            progress,
            &name,
            pool.as_ref(),
        )
        .await?
    } else {
        crate::download::stream_download_decrypt(
            api,
            &master_key,
            file_id,
            pin,
            chunk_count,
            out_path,
            progress,
            &name,
            pool.as_ref(),
        )
        .await?
    };

    let ms = start.elapsed().as_millis() as u64;
    run.pulled_bytes += stats.plaintext_bytes;

    if crate::ui::is_quiet() {
        run.progress.note(&out_path.display().to_string(), Stream::Stdout);
    } else if crate::ui::is_rich() {
        report::print_file(
            &run.progress,
            "    ",
            (&name, stats.plaintext_bytes, stats.transferred_bytes, ms),
            &report::time_text(ms, "streamed"),
        );
    }
    // In JSON mode, the folder pull prints nothing per-file — the caller handles output.
    Ok(())
}

/// Closes a buffered file. A file that failed after its body counted (`counted`: the directory or the write) has what
/// the body counted taken back off the run first, as the API stream does for its file (spec §4.3); the file then
/// closes as failed, so the lines left above an error do not show it as active (spec §6.7).
fn close_buffered(progress: &LiveProgress, file: Box<dyn FileProgress>, counted: Option<Tally>, ok: bool) {
    if let (false, Some(tally)) = (ok, counted) {
        progress.rollback_failed(tally);
    }
    file.finish(ok);
}

#[cfg(test)]
mod tests {
    use super::close_buffered;
    use crate::transfer_stats::Direction;
    use crate::transfer_view::LiveProgress;
    use crate::transfer_view::sink::Level;
    use crate::upload::{Route, Tally};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    /// Review P3 (#143): a request file that fails AFTER its body counted (the directory or the write) takes back what
    /// the body counted, as the API stream does (spec §4.3); the file closes as failed. A file that did not fail keeps it.
    #[test]
    fn a_request_file_that_fails_after_its_body_counted_takes_it_back() {
        for (ok, bytes_done, chunks_done, files_done) in [(false, 0, 0, 0), (true, 1028, 2, 1)] {
            let (progress, _record) =
                LiveProgress::for_test(Direction::Download, Level::Default, 80, Arc::new(AtomicU64::new(0)));
            progress.plan_file(1028, 2);
            progress.seal_plan();
            let file = progress.begin_buffered("request.bin", 1028, 2);
            file.body_done(2, 1028, Route::Api);
            let counted = Some(Tally {
                bytes: 1028,
                chunks: 2,
                ..Tally::default()
            });
            close_buffered(&progress, file, counted, ok);
            let end = progress.summary();
            assert_eq!(
                (end.bytes_done, end.chunks_done, end.files_done),
                (bytes_done, chunks_done, files_done),
                "ok={ok}: {end:?}"
            );
        }
    }

    /// A file that failed before its body counted has nothing to take back: the run's counts stay as they were.
    #[test]
    fn a_request_file_that_fails_before_its_body_counted_has_nothing_to_take_back() {
        let (progress, _record) =
            LiveProgress::for_test(Direction::Download, Level::Default, 80, Arc::new(AtomicU64::new(0)));
        progress.plan_file(1028, 2);
        progress.seal_plan();
        let file = progress.begin_buffered("request.bin", 1028, 2);
        close_buffered(&progress, file, None, false);
        let end = progress.summary();
        assert_eq!((end.bytes_done, end.chunks_done, end.files_done), (0, 0, 0), "{end:?}");
    }
}
