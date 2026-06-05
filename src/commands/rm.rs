//! `bb rm <path-or-id>` — soft-trash a single file (reversible via `bb restore`).
//!
//! Resolves a vault path (or accepts a raw UUID), refuses a folder unless `-r`
//! is given, asks for confirmation on an interactive terminal (skipped with
//! `-f`/`--yes`, `--json`, `--quiet`, or a non-TTY), then calls
//! `DELETE /api/v1/files/{id}` (soft-trash). The file lands in the trash and
//! can be brought back with `bb restore`.

use std::io::Write;

use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::{colors, path, ui};

/// Entry point for `bb rm`.
/// - `recursive`: allow trashing a folder (mirrors `rm -r`).
/// - `yes`: skip the confirmation prompt (`-f`/`--yes`).
pub async fn run(target: String, recursive: bool, yes: bool) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = load_master_key()?;

    // Resolve the target to (id, is_folder, display_name).
    let (file_id, is_folder, display) = if uuid::Uuid::parse_str(&target).is_ok() {
        let meta = api.get_file(&target).await?;
        let is_folder = meta.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        (target.clone(), is_folder, target.clone())
    } else {
        let resolved = path::resolve_path(&api, &master_key, &target).await?;
        let id = resolved.file_id.ok_or("cannot trash the vault root")?;
        (id, resolved.is_folder, target.clone())
    };

    if is_folder && !recursive {
        return Err(format!("'{display}' is a folder. Use -r to trash it and its contents."));
    }

    // Confirm on an interactive terminal unless the user opted out.
    if !yes && ui::is_rich() && !confirm(&display, is_folder)? {
        if !ui::is_quiet() {
            println!("  {}", "cancelled".custom_color(colors::INK_DIM));
        }
        return Ok(());
    }

    api.trash_file(&file_id).await?;
    path::invalidate_cache();

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "trashed": [{ "id": file_id, "target": display }],
            }))
            .unwrap()
        );
    } else if !ui::is_quiet() {
        println!(
            "  {} {} {}",
            "trashed ·".custom_color(colors::AMBER),
            display.custom_color(colors::INK),
            "(restore with `bb restore`)".custom_color(colors::INK_DIM),
        );
    }
    Ok(())
}

/// Minimal interactive y/N confirmation (no extra deps). Returns `Ok(true)` if
/// the user confirms. Only ever called on an interactive terminal.
fn confirm(target: &str, is_folder: bool) -> Result<bool, String> {
    let what = if is_folder { "folder (and its contents)" } else { "file" };
    print!(
        "  {} trash {what} {}? [y/N] ",
        "?".custom_color(colors::AMBER),
        target.custom_color(colors::INK),
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}
