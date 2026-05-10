use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

/// Decrypt a `name_encrypted` JSON blob using the file's UUID and the master key.
fn decrypt_name(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> Option<String> {
    let file_uuid: uuid::Uuid = file_id_str.parse().ok()?;
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

    // The name blob is a JSON-encoded EncryptedBlob from beebeeb-types.
    let blob: beebeeb_types::EncryptedBlob =
        serde_json::from_str(name_encrypted_str).ok()?;
    beebeeb_core::encrypt::decrypt_metadata(&file_key, &blob).ok()
}

pub async fn run(path: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Load master key for local decryption — ls is a read-only command, but
    // Beebeeb is zero-knowledge so the server always returns name_encrypted.
    let master_key = load_master_key().ok();

    let result = api.list_files(path.as_deref()).await?;

    let files = result
        .as_array()
        .or_else(|| result.get("files").and_then(|f| f.as_array()));

    let Some(files) = files else {
        println!(
            "  {}",
            "empty — no files here".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    };

    if files.is_empty() {
        println!(
            "  {}",
            "empty — no files here".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    }

    // Column header
    println!(
        "  {}",
        format!(
            "{:<40}  {:>10}  {:<16}  {}",
            "name", "size", "modified", "id"
        )
        .custom_color(crate::colors::INK_DIM),
    );

    for file in files {
        let file_id = file
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Prefer decrypted name; fall back to encrypted blob or raw `name` field.
        let name: String = match (&master_key, file.get("name_encrypted").and_then(|v| v.as_str())) {
            (Some(mk), Some(enc)) => {
                decrypt_name(mk, file_id, enc)
                    .unwrap_or_else(|| format!("<encrypted:{}>", &enc[..enc.len().min(8)]))
            }
            (None, Some(enc)) => {
                // Not logged in or no master key — show truncated ciphertext
                format!("<locked:{}>", &enc[..enc.len().min(8)])
            }
            _ => file
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
        };

        let size = file
            .get("size_bytes")
            .or_else(|| file.get("size"))
            .and_then(|v| v.as_u64())
            .map(format_size)
            .unwrap_or_else(|| "-".to_string());

        let modified = file
            .get("updated_at")
            .or_else(|| file.get("modified"))
            .and_then(|v| v.as_str())
            .and_then(|s| {
                // Show only YYYY-MM-DD HH:MM
                let s = s.trim_end_matches('Z');
                let s = s.replace('T', " ");
                let s = &s[..s.len().min(16)];
                Some(s.to_string())
            })
            .unwrap_or_else(|| "-".to_string());

        let is_folder = file
            .get("is_folder")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let name_colored = if is_folder {
            format!("{name}/").custom_color(crate::colors::AMBER)
        } else {
            name.to_string().custom_color(crate::colors::INK_WARM)
        };

        // Truncate ID to 8 chars for readability (first segment of UUID)
        let short_id = &file_id[..file_id.len().min(8)];
        let type_label = if is_folder { "folder" } else { "file" };

        println!(
            "  {:<40}  {:>10}  {:<16}  {} {}",
            name_colored,
            size.custom_color(crate::colors::INK_DIM),
            modified.custom_color(crate::colors::INK_DIM),
            short_id.custom_color(crate::colors::INK_DIM),
            type_label.custom_color(crate::colors::INK_SAGE),
        );
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
