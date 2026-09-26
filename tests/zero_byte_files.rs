//! Zero-byte files (flow "CLI end to end", flow-6, P1).
//!
//! What the flow found against a real local stack:
//! - `bb push folder2` where `folder2` = [`0-empty.txt`, `a.txt`, `z.txt`] hit the
//!   server's `file_size_bytes must be positive` reject on the empty file and
//!   stopped: `a.txt` and `z.txt` were never uploaded (`bb ls folder2` → empty).
//! - `bb sync --once` printed `! upload failed: file_size_bytes must be positive`
//!   without saying WHICH file.
//!
//! The server half (accepting a 0-byte file on `/uploads/init`) is server PR
//! #103. These tests pin the client half, against an in-process mock API:
//! - a folder push carries on past a failed file, names it, uploads the rest,
//!   and exits non-zero;
//! - against a server that accepts empty files, an empty file goes up as the
//!   canonical one-chunk plan (`plan_chunks(0)` → 1 chunk holding the 28-byte
//!   AEAD of zero bytes) and the push exits 0;
//! - `bb sync` names the file whose upload failed.
//!
//! Every run uses a scratch `HOME` / `XDG_CONFIG_HOME` (the developer's real
//! config may hold a live session) and `BB_NO_UPDATE=1`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use futures_util::StreamExt;
use serde_json::{Value, json};

const SYNC_FOLDER_ID: &str = "22222222-2222-4222-8222-222222222222";

#[derive(Clone, Default)]
struct Mock {
    /// Mirror the pre-#103 server: 400 on `file_size_bytes == 0`.
    reject_empty: bool,
    /// Every `/uploads/init` body the client sent.
    inits: Arc<Mutex<Vec<Value>>>,
    /// Byte length of every chunk PUT.
    chunk_sizes: Arc<Mutex<Vec<usize>>>,
    /// Number of `/uploads/:session/complete` calls.
    completes: Arc<Mutex<usize>>,
}

async fn start_mock(mock: Mock) -> String {
    let app = Router::new()
        .route(
            "/api/v1/files/usage",
            get(|| async { Json(json!({ "used_bytes": 0 })) }),
        )
        .route(
            "/api/v1/files/check-conflict",
            post(|| async { Json(json!({ "files": [] })) }),
        )
        .route(
            "/api/v1/files/folder",
            post(|Json(body): Json<Value>| async move {
                let id = body
                    .get("folder_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                Json(json!({ "id": id, "is_folder": true }))
            }),
        )
        .route(
            "/api/v1/files",
            get(
                |Query(q): Query<std::collections::HashMap<String, String>>| async move {
                    match q.get("parent_id") {
                        None => Json(json!({ "files": [
                            { "id": SYNC_FOLDER_ID, "is_folder": true, "name_encrypted": "SyncUp" }
                        ] })),
                        Some(_) => Json(json!({ "files": [] })),
                    }
                },
            ),
        )
        .route(
            "/api/v1/uploads/init",
            post(|State(m): State<Mock>, Json(body): Json<Value>| async move {
                m.inits.lock().unwrap().push(body.clone());
                let size = body.get("file_size_bytes").and_then(|v| v.as_i64()).unwrap_or(-1);
                if m.reject_empty && size == 0 {
                    // The exact reject the flow hit on server main (d841950).
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": "file_size_bytes must be positive" })),
                    )
                        .into_response();
                }
                let file_id = body
                    .get("file_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                Json(json!({
                    "upload_session_id": uuid::Uuid::new_v4().to_string(),
                    "file_id": file_id,
                    "chunk_size_bytes": body.get("chunk_size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                    "chunk_count": body.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0),
                }))
                .into_response()
            }),
        )
        .route(
            "/api/v1/uploads/:session/chunks/:idx",
            put(
                |State(m): State<Mock>, UrlPath((_s, idx)): UrlPath<(String, u32)>, body: Body| async move {
                    let mut stream = body.into_data_stream();
                    let mut n = 0usize;
                    while let Some(frame) = stream.next().await {
                        n += frame.map(|b| b.len()).unwrap_or(0);
                    }
                    m.chunk_sizes.lock().unwrap().push(n);
                    Json(json!({ "index": idx, "size": n, "skipped": false }))
                },
            ),
        )
        .route(
            "/api/v1/uploads/:session/complete",
            post(|State(m): State<Mock>| async move {
                *m.completes.lock().unwrap() += 1;
                Json(json!({ "id": uuid::Uuid::new_v4().to_string(), "size_bytes": 0, "chunk_count": 1 }))
            }),
        )
        .fallback(|| async {
            let r: Response = (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "not_found", "message": "not found" })),
            )
                .into_response();
            r
        })
        .with_state(mock);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// A scratch HOME holding a logged-in config that points at `api`.
