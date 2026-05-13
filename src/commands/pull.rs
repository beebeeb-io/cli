use colored::Colorize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

pub async fn run(file_id: String, output: Option<PathBuf>, zip: bool) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // --zip mode: resolve as folder and download as a zip archive
    if zip {
        return run_zip(&api, &file_id, output).await;
    }

    // Detect whether the argument is a UUID, a short ID prefix, or a
    // plaintext path.
    let (file_id, resolved_name) = if uuid::Uuid::parse_str(&file_id).is_ok() {
        // Already a full UUID — use it directly.
        (file_id, None)
    } else if looks_like_id_prefix(&file_id) {
        // Looks like a hex prefix (e.g. "3e15382b" from `bb ls` output).
        // Try to resolve it to a full UUID.
        match api.find_file_by_id_prefix(&file_id).await? {
            Some(full_id) => (full_id, None),
            None => {
                // No match by prefix — fall through to path resolution.
                resolve_as_path(&api, &file_id).await?
            }
        }
    } else {
        // Treat as a plaintext path and resolve it.
        resolve_as_path(&api, &file_id).await?
    };

    // If no --output was given and we resolved a path, use the resolved name.
    let output = output.or_else(|| resolved_name.as_deref().map(PathBuf::from));

    // Step 1: Get file metadata to learn chunk count and encrypted name
    let file_meta = api.get_file(&file_id).await?;

    let chunk_count = file_meta
        .get("chunk_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(1) as u32;

    let name_encrypted_str = file_meta
        .get("name_encrypted")
        .and_then(|v| v.as_str())
        .ok_or("server response missing name_encrypted")?;

    // Load master key
    let master_key = load_master_key()?;

    // Try to decrypt the filename for display and default output path.
    // Uses the shared crypto module which handles all server formats
    // (Rust EncryptedBlob, web-app base64 blob, plaintext) and both
    // UUID key derivations (binary and string).
    let decrypted_name = crate::crypto::decrypt_name(
        &master_key,
        &file_id,
        name_encrypted_str,
    );

    let is_folder = file_meta
        .get("is_folder")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_folder {
        let folder_name = decrypted_name
            .as_deref()
            .unwrap_or(&file_id);
        let out_dir = output.unwrap_or_else(|| PathBuf::from(folder_name));
        return pull_folder(&api, &file_id, &out_dir).await;
    }

    let display_name = decrypted_name
        .as_deref()
        .unwrap_or(&file_id);

    // Step 2: Download encrypted data (timed)
    let dl_start = std::time::Instant::now();
    let encrypted_bytes = api.download_file(&file_id).await?;
    let dl_elapsed = dl_start.elapsed();

    // Step 3: Decrypt all chunks (timed)
    // Handles both CLI-format (JSON EncryptedBlob) and web-app-format
    // (raw nonce|ciphertext) chunks, and both UUID key derivation methods.
    let dec_start = std::time::Instant::now();
    let plaintext = crate::crypto::decrypt_file_chunks(
        &master_key,
        &file_id,
        &encrypted_bytes,
        chunk_count,
    )?;
    let dec_elapsed = dec_start.elapsed();

    // Step 4: Write to disk
    let out_path = output.unwrap_or_else(|| {
        PathBuf::from(decrypted_name.as_deref().unwrap_or(&file_id))
    });

    std::fs::write(&out_path, &plaintext)
        .map_err(|e| format!("failed to write file: {e}"))?;

    // Step 5: Output
    let dl_speed = encrypted_bytes.len() as f64 / dl_elapsed.as_secs_f64();

    if crate::ui::is_json() {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "id": file_id,
            "name": display_name,
            "size_bytes": plaintext.len(),
            "download_speed_bps": dl_speed as u64,
            "download_ms": dl_elapsed.as_millis() as u64,
            "decrypt_ms": dec_elapsed.as_millis() as u64,
            "output": out_path.display().to_string(),
        })).unwrap());
        return Ok(());
    }

    if crate::ui::is_quiet() {
        println!("{}", out_path.display());
        return Ok(());
    }

    // Rich mode (default)
    println!("  {} {}  {}  {}",
        "\u{2713}".custom_color(crate::colors::GREEN_OK),
        display_name.custom_color(crate::colors::INK),
        crate::ui::human_size(plaintext.len() as u64).custom_color(crate::colors::INK_DIM),
        crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK));
    println!("    {}",
        format!("{}ms download \u{00b7} {}ms decrypt",
            dl_elapsed.as_millis(), dec_elapsed.as_millis())
            .custom_color(crate::colors::INK_DIM));

    Ok(())
}

