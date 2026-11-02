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
    let dst_id = destination_parent(&resolved_dst)?;

    // Resolve every source first so a typo doesn't half-finish the move.
    let mut src_keys = crate::owned_keys::OwnedKeyResolver::new();
    let mut resolved_srcs: Vec<(String, String, Option<String>, bool)> = Vec::with_capacity(srcs.len());
    for s in srcs {
        let r = path::resolve_path_with_keys(api, master_key, s, &mut src_keys).await?;
        let Some(id) = r.file_id else {
            return Err(format!("cannot move the vault root ({s})"));
        };
        // A folder cannot move into its own descendant.
        let s_norm = format!("/{}", s.trim_matches('/'));
        let d_norm = format!("/{}", dst.trim_matches('/'));
        if r.is_folder && (d_norm == s_norm || d_norm.starts_with(&format!("{s_norm}/"))) {
            return Err(format!("cannot move {s} into {dst} (would create a cycle)"));
        }
        resolved_srcs.push((s.clone(), id, r.parent_id.map(|p| p.to_string()), r.is_folder));
    }

    let mut ok = 0u32;
    let mut failures: Vec<(String, String)> = Vec::new();
    for (input, id, parent_id, is_folder) in resolved_srcs {
        // A19: a source leaving a shared root adopts its foreign subtree first.
        if let Err(e) = src_keys
            .adopt_before_move(api, master_key, &id, parent_id.as_deref(), is_folder)
            .await
        {
            failures.push((input, e));
            continue;
        }
        match api.move_file(&id, None, Some(dst_id)).await {
            Ok(_) => ok += 1,
            Err(e) => failures.push((input, e.to_string())),
        }
    }

    path::invalidate_cache();


    // L15 = A (P2-11): one notice for the command when anything landed in a
    // shared folder (the user may move many sources in one go).
    if ok > 0 && !ui::is_json() {
        if let Some(dst) = dst_id {
            crate::owned_keys::announce_owner_only(api, master_key, Some(&dst.to_string())).await;
        }
    }
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
    // Resolve src. Its resolver keeps the key the row opened under, for the
    // SF-0002 write check below.
    let mut src_keys = crate::owned_keys::OwnedKeyResolver::new();
    let src = path::resolve_path_with_keys(api, master_key, src_arg, &mut src_keys).await?;
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
    let dst_existing = path::resolve_path_if_exists(api, master_key, dst_arg).await?;

    let (new_parent_id, new_name): (Option<Uuid>, Option<String>) = if let Some(d) = dst_existing {
        if d.is_folder {
            // Case 1: dst is an existing folder → move into it, keep the name.
            (destination_parent(&d)?, None)
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
                destination_parent(&parent)?
            }
            None => None,
        };
        (parent_id, Some(leaf))
    };

    // SF-0002: a row sealed by the old iOS File Provider is read-only: a move
    // is fine (it re-encrypts nothing), a rename would rewrite its name under a
    // convention the row does not use. The name may resolve normally while the
    // CURRENT version's content is still uppercase-sealed, so a rename asks the
    // one write check; a check that could not decide refuses as well.
    if new_name.as_deref().is_some_and(|n| n != src.name) {
        // P2-10: a plaintext name is not the SF-0002 repair convention.
        if src.read_only && src_keys.name_is_plaintext_for(&src_id) {
            return Err(crate::owned_keys::PLAINTEXT_NAME_READ_ONLY.to_string());
        }
        // The listing that resolved this path already carries the version's
        // geometry, so no second GET is made.
        let row_key = src_keys.key_for(&src_id);
        crate::owned_keys::write_check(
            api,
            master_key,
            &crate::owned_keys::WriteTarget {
                file_id: &src_id,
                is_folder: src.is_folder,
                name_read_only: src.read_only,
                row_key: row_key.as_deref(),
                size_bytes: src.size_bytes,
                chunk_count: src.chunk_count,
            },
        )
        .await
        .ensure()?;
    }

    // Preserve omitted/NULL/UUID: a changed parent at root is an explicit
    // Some(None), while a rename within the same directory omits parent_id.
    let new_parent_send = (src.parent_id != new_parent_id).then_some(new_parent_id);
    let dest_parent = new_parent_send.clone().flatten();
    let mut name_changed = false;
    let new_name_encrypted = match new_name {
        Some(name) if name != src.name => {
            name_changed = true;
            // L13: a rename seals the new name through the resolver's write
            // target (key + chunk-format pin). A request row has no target: its
            // content stays under `C`, so its name uses the string convention.
            let target = src_keys.write_target_for(&src_id);
            let (file_key, pin) = match &target {
                Some(target) => (crate::crypto::key_copy(target.key()), target.pin()),
                // Only a request row falls back: its content stays under `C`, so
                // its name uses the string convention. Anything else without a
                // target would seal the new name under a key that opens nothing.
                None if src_keys.origin_for(&src_id)
                    == Some(beebeeb_core::owned_keys::KeyOrigin::RequestContent) =>
                (
                    beebeeb_core::kdf::derive_file_key(master_key, src_id.as_bytes()),
                    beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                ),
                None => return Err(crate::owned_keys::RENAME_UNKNOWN_KEY.to_string()),
            };
            Some(
                beebeeb_core::encrypt::encrypt_name_for(&file_key, &name, None, pin)
                    .map_err(|e| format!("failed to encrypt new name: {e}"))?,
            )
        }
        _ => None,
    };

    if new_parent_send.is_none() && new_name_encrypted.is_none() {
        return noop(dst_arg);
    }

    // L12/A19: a move of a shared item (or a folder holding shared descendants)
    // adopts every foreign item involved first, confirmed by a readback. The
    // helper fails closed when the ancestry cannot be read.
    let src_parent = src.parent_id.map(|p| p.to_string());
    src_keys
        .adopt_before_move(api, master_key, &src_id, src_parent.as_deref(), src.is_folder)
        .await?;

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
            format!("moved {src_arg} → {}/{}", dst_arg.trim_end_matches('/'), src.name)
        };
        println!(
            "  {} {}",
            "ok ·".custom_color(colors::GREEN_OK),
            msg.custom_color(colors::INK)
        );
    }

    // L15 = A: a move into a shared folder stays owner-only.
    if !ui::is_json() && new_parent_send.is_some() {
        if let Some(parent) = dest_parent {
            crate::owned_keys::announce_owner_only(api, master_key, Some(&parent.to_string())).await;
        }
    }

    Ok(())
}

