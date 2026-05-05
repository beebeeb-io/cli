use base64::Engine;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;

use crate::api::ApiClient;
use crate::config::load_config;

/// 1 MiB chunk size (matches beebeeb_types::CHUNK_SIZE)
const CHUNK_SIZE: usize = 1024 * 1024;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Load the master key from config, returning a beebeeb_core MasterKey.
fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
    let config = load_config();
    let mk_b64 = config
        .master_key
        .ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes = b64()
        .decode(&mk_b64)
        .map_err(|e| format!("invalid master key in config: {e}"))?;
    if mk_bytes.len() != 32 {
        return Err(format!(
            "master key must be 32 bytes, got {}",
            mk_bytes.len()
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&mk_bytes);
    Ok(beebeeb_core::kdf::MasterKey::from_bytes(arr))
}

pub async fn run(path: PathBuf, parent_id: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }

    if path.is_dir() {
        return push_directory(&api, &path, parent_id).await;
    }

    push_single_file(&api, &path, parent_id).await
}

async fn push_single_file(api: &ApiClient, path: &std::path::Path, parent_id: Option<String>) -> Result<(), String> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let file_bytes =
        std::fs::read(&path).map_err(|e| format!("failed to read file: {e}"))?;
    let file_size = file_bytes.len() as u64;

    // Load master key and derive a per-file key
    let master_key = load_master_key()?;
    let file_id = uuid::Uuid::new_v4();
    let file_key =
        beebeeb_core::kdf::derive_file_key(&master_key, file_id.as_bytes());

    // Encrypt the filename
    let name_blob = beebeeb_core::encrypt::encrypt_metadata(&file_key, &file_name)
        .map_err(|e| format!("failed to encrypt filename: {e}"))?;
    let name_encrypted =
        serde_json::to_string(&name_blob).map_err(|e| format!("failed to serialize name blob: {e}"))?;

    // Chunk the file and encrypt each chunk
    let total_chunks = if file_bytes.is_empty() {
        1 // at least one chunk for empty files
    } else {
        file_bytes.len().div_ceil(CHUNK_SIZE)
    };

    let pb = ProgressBar::new(total_chunks as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.yellow} encrypting  {pos}/{len} chunks  {bar:24.yellow/dark_gray}",
        )
        .unwrap()
        .progress_chars("---"),
    );

    let mut encrypted_chunks: Vec<(u32, Vec<u8>)> = Vec::with_capacity(total_chunks);
    let mut total_encrypted_size: i64 = 0;

    if file_bytes.is_empty() {
        // Encrypt an empty chunk
        let blob = beebeeb_core::encrypt::encrypt_chunk(&file_key, &[])
            .map_err(|e| format!("failed to encrypt chunk: {e}"))?;
        let serialized =
            serde_json::to_vec(&blob).map_err(|e| format!("failed to serialize chunk: {e}"))?;
        total_encrypted_size += serialized.len() as i64;
        encrypted_chunks.push((0, serialized));
        pb.inc(1);
    } else {
        for (i, chunk) in file_bytes.chunks(CHUNK_SIZE).enumerate() {
            let blob = beebeeb_core::encrypt::encrypt_chunk(&file_key, chunk)
                .map_err(|e| format!("failed to encrypt chunk {i}: {e}"))?;
            let serialized = serde_json::to_vec(&blob)
                .map_err(|e| format!("failed to serialize chunk {i}: {e}"))?;
            total_encrypted_size += serialized.len() as i64;
            encrypted_chunks.push((i as u32, serialized));
            pb.inc(1);
        }
    }
    pb.finish_and_clear();

    // Build metadata JSON matching the server's UploadMetadata struct
    let parent_uuid = match &parent_id {
        Some(pid) => {
            let parsed: uuid::Uuid =
                pid.parse().map_err(|e| format!("invalid parent ID: {e}"))?;
            Some(parsed)
        }
        None => None,
    };

    let metadata = serde_json::json!({
        "name_encrypted": name_encrypted,
        "parent_id": parent_uuid,
        "mime_type": guess_mime_type(&file_name),
        "size_bytes": total_encrypted_size,
    });
    let metadata_json = serde_json::to_string(&metadata)
        .map_err(|e| format!("failed to serialize metadata: {e}"))?;

    // Upload
    let upload_pb = ProgressBar::new_spinner();
    upload_pb.set_style(
        ProgressStyle::with_template("  {spinner:.yellow} uploading {msg}")
            .unwrap(),
    );
    upload_pb.set_message(format!(
        "{} ({} chunk{})",
        file_name,
        encrypted_chunks.len(),
        if encrypted_chunks.len() == 1 { "" } else { "s" },
    ));
    upload_pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let result = api
        .upload_encrypted(&metadata_json, &encrypted_chunks)
        .await?;

    upload_pb.finish_and_clear();

    let file_id_str = file_id.to_string();
    let server_id = result
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(&file_id_str);

    let size_str = format_size(file_size);
    println!(
        "  {} {} {} {}",
        "OK".custom_color(crate::colors::GREEN_OK),
        file_name.custom_color(crate::colors::INK),
        "·".custom_color(crate::colors::INK_DIM),
        format!(
            "{size_str} · {} chunk{} · encrypted · {}",
            encrypted_chunks.len(),
            if encrypted_chunks.len() == 1 { "" } else { "s" },
            server_id,
        )
        .custom_color(crate::colors::INK_DIM),
    );

    Ok(())
}

