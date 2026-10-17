//! Task 1951: `bb push --replace` of the same file, again and again.
//!
//! The server versions a replace by file id and refuses one whose
//! `base_version_number` is not the file's CURRENT version ("stale base
//! version", optimistic concurrency). `GET /api/v1/files/{id}` reports that
//! version inside `download_version`, not at the top level. The CLI used to read
//! only the top level, so every replace after the first sent base 1 and failed.
//!
//! Black box: the real `bb` binary against an in-process mock that keeps ONE
//! file, enforces the stale-base rule like the server, stores the uploaded
//! frames and serves them back, with a scratch `HOME`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

/// The one file the mock holds, as of its latest completed version.
#[derive(Clone)]
struct Current {
    id: String,
    version: i64,
    name_encrypted: String,
    size: u64,
    chunk_size: u64,
    frames: Vec<Vec<u8>>,
}

#[derive(Default)]
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
    /// `base_version_number` of every init, in order (`None` = a new file).
    init_bases: Vec<Option<i64>>,
}

type Shared = Arc<Mutex<Vault>>;

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg, "message": msg }))).into_response()
}

async fn start_mock(vault: Shared) -> String {
    let app = Router::new()
        .route(
            "/api/v1/files/usage",
            get(|| async { Json(json!({ "used_bytes": 0 })) }),
        )
        .route(
            "/api/v1/files/check-conflict",
            post(|State(v): State<Shared>| async move {
                let v = v.lock().unwrap();
                let files: Vec<Value> = v
                    .file
                    .iter()
                    .map(|f| json!({ "id": f.id, "name_encrypted": f.name_encrypted }))
                    .collect();
                Json(json!({ "files": files }))
            }),
        )
        .route(
            "/api/v1/files/:id",
            // The real server's shape: the current version sits in
            // `download_version`; there is NO top-level `version_number`.
            get(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let v = v.lock().unwrap();
                match v.file.as_ref().filter(|f| f.id == id) {
                    Some(f) => Json(json!({
                        "id": f.id,
                        "parent_id": null,
                        "name_encrypted": f.name_encrypted,
                        "is_folder": false,
                        "is_trashed": false,
                        "size_bytes": f.size,
                        "chunk_count": f.frames.len(),
                        "chunk_size_bytes": f.chunk_size,
                        "is_uploading": false,
                        "download_version": {
                            "version_number": f.version,
                            "chunk_count": f.frames.len(),
                            "chunk_size_bytes": f.chunk_size,
                            "plaintext_size_bytes": f.size,
                        },
                    }))
                    .into_response(),
                    None => err(StatusCode::NOT_FOUND, "not_found"),
                }
            }),
        )
        .route(
            "/api/v1/files/:id/download",
            get(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let v = v.lock().unwrap();
                match v.file.as_ref().filter(|f| f.id == id) {
                    Some(f) => {
                        let body: Vec<u8> = f.frames.concat();
                        (
                            [
                                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                                (
                                    header::HeaderName::from_static("x-chunk-size"),
                                    f.chunk_size.to_string(),
                                ),
                                (header::HeaderName::from_static("x-original-size"), f.size.to_string()),
                            ],
                            body,
                        )
                            .into_response()
                    }
                    None => err(StatusCode::NOT_FOUND, "not_found"),
                }
            }),
        )
        .route(
            "/api/v1/uploads/init",
            post(|State(v): State<Shared>, Json(body): Json<Value>| async move {
                let mut v = v.lock().unwrap();
                let base = body.get("base_version_number").and_then(Value::as_i64);
                v.init_bases.push(base);
                let id = body.get("file_id").and_then(Value::as_str).unwrap_or("").to_string();
                // Optimistic concurrency, as on the server: a replace of the
                // file must name its current version.
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
                    let mut v = v.lock().unwrap();
                    match v.sessions.get_mut(&sid) {
                        Some(s) => {
                            s.frames.insert(idx, body.to_vec());
                            Json(json!({ "index": idx, "size": body.len(), "skipped": false })).into_response()
                        }
                        None => err(StatusCode::NOT_FOUND, "not_found"),
                    }
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
        .with_state(vault);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn scratch_home(api: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-replace-{}", uuid::Uuid::new_v4()));
    let config = json!({
        "api_url": api,
        "session_token": "test-session-token",
        "email": "replace@beebeeb.io",
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

async fn bb(home: &Path, args: &[&str]) -> Output {
    let home = home.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let shown = args.join(" ");
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
    eprintln!(
        "[bb {} -> {:?}]\n--- stdout\n{}--- stderr\n{}",
        shown,
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Version `k` of the document: different bytes (and length) every time.
fn content(k: u8) -> Vec<u8> {
    (0..3000 + k as usize)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(k))
        .collect()
}

/// Push a file, then replace it three times in a row: every replace exits 0,
/// sends the CURRENT version as its base, the server ends at version 4, and
/// pulling the file returns exactly the last upload.
#[tokio::test(flavor = "multi_thread")]
async fn replacing_the_same_file_three_times_reaches_version_4() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let doc = home.join("doc.bin");

    std::fs::write(&doc, content(1)).unwrap();
    let out = bb(&home, &["--api", &api, "push", "doc.bin"]).await;
    assert!(
        out.status.success(),
        "first push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for k in 2..=4u8 {
        std::fs::write(&doc, content(k)).unwrap();
        let out = bb(&home, &["--api", &api, "push", "--replace", "doc.bin"]).await;
        assert!(
            out.status.success(),
            "replace #{} (server at version {}) failed: {}",
            k - 1,
            k - 1,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let id = {
        let v = vault.lock().unwrap();
        let f = v.file.as_ref().expect("the file exists");
        assert_eq!(f.version, 4, "a push plus three replaces end at version 4");
        assert_eq!(
            v.init_bases,
            vec![None, Some(1), Some(2), Some(3)],
            "each replace names the current version"
        );
        f.id.clone()
    };

    let out = bb(&home, &["--api", &api, "pull", &id, "-o", "pulled.bin"]).await;
    assert!(
        out.status.success(),
        "pull failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(home.join("pulled.bin")).unwrap(),
        content(4),
        "the pulled file is the LAST upload"
    );
}
