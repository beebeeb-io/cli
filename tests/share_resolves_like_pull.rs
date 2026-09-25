//! `bb share` must accept the same file references `bb pull` does: a vault
//! path (`note.txt`), the 8-character short ID that `bb ls` prints, or a full
//! UUID. Before this test existed, `bb share note.txt` and `bb share 4c53f27f`
//! both failed with "invalid file id (expected UUID)", so the only way to
//! share was to dig the UUID out of `bb ls --json`.
//!
//! The test runs the real `bb` binary against an in-process mock API (axum)
//! with a scratch HOME — it never touches the developer's real config — and
//! asserts that the `file_id` in the `POST /api/v1/shares` body is the full
//! UUID, identical to the id `bb pull` resolves for the same argument.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{Value, json};

const NOTE_ID: &str = "4c53f27f-1a2b-4c3d-8e9f-0123456789ab";
const OTHER_ID: &str = "081e6283-aaaa-4bbb-8ccc-dddddddddddd";
const FOLDER_ID: &str = "9f00d1e2-1111-4222-8333-444444444444";
const CHILD_ID: &str = "4c53f000-5555-4666-8777-888888888888";
const MASTER_KEY: [u8; 32] = [7u8; 32];

#[derive(Default)]
struct Recorded {
    /// `file_id` field of every `POST /api/v1/shares` body.
    share_file_ids: Vec<String>,
    /// Every id requested via `GET /api/v1/files/{id}` (what `bb pull` resolved).
    metadata_ids: Vec<String>,
}

type Shared = Arc<Mutex<Recorded>>;

/// A real `name_encrypted` blob, the way the vault stores it: AES-256-GCM
/// under the file key derived from the master key + the UUID string.
fn encrypted_name(file_id: &str, name: &str) -> String {
    let mk = beebeeb_core::kdf::MasterKey::from_bytes(MASTER_KEY);
    let fk = beebeeb_core::kdf::derive_file_key(&mk, file_id.as_bytes());
    let blob = beebeeb_core::encrypt::encrypt_metadata(&fk, name).expect("encrypt name");
    serde_json::to_string(&blob).expect("serialize blob")
}

fn entry(id: &str, name: &str, is_folder: bool) -> Value {
    json!({
        "id": id,
        "name_encrypted": encrypted_name(id, name),
        "is_folder": is_folder,
        "size_bytes": 12,
        "created_at": "2026-09-25T10:00:00Z",
    })
}

async fn list_files(Query(q): Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    let files = match q.get("parent_id").map(String::as_str) {
        None => vec![
            entry(NOTE_ID, "note.txt", false),
            entry(OTHER_ID, "other.bin", false),
            entry(FOLDER_ID, "folder1", true),
        ],
        Some(FOLDER_ID) => vec![entry(CHILD_ID, "a.bin", false)],
        Some(_) => vec![],
    };
    Json(json!({ "files": files }))
}

async fn file_meta(State(s): State<Shared>, AxPath(id): AxPath<String>) -> (StatusCode, Json<Value>) {
    s.lock().unwrap().metadata_ids.push(id);
    // Stop `bb pull` right after it has told us which id it resolved.
    (StatusCode::NOT_FOUND, Json(json!({ "error": "test stop" })))
}

async fn create_share(State(s): State<Shared>, Json(body): Json<Value>) -> Json<Value> {
    let fid = body
        .get("file_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    s.lock().unwrap().share_file_ids.push(fid);
    Json(json!({
        "id": "11111111-2222-4333-8444-555555555555",
        "token": "tok",
        "url": "http://localhost/s/tok#k",
        "expires_at": null,
    }))
}

async fn spawn_mock(state: Shared) -> SocketAddr {
    let app = Router::new()
        .route("/api/v1/files", get(list_files))
        .route("/api/v1/files/:id", get(file_meta))
        .route("/api/v1/shares", post(create_share))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Scratch HOME with a logged-in config pointing at the mock. Writes the
/// config to both the macOS and the XDG location so the test runs on either.
fn scratch_home(tag: &str, api: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-share-resolve-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let cfg = json!({
        "api_url": api,
        "session_token": "test-token",
        "email": "test@beebeeb.io",
        "master_key": base64::engine::general_purpose::STANDARD.encode(MASTER_KEY),
    })
    .to_string();
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), &cfg).unwrap();
    }
    home
}

fn bb(home: &Path, api: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .arg("--api")
        .arg(api)
        .arg("--json")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env_remove("APP_URL")
        .output()
        .expect("run bb")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_accepts_path_and_short_id_and_resolves_like_pull() {
    let state: Shared = Arc::default();
    let addr = spawn_mock(state.clone()).await;
    let api = format!("http://{addr}");
    let home = scratch_home("main", &api);

    let cases: &[(&[&str], &str)] = &[
        (&["share", "note.txt"], NOTE_ID),
        (&["share", "4c53f27f"], NOTE_ID),
        (&["share", "4c53f27f", "--no-double-encrypt"], NOTE_ID),
        (&["share", "folder1/a.bin"], CHILD_ID),
        (&["share", NOTE_ID], NOTE_ID),
    ];

    for (args, want) in cases {
        let (h, a, args_v) = (home.clone(), api.clone(), args.to_vec());
        let out = tokio::task::spawn_blocking(move || bb(&h, &a, &args_v)).await.unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "`bb {}` failed: {stderr}", args.join(" "));
        let got = state.lock().unwrap().share_file_ids.pop();
        assert_eq!(
            got.as_deref(),
            Some(*want),
            "`bb {}` sent the wrong file_id to POST /api/v1/shares",
            args.join(" ")
        );
    }

    // `bb pull` resolves the same arguments to the same UUIDs.
    for (arg, want) in [
        ("note.txt", NOTE_ID),
        ("4c53f27f", NOTE_ID),
        ("folder1/a.bin", CHILD_ID),
    ] {
        let (h, a) = (home.clone(), api.clone());
        let _ = tokio::task::spawn_blocking(move || bb(&h, &a, &["pull", arg]))
            .await
            .unwrap();
        let got = state.lock().unwrap().metadata_ids.pop();
        assert_eq!(got.as_deref(), Some(want), "`bb pull {arg}` resolved a different id");
    }

    // A folder is not shareable as a file; the user gets a clear error, and
    // nothing is POSTed.
    let (h, a) = (home.clone(), api.clone());
    let out = tokio::task::spawn_blocking(move || bb(&h, &a, &["share", "folder1"]))
        .await
        .unwrap();
    assert!(!out.status.success(), "`bb share folder1` should fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is a folder"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(state.lock().unwrap().share_file_ids.is_empty());

    let _ = std::fs::remove_dir_all(&home);
}
