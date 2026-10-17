//! Task 1950: re-running an interrupted `bb push` resumes it.
//!
//! The server creates the file row when the upload is inited, so a push killed
//! mid-stream leaves a same-named row that is still uploading. Re-running the
//! identical command used to treat that row as a name conflict and refuse
//! ("already exists — pass --replace or --keep-both"). Now, when the row is
//! this machine's own unfinished first upload of the same file (the local
//! resume record names it AND the server reports it still uploading with no
//! completed version), the re-run resumes it under core's `resume_policy`. A
//! completed file, and the interrupted replace of one, keep the refusal.
//!
//! Black box: the real `bb` binary, SIGKILLed mid-upload, against an in-process
//! mock that creates the row at init like the server, stalls chunk PUTs on
//! request, answers already-stored chunks with `skipped`, and serves the
//! completed file back. Scratch `HOME`, stdin not a terminal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use beebeeb_core::chunk_stream::{ExpectedChunkFormat, SessionResume, chunk_format_for_pin, resume_policy};
use serde_json::{Value, json};

/// The file as of its latest completed version.
struct Current {
    id: String,
    version: i64,
    name_encrypted: String,
    size: u64,
    chunk_size: u64,
    frames: Vec<Vec<u8>>,
}

struct Session {
    id: String,
    base: Option<i64>,
    name_encrypted: String,
    size: u64,
    chunk_size: u64,
    chunk_count: u32,
    frames: BTreeMap<u32, Vec<u8>>,
}

#[derive(Default)]
struct Vault {
    file: Option<Current>,
    sessions: BTreeMap<String, Session>,
    /// While set, every chunk PUT past index 0 hangs (the test then kills `bb`).
    stall: bool,
    inits: usize,
    abandons: usize,
    /// Chunk PUTs answered `skipped` (the session already held that index).
    skipped: usize,
}

type Shared = Arc<Mutex<Vault>>;

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg, "message": msg }))).into_response()
}

/// `GET /api/v1/files/{id}` in the server's shape: a row exists from init on;
/// `is_uploading` while a session for it is open; `download_version` only once
/// a version completed (no top-level `version_number`).
fn file_row(v: &Vault, id: &str) -> Option<Value> {
    let uploading = v.sessions.values().any(|s| s.id == id);
    if let Some(f) = v.file.as_ref().filter(|f| f.id == id) {
        return Some(json!({
            "id": f.id,
            "parent_id": null,
            "name_encrypted": f.name_encrypted,
            "is_folder": false,
            "is_trashed": false,
            "size_bytes": f.size,
            "chunk_count": f.frames.len(),
            "chunk_size_bytes": f.chunk_size,
            "is_uploading": uploading,
            "download_version": {
                "version_number": f.version,
                "chunk_count": f.frames.len(),
                "chunk_size_bytes": f.chunk_size,
                "plaintext_size_bytes": f.size,
            },
        }));
    }
    v.sessions.values().find(|s| s.id == id).map(|s| {
        json!({
            "id": s.id,
            "parent_id": null,
            "name_encrypted": s.name_encrypted,
            "is_folder": false,
            "is_trashed": false,
            "size_bytes": s.size,
            "chunk_count": s.chunk_count,
            "chunk_size_bytes": s.chunk_size,
            "is_uploading": true,
            "download_version": null,
        })
    })
}

