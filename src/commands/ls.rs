use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::crypto::decrypt_name;

pub async fn run(path: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Load master key for local decryption — ls is a read-only command, but
    // Beebeeb is zero-knowledge so the server always returns name_encrypted.
    let master_key = load_master_key()?;

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

        let name_encrypted = file
            .get("name_encrypted")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("server response missing name_encrypted for file {file_id}"))?;
        let name = decrypt_name(&master_key, file_id, name_encrypted)
            .ok_or_else(|| format!("failed to decrypt filename for file {file_id}"))?;

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
            .map(|s| {
                // Show only YYYY-MM-DD HH:MM
                let s = s.trim_end_matches('Z');
                let s = s.replace('T', " ");
                let s = &s[..s.len().min(16)];
                s.to_string()
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
