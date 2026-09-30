//! Run real `bb mv` against a recording API, including root moves that used
//! to exit successfully without ever issuing a PATCH.
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde_json::{Value, json};

const SOURCE: &str = "11111111-1111-4111-8111-111111111111";
const TARGET: &str = "22222222-2222-4222-8222-222222222222";
const FILE: &str = "33333333-3333-4333-8333-333333333333";
const FOLDER: &str = "44444444-4444-4444-8444-444444444444";
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

async fn run(args: &[&str]) -> Vec<(String, Value)> {
    let calls = Calls::default();
    let app = Router::new()
        .route(
            "/api/v1/files",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                Json(json!({"files": match q.get("parent_id").map(String::as_str) {
                    None => json!([
                        {"id": SOURCE, "name_encrypted": "Source", "is_folder": true},
                        {"id": TARGET, "name_encrypted": "Target", "is_folder": true}
                    ]),
                    Some(SOURCE) => json!([
                        {"id": FILE, "name_encrypted": "file.txt", "is_folder": false, "parent_id": SOURCE},
                        {"id": FOLDER, "name_encrypted": "Child", "is_folder": true, "parent_id": SOURCE}
                    ]),
                    _ => json!([])
                }}))
            }),
        )
        .route(
            "/api/v1/files/:id",
            patch(
                |State(c): State<Calls>, Path(id): Path<String>, Json(body): Json<Value>| async move {
                    c.lock().unwrap().push((id, body));
                    Json(json!({"ok": true}))
                },
            ),
        )
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let home = std::env::temp_dir().join(format!("bb-move-{}", uuid::Uuid::new_v4()));
    for dir in [
        home.join(".config/beebeeb"),
        home.join("Library/Application Support/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            json!({
                "api_url": api, "session_token": "test-token",
                "master_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", "email": "test@example.test"
            })
            .to_string(),
        )
        .unwrap();
    }
    let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = tokio::task::spawn_blocking({
        let home = home.clone();
        move || {
            Command::new(env!("CARGO_BIN_EXE_bb"))
                .args(["--json", "mv"])
                .args(argv)
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("BB_NO_UPDATE", "1")
                .stdin(Stdio::null())
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    std::fs::remove_dir_all(home).unwrap();
    server.abort();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    calls.lock().unwrap().clone()
}

#[tokio::test]
async fn single_file_and_folder_moves_to_root_issue_patch() {
    for (src, id) in [("/Source/file.txt", FILE), ("/Source/Child", FOLDER)] {
        let calls = run(&[src, "/"]).await;
        assert_eq!(calls.len(), 1, "root move must issue one PATCH for {src}");
        assert_eq!(calls[0], (id.to_string(), json!({"parent_id": null})));
    }
}

#[tokio::test]
async fn bulk_move_to_root_sends_null_for_every_item() {
    let calls = run(&["/Source/file.txt", "/Source/Child", "/"]).await;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], (FILE.to_string(), json!({"parent_id": null})));
    assert_eq!(calls[1], (FOLDER.to_string(), json!({"parent_id": null})));
}

#[tokio::test]
async fn move_and_rename_to_root_sends_both_fields() {
    let calls = run(&["/Source/file.txt", "/renamed.txt"]).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.get("parent_id"), Some(&Value::Null));
    assert!(calls[0].1.get("name_encrypted").unwrap().is_string());
    assert_eq!(calls[0].1.as_object().unwrap().len(), 2);
}

#[tokio::test]
async fn folder_destination_sends_uuid_and_rename_only_omits_parent() {
    let calls = run(&["/Source/file.txt", "/Target"]).await;
    assert_eq!(calls, vec![(FILE.to_string(), json!({"parent_id": TARGET}))]);
    let calls = run(&["/Source/file.txt", "/Source/renamed.txt"]).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.as_object().unwrap().len(), 1);
    assert!(calls[0].1.get("parent_id").is_none());
    assert!(calls[0].1.get("name_encrypted").unwrap().is_string());
}
