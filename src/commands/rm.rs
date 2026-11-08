//! `bb rm <target>...` — soft-trash one or more files/folders (reversible via
//! `bb restore`).
//!
//! Resolves each vault path (or raw UUID), refuses a folder unless `-r` is
//! given, asks for confirmation on an interactive terminal (skipped with
//! `-f`/`--force`/`--yes`, `--json` or `--quiet`; with stdin not a terminal and none of
//! those, it refuses with exit 2 instead of prompting), then trashes everything in
//! one or more `POST /api/v1/files/trash` batches (max 500 ids each). The
//! server cascades the trashed flag to folder contents, so trashing a folder
//! id trashes its whole subtree — no client-side walk. Renders the
//! `{trashed, already_trashed, missing}` counts the endpoint returns.
//!
//! LD-0113: under the Backups root core's app rule decides first. The root
//! and a device's manifest are refused with core's words; a device or
//! category folder asks "Delete <device>'s backup?" with its item count
//! (only `--yes` answers it for scripts, never `-f`); a server `409 protected_folder` prints
//! core's words too.

use std::io::Write;

use colored::Colorize;

use beebeeb_core::backup::{BackupPosition, Mutation};

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::{backup_guard, colors, path, ui};

/// Server batch cap for `POST /files/trash`.
const BULK_TRASH_BATCH: usize = 500;

struct Target {
    id: String,
    display: String,
    is_folder: bool,
    /// LD-0113: where the row is in the Backups tree.
    position: BackupPosition,
}

/// The destructive-behaviour flags `bb rm` carries, grouped so the call site
/// reads as the question being asked.
pub struct RmOptions {
    /// Allow trashing folders (mirrors `rm -r`).
    pub recursive: bool,
    /// Permanently delete instead of soft-trashing; still needs a step-up
    /// password, which no flag bypasses.
    pub permanent: bool,
    /// Skip the soft-trash confirmation prompt (`-f`/`--force`/`--yes`).
    pub skip_confirm: bool,
    /// Answer "Delete <device>'s backup?" (`--yes` only: the habitual `rm -rf`
    /// must not delete a device's backup unasked).
    pub answer_backup_question: bool,
}

/// Entry point for `bb rm`.
pub async fn run(targets: Vec<String>, options: RmOptions) -> Result<(), String> {
    let RmOptions {
        recursive,
        permanent,
        skip_confirm,
        answer_backup_question,
    } = options;
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
    let mutation = if permanent {
        Mutation::DeletePermanently
    } else {
        Mutation::Trash
    };
    for t in &targets {
        let (id, is_folder, position) = if uuid::Uuid::parse_str(t).is_ok() {
            let meta = api.get_file(t).await?;
            let is_folder = meta.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
            let position = position_by_id(&api, &master_key, t, &meta, is_folder).await?;
            (t.clone(), is_folder, position)
        } else {
            let r = path::resolve_path(&api, &master_key, t).await?;
            let position = r.backup_position();
            let id = r.file_id.ok_or("cannot remove the vault root")?;
            (id, r.is_folder, position)
        };
        // LD-0113: core's app rule, before anything is removed.
        backup_guard::check(position, mutation)?;
        if is_folder && !recursive {
            let verb = if permanent { "permanently delete" } else { "trash" };
            return Err(format!("'{t}' is a folder. Use -r to {verb} it and its contents."));
        }
        resolved.push(Target {
            id,
            display: t.clone(),
            is_folder,
            position,
        });
    }

    // LD-0113: deleting a device's backup (or one of its folders) is the large,
    // real loss, so it is asked about on its own, with the item count.
    let Some(only_backups) = confirm_backup_deletes(&api, &master_key, &resolved, answer_backup_question).await? else {
        if !ui::is_quiet() {
            println!("  {}", "cancelled".custom_color(colors::INK_DIM));
        }
        return Ok(());
    };

    if permanent {
        return permanent_delete_flow(&api, &resolved).await;
    }

    // Confirm on an interactive terminal unless opted out. With no terminal to
    // answer the prompt, refuse loudly (exit 2) rather than reading EOF as
    // "no" and exiting 0 with nothing trashed (flow-6).
    let folder_count = resolved.iter().filter(|t| t.is_folder).count();
    if !skip_confirm && ui::is_rich() && !crate::exit::stdin_is_interactive() {
        return Err(crate::exit::with_code(
            crate::exit::USAGE,
            "refusing to trash without -f in non-interactive mode (stdin is not a terminal) \u{2014} \
             re-run with -f to confirm",
        ));
    }
    if !skip_confirm && !only_backups && ui::is_rich() && !confirm(&resolved, folder_count)? {
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
        let resp = api
            .bulk_trash(chunk)
            .await
            .map_err(|e| backup_guard::server_refusal(e, Mutation::Trash))?;
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

/// `bb rm --permanent <target>...` — irreversible delete behind a mandatory step-up.
///
/// Prints an unmissable warning WITH the item count, then requires ONE fresh
/// password confirmation (`X-Confirm-Token`, minted by `acquire_confirm_token`)
/// and erases the whole batch in a single `POST /files/permanent` call. `--yes`
/// does NOT bypass the step-up — that token IS the confirmation. If the token
/// can't be obtained, nothing is deleted (the refuse-path); the server also
/// refuses without it, and only erases items that are owned AND already trashed
/// (others come back as `skipped_not_trashed` / `missing`).
async fn permanent_delete_flow(api: &ApiClient, targets: &[Target]) -> Result<(), String> {
    if !ui::is_quiet() && !ui::is_json() {
        println!(
            "  {} {}",
            "!".custom_color(colors::RED_ERR),
            format!(
                "This permanently deletes {} item(s). We cannot recover them.",
                targets.len()
            )
            .custom_color(colors::RED_ERR),
        );
    }

    // Mandatory step-up (one token for the whole batch). On failure we return
    // here, BEFORE anything is touched — the refuse-path.
    let confirm_token = crate::commands::confirm::acquire_confirm_token(api).await?;

    let ids: Vec<String> = targets.iter().map(|t| t.id.clone()).collect();
    // `rm --permanent` may target LIVE files; the permanent endpoint only erases
    // already-trashed items. Move them to trash first (idempotent, reversible),
    // then erase the batch with the one confirm token. Net effect: "delete
    // forever, skipping the trash step" — but through the owned+trashed gate.
    api.bulk_trash(&ids)
        .await
        .map_err(|e| backup_guard::server_refusal(e, Mutation::DeletePermanently))?;
    let resp = api
        .bulk_permanent_delete(&ids, &confirm_token)
        .await
        .map_err(|e| backup_guard::server_refusal(e, Mutation::DeletePermanently))?;
    path::invalidate_cache();

    let deleted = count(&resp, "deleted");
    let skipped = count(&resp, "skipped_not_trashed");
    let missing = count(&resp, "missing");

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": skipped == 0 && missing == 0,
                "permanently_deleted": deleted,
                "skipped_not_trashed": skipped,
                "missing": missing,
            }))
            .unwrap()
        );
    } else if !ui::is_quiet() {
        let mut parts = vec![format!("{deleted} permanently removed")];
        if skipped > 0 {
            parts.push(format!("{skipped} skipped (still live)"));
        }
        if missing > 0 {
            parts.push(format!("{missing} not found"));
        }
        println!(
            "  {} {}",
            "deleted \u{00b7}".custom_color(colors::RED_ERR),
            parts.join(" \u{00b7} ").custom_color(colors::INK),
        );
    }
    Ok(())
}

