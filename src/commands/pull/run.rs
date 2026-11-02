//! The `bb pull` entry point: load the key, dispatch to single-file or folder
//! download, and retry only when the remote file changed underneath us.
//!
//! Owns: `run` and `run_once`, including the retry loop over `PullError`.
//! Must not: resolve targets or write bytes itself (see `pull::resolve` and
//! `pull::folder`).

use super::*;

pub async fn run(file_id: String, output: Option<PathBuf>, options: PullOptions) -> Result<(), String> {
    match run_once(file_id.clone(), output.clone(), options, false).await {
        // Someone published a new version while this one was downloading. Nothing was written;
        // the new version's chunk count and read pin need a fresh look, so start over, once.
        Err(PullError::Changed(_)) => {
            if !crate::ui::is_quiet() {
                eprintln!("The file changed while it was downloading; starting again from its new version.");
            }
            // The overwrite question was answered in the first attempt (it is asked before any
            // byte is fetched); the retry must not ask it again.
            run_once(file_id, output, options, true).await.map_err(|e| match e {
                PullError::Changed(m) => format!("{m}. It changed again; run the command again"),
                PullError::Other(m) => m,
            })
        }
        other => other.map_err(String::from),
    }
}

pub(super) async fn run_once(
    file_id: String,
    output: Option<PathBuf>,
    options: PullOptions,
    already_confirmed: bool,
) -> Result<(), PullError> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // --zip mode: resolve as folder and download as a zip archive
    if options.zip {
        return Ok(run_zip(&api, &file_id, output, options.force).await?);
    }

    let (file_id, resolved_name) = resolve_file_arg(&api, &file_id).await?;

    // If no --output was given and we resolved a path, use the resolved name.
    // That name still goes through the same safety check as any server-provided
    // name (task 1733): an explicit `-o` is the only unchecked path.
    let output = match (output, resolved_name.as_deref()) {
        (Some(o), _) => Some(o),
        (None, Some(n)) => Some(crate::safe_path::default_output_path(n)?),
        (None, None) => None,
    };

    // Step 1: Get file metadata to learn chunk count and encrypted name
    let file_meta = api.get_file(&file_id).await?;

    let chunk_count = file_meta.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;

    let name_encrypted_str = file_meta
        .get("name_encrypted")
        .and_then(|v| v.as_str())
        .ok_or("server response missing name_encrypted")?;

    let master_key = load_master_key()?;

    let mut keys = crate::owned_keys::OwnedKeyResolver::new();
    let resolved = keys.resolve(&api, &master_key, &file_meta).await?;
    let request_content_key =
        if crate::commands::request::is_request_upload(&file_meta) && file_meta["owner_wrapped_file_key"].is_null() {
            Some(beebeeb_core::kdf::FileKey::from_bytes(*resolved.key.as_bytes()))
        } else {
            None
        };
    let explicit_key =
        if !crate::owned_keys::is_derived_origin(resolved.origin) && request_content_key.is_none() {
            Some(resolved.key.clone())
        } else {
            None
        };
    let decrypted_name = Some(resolved.name);

    let is_folder = file_meta.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);

    if is_folder {
        let folder_name = decrypted_name.as_deref().unwrap_or(&file_id);
        let out_dir = match output {
            Some(o) => o,
            None => crate::safe_path::default_output_path(folder_name)?,
        };
        return Ok(pull_folder(&api, &file_id, &out_dir, options.force).await?);
    }

    let display_name = decrypted_name.as_deref().unwrap_or(&file_id);

    // Output path (needed up front for the streaming write).
    // The default (no `-o`) comes from a server-provided name, so it must be a
    // single safe component; `-o` is the user's own choice and is taken as-is.
    let out_path = match output {
        Some(o) => o,
        None => crate::safe_path::default_output_path(decrypted_name.as_deref().unwrap_or(&file_id))?,
    };

    // Never clobber a local file silently (flow "CLI end to end", issue 5):
    // checked before any bytes are downloaded, for both decrypt paths below.
    ask_once(already_confirmed, &out_path, options.force, guard_existing_output)?;

    // The chunk format comes from the file's authenticated metadata, never a
    // server field (task 1943): core's reader opens a v2-pinned file only as v2.
    // L12: a request row's NAME key is derived-first (core's resolver), while its
    // content stays under C. The chunk format lives in the name's metadata.
    let pin_key = resolved.name_key.as_deref().unwrap_or(&resolved.key);
    let pin = crate::crypto::content_pin_with_key(pin_key, &file_id, name_encrypted_str)?;

    // File-request uploads are sealed under a per-file content key C, not the
    // master-derived key the streaming path assumes. Decrypt them via the
    // buffered request-key path (download whole blob → decrypt with C → write).
    if let Some(c) = &request_content_key {
        let dl_start = std::time::Instant::now();
        let encrypted_bytes = api.download_file(&file_id).await?;
        let dl_elapsed = dl_start.elapsed();

        let dec_start = std::time::Instant::now();
        let plaintext = crate::crypto::try_decrypt_all_chunks(c, &file_id, pin, &encrypted_bytes, chunk_count)?;
        let dec_elapsed = dec_start.elapsed();

        std::fs::write(&out_path, &plaintext).map_err(|e| format!("failed to write file: {e}"))?;

        let dl_speed = if dl_elapsed.as_secs_f64() > 0.0 {
            encrypted_bytes.len() as f64 / dl_elapsed.as_secs_f64()
        } else {
            0.0
        };

        if crate::ui::is_json() {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "id": file_id,
                    "name": display_name,
                    "size_bytes": plaintext.len(),
                    "download_speed_bps": dl_speed as u64,
                    "download_ms": dl_elapsed.as_millis() as u64,
                    "decrypt_ms": dec_elapsed.as_millis() as u64,
                    "output": out_path.display().to_string(),
                }))
                .unwrap()
            );
        } else if crate::ui::is_quiet() {
            println!("{}", out_path.display());
        } else {
            println!(
                "  {} {}  {}  {}",
                "\u{2713}".custom_color(crate::colors::GREEN_OK),
                display_name.custom_color(crate::colors::INK),
                crate::ui::human_size(plaintext.len() as u64).custom_color(crate::colors::INK_DIM),
                crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK)
            );
            println!(
                "    {}",
                format!(
                    "{}ms download \u{00b7} {}ms decrypt",
                    dl_elapsed.as_millis(),
                    dec_elapsed.as_millis()
                )
                .custom_color(crate::colors::INK_DIM)
            );
        }
        return Ok(());
    }

    // Estimate the encrypted size for the progress bar length.
    let size_bytes = file_meta.get("size_bytes").and_then(|v| v.as_i64()).unwrap_or(0) as u64;
    let est_encrypted = size_bytes + 28 * chunk_count as u64;

    // Stream download + decrypt with constant memory + a live progress bar.
    let show_bar = crate::ui::is_rich() && std::io::stdout().is_terminal();
    let progress: Box<dyn crate::upload::ChunkProgress> = if show_bar {
        Box::new(crate::upload::BarProgress::new(1, est_encrypted))
    } else {
        Box::new(crate::upload::NoopProgress)
    };

    let start = std::time::Instant::now();
    let stats = if let Some(key) = explicit_key {
        crate::download::stream_download_decrypt_with_file_key(
            &api,
            key,
            &file_id,
            pin,
            chunk_count,
            &out_path,
            progress.as_ref(),
            display_name,
        )
        .await?
    } else {
        crate::download::stream_download_decrypt(
            &api,
            &master_key,
            &file_id,
            pin,
            chunk_count,
            &out_path,
            progress.as_ref(),
            display_name,
        )
        .await?
    };

    progress.finish_all();
    let elapsed = start.elapsed();

    let dl_speed = if elapsed.as_secs_f64() > 0.0 {
        stats.encrypted_bytes as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    if crate::ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": file_id,
                "name": display_name,
                "size_bytes": stats.plaintext_bytes,
                "download_speed_bps": dl_speed as u64,
                "elapsed_ms": elapsed.as_millis() as u64,
                "transport": stats.transport,
                "output": out_path.display().to_string(),
            }))
            .unwrap()
        );
        return Ok(());
    }

    if crate::ui::is_quiet() {
        println!("{}", out_path.display());
        return Ok(());
    }

    // Rich mode (default)
    println!(
        "  {} {}  {}  {}",
        "\u{2713}".custom_color(crate::colors::GREEN_OK),
        display_name.custom_color(crate::colors::INK),
        crate::ui::human_size(stats.plaintext_bytes).custom_color(crate::colors::INK_DIM),
        crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK)
    );
    println!(
        "    {}",
        format!("{}ms · streamed", elapsed.as_millis()).custom_color(crate::colors::INK_DIM)
    );

    Ok(())
}