fn scratch_home(api: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-zero-{}", uuid::Uuid::new_v4()));
    let config = json!({
        "api_url": api,
        "session_token": "test-session-token",
        "email": "zero-byte@beebeeb.io",
        // 32 zero bytes — a throwaway key; the mock never checks crypto.
        "master_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    });
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
    }
    home
}

/// `folder2` exactly as in the flow: the empty file sorts FIRST.
fn make_folder2(home: &Path) -> PathBuf {
    let dir = home.join("folder2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0-empty.txt"), b"").unwrap();
    std::fs::write(dir.join("a.txt"), b"alpha").unwrap();
    std::fs::write(dir.join("z.txt"), b"zulu").unwrap();
    dir
}

async fn bb(home: &Path, args: &[&str]) -> Output {
    let home = home.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(&args)
            .current_dir(&home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("BB_NO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run bb")
    })
    .await
    .unwrap();
    let (stdout, stderr) = text(&out);
    eprintln!(
        "[exit {:?}]\n--- stdout\n{stdout}--- stderr\n{stderr}",
        out.status.code()
    );
    out
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn folder_push_carries_on_past_a_rejected_file_and_names_it() {
    let mock = Mock {
        reject_empty: true,
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "push", "folder2"]).await;
    let (stdout, stderr) = text(&out);

    assert_eq!(
        *mock.completes.lock().unwrap(),
        2,
        "a.txt and z.txt must still be uploaded after 0-empty.txt is rejected\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(mock.inits.lock().unwrap().len(), 3, "all three files must be attempted");
    assert!(
        !out.status.success(),
        "a push that left a file behind must not exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("0-empty.txt") && stderr.contains("file_size_bytes must be positive"),
        "stderr must name the failed file and the reason, got: {stderr}"
    );
    assert!(
        stderr.contains("1 of 3 files failed"),
        "stderr must carry the failure count, got: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn folder_push_json_lists_the_failed_file() {
    let mock = Mock {
        reject_empty: true,
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "--json", "push", "folder2"]).await;
    let (stdout, _stderr) = text(&out);

    assert!(!out.status.success(), "must not exit 0");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    assert_eq!(v["total_files"], 2);
    assert_eq!(v["total_failed"], 1);
    assert_eq!(v["failed"][0]["path"], "0-empty.txt");
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_file_uploads_as_one_aead_chunk_when_the_server_accepts_it() {
    // Server PR #103 behaviour: 0-byte init accepted.
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "push", "folder2"]).await;
    let (stdout, stderr) = text(&out);

    assert!(
        out.status.success(),
        "push must succeed\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(*mock.completes.lock().unwrap(), 3, "all three files complete");
    let inits = mock.inits.lock().unwrap().clone();
    let empty = inits
        .iter()
        .find(|b| b["file_size_bytes"] == 0)
        .expect("an init with file_size_bytes 0 for the empty file");
    assert_eq!(empty["chunk_count"], 1, "plan_chunks(0) is ONE chunk: {empty}");
    let sizes = mock.chunk_sizes.lock().unwrap().clone();
    // 28-byte frames: the empty file (0 + 28). a.txt is 5 + 28, z.txt 4 + 28.
    assert_eq!(sizes.len(), 3, "one chunk per file, got {sizes:?}");
    assert!(
        sizes.contains(&28),
        "the empty file's chunk is the 28-byte AEAD of b\"\": {sizes:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_names_the_file_whose_upload_failed() {
    let mock = Mock {
        reject_empty: true,
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let local = home.join("syncdir");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("empty.txt"), b"").unwrap();

    let out = bb(
        &home,
        &["--api", &api, "sync", "--once", local.to_str().unwrap(), "/SyncUp"],
    )
    .await;
    let (_stdout, stderr) = text(&out);

    assert!(
        !mock.inits.lock().unwrap().is_empty(),
        "the upload must have been attempted"
    );
    assert!(
        stderr.contains("upload failed: empty.txt: file_size_bytes must be positive"),
        "the failure must name the file, got: {stderr}"
    );
}
