use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::crypto::decrypt_name;
use crate::{colors, ui};

pub async fn run(path: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Load master key for local decryption — ls is a read-only command, but
    // Beebeeb is zero-knowledge so the server always returns name_encrypted.
    let master_key = load_master_key()?;

    // Resolve plaintext folder paths to UUIDs. If the argument is already a
    // valid UUID we pass it through directly; otherwise we walk the encrypted
    // tree via the shared path module.
    let parent_id = match &path {
        Some(p) if uuid::Uuid::parse_str(p).is_err() => {
            let resolved = crate::path::resolve_path(&api, &master_key, p).await?;
            if !resolved.is_folder {
                return Err(format!(
                    "'{}' is a file, not a folder. Use `bb pull` to download it.",
                    resolved.name
                ));
            }
            resolved.file_id
        }
        Some(p) => Some(p.clone()),
        None => None,
    };

    let result = api.list_files(parent_id.as_deref()).await?;

    let files = result
        .as_array()
        .or_else(|| result.get("files").and_then(|f| f.as_array()));

    let Some(files) = files else {
        if !ui::is_json() {
            println!(
                "  {}",
                "empty \u{2014} no files here".custom_color(colors::INK_DIM),
            );
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "files": [],
                    "total_items": 0,
                    "total_bytes": 0,
                }))
                .unwrap()
            );
        }
        return Ok(());
    };

    if files.is_empty() {
        if !ui::is_json() {
            println!(
                "  {}",
                "empty \u{2014} no files here".custom_color(colors::INK_DIM),
            );
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "files": [],
                    "total_items": 0,
                    "total_bytes": 0,
                }))
                .unwrap()
            );
        }
        return Ok(());
    }

    // Decrypt all files upfront so we can iterate for any output mode.
    struct DecryptedFile {
        id: String,
        decrypted_name: String,
        is_folder: bool,
        size_bytes: u64,
        modified: String,
    }

    let mut decrypted: Vec<DecryptedFile> = Vec::with_capacity(files.len());

    for file in files {
        let file_id = file.get("id").and_then(|v| v.as_str()).unwrap_or("");

        let name_encrypted = file
            .get("name_encrypted")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("server response missing name_encrypted for file {file_id}"))?;
        let name = decrypt_name(&master_key, file_id, name_encrypted)
            .ok_or_else(|| format!("failed to decrypt filename for file {file_id}"))?;

        let is_folder = file
            .get("is_folder")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let size_bytes = file
            .get("size_bytes")
            .or_else(|| file.get("size"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let modified = file
            .get("updated_at")
            .or_else(|| file.get("modified"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        decrypted.push(DecryptedFile {
            id: file_id.to_string(),
            decrypted_name: name,
            is_folder,
            size_bytes,
            modified,
        });
    }

    // ── JSON mode ────────────────────────────────────────────────────────────
    if ui::is_json() {
        let total_bytes: u64 = decrypted
            .iter()
            .filter(|f| !f.is_folder)
            .map(|f| f.size_bytes)
            .sum();
        let json_files: Vec<serde_json::Value> = decrypted
            .iter()
            .map(|f| {
                serde_json::json!({
                    "id": f.id,
                    "name": f.decrypted_name,
                    "is_folder": f.is_folder,
                    "size_bytes": f.size_bytes,
                    "modified": f.modified,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "files": json_files,
                "total_items": decrypted.len(),
                "total_bytes": total_bytes,
            }))
            .unwrap()
        );
        return Ok(());
    }

    // ── Rich / quiet mode ────────────────────────────────────────────────────

    // Column header (rich mode only)
    if !ui::is_quiet() {
        println!(
            "  {}",
            format!(
                "{:<44}{:>8}  {:<14}{}",
                "NAME", "SIZE", "MODIFIED", "ID"
            )
            .custom_color(colors::INK_DIM),
        );
    }

    let mut total_items = 0u64;
    let mut total_bytes = 0u64;

    for file in &decrypted {
        total_items += 1;
        if !file.is_folder {
            total_bytes += file.size_bytes;
        }

        // Quiet mode: one filename per line, no decoration
        if ui::is_quiet() {
            println!("{}", file.decrypted_name);
            continue;
        }

        let icon = ui::file_icon(&file.decrypted_name, file.is_folder);
        let display_name = if file.is_folder {
            format!("{}/", file.decrypted_name)
                .custom_color(colors::PATH)
                .to_string()
        } else {
            file.decrypted_name
                .custom_color(colors::INK)
                .to_string()
        };
        let size_str = if file.is_folder {
            "\u{2014}".to_string() // —
        } else {
            ui::human_size(file.size_bytes)
        };
        let modified = ui::relative_time(&file.modified);
        let id_short = &file.id[..8.min(file.id.len())];

        println!(
            "  {} {:<42}{:>8}  {:<14}{}",
            icon,
            display_name,
            size_str.custom_color(colors::INK_DIM),
            modified.custom_color(colors::INK_DIM),
            id_short.custom_color(colors::INK_DIM),
        );
    }

    // Summary footer (rich mode only)
    if !ui::is_quiet() {
        println!(
            "\n  {}",
            format!(
                "{} items \u{00B7} {} \u{00B7} {}",
                total_items,
                ui::human_size(total_bytes),
                "e2ee".custom_color(colors::GREEN_OK),
            )
            .custom_color(colors::INK_DIM),
        );
    }

    Ok(())
}