/// The path resolver creates the root entry with name "/" and no file ID.
/// Missing IDs on any other resolved entry are corrupt metadata, not root.
fn destination_parent(entry: &path::ResolvedPath) -> Result<Option<Uuid>, String> {
    use super::move_target::{DestinationParent, parent_target};
    let target = if !entry.is_folder {
        DestinationParent::NotFolder
    } else if entry.name == "/" && entry.file_id.is_none() {
        DestinationParent::Root
    } else {
        DestinationParent::Folder(entry.file_id.as_deref())
    };
    parent_target(true, target)
        .map(Option::flatten)
        .map_err(|e| e.to_string())
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

#[cfg(test)]
mod parent_target_tests {
    use super::*;

    fn folder(name: &str, id: Option<&str>) -> path::ResolvedPath {
        path::ResolvedPath {
            file_id: id.map(str::to_owned),
            parent_id: None,
            name: name.into(),
            is_folder: true,
            read_only: false,
            size_bytes: None,
            chunk_count: 1,
        }
    }

    #[test]
    fn mv_parent_target_root_and_folder() {
        let id = Uuid::new_v4();
        assert_eq!(destination_parent(&folder("/", None)), Ok(None));
        assert_eq!(
            destination_parent(&folder("Target", Some(&id.to_string()))),
            Ok(Some(id))
        );
    }

    #[test]
    fn mv_parent_target_missing_id() {
        assert_eq!(
            destination_parent(&folder("Target", None)),
            Err("destination folder is missing its file ID".into())
        );
    }

    #[test]
    fn mv_parent_target_invalid_id() {
        assert_eq!(
            destination_parent(&folder("Target", Some("invalid"))),
            Err("destination folder has an invalid UUID".into())
        );
    }
}
