//! The in-process mock API and `bb` runner of the binary push tests (`zero_byte_files`, `transfer_stats_output`),
//! moved here unchanged from `zero_byte_files.rs` (task 2333) and given three knobs: extra init-answer fields, a
//! held first init and held conflict lookups. Every run uses a scratch `HOME` / `XDG_CONFIG_HOME` (the developer's
//! real config may hold a live session), `BB_REFUSE_NON_LOOPBACK_API=1`, an explicit loopback `--api` and
//! `BB_NO_UPDATE=1`. Each test binary declares it `pub mod`, so what one of them does not use is not dead code.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use futures_util::StreamExt;
pub use serde_json::{Value, json};

pub const SYNC_FOLDER_ID: &str = "22222222-2222-4222-8222-222222222222";

#[derive(Clone, Default)]
pub struct Mock {
    /// Mirror the pre-#103 server: 400 on `file_size_bytes == 0`.
    pub reject_empty: bool,
    /// Every `/uploads/init` body the client sent.
    pub inits: Arc<Mutex<Vec<Value>>>,
    /// Byte length of every chunk PUT.
    pub chunk_sizes: Arc<Mutex<Vec<usize>>>,
    /// Number of `/uploads/:session/complete` calls.
    pub completes: Arc<Mutex<usize>>,
    /// session id -> the file id the init was given (L13: complete echoes it).
    pub sessions: Arc<Mutex<HashMap<String, String>>>,
    /// Task 2333: fields set on every init answer (a pool id, a datacenter code, a location).
    pub init_extra: Option<Value>,
    /// Task 2333: while set, the FIRST init is held (not answered) — a test changes a file meanwhile.
    pub hold_first_init: Arc<AtomicBool>,
    /// Task 2333: while set, every conflict lookup (`check-conflict`) is held — a test changes a file meanwhile.
    pub hold_conflict_checks: Arc<AtomicBool>,
    /// Number of conflict lookups that arrived.
    pub conflict_checks: Arc<Mutex<usize>>,
    /// Task 2333: every chunk PUT is answered this many milliseconds after its body arrived (chunks overlap).
    pub chunk_delay_ms: u64,
}

pub async fn start_mock(mock: Mock) -> String {
    let app = Router::new()
        .route(
            "/api/v1/auth/me",
            get(|| async { Json(json!({"id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"})) }),
        )
        .route(
            "/api/v1/files/usage",
            get(|| async { Json(json!({ "used_bytes": 0 })) }),
        )
        .route(
            "/api/v1/files/check-conflict",
            post(|State(m): State<Mock>| async move {
                *m.conflict_checks.lock().unwrap() += 1;
                while m.hold_conflict_checks.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Json(json!({ "files": [] }))
            }),
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
                let first = {
                    let mut inits = m.inits.lock().unwrap();
                    inits.push(body.clone());
                    inits.len() == 1
                };
                while first && m.hold_first_init.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
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
                let session_id = uuid::Uuid::new_v4().to_string();
                // L13 (SF-0001): an honest server echoes the client's own id,
                // at init and again at complete.
                m.sessions.lock().unwrap().insert(session_id.clone(), file_id.clone());
                let mut answer = json!({
                    "upload_session_id": session_id,
                    "file_id": file_id,
                    "chunk_size_bytes": body.get("chunk_size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                    "chunk_count": body.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0),
                });
                if let (Some(fields), Some(Value::Object(extra))) = (answer.as_object_mut(), &m.init_extra) {
                    for (k, v) in extra {
                        fields.insert(k.clone(), v.clone());
                    }
                }
                Json(answer).into_response()
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
                    tokio::time::sleep(std::time::Duration::from_millis(m.chunk_delay_ms)).await;
                    Json(json!({ "index": idx, "size": n, "skipped": false }))
                },
            ),
        )
        .route(
            "/api/v1/uploads/:session/complete",
            post(|State(m): State<Mock>, UrlPath(sid): UrlPath<String>| async move {
                *m.completes.lock().unwrap() += 1;
                let id = m
                    .sessions
                    .lock()
                    .unwrap()
                    .get(&sid)
                    .cloned()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                Json(json!({ "id": id, "size_bytes": 0, "chunk_count": 1 }))
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
pub fn scratch_home(api: &str) -> PathBuf {
    // 32 zero bytes — a throwaway key; the mock never checks crypto.
    scratch_home_with(
        api,
        "test-session-token",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
}

/// [`scratch_home`] with this session token and master key (base64 of 32 bytes), for a test that looks for
/// them in the output.
pub fn scratch_home_with(api: &str, session_token: &str, master_key_b64: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-zero-{}", uuid::Uuid::new_v4()));
    let config = json!({
        "api_url": api,
        "session_token": session_token,
        "email": "zero-byte@beebeeb.io",
        "master_key": master_key_b64,
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
pub fn make_folder2(home: &Path) -> PathBuf {
    let dir = home.join("folder2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0-empty.txt"), b"").unwrap();
    std::fs::write(dir.join("a.txt"), b"alpha").unwrap();
    std::fs::write(dir.join("z.txt"), b"zulu").unwrap();
    dir
}

pub async fn bb(home: &Path, args: &[&str]) -> Output {
    run_bb(home, args, false).await
}

/// [`bb`] with colour forced on (`CLICOLOR_FORCE=1`, no `NO_COLOR`): stdout is coloured although it is a pipe.
pub async fn bb_coloured(home: &Path, args: &[&str]) -> Output {
    run_bb(home, args, true).await
}

async fn run_bb(home: &Path, args: &[&str], coloured: bool) -> Output {
    let home = home.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bb"));
        command
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
            .args(&args)
            .current_dir(&home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("BB_CONFIG_DIR", home.join(".config").join("beebeeb"))
            .env("BB_NO_UPDATE", "1")
            .env_remove("CLICOLOR_FORCE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if coloured {
            command.env_remove("NO_COLOR").env("CLICOLOR_FORCE", "1");
        } else {
            command.env("NO_COLOR", "1");
        }
        command.output().expect("run bb")
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

pub fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}
