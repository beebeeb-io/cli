//! `bb rm <target>...` — soft-trash one or more files/folders (reversible via
//! `bb restore`).
//!
//! Resolves each vault path (or raw UUID), refuses a folder unless `-r` is
//! given, asks for confirmation on an interactive terminal (skipped with
//! `-f`/`--yes`, `--json`, `--quiet`, or a non-TTY), then trashes everything in
//! one or more `POST /api/v1/files/trash` batches (max 500 ids each). The
//! server cascades the trashed flag to folder contents, so trashing a folder
//! id trashes its whole subtree — no client-side walk. Renders the
//! `{trashed, already_trashed, missing}` counts the endpoint returns.

use std::io::Write;

use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::{colors, path, ui};

/// Server batch cap for `POST /files/trash`.
const BULK_TRASH_BATCH: usize = 500;

struct Target {
    id: String,
    display: String,
    is_folder: bool,
}

/// Entry point for `bb rm`.
/// - `recursive`: allow trashing folders (mirrors `rm -r`).
/// - `permanent`: PERMANENTLY delete (irreversible) instead of soft-trashing —
///   always requires a step-up password confirmation (`--yes` cannot bypass it).
/// - `yes`: skip the soft-trash confirmation prompt (`-f`/`--yes`).
pub async fn run(targets: Vec<String>, recursive: bool, permanent: bool, yes: bool) -> Result<(), String> {
    if targets.is_empty() {
        return Err("at least one target required".to_string());
    }
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = load_master_key()?;

    // Resolve every target up front so a typo fails before anything is removed.
    // (A trashed entry no longer resolves by path — permanently deleting one
    // means passing its UUID from `bb trash list`; active paths resolve normally.)
    let mut resolved: Vec<Target> = Vec::with_capacity(targets.len());
    for t in &targets {
        let (id, is_folder) = if uuid::Uuid::parse_str(t).is_ok() {
            let meta = api.get_file(t).await?;
            (
                t.clone(),
                meta.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false),
            )
        } else {
            let r = path::resolve_path(&api, &master_key, t).await?;
            let id = r.file_id.ok_or("cannot remove the vault root")?;
            (id, r.is_folder)
        };
        if is_folder && !recursive {
            let verb = if permanent { "permanently delete" } else { "trash" };
            return Err(format!("'{t}' is a folder. Use -r to {verb} it and its contents."));
        }
        resolved.push(Target {
            id,
            display: t.clone(),
            is_folder,
        });
    }

    if permanent {
        return permanent_delete_flow(&api, &resolved).await;
    }

    // Confirm on an interactive terminal unless opted out.
    let folder_count = resolved.iter().filter(|t| t.is_folder).count();
    if !yes && ui::is_rich() && !confirm(&resolved, folder_count)? {
        if !ui::is_quiet() {
            println!("  {}", "cancelled".custom_color(colors::INK_DIM));
        }
        return Ok(());
    }

    // Trash in batches of <=500 ids.
    let ids: Vec<String> = resolved.iter().map(|t| t.id.clone()).collect();
    let mut trashed = 0u64;
    let mut already = 0u64;
    let mut missing = 0u64;
    for chunk in ids.chunks(BULK_TRASH_BATCH) {
        let resp = api.bulk_trash(chunk).await?;
        trashed += count(&resp, "trashed");
        already += count(&resp, "already_trashed");
        missing += count(&resp, "missing");
    }
    path::invalidate_cache();

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": missing == 0,
                "trashed": trashed,
                "already_trashed": already,
                "missing": missing,
            }))
            .unwrap()
        );
    } else if !ui::is_quiet() {
        let mut parts = vec![format!("{trashed} trashed")];
        if already > 0 {
            parts.push(format!("{already} already in trash"));
        }
        if missing > 0 {
            parts.push(format!("{missing} not found"));
        }
        println!(
            "  {} {} {}",
            "trashed \u{00b7}".custom_color(colors::AMBER),
            parts.join(" \u{00b7} ").custom_color(colors::INK),
            "(restore with `bb restore`)".custom_color(colors::INK_DIM),
        );
    }
    Ok(())
}

/// `bb rm --permanent` — irreversible delete behind a mandatory step-up.
///
/// Prints an unmissable warning, then requires a fresh password confirmation
/// (`X-Confirm-Token`). `--yes` does NOT bypass the step-up — that token IS the
/// confirmation. If the token can't be obtained (wrong password / cancelled),
/// nothing is deleted (the refuse-path); the server also refuses the
/// `DELETE …/permanent` call without the token. The token is reused across
/// targets until it expires.
async fn permanent_delete_flow(api: &ApiClient, targets: &[Target]) -> Result<(), String> {
    if !ui::is_quiet() && !ui::is_json() {
        println!(
            "  {} {}",
            "!".custom_color(colors::RED_ERR),
            format!(
                "PERMANENT DELETE \u{00b7} {} item(s) will be erased forever \u{2014} this cannot be undone.",
                targets.len()
            )
            .custom_color(colors::RED_ERR),
        );
    }

    // Mandatory step-up. acquire_confirm_token prompts for the password and
    // exchanges it for a single-use 5-minute token. On failure we return here,
    // BEFORE any delete call — the refuse-path.
    let confirm_token = crate::commands::confirm::acquire_confirm_token(api).await?;

    let mut deleted = 0u64;
    let mut failures: Vec<(String, String)> = Vec::new();
    for t in targets {
        match api.permanent_delete(&t.id, &confirm_token).await {
            Ok(_) => deleted += 1,
            Err(e) => failures.push((t.display.clone(), e)),
        }
    }
    path::invalidate_cache();

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": failures.is_empty(),
                "permanently_deleted": deleted,
                "failures": failures
                    .iter()
                    .map(|(p, e)| serde_json::json!({ "path": p, "error": e }))
                    .collect::<Vec<_>>(),
            }))
            .unwrap()
        );
    } else if !ui::is_quiet() {
        println!(
            "  {} {}",
            "deleted \u{00b7}".custom_color(colors::RED_ERR),
            format!("{deleted} permanently removed").custom_color(colors::INK),
        );
        for (p, e) in &failures {
            eprintln!("  {} {p}: {e}", "error:".custom_color(colors::RED_ERR));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} of {} failed",
            failures.len(),
            deleted as usize + failures.len()
        ))
    }
}

fn count(resp: &serde_json::Value, key: &str) -> u64 {
    resp.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or(0)
}

/// Minimal interactive y/N confirmation (no extra deps). Only called on a TTY.
fn confirm(targets: &[Target], folder_count: usize) -> Result<bool, String> {
    let what = if targets.len() == 1 {
        let t = &targets[0];
        if t.is_folder {
            format!("folder {} and its contents", t.display)
        } else {
            format!("file {}", t.display)
        }
    } else {
        let folder_note = if folder_count > 0 {
            format!(" ({folder_count} folder(s) incl. contents)")
        } else {
            String::new()
        };
        format!("{} items{folder_note}", targets.len())
    };
    print!("  {} trash {what}? [y/N] ", "?".custom_color(colors::AMBER));
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}
