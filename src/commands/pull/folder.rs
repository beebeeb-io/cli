//! Walking a remote folder and writing its files locally: create directories,
//! skip what the user declined, and stream each file through the download
//! driver.
//!
//! Owns: `pull_folder`, `pull_folder_inner`, `pull_single_file`,
//! `warn_skipped`.
//! Must not: parse the CLI argument (see `pull::resolve`) or build archives
//! (see `pull::zip`).

use super::*;

pub(super) async fn pull_folder(api: &ApiClient, folder_id: &str, out_dir: &std::path::Path, force: bool) -> Result<(), String> {
    // A single resolver, shared across the whole folder tree, so request-uploaded
    // files only trigger one `GET /file-requests` and each R_priv is unwrapped once.
    let mut request_keys: Option<crate::commands::request::RequestKeyResolver> = None;
    let mut keys = crate::owned_keys::OwnedKeyResolver::new();
    let skipped = pull_folder_inner(api, folder_id, out_dir, force, &mut request_keys, &mut keys).await?;
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
pub(super) fn warn_skipped(what: &str) {
    eprintln!("  {} skipped {what}", "!".custom_color(crate::colors::AMBER));
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
) -> Result<usize, String> {
    let master_key = load_master_key()?;

    std::fs::create_dir_all(out_dir).map_err(|e| format!("failed to create directory: {e}"))?;

    let listing = api.list_files(Some(folder_id)).await?;
    let files = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("invalid file listing response")?;

    println!(
        "  {} {} ({})",
        "pulling".custom_color(crate::colors::GREEN_OK),
        out_dir.display().to_string().custom_color(crate::colors::INK),
        format!("{} items", files.len()).custom_color(crate::colors::INK_DIM),
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
        let explicit_key = if resolved.origin != beebeeb_core::owned_keys::KeyOrigin::Derived && content_key.is_none() {
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
            skipped += Box::pin(pull_folder_inner(api, item_id, &target, force, request_keys, keys)).await?;
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
            if let Err(e) = crate::crypto::content_pin_with_key(&resolved.key, item_id, name_enc) {
                warn_skipped(&format!("{}: {e}", target.display()));
                skipped += 1;
                continue;
            }
            // A new version published while this file downloaded: nothing was written, so read
            // THIS file again from fresh metadata, once, and carry on with the rest of the folder.
            // (This used to abort the whole folder pull.)
            match pull_single_file(api, item_id, &target, content_key.as_ref(), explicit_key.clone()).await {
                Ok(()) => {}
                Err(PullError::Changed(_)) => {
                    eprintln!(
                        "  {} {}: changed while it downloaded; reading it again",
                        "!".custom_color(crate::colors::AMBER),
                        target.display()
                    );
                    pull_single_file(api, item_id, &target, content_key.as_ref(), explicit_key.clone())
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
    explicit_key: Option<std::sync::Arc<beebeeb_core::kdf::FileKey>>,
) -> Result<(), PullError> {
    let master_key = load_master_key()?;

    let file_meta = api.get_file(file_id).await?;
    let chunk_count = file_meta.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;
    let name_encrypted = file_meta
        .get("name_encrypted")
        .and_then(|v| v.as_str())
        .ok_or("server response missing name_encrypted")?;
    // Read pin from the file's authenticated metadata (task 1943).
    let pin = match &explicit_key {
        Some(key) => crate::crypto::content_pin_with_key(key, file_id, name_encrypted)?,
        None => crate::crypto::file_row_pin(&master_key, file_id, name_encrypted, request_content_key)?,
    };

    let name = out_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    // File-request uploads are sealed under a per-file content key C, which the
    // streaming path (master-derived key) can't decrypt — use the buffered
    // request-key path and return early.
    if let Some(c) = request_content_key {
        let dl_start = std::time::Instant::now();
        let encrypted_bytes = api.download_file(file_id).await?;
        let dl_elapsed = dl_start.elapsed();
        let plaintext = crate::crypto::try_decrypt_all_chunks(c, file_id, pin, &encrypted_bytes, chunk_count)?;

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("failed to create dir: {e}"))?;
        }
        std::fs::write(out_path, &plaintext).map_err(|e| format!("failed to write: {e}"))?;

        if crate::ui::is_quiet() {
            println!("{}", out_path.display());
        } else if crate::ui::is_rich() {
            let dl_speed = if dl_elapsed.as_secs_f64() > 0.0 {
                encrypted_bytes.len() as f64 / dl_elapsed.as_secs_f64()
            } else {
                0.0
            };
            println!(
                "    {} {}  {}  {}",
                "\u{2713}".custom_color(crate::colors::GREEN_OK),
                name.custom_color(crate::colors::INK),
                crate::ui::human_size(plaintext.len() as u64).custom_color(crate::colors::INK_DIM),
                crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK)
            );
            println!(
                "      {}",
                format!("{}ms · request", dl_elapsed.as_millis()).custom_color(crate::colors::INK_DIM)
            );
        }
        return Ok(());
    }

    // Streaming, constant-memory decrypt. Folder pulls print a per-file line, so
    // use a no-op progress sink (no nested bars).
    let start = std::time::Instant::now();
    let stats = if let Some(key) = explicit_key {
        crate::download::stream_download_decrypt_with_file_key(
            api,
            key,
            file_id,
            pin,
            chunk_count,
            out_path,
            &crate::upload::NoopProgress,
            &name,
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
            &crate::upload::NoopProgress,
            &name,
        )
        .await?
    };

    let elapsed = start.elapsed();

    if crate::ui::is_quiet() {
        println!("{}", out_path.display());
    } else if crate::ui::is_rich() {
        let dl_speed = if elapsed.as_secs_f64() > 0.0 {
            stats.encrypted_bytes as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        println!(
            "    {} {}  {}  {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            name.custom_color(crate::colors::INK),
            crate::ui::human_size(stats.plaintext_bytes).custom_color(crate::colors::INK_DIM),
            crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK)
        );
        println!(
            "      {}",
            format!("{}ms · streamed", elapsed.as_millis()).custom_color(crate::colors::INK_DIM)
        );
    }
    // In JSON mode, the folder pull prints nothing per-file — the caller handles output.

    Ok(())
}