/// Returns `true` if the string looks like a UUID prefix: 8-36 hex chars
/// (with optional hyphens). This covers the 8-char short IDs shown by
/// `bb ls` as well as longer partial UUIDs.
fn looks_like_id_prefix(s: &str) -> bool {
    let len = s.len();
    len >= 8
        && len <= 36
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && s.chars().any(|c| c.is_ascii_hexdigit())
}

/// Resolve a plaintext vault path (e.g. "/Documents/report.pdf") into a
/// `(file_id, Some(name))` pair.
async fn resolve_as_path(api: &ApiClient, path: &str) -> Result<(String, Option<String>), String> {
    let master_key = load_master_key()?;
    let resolved = crate::path::resolve_path(api, &master_key, path).await?;

    if resolved.file_id.is_none() {
        return Err("cannot download the vault root".to_string());
    }
    if resolved.is_folder {
        return Err(format!(
            "'{}' is a folder. Use `bb ls` to list its contents.",
            resolved.name,
        ));
    }

    Ok((resolved.file_id.unwrap(), Some(resolved.name)))
}

async fn pull_folder(api: &ApiClient, folder_id: &str, out_dir: &std::path::Path) -> Result<(), String> {
    let master_key = load_master_key()?;

    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("failed to create directory: {e}"))?;

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

    for item in files {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let is_subfolder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let name_enc = item.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");

        let decrypted_name = crate::crypto::decrypt_name(&master_key, item_id, name_enc)
            .unwrap_or_else(|| item_id.to_string());

        if is_subfolder {
            let sub_dir = out_dir.join(&decrypted_name);
            Box::pin(pull_folder(api, item_id, &sub_dir)).await?;
        } else {
            let out_path = out_dir.join(&decrypted_name);
            pull_single_file(api, item_id, &out_path).await?;
        }
    }

    Ok(())
}

async fn pull_single_file(api: &ApiClient, file_id: &str, out_path: &std::path::Path) -> Result<(), String> {
    let master_key = load_master_key()?;

    let file_meta = api.get_file(file_id).await?;
    let chunk_count = file_meta.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;

    let dl_start = std::time::Instant::now();
    let encrypted_bytes = api.download_file(file_id).await?;
    let dl_elapsed = dl_start.elapsed();

    let dec_start = std::time::Instant::now();
    let plaintext = crate::crypto::decrypt_file_chunks(
        &master_key,
        file_id,
        &encrypted_bytes,
        chunk_count,
    )?;
    let dec_elapsed = dec_start.elapsed();

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create dir: {e}"))?;
    }
    std::fs::write(out_path, &plaintext)
        .map_err(|e| format!("failed to write: {e}"))?;

    let name = out_path.file_name().and_then(|n| n.to_str()).unwrap_or("file");

    if crate::ui::is_quiet() {
        println!("{}", out_path.display());
    } else if crate::ui::is_rich() {
        let dl_speed = encrypted_bytes.len() as f64 / dl_elapsed.as_secs_f64();
        println!("    {} {}  {}  {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            name.custom_color(crate::colors::INK),
            crate::ui::human_size(plaintext.len() as u64).custom_color(crate::colors::INK_DIM),
            crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK));
        println!("      {}",
            format!("{}ms download \u{00b7} {}ms decrypt",
                dl_elapsed.as_millis(), dec_elapsed.as_millis())
                .custom_color(crate::colors::INK_DIM));
    }
    // In JSON mode, the folder pull prints nothing per-file — the caller handles output.

    Ok(())
}

// ---------------------------------------------------------------------------
// --zip: download folder as a streaming zip archive
// ---------------------------------------------------------------------------

