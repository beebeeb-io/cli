//! `bb mkdir <path>` — create a folder.
//!
//! Walks the parent path with the shared resolver, generates a UUID
//! client-side, encrypts the leaf name under that UUID, then calls
//! `POST /api/v1/files/folder`. Cache is invalidated after a successful POST
//! so `bb ls /Foo` works immediately.
//!
//! `bb mkdir -p <path>` walks every segment, creating each missing folder
//! under the right parent. Idempotent — already-existing segments are skipped.

use colored::Colorize;
use uuid::Uuid;

use crate::api::ApiClient;
use crate::colors;
use crate::commands::push::load_master_key;
use crate::path;
use crate::ui;

/// Entry point for `bb mkdir`. `parents` is `true` when the user passed `-p`.
pub async fn run(path_arg: String, parents: bool) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = load_master_key()?;

    if parents {
        return run_recursive(&api, &master_key, &path_arg).await;
    }

    let (parent_path, leaf) = split_parent_and_leaf(&path_arg)?;

    // Parent must exist (we are not recursive).
    let parent_uuid: Option<String> = match parent_path {
        Some(p) => {
            let resolved = path::resolve_path(&api, &master_key, &p).await?;
            if !resolved.is_folder {
                return Err(format!("'{p}' is not a folder"));
            }
            resolved.file_id
        }
        None => None,
    };

    // Reject if the leaf already exists in the parent.
    if let Some(existing) = path::find_child_by_name(&api, &master_key, parent_uuid.as_deref(), &leaf).await? {
        if existing.is_folder {
            return Err(format!("{path_arg} already exists. use --parents to ignore."));
        } else {
            return Err(format!(
                "cannot create folder '{path_arg}': a file with that name exists"
            ));
        }
    }

    let folder_id = Uuid::new_v4();
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(&master_key, &folder_id.to_string(), &leaf, None)
        .map_err(|e| format!("failed to encrypt folder name: {e}"))?;

    let parent_uuid_typed: Option<Uuid> = parent_uuid.as_deref().and_then(|s| s.parse().ok());
    let created = api
        .create_folder(&name_encrypted, parent_uuid_typed, Some(folder_id))
        .await?;
    let created_id = created
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("server response missing id")?
        .to_string();

    path::invalidate_cache();

    if ui::is_json() {
        let json = serde_json::json!({
            "ok": true,
            "created": [{ "path": path_arg, "id": created_id }],
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    } else {
        print_created(&path_arg, &created_id);
    }
    Ok(())
}

/// Recursive `mkdir -p` implementation: walk segments, create each missing
/// folder under the right parent, silently skipping ones that already exist.
async fn run_recursive(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    path_arg: &str,
) -> Result<(), String> {
    let trimmed = path_arg.trim_matches('/');
    if trimmed.is_empty() {
        return Err("path required".to_string());
    }

    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();

    let mut parent_id: Option<String> = None;
    let mut cumulative = String::new();
    let mut created: Vec<(String, String)> = Vec::new(); // (path, id)

    for segment in &segments {
        cumulative.push('/');
        cumulative.push_str(segment);

        // Does this segment already exist?
        if let Some(existing) = path::find_child_by_name(api, master_key, parent_id.as_deref(), segment).await? {
            if !existing.is_folder {
                return Err(format!(
                    "cannot create folder '{cumulative}': a file with that name exists"
                ));
            }
            parent_id = existing.file_id;
            continue;
        }

        // Create it.
        let folder_id = Uuid::new_v4();
        let name_encrypted = beebeeb_core::encrypt::encrypt_name(master_key, &folder_id.to_string(), segment, None)
            .map_err(|e| format!("failed to encrypt folder name '{segment}': {e}"))?;

        let parent_uuid_typed: Option<Uuid> = parent_id.as_deref().and_then(|s| s.parse().ok());
        let resp = api
            .create_folder(&name_encrypted, parent_uuid_typed, Some(folder_id))
            .await?;
        let new_id = resp
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("server response missing id")?
            .to_string();

        // Invalidate immediately so the next loop iteration finds the new child.
        path::invalidate_cache();

        created.push((cumulative.clone(), new_id.clone()));
        parent_id = Some(new_id);
    }

    if ui::is_json() {
        let json = serde_json::json!({
            "ok": true,
            "created": created
                .iter()
                .map(|(p, id)| serde_json::json!({ "path": p, "id": id }))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    } else {
        // All segments already existed → print nothing, exit 0 (idempotent).
        for (p, id) in &created {
            print_created(p, id);
        }
    }

    Ok(())
}

/// Split `/Photos/2026` → (`Some("/Photos")`, `"2026"`).
/// Split `/Music` or `Music` → (`None`, `"Music"`).
fn split_parent_and_leaf(path_arg: &str) -> Result<(Option<String>, String), String> {
    let trimmed = path_arg.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return Err("path required".to_string());
    }
    match trimmed.rsplit_once('/') {
        Some(("", leaf)) | Some(("/", leaf)) => Ok((None, leaf.to_string())),
        Some((parent, leaf)) if !leaf.is_empty() => Ok((Some(parent.to_string()), leaf.to_string())),
        // No slash at all → root-level leaf.
        _ => Ok((None, trimmed.to_string())),
    }
}

fn print_created(path_arg: &str, id: &str) {
    let id_short = &id[..8.min(id.len())];
    println!(
        "  {} {} {}",
        "ok ·".custom_color(colors::GREEN_OK),
        format!("created {path_arg}").custom_color(colors::INK),
        format!("({id_short})").custom_color(colors::INK_DIM),
    );
}
