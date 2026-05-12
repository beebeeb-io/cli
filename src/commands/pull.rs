use colored::Colorize;
use std::path::PathBuf;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

pub async fn run(file_id: String, output: Option<PathBuf>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

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

