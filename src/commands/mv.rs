//! `bb mv <src> <dst>` — move and/or rename.
//!
//! Three cases (matching `mv` muscle memory):
//!   1. `dst` exists and is a folder → move `src` into it, keep its name.
//!   2. `dst` does not exist, parent exists → rename (and possibly move).
//!   3. `dst` is an existing file → error (mirrors POSIX `mv -n`).
//!
//! Filenames are encrypted client-side. The file's UUID does not change on
//! move or rename, so the file key stays identical — no chunk re-encryption.

use colored::Colorize;
use uuid::Uuid;

use crate::api::ApiClient;
use crate::colors;
use crate::commands::push::load_master_key;
use crate::path;
use crate::ui;

/// Entry point. `srcs` may have one source (single, this task) or many (bulk,
/// task 0500). Only the single-source path is implemented here.
pub async fn run(srcs: Vec<String>, dst: String) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = load_master_key()?;

    match srcs.len() {
        0 => Err("at least one source required".to_string()),
        1 => move_single(&api, &master_key, &srcs[0], &dst).await,
        _ => move_bulk(&api, &master_key, &srcs, &dst).await,
    }
}

/// Bulk move: every source moves into an existing destination folder, keeping
/// its name (no rename in bulk mode — mirrors POSIX `mv` with multiple args).
/// All sources are resolved up front so a typo fails before anything moves;
/// per-file errors are collected and the command exits non-zero if any failed.
async fn move_bulk(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    srcs: &[String],
    dst: &str,
) -> Result<(), String> {
    // dst MUST already be a folder (bulk mode never renames).
    let resolved_dst = path::resolve_path(api, master_key, dst).await?;
    if !resolved_dst.is_folder {
        return Err(format!(
            "{dst} is a file, not a folder (bulk mv needs a folder destination)"
        ));
    }
    let dst_id: Option<Uuid> = resolved_dst.file_id.as_deref().and_then(|s| s.parse().ok());

    // Resolve every source first so a typo doesn't half-finish the move.
    let mut resolved_srcs: Vec<(String, String)> = Vec::with_capacity(srcs.len());
    for s in srcs {
        let r = path::resolve_path(api, master_key, s).await?;
        let Some(id) = r.file_id else {
            return Err(format!("cannot move the vault root ({s})"));
        };
        // A folder cannot move into its own descendant.
        let s_norm = format!("/{}", s.trim_matches('/'));
        let d_norm = format!("/{}", dst.trim_matches('/'));
        if r.is_folder && (d_norm == s_norm || d_norm.starts_with(&format!("{s_norm}/"))) {
            return Err(format!("cannot move {s} into {dst} (would create a cycle)"));
        }
        resolved_srcs.push((s.clone(), id));
    }

    let mut ok = 0u32;
    let mut failures: Vec<(String, String)> = Vec::new();
    for (input, id) in resolved_srcs {
        match api.move_file(&id, None, dst_id).await {
            Ok(_) => ok += 1,
            Err(e) => failures.push((input, e)),
        }
    }

    path::invalidate_cache();

    if ui::is_json() {
        let json = serde_json::json!({
            "ok": failures.is_empty(),
            "moved": ok,
            "failures": failures
                .iter()
                .map(|(p, e)| serde_json::json!({ "path": p, "error": e }))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    } else {
        if !ui::is_quiet() {
            println!(
                "  {} {}",
                "ok ·".custom_color(colors::GREEN_OK),
                format!("{ok} moved → {dst}").custom_color(colors::INK),
            );
        }
        for (p, e) in &failures {
            eprintln!("  {} {p}: {e}", "error:".custom_color(colors::RED_ERR));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("{} of {} failed", failures.len(), ok as usize + failures.len()))
    }
}

async fn move_single(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    src_arg: &str,
    dst_arg: &str,
) -> Result<(), String> {
    // Resolve src.
    let src = path::resolve_path(api, master_key, src_arg).await?;
    let Some(src_id) = src.file_id.clone() else {
        return Err("cannot move the vault root".to_string());
    };

    // Quick same-path check on the literal argument.
    if src_arg.trim_end_matches('/') == dst_arg.trim_end_matches('/') {
        return noop(dst_arg);
    }

    // Guard the obvious cycle: a folder cannot move into its own descendant.
    let src_norm = format!("/{}", src_arg.trim_matches('/'));
    let dst_norm = format!("/{}", dst_arg.trim_matches('/'));
    if src.is_folder && dst_norm.starts_with(&format!("{src_norm}/")) {
        return Err(format!("cannot move {src_arg} into {dst_arg} (would create a cycle)"));
    }

    // Resolve dst — three branches.
    let dst_existing = path::resolve_path(api, master_key, dst_arg).await.ok();

    let (new_parent_id, new_name): (Option<Uuid>, Option<String>) = if let Some(d) = dst_existing {
        if d.is_folder {
            // Case 1: dst is an existing folder → move into it, keep the name.
            (d.file_id.and_then(|s| s.parse::<Uuid>().ok()), None)
        } else {
            // Case 3: dst is an existing file → refuse to overwrite.
            return Err(format!(
                "{dst_arg} already exists. use `bb rm` first, or pick a new name."
            ));
        }
    } else {
        // Case 2: dst does not exist → resolve its parent and split off the leaf.
        let (parent_path, leaf) = split_dst(dst_arg)?;
        let parent_id = match parent_path.as_deref() {
            Some(p) => {
                let parent = path::resolve_path(api, master_key, p).await?;
                if !parent.is_folder {
                    return Err(format!("{p} is a file, not a folder"));
                }
                parent.file_id.and_then(|s| s.parse::<Uuid>().ok())
            }
            None => None,
        };
        (parent_id, Some(leaf))
    };

    // Build the PATCH: send parent_id only when we have a target parent (a
    // move); send name_encrypted only when the name actually changes.
    let new_parent_send = new_parent_id;
    let mut name_changed = false;
    let new_name_encrypted = match new_name {
        Some(name) if name != src.name => {
            name_changed = true;
            Some(
                beebeeb_core::encrypt::encrypt_name(master_key, &src_id, &name, None)
                    .map_err(|e| format!("failed to encrypt new name: {e}"))?,
            )
        }
        _ => None,
    };

    if new_parent_send.is_none() && new_name_encrypted.is_none() {
        return noop(dst_arg);
    }

    api.move_file(&src_id, new_name_encrypted.as_deref(), new_parent_send)
        .await?;
    path::invalidate_cache();

    if ui::is_json() {
        let json = serde_json::json!({
            "ok": true,
            "moved": { "id": src_id, "from": src_arg, "to": dst_arg, "renamed": name_changed },
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    } else if !ui::is_quiet() {
        let msg = if name_changed && new_parent_send.is_none() {
            format!("renamed → {dst_arg}")
        } else if name_changed {
            format!("moved + renamed {src_arg} → {dst_arg}")
        } else {
            format!("moved {src_arg} → {dst_arg}/{}", src.name)
        };
        println!(
            "  {} {}",
            "ok ·".custom_color(colors::GREEN_OK),
            msg.custom_color(colors::INK)
        );
    }

    Ok(())
}

fn noop(dst_arg: &str) -> Result<(), String> {
    if ui::is_json() {
        println!("{}", serde_json::json!({ "ok": true, "noop": true }));
    } else if !ui::is_quiet() {
        let _ = dst_arg;
        println!("  {}", "nothing to do".custom_color(colors::INK_DIM));
    }
    Ok(())
}

/// Split `/Photos/2026` → (`Some("/Photos")`, `"2026"`); `/Music` → (`None`, `"Music"`).
fn split_dst(dst: &str) -> Result<(Option<String>, String), String> {
    let trimmed = dst.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("destination must not be empty".to_string());
    }
    match trimmed.rsplit_once('/') {
        Some(("", leaf)) | Some(("/", leaf)) => Ok((None, leaf.to_string())),
        Some((parent, leaf)) if !leaf.is_empty() => Ok((Some(parent.to_string()), leaf.to_string())),
        _ => Ok((None, trimmed.to_string())),
    }
}
