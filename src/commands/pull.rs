use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

pub async fn run(file_id: String, output: Option<PathBuf>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Step 1: Get file metadata to learn chunk count and encrypted name
    let metadata_pb = ProgressBar::new_spinner();
    metadata_pb.set_style(
        ProgressStyle::with_template("  {spinner:.yellow} fetching metadata")
            .unwrap(),
    );
    metadata_pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let file_meta = api.get_file(&file_id).await?;
    metadata_pb.finish_and_clear();

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

    // Step 2: Download encrypted data
    let dl_pb = ProgressBar::new_spinner();
    dl_pb.set_style(
        ProgressStyle::with_template("  {spinner:.yellow} downloading {msg}")
            .unwrap(),
    );
    dl_pb.set_message(display_name.to_string());
    dl_pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let encrypted_bytes = api.download_file(&file_id).await?;
    dl_pb.finish_and_clear();

    // Step 3: Decrypt all chunks using the shared helper that handles both
    // CLI-format (JSON EncryptedBlob) and web-app-format (raw nonce|ciphertext)
    // chunks, and both UUID key derivation methods (binary and string).
    let decrypt_pb = ProgressBar::new_spinner();
    decrypt_pb.set_style(
        ProgressStyle::with_template("  {spinner:.yellow} decrypting")
            .unwrap(),
    );
    decrypt_pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let plaintext = crate::crypto::decrypt_file_chunks(
        &master_key,
        &file_id,
        &encrypted_bytes,
        chunk_count,
    )?;

    decrypt_pb.finish_and_clear();

    // Step 4: Write to disk
    let out_path = output.unwrap_or_else(|| {
        PathBuf::from(decrypted_name.as_deref().unwrap_or(&file_id))
    });

    std::fs::write(&out_path, &plaintext)
        .map_err(|e| format!("failed to write file: {e}"))?;

    let size_str = format_size(plaintext.len() as u64);
    println!(
        "  {} {} {} {}",
        "OK".custom_color(crate::colors::GREEN_OK),
        out_path.display().to_string().custom_color(crate::colors::INK),
        "·".custom_color(crate::colors::INK_DIM),
        format!("{size_str} · {chunk_count} chunk{} · decrypted",
            if chunk_count == 1 { "" } else { "s" },
        )
        .custom_color(crate::colors::INK_DIM),
    );

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

    let encrypted_bytes = api.download_file(file_id).await?;

    let plaintext = crate::crypto::decrypt_file_chunks(
        &master_key,
        file_id,
        &encrypted_bytes,
        chunk_count,
    )?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create dir: {e}"))?;
    }
    std::fs::write(out_path, &plaintext)
        .map_err(|e| format!("failed to write: {e}"))?;

    let name = out_path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    println!(
        "  {} {}",
        "OK".custom_color(crate::colors::GREEN_OK),
        name.custom_color(crate::colors::INK_DIM),
    );
    Ok(())
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} kB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
