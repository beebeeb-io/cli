//! `bb restore <path-or-id>` — restore a single trashed file/folder.
//!
//! Calls `POST /api/v1/files/{id}/restore`. A raw UUID restores directly; a
//! name/path is matched (case-insensitively, by leaf name) against the trashed
//! listing (`GET /api/v1/files?trashed=true`), since a trashed entry no longer
//! resolves through the active tree. Cache invalidated after a successful
//! restore so `bb ls` shows it immediately.

use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::crypto::decrypt_name;
use crate::{colors, path, ui};

pub async fn run(target: String) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = load_master_key()?;

    // Resolve the trashed entry to its id.
    let (file_id, display) = if uuid::Uuid::parse_str(&target).is_ok() {
        (target.clone(), target.clone())
    } else {
        // Match the leaf name against the trashed listing.
        let leaf = target
            .trim_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&target)
            .to_string();
        let listing = api.list_trashed().await?;
        let files = listing
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or("unexpected API response: missing files array")?;

        let mut matches: Vec<(String, String)> = Vec::new(); // (id, name)
        for entry in files {
            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let name_enc = entry.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or_default();
            let name = decrypt_name(&master_key, id, name_enc).unwrap_or_else(|| name_enc.to_string());
            if name.eq_ignore_ascii_case(&leaf) {
                matches.push((id.to_string(), name));
            }
        }

        match matches.len() {
            0 => return Err(format!("no trashed entry named '{leaf}' found (see `bb trash list`)")),
            1 => (matches[0].0.clone(), matches[0].1.clone()),
            n => {
                return Err(format!(
                    "{n} trashed entries named '{leaf}' — restore by id instead (see `bb trash list`)"
                ));
            }
        }
    };

    api.restore_file(&file_id).await?;
    path::invalidate_cache();

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "restored": [{ "id": file_id, "target": display }],
            }))
            .unwrap()
        );
    } else if !ui::is_quiet() {
        println!(
            "  {} {}",
            "restored ·".custom_color(colors::GREEN_OK),
            display.custom_color(colors::INK),
        );
    }
    Ok(())
}