/// The Backups position of a target given by id: its depth through its
/// parents, and (for a file two levels down, where a device's manifest sits)
/// its name. A file there whose name cannot be read is treated as the
/// manifest: refused, never guessed deletable.
async fn position_by_id(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    id: &str,
    row: &serde_json::Value,
    is_folder: bool,
) -> Result<BackupPosition, String> {
    let depth = backup_guard::depth_of(api, id).await?;
    let name = if depth == Some(2) && !is_folder {
        decrypted_name(api, master_key, row)
            .await
            .unwrap_or_else(|| beebeeb_core::backup::MANIFEST_NAME.to_string())
    } else {
        String::new()
    };
    Ok(BackupPosition::at_depth(depth, &name))
}

async fn decrypted_name(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    row: &serde_json::Value,
) -> Option<String> {
    crate::owned_keys::OwnedKeyResolver::new()
        .resolve(api, master_key, row)
        .await
        .ok()
        .map(|r| r.name)
}

/// Ask "Delete <device>'s backup?" for each device or category folder among
/// `targets` (spec §8, Amendment 8). `Some(true)`: every target is such a
/// folder and each was confirmed (no second question follows); `Some(false)`:
/// confirmed, or none of them is one; `None`: the person said no. `--yes`
/// answers for scripts; without it a run that cannot ask (not a terminal,
/// `--json`, `--quiet`) is refused with exit 2, never read as yes.
async fn confirm_backup_deletes(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    targets: &[Target],
    yes: bool,
) -> Result<Option<bool>, String> {
    let backups: Vec<&Target> = targets
        .iter()
        .filter(|t| {
            matches!(
                t.position,
                BackupPosition::DeviceFolder | BackupPosition::CategoryFolder
            )
        })
        .collect();
    if backups.is_empty() {
        return Ok(Some(false));
    }
    if yes {
        return Ok(Some(backups.len() == targets.len()));
    }
    if !ui::is_rich() || !crate::exit::stdin_is_interactive() {
        return Err(crate::exit::with_code(
            crate::exit::USAGE,
            "this deletes a device's backup \u{2014} re-run with --yes to confirm (there is no terminal to ask)",
        ));
    }
    for t in backups.iter().copied() {
        let row = api.get_file(t.id.as_str()).await?;
        let own = decrypted_name(api, master_key, &row)
            .await
            .unwrap_or_else(|| t.display.clone());
        let (device, folder) = if t.position == BackupPosition::DeviceFolder {
            (own, None)
        } else {
            let parent = row.get("parent_id").and_then(|v| v.as_str()).unwrap_or_default();
            let parent_row = api.get_file(parent).await?;
            let device = decrypted_name(api, master_key, &parent_row).await.unwrap_or_default();
            (device, Some(own))
        };
        let (items, exact) = backup_guard::count_items(api, &t.id).await?;
        let question = backup_guard::delete_backup_question(&device, folder.as_deref(), items, exact);
        if !ask(&question)? {
            return Ok(None);
        }
    }
    Ok(Some(backups.len() == targets.len()))
}

fn ask(question: &str) -> Result<bool, String> {
    print!("  {} {question} [y/N] ", "?".custom_color(colors::AMBER));
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
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
