//! `bb trash list` — browse the trash.
//!
//! `GET /api/v1/files?trashed=true` returns trashed entries in the same shape
//! as the active listing. Names are decrypted locally (zero-knowledge). The
//! `updated_at` timestamp reflects when each entry was trashed.

use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::crypto::decrypt_name;
use crate::{colors, ui};

/// `bb trash list`
pub async fn list() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = load_master_key()?;

    let listing = api.list_trashed().await?;
    let files = listing
        .get("files")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if ui::is_json() {
        // Re-emit with decrypted names alongside the raw row.
        let rows: Vec<serde_json::Value> = files
            .iter()
            .map(|f| {
                let id = f.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                let name_enc = f.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or_default();
                let name = decrypt_name(&master_key, id, name_enc).unwrap_or_else(|| name_enc.to_string());
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "is_folder": f.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false),
                    "size_bytes": f.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                    "trashed_at": f.get("updated_at").and_then(|v| v.as_str()).unwrap_or_default(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "trash": rows })).unwrap()
        );
        return Ok(());
    }

    if files.is_empty() {
        if !ui::is_quiet() {
            println!("  {}", "trash is empty".custom_color(colors::INK_DIM));
        }
        return Ok(());
    }

    if !ui::is_quiet() {
        println!(
            "  {}",
            format!("{:<44}{:>8}  {:<14}{}", "NAME", "SIZE", "TRASHED", "ID").custom_color(colors::INK_DIM),
        );
    }

    let mut count = 0u64;
    for f in &files {
        let id = f.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        let name_enc = f.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or_default();
        let is_folder = f.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let size_bytes = f.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let trashed_at = f.get("updated_at").and_then(|v| v.as_str()).unwrap_or_default();
        let name = decrypt_name(&master_key, id, name_enc).unwrap_or_else(|| name_enc.to_string());
        count += 1;

        if ui::is_quiet() {
            println!("{name}");
            continue;
        }

        let icon = ui::file_icon(&name, is_folder);
        let display_name = if is_folder {
            format!("{name}/").custom_color(colors::PATH).to_string()
        } else {
            name.custom_color(colors::INK).to_string()
        };
        let size_str = if is_folder {
            "\u{2014}".to_string()
        } else {
            ui::human_size(size_bytes)
        };
        let when = ui::relative_time(trashed_at);
        let id_short = &id[..8.min(id.len())];

        println!(
            "  {} {:<42}{:>8}  {:<14}{}",
            icon,
            display_name,
            size_str.custom_color(colors::INK_DIM),
            when.custom_color(colors::INK_DIM),
            id_short.custom_color(colors::INK_DIM),
        );
    }

    if !ui::is_quiet() {
        println!();
        println!(
            "  {}",
            format!("{count} trashed \u{00b7} restore with `bb restore <name|id>`").custom_color(colors::INK_DIM),
        );
    }
    Ok(())
}