fn guess_mime_type(filename: &str) -> Option<String> {
    let ext = filename.rsplit('.').next()?.to_lowercase();
    let mime = match ext.as_str() {
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "gzip" => "application/gzip",
        "tar" => "application/x-tar",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "md" => "text/markdown",
        "rs" => "text/x-rust",
        "toml" => "application/toml",
        _ => "application/octet-stream",
    };
    Some(mime.to_string())
}

async fn push_directory(
    api: &ApiClient,
    dir_path: &std::path::Path,
    parent_id: Option<String>,
) -> Result<(), String> {
    let master_key = load_master_key()?;
    let dir_name = dir_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("folder")
        .to_string();

    println!(
        "  {} {}",
        "scanning".custom_color(crate::colors::GREEN_OK),
        dir_path.display().to_string().custom_color(crate::colors::INK),
    );

    let mut entries: Vec<(PathBuf, bool)> = Vec::new();
    collect_entries(dir_path, &mut entries)?;

    let file_count = entries.iter().filter(|(_, is_dir)| !*is_dir).count();
    let folder_count = entries.iter().filter(|(_, is_dir)| *is_dir).count();

    println!(
        "  {} {}",
        "found".custom_color(crate::colors::GREEN_OK),
        format!("{file_count} files, {folder_count} folders").custom_color(crate::colors::INK_DIM),
    );

    let folder_id = uuid::Uuid::new_v4();
    let folder_key = beebeeb_core::kdf::derive_file_key(&master_key, folder_id.as_bytes());
    let name_blob = beebeeb_core::encrypt::encrypt_metadata(&folder_key, &dir_name)
        .map_err(|e| format!("failed to encrypt folder name: {e}"))?;
    let name_encrypted = serde_json::to_string(&name_blob)
        .map_err(|e| format!("failed to serialize: {e}"))?;

    let parent_uuid = parent_id
        .as_deref()
        .map(|p| p.parse::<uuid::Uuid>())
        .transpose()
        .map_err(|e| format!("invalid parent ID: {e}"))?;

    let root_folder = api
        .create_folder(&name_encrypted, parent_uuid, Some(folder_id))
        .await?;
    let root_id: uuid::Uuid = root_folder
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("missing folder id")?
        .parse()
        .map_err(|e| format!("invalid folder id: {e}"))?;

    let mut folder_map: std::collections::HashMap<PathBuf, uuid::Uuid> =
        std::collections::HashMap::new();
    folder_map.insert(dir_path.to_path_buf(), root_id);

    let pb = ProgressBar::new(entries.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.yellow} uploading  {pos}/{len}  {bar:24.yellow/dark_gray}  {msg}",
        )
        .unwrap()
        .progress_chars("---"),
    );

    for (entry_path, is_dir) in &entries {
        let entry_parent = entry_path.parent().unwrap_or(dir_path);
        let entry_parent_id = folder_map.get(entry_parent).copied().unwrap_or(root_id);

        if *is_dir {
            let name = entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("folder");

            let sub_id = uuid::Uuid::new_v4();
            let sub_key = beebeeb_core::kdf::derive_file_key(&master_key, sub_id.as_bytes());
            let sub_blob = beebeeb_core::encrypt::encrypt_metadata(&sub_key, name)
                .map_err(|e| format!("failed to encrypt subfolder name: {e}"))?;
            let sub_enc = serde_json::to_string(&sub_blob)
                .map_err(|e| format!("serialize error: {e}"))?;

            let result = api
                .create_folder(&sub_enc, Some(entry_parent_id), Some(sub_id))
                .await?;
            let created_id: uuid::Uuid = result
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing folder id")?
                .parse()
                .map_err(|e| format!("invalid id: {e}"))?;

            folder_map.insert(entry_path.clone(), created_id);
            pb.set_message(format!("folder: {name}"));
        } else {
            let name = entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");

            pb.set_message(name.to_string());

            push_single_file(api, entry_path, Some(entry_parent_id.to_string())).await?;
        }
        pb.inc(1);
    }

    pb.finish_and_clear();
    println!(
        "  {} {} {} {}",
        "OK".custom_color(crate::colors::GREEN_OK),
        dir_name.custom_color(crate::colors::INK),
        "·".custom_color(crate::colors::INK_DIM),
        format!("{file_count} files, {folder_count} folders uploaded").custom_color(crate::colors::INK_DIM),
    );

    Ok(())
}

fn collect_entries(dir: &std::path::Path, entries: &mut Vec<(PathBuf, bool)>) -> Result<(), String> {
    let read_dir = std::fs::read_dir(dir).map_err(|e| format!("failed to read directory: {e}"))?;
    let mut sorted: Vec<_> = read_dir
        .filter_map(|e| e.ok())
        .collect();
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
