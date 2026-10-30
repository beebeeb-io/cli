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

    let parent_uuid_typed: Option<Uuid> = parent_uuid.as_deref().and_then(|s| s.parse().ok());
    let created_id = create_leaf_folder(&api, &master_key, &leaf, parent_uuid_typed).await?;

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
        let parent_uuid_typed: Option<Uuid> = parent_id.as_deref().and_then(|s| s.parse().ok());
        let new_id = create_leaf_folder(api, master_key, segment, parent_uuid_typed).await?;

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
/// L13 (SF-0001): create one owner folder with an id this client mints, its
/// name sealed under that id, and the answer required to echo the same id. A
/// different id aborts; nothing is created under or inside the echoed one.
async fn create_leaf_folder(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    name: &str,
    parent: Option<Uuid>,
) -> Result<String, String> {
    Ok(crate::item_id::create_owned_folder(api, master_key, name, parent, Uuid::new_v4())
        .await?
        .to_string())
}

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

#[cfg(test)]
mod item_id_echo_tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// A mock create route that records the `folder_id` it was sent and answers
    /// `answer` (a fixed id, or the requested one when `None`).
    async fn mock_create(answer: Option<uuid::Uuid>) -> (String, Arc<Mutex<Option<String>>>) {
        let seen = Arc::new(Mutex::new(None::<String>));
        let record = seen.clone();
        let app = Router::new().route(
            "/api/v1/files/folder",
            post(move |Json(body): Json<serde_json::Value>| {
                let record = record.clone();
                async move {
                    let requested = body.get("folder_id").and_then(|v| v.as_str()).map(String::from);
                    *record.lock().unwrap() = requested.clone();
                    let echoed = answer
                        .map(|a| a.to_string())
                        .or(requested)
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    Json(json!({"id": echoed}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), seen)
    }

    fn master() -> beebeeb_core::kdf::MasterKey {
        beebeeb_core::kdf::MasterKey::from_bytes([0u8; 32])
    }

    /// A8 (mkdir): a folder create whose answer echoes a different id aborts
    /// with the plain message; nothing is returned for use as a parent.
    #[tokio::test]
    async fn folder_create_echoing_a_different_id_aborts() {
        let other = uuid::Uuid::new_v4();
        let (base, seen) = mock_create(Some(other)).await;
        let api = crate::api::ApiClient::new_for_test(base);
        let err = create_leaf_folder(&api, &master(), "Folder", None)
            .await
            .expect_err("must abort");
        assert_eq!(err, crate::item_id::CHANGED_ID_MESSAGE);
        let requested = seen.lock().unwrap().clone().expect("folder_id was sent");
        assert_ne!(requested, other.to_string(), "client proposed its own id");
    }

    /// Normal flow: the answer echoes the client's id and it is used.
    #[tokio::test]
    async fn folder_create_echoing_the_clients_id_succeeds() {
        let (base, seen) = mock_create(None).await;
        let api = crate::api::ApiClient::new_for_test(base);
        let id = create_leaf_folder(&api, &master(), "Folder", None)
            .await
            .expect("succeeds");
        assert_eq!(Some(id), *seen.lock().unwrap());
    }
}