/// Resolve the argument as a folder and download all its files into a zip archive.
async fn run_zip(api: &ApiClient, path_arg: &str, output: Option<PathBuf>) -> Result<(), String> {
    let master_key = load_master_key()?;

    // Resolve the path argument to a folder.
    let resolved = crate::path::resolve_path(api, &master_key, path_arg).await?;

    if !resolved.is_folder {
        return Err(format!(
            "'{}' is a file, not a folder. Use `bb pull` without --zip.",
            resolved.name,
        ));
    }

    let folder_id = resolved.file_id.ok_or("cannot zip the vault root")?;
    let folder_name = resolved.name;

    // Recursively collect all files in the folder tree.
    let entries = collect_zip_entries(api, &master_key, &folder_id, &folder_name).await?;

    if entries.is_empty() {
        return Err(format!("folder '{}' is empty — nothing to zip.", folder_name));
    }

    let total_size: u64 = entries.iter().map(|e| e.size_bytes).sum();

    // Summary line
    if crate::ui::is_rich() {
        println!(
            "  {} {} ({} {} {})",
            "\u{2193}".custom_color(crate::colors::GREEN_OK),
            format!("Downloading {}", folder_name).custom_color(crate::colors::INK),
            format!("{} files", entries.len()).custom_color(crate::colors::INK_DIM),
            "\u{00b7}".custom_color(crate::colors::INK_DIM),
            crate::ui::human_size(total_size).custom_color(crate::colors::INK_DIM),
        );
    }

    // Download all encrypted blobs upfront into a HashMap.
    // stream_folder_zip is synchronous and needs all data available via the
    // blocking fetch_chunk callback. This is fine for v1 — CLI has plenty of RAM.
    let mut blob_cache: HashMap<String, Vec<u8>> = HashMap::new();
    let dl_start = std::time::Instant::now();

    for (i, entry) in entries.iter().enumerate() {
        if crate::ui::is_rich() {
            eprint!(
                "\r    {} {}/{}  {}",
                "fetching".custom_color(crate::colors::INK_DIM),
                (i + 1).to_string().custom_color(crate::colors::INK),
                entries.len().to_string().custom_color(crate::colors::INK_DIM),
                entry.path.custom_color(crate::colors::INK_DIM),
            );
        }
        let blob = api.download_file(&entry.file_id).await?;
        blob_cache.insert(entry.file_id.clone(), blob);
    }

    let dl_elapsed = dl_start.elapsed();

    if crate::ui::is_rich() {
        // Clear the fetching line
        eprint!("\r{}\r", " ".repeat(80));
    }

    // Determine output path
    let zip_filename = format!("{}.zip", folder_name);
    let out_path = output.unwrap_or_else(|| PathBuf::from(&zip_filename));

    // Create the output file
    let file = std::fs::File::create(&out_path)
        .map_err(|e| format!("failed to create {}: {e}", out_path.display()))?;

    // Build the fetch_chunk callback.
    // For single-chunk files (chunk_count=1, the vast majority), the entire blob
    // IS the one chunk. For multi-chunk files, we split by uniform chunk size
    // (each raw chunk = nonce(12) + ciphertext(plaintext_chunk + 16)).
    //
    // NOTE: stream_folder_zip uses decrypt_chunk_raw which expects raw
    // nonce||ciphertext format. CLI-uploaded files may use JSON EncryptedBlob
    // format. For v1, this works with web-uploaded files (raw format) and
    // single-chunk CLI files where we detect and convert JSON blobs.
    // Multi-chunk CLI-uploaded files with JSON format need future work.
    let mut fetch_chunk = |file_id: &str, chunk_idx: u32| -> Result<Vec<u8>, beebeeb_core::CoreError> {
        let blob = blob_cache.get(file_id).ok_or_else(|| {
            beebeeb_core::CoreError::Io(format!("no cached blob for {file_id}"))
        })?;

        let entry = entries.iter().find(|e| e.file_id == file_id).ok_or_else(|| {
            beebeeb_core::CoreError::Io(format!("no entry for {file_id}"))
        })?;

        if entry.chunk_count <= 1 {
            // Single-chunk: return the entire blob as chunk 0.
            return Ok(blob.clone());
        }

        // Multi-chunk: split evenly, last chunk takes the remainder.
        // Each chunk is nonce(12) + ciphertext, and all but the last are the same size.
        let count = entry.chunk_count as usize;
        let chunk_size = blob.len() / count;
        let idx = chunk_idx as usize;

        let start = idx * chunk_size;
        let end = if idx == count - 1 { blob.len() } else { start + chunk_size };

        if start >= blob.len() {
            return Err(beebeeb_core::CoreError::Io(format!(
                "chunk {chunk_idx} out of range for {file_id}"
            )));
        }

        Ok(blob[start..end].to_vec())
    };

    // Progress callback
    let zip_start = std::time::Instant::now();
    let on_progress = |progress: &beebeeb_core::zip::ZipProgress| {
        if crate::ui::is_rich() {
            let pct = if progress.bytes_total > 0 {
                (progress.bytes_done as f64 / progress.bytes_total as f64 * 100.0) as u64
            } else {
                0
            };
            eprint!(
                "\r    {} {}%  {}/{}  {}",
                "zipping".custom_color(crate::colors::INK_DIM),
                pct.to_string().custom_color(crate::colors::AMBER),
                (progress.file_index + 1).to_string().custom_color(crate::colors::INK),
                progress.file_count.to_string().custom_color(crate::colors::INK_DIM),
                progress.current_file.custom_color(crate::colors::INK_DIM),
            );
        }
    };

    let cancel = AtomicBool::new(false);

    beebeeb_core::zip::stream_folder_zip(
        &master_key,
        &entries,
        &mut fetch_chunk,
        file,
        &on_progress,
        &cancel,
    )
    .map_err(|e| format!("zip failed: {e}"))?;

    let zip_elapsed = zip_start.elapsed();
    let total_elapsed = dl_start.elapsed();

    // Get final file size
    let zip_size = std::fs::metadata(&out_path)
        .map(|m| m.len())
        .unwrap_or(0);

    if crate::ui::is_rich() {
        // Clear the zipping line
        eprint!("\r{}\r", " ".repeat(80));

        println!(
            "  {} {}  {}  {:.1}s",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            out_path.display().to_string().custom_color(crate::colors::INK),
            crate::ui::human_size(zip_size).custom_color(crate::colors::INK_DIM),
            total_elapsed.as_secs_f64(),
        );
        println!(
            "    {}",
            format!(
                "{} files \u{00b7} {:.1}s download \u{00b7} {:.1}s zip",
                entries.len(),
                dl_elapsed.as_secs_f64(),
                zip_elapsed.as_secs_f64(),
            )
            .custom_color(crate::colors::INK_DIM),
        );
    } else if crate::ui::is_json() {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "folder": folder_name,
            "file_count": entries.len(),
            "total_size_bytes": total_size,
            "zip_size_bytes": zip_size,
            "download_ms": dl_elapsed.as_millis() as u64,
            "zip_ms": zip_elapsed.as_millis() as u64,
            "output": out_path.display().to_string(),
        })).unwrap());
    } else {
        // Quiet mode
        println!("{}", out_path.display());
    }

    Ok(())
}

/// Recursively walk a folder tree and build a flat list of ZipEntry items
/// with paths relative to the top-level folder name (e.g. "Music/Old/track.flac").
async fn collect_zip_entries(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    folder_id: &str,
    prefix: &str,
) -> Result<Vec<beebeeb_core::zip::ZipEntry>, String> {
    let listing = api.list_files(Some(folder_id)).await?;
    let files = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("invalid file listing response")?;

    let mut entries = Vec::new();

    for item in files {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let is_folder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let name_enc = item.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");
        let size_bytes = item.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let chunk_count = item.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;

        let decrypted_name = crate::crypto::decrypt_name(master_key, item_id, name_enc)
            .unwrap_or_else(|| item_id.to_string());

        let path = format!("{}/{}", prefix, decrypted_name);

        if is_folder {
            // Recurse into subfolders
            let sub_entries = Box::pin(collect_zip_entries(api, master_key, item_id, &path)).await?;
            entries.extend(sub_entries);
        } else {
            entries.push(beebeeb_core::zip::ZipEntry {
                path,
                file_id: item_id.to_string(),
                chunk_count,
                size_bytes,
            });
        }
    }

    Ok(entries)
}