async fn start_mock(vault: Shared) -> String {
    let app = Router::new()
        .route(
            "/api/v1/files/usage",
            get(|| async { Json(json!({ "used_bytes": 0 })) }),
        )
        .route(
            "/api/v1/files/check-conflict",
            // Every row in the folder, completed or still uploading.
            post(|State(v): State<Shared>| async move {
                let v = v.lock().unwrap();
                let mut rows: BTreeMap<String, String> = BTreeMap::new();
                for s in v.sessions.values().filter(|s| s.base.is_none()) {
                    rows.insert(s.id.clone(), s.name_encrypted.clone());
                }
                if let Some(f) = v.file.as_ref() {
                    rows.insert(f.id.clone(), f.name_encrypted.clone());
                }
                let files: Vec<Value> = rows
                    .into_iter()
                    .map(|(id, name)| json!({ "id": id, "name_encrypted": name }))
                    .collect();
                Json(json!({ "files": files }))
            }),
        )
        .route(
            "/api/v1/files/:id",
            get(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let v = v.lock().unwrap();
                match file_row(&v, &id) {
                    Some(row) => Json(row).into_response(),
                    None => err(StatusCode::NOT_FOUND, "not_found"),
                }
            }),
        )
        .route(
            "/api/v1/files/:id/upload/abandon",
            post(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let mut v = v.lock().unwrap();
                v.abandons += 1;
                let before = v.sessions.len();
                v.sessions.retain(|_, s| s.id != id);
                if v.sessions.len() == before {
                    return err(StatusCode::NOT_FOUND, "nothing to abandon");
                }
                Json(json!({ "abandoned": true })).into_response()
            }),
        )
        .route(
            "/api/v1/files/:id/download",
            get(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let v = v.lock().unwrap();
                match v.file.as_ref().filter(|f| f.id == id) {
                    Some(f) => (
                        [
                            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                            (
                                header::HeaderName::from_static("x-chunk-size"),
                                f.chunk_size.to_string(),
                            ),
                            (header::HeaderName::from_static("x-original-size"), f.size.to_string()),
                        ],
                        f.frames.concat(),
                    )
                        .into_response(),
                    None => err(StatusCode::NOT_FOUND, "not_found"),
                }
            }),
        )
        .route(
            "/api/v1/uploads/init",
            post(|State(v): State<Shared>, Json(body): Json<Value>| async move {
                let mut v = v.lock().unwrap();
                v.inits += 1;
                let id = body.get("file_id").and_then(Value::as_str).unwrap_or("").to_string();
                let base = body.get("base_version_number").and_then(Value::as_i64);
                // A live session holds the file's lease, as on the server.
                if v.sessions.values().any(|s| s.id == id) {
                    return err(StatusCode::CONFLICT, "an upload for this file is already in progress");
                }
                if let Some(f) = v.file.as_ref().filter(|f| f.id == id)
                    && base != Some(f.version)
                {
                    return err(StatusCode::CONFLICT, "stale base version for replacement upload");
                }
                let sid = uuid::Uuid::new_v4().to_string();
                let chunk_size = body.get("chunk_size_bytes").and_then(Value::as_u64).unwrap_or(0);
                let chunk_count = body.get("chunk_count").and_then(Value::as_u64).unwrap_or(0) as u32;
                v.sessions.insert(
                    sid.clone(),
                    Session {
                        id: id.clone(),
                        base,
                        name_encrypted: body.get("file_name").and_then(Value::as_str).unwrap_or("").to_string(),
                        size: body.get("file_size_bytes").and_then(Value::as_u64).unwrap_or(0),
                        chunk_size,
                        chunk_count,
                        frames: BTreeMap::new(),
                    },
                );
                Json(json!({
                    "upload_session_id": sid,
                    "file_id": id,
                    "chunk_size_bytes": chunk_size,
                    "chunk_count": chunk_count,
                }))
                .into_response()
            }),
        )
        .route(
            "/api/v1/uploads/:sid/chunks/:idx",
            put(
                |State(v): State<Shared>, UrlPath((sid, idx)): UrlPath<(String, u32)>, body: Bytes| async move {
                    let stall = v.lock().unwrap().stall && idx > 0;
                    if stall {
                        std::future::pending::<()>().await;
                    }
                    let mut v = v.lock().unwrap();
                    let Some(s) = v.sessions.get_mut(&sid) else {
                        return err(StatusCode::NOT_FOUND, "not_found");
                    };
                    if s.frames.contains_key(&idx) {
                        v.skipped += 1;
                        return Json(json!({ "index": idx, "size": body.len(), "skipped": true })).into_response();
                    }
                    s.frames.insert(idx, body.to_vec());
                    Json(json!({ "index": idx, "size": body.len(), "skipped": false })).into_response()
                },
            ),
        )
        .route(
            "/api/v1/uploads/:sid/complete",
            post(|State(v): State<Shared>, UrlPath(sid): UrlPath<String>| async move {
                let mut v = v.lock().unwrap();
                let Some(s) = v.sessions.remove(&sid) else {
                    return err(StatusCode::NOT_FOUND, "not_found");
                };
                if s.frames.len() as u32 != s.chunk_count {
                    return err(StatusCode::BAD_REQUEST, "missing chunks");
                }
                let version = s.base.map_or(1, |b| b + 1);
                let id = s.id.clone();
                v.file = Some(Current {
                    id: s.id,
                    version,
                    name_encrypted: s.name_encrypted,
                    size: s.size,
                    chunk_size: s.chunk_size,
                    frames: s.frames.into_values().collect(),
                });
                Json(json!({ "id": id, "version_number": version })).into_response()
            }),
        )
        .fallback(|| async { err(StatusCode::NOT_FOUND, "not_found") })
        // A 4 MiB chunk frame exceeds axum's 2 MB default body limit.
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .with_state(vault);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn scratch_home(api: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-rerun-{}", uuid::Uuid::new_v4()));
    let config = json!({
        "api_url": api,
        "session_token": "test-session-token",
        "email": "rerun@beebeeb.io",
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

fn bb_command(home: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    cmd
}

fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

async fn bb(home: &Path, args: &[&str]) -> Output {
    let mut cmd = bb_command(home, &owned(args));
    let shown = args.join(" ");
    let out = tokio::task::spawn_blocking(move || {
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run bb")
    })
    .await
    .unwrap();
    eprintln!(
        "[bb {} -> {:?}]\n--- stdout\n{}--- stderr\n{}",
        shown,
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Start `bb <args>` with chunk PUTs stalled, wait until the server holds chunk
/// 0 of a session that needs more, then SIGKILL it: an interrupted push.
async fn push_and_kill(vault: &Shared, home: &Path, args: &[&str]) {
    vault.lock().unwrap().stall = true;
    let mut child = bb_command(home, &owned(args))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bb");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let started = vault
            .lock()
            .unwrap()
            .sessions
            .values()
            .any(|s| s.chunk_count >= 2 && s.frames.contains_key(&0));
        if started {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "bb {} exited before it could be interrupted: {status:?}",
                args.join(" ")
            );
        }
        assert!(Instant::now() < deadline, "the upload never got chunk 0 to the server");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().expect("SIGKILL bb");
    let status = child.wait().unwrap();
    assert!(!status.success(), "the killed push must not report success");
    vault.lock().unwrap().stall = false;
}

/// Two chunks at the 4 MiB minimum chunk size; `k` varies the bytes.
fn content(k: u8) -> Vec<u8> {
    (0..4 * 1024 * 1024 + 1000usize)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(k).wrapping_add((i >> 12) as u8))
        .collect()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The core rule for re-running a new-file push: the session the first run
/// opened was written in `writing`, and so is the re-run.
fn expected_resume() -> SessionResume {
    let writing = chunk_format_for_pin(ExpectedChunkFormat::Unrecorded);
    resume_policy(Some(writing), writing)
}

/// Kill a push mid-upload, then run the identical command again: it exits 0,
/// completes THAT file (no second file, no "(1)" copy), follows core's resume
/// rule, and the file pulls back byte for byte.
#[tokio::test(flavor = "multi_thread")]
async fn rerunning_an_interrupted_push_resumes_it() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    std::fs::write(home.join("big.bin"), content(1)).unwrap();

    push_and_kill(&vault, &home, &["--api", &api, "push", "big.bin"]).await;
    let interrupted_id = {
        let v = vault.lock().unwrap();
        assert!(v.file.is_none(), "nothing completed before the kill");
        assert_eq!(v.sessions.len(), 1);
        v.sessions.values().next().unwrap().id.clone()
    };

    let out = bb(&home, &["--api", &api, "push", "big.bin"]).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "re-running the interrupted push must resume it, not refuse: {}",
        stderr(&out)
    );

    {
        let v = vault.lock().unwrap();
        let f = v.file.as_ref().expect("the resumed upload completed");
        assert_eq!(f.id, interrupted_id, "the interrupted file itself completed");
        assert_eq!(f.version, 1, "a first upload, not a replace");
        assert!(v.sessions.is_empty(), "no session left open");
        match expected_resume() {
            SessionResume::ContinueSession => {
                assert_eq!(v.inits, 1, "the stored session is continued, not re-inited");
                assert_eq!(v.abandons, 0);
                assert!(v.skipped >= 1, "chunk 0 the server already held is skipped");
            }
            SessionResume::RestartWithFreshSession => {
                assert_eq!(v.abandons, 1, "the old session is abandoned");
                assert_eq!(v.inits, 2, "and the same file is inited again from chunk 0");
            }
        }
    }

    let out = bb(&home, &["--api", &api, "pull", &interrupted_id, "-o", "pulled.bin"]).await;
    assert!(out.status.success(), "pull failed: {}", stderr(&out));
    assert!(
        std::fs::read(home.join("pulled.bin")).unwrap() == content(1),
        "the pulled file is the original"
    );
}

/// A completed push keeps today's refusal on a re-run: exit 2 with the hint,
/// and no upload starts.
#[tokio::test(flavor = "multi_thread")]
async fn rerunning_a_completed_push_still_refuses() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    std::fs::write(home.join("big.bin"), content(1)).unwrap();

    let out = bb(&home, &["--api", &api, "push", "big.bin"]).await;
    assert!(out.status.success(), "first push failed: {}", stderr(&out));

    let out = bb(&home, &["--api", &api, "push", "big.bin"]).await;
    assert_eq!(out.status.code(), Some(2), "a completed file is a real conflict");
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    assert!(stderr(&out).contains("--replace"), "{}", stderr(&out));
    let v = vault.lock().unwrap();
    assert_eq!(v.inits, 1, "the refused re-run starts no upload");
    assert_eq!(v.file.as_ref().unwrap().version, 1);
}

/// An interrupted `--replace` of a completed file leaves a resume record AND an
/// uploading row, but the file has a completed version: a plain re-run is still
/// a name conflict (exit 2), and the completed version is untouched.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_replace_is_not_resumed_by_a_plain_push() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let doc = home.join("big.bin");
    std::fs::write(&doc, content(1)).unwrap();

    let out = bb(&home, &["--api", &api, "push", "big.bin"]).await;
    assert!(out.status.success(), "first push failed: {}", stderr(&out));

    std::fs::write(&doc, content(2)).unwrap();
    push_and_kill(&vault, &home, &["--api", &api, "push", "--replace", "big.bin"]).await;
    assert_eq!(vault.lock().unwrap().sessions.len(), 1, "the replace is left uploading");

    let out = bb(&home, &["--api", &api, "push", "big.bin"]).await;
    assert_eq!(
        out.status.code(),
        Some(2),
        "a plain push must not finish someone's replace of a completed file"
    );
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    let v = vault.lock().unwrap();
    assert_eq!(
        v.file.as_ref().unwrap().version,
        1,
        "the completed version is untouched"
    );
    assert_eq!(
        v.inits, 2,
        "push + the killed replace; the refused re-run inits nothing"
    );
}
