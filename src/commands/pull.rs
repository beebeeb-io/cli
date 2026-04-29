use base64::Engine;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;

use beebeeb_types::EncryptedBlob;

use crate::api::ApiClient;
use crate::config::load_config;

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

pub async fn run(file_id: String, output: Option<PathBuf>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Parse and validate the file ID as UUID
    let file_uuid: uuid::Uuid = file_id
        .parse()
        .map_err(|e| format!("invalid file ID (expected UUID): {e}"))?;

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

    let is_folder = file_meta
        .get("is_folder")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_folder {
        return Err("cannot download a folder; use `bb ls` to list contents".to_string());
    }

    // Load master key and derive the file key
    let master_key = load_master_key()?;
    let file_key =
        beebeeb_core::kdf::derive_file_key(&master_key, file_uuid.as_bytes());

    // Try to decrypt the filename for display and default output path
    let decrypted_name = serde_json::from_str::<EncryptedBlob>(name_encrypted_str)
        .ok()
        .and_then(|blob| beebeeb_core::encrypt::decrypt_metadata(&file_key, &blob).ok());

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

    // Step 3: Split the downloaded data into encrypted chunks and decrypt each.
    //
    // The server concatenates all stored chunk blobs. Each chunk was stored as
    // a JSON-serialized EncryptedBlob. We need to parse them back.
    //
    // Since chunks are JSON objects concatenated together, we use a streaming
    // JSON deserializer approach: try parsing from the beginning, consume the
    // parsed bytes, repeat.
    let decrypt_pb = ProgressBar::new(chunk_count as u64);
    decrypt_pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.yellow} decrypting  {pos}/{len} chunks  {bar:24.yellow/dark_gray}",
        )
        .unwrap()
        .progress_chars("---"),
    );

    let mut plaintext = Vec::new();
    let mut offset = 0;

    for i in 0..chunk_count {
        if offset >= encrypted_bytes.len() {
            return Err(format!(
                "unexpected end of data at chunk {i}/{chunk_count} (offset {offset}, total {})",
                encrypted_bytes.len()
            ));
        }

        // Parse a JSON EncryptedBlob from the remaining bytes
        let remaining = &encrypted_bytes[offset..];
        let mut de = serde_json::Deserializer::from_slice(remaining).into_iter::<EncryptedBlob>();

        let blob = match de.next() {
            Some(Ok(blob)) => blob,
            Some(Err(e)) => {
                return Err(format!("failed to parse encrypted chunk {i}: {e}"));
            }
            None => {
                return Err(format!("no more data for chunk {i}/{chunk_count}"));
            }
        };

        // Advance offset by how many bytes were consumed
        offset += de.byte_offset();

        let decrypted = beebeeb_core::encrypt::decrypt_chunk(&file_key, &blob)
            .map_err(|e| format!("failed to decrypt chunk {i}: {e}"))?;

        plaintext.extend_from_slice(&decrypted);
        decrypt_pb.inc(1);
    }

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
        "OK".truecolor(143, 193, 139),
        out_path.display().to_string().truecolor(233, 230, 221),
        "·".truecolor(106, 101, 91),
        format!("{size_str} · {chunk_count} chunk{} · decrypted",
            if chunk_count == 1 { "" } else { "s" },
        )
        .truecolor(106, 101, 91),
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
