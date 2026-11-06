//! Test-only: ONE loopback mock of the API routes an upload or a create touches,
//! shared by every module's tests (L13 / SF-0001 round 2: one helper instead of a
//! copy per module).
//!
//! It models what the upload driver needs from the server (task 1589's leases and
//! swept sessions, task 1571's abandon) and what an L13 test has to observe: the
//! id every init and folder create was sent, what each answer echoed, every chunk
//! PUT, completion, abandon and trash, and the order they happened in
//! ([`Mock::events`]). Every answer that names an item echoes the id it was sent
//! unless a test sets `init_echo` / `complete_echo` / `folder_echo`, as an honest
//! server does.
//!
//! Binds `127.0.0.1:0` only: nothing here can reach another host.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::json;
use uuid::Uuid;

/// What the mock answers for a session it no longer has.
#[derive(Clone, Copy)]
pub enum Gone {
    /// Current servers (PR #120): 404.
    NotFound,
    /// Older servers for a kept `status='expired'` row.
    BadRequestExpired,
}

#[derive(Default)]
pub struct Mock {
    /// Sessions the server holds. Anything else is "swept".
    pub live: HashSet<String>,
    /// Sessions whose complete answers "gone" although chunks were taken.
    pub complete_gone: HashSet<String>,
    /// When set, sessions opened by init are dead immediately (to prove the
    /// driver re-inits at most once).
    pub kill_new_sessions: bool,
    /// Init answers this status for the given file_id, every time.
    pub init_refuse: HashMap<String, u16>,
    /// Init answers `.0` for the given file_id, `.1` more times (counting
    /// down), then succeeds normally — for proving a bounded SAME-id retry
    /// (task 1589, Codex thread #1). Checked before `init_refuse`.
    pub init_fail_then_succeed: HashMap<String, (u16, u32)>,
    /// A session in `live` still goes "gone" once it has already ACCEPTED
    /// this many chunk PUTs — proving a MID-pipeline sweep (some chunks
    /// already confirmed) rolls those bytes back rather than
    /// double-counting them on the retry's full resend (task 1589, Codex
    /// thread #2).
    pub dies_after_chunks: HashMap<String, u32>,
    pub chunk_accept_count: HashMap<String, u32>,
    pub gone: Option<Gone>,
    /// file_id sent on every init, in order.
    pub inits: Vec<String>,
    /// The whole body of every init, in order (`base_version_number`, ...).
    pub init_bodies: Vec<serde_json::Value>,
    /// (session, index) of every chunk PUT, in order.
    pub chunk_puts: Vec<(String, u32)>,
    /// session of every successful complete.
    pub completed: Vec<String>,
    /// session -> file_id, for complete's response.
    pub session_file: HashMap<String, String>,
    /// The encrypted name (`file_name`) sent on every init, in order (task
    /// 1943: the pin a writer records).
    pub init_names: Vec<String>,
    /// The bytes of every chunk PUT, keyed by (session, index).
    pub chunk_bodies: HashMap<(String, u32), Vec<u8>>,
    /// file_id of every `POST /files/{id}/upload/abandon`, in order.
    pub abandons: Vec<String>,
    /// L13 (SF-0001): init answers THIS id instead of the one it was sent.
    pub init_echo: Option<String>,
    /// L13 (SF-0001): complete answers THIS id instead of the session's file.
    pub complete_echo: Option<String>,
    /// L13 (SF-0001 L7): abandon answers this status instead of ending the
    /// session (a transient failure).
    pub abandon_status: Option<u16>,
    /// L13 (SF-0001): a folder create answers THIS id instead of the one sent.
    pub folder_echo: Option<String>,
    /// `folder_id` sent on every folder create, in order (empty = none sent).
    pub folder_requests: Vec<String>,
    /// The body of every folder create, in order (the sealed name it carried).
    pub folder_bodies: Vec<serde_json::Value>,
    /// id of every `DELETE /files/{id}` (trash), in order.
    pub trashed: Vec<String>,
    /// `GET /files/{id}` answers: id -> metadata. Absent = 404.
    pub files: HashMap<String, serde_json::Value>,
    /// Task 2334: `GET /files/{id}` answers this status for every id instead
    /// (a server that cannot say where an upload was going).
    pub file_status: Option<u16>,
    /// `GET /files/{id}/download` answers: id -> body. Absent = 404.
    pub downloads: HashMap<String, Vec<u8>>,
    /// SF-0002 r3: a move/rename (`PATCH /files/{id}`) answers this status
    /// instead of succeeding.
    pub move_status: Option<u16>,
    /// (file_id, body) of every move/rename PATCH, in order (N6: the sealed
    /// name a rename sends).
    pub move_bodies: Vec<(String, serde_json::Value)>,
    /// `GET /file-requests` answers this document. Unset = no requests, so a
    /// request row resolves as "keys not held" rather than the list failing.
    pub requests_doc: serde_json::Value,
    /// Every request that changes state, in order: `init:<id>`,
    /// `chunk:<session>:<index>`, `complete:<session>`, `abandon:<id>`,
    /// `trash:<id>`, `folder:<id>`, `move:<id>` (a successful PATCH).
    pub events: Vec<String>,
    /// Task 2333: merged into every init answer (a pool id, a location, ...).
    pub init_extra: Option<serde_json::Value>,
    /// Task 2333: merged into every complete answer.
    pub complete_extra: Option<serde_json::Value>,
    /// Task 2333: the chunk PUT of this index answers this status, every time.
    pub chunk_status: HashMap<u32, u16>,
    /// Task 2333: the FIRST PUT of each of these indexes answers 429 with
    /// `Retry-After: 0` (a transient refusal the client retries).
    pub chunk_rate_limited_once: HashSet<u32>,
    /// `--parallel`: the `Retry-After` (seconds) of every 429 and 503 the mock answers (0 unless a test needs a
    /// real pause).
    pub retry_after_secs: u64,
    /// `--parallel` Task 4: the FIRST PUT of each of these indexes answers 503 (the storage pool is briefly
    /// unavailable), with `retry_after_secs`.
    pub chunk_busy_once: HashSet<u32>,
    /// `--parallel` Task 4: the first `GET /files/{id}` answers 429 with `retry_after_secs` (an unpaced
    /// caller meeting a rate limit).
    pub file_rate_limited_once: bool,
    /// `--parallel` Task 1: when set, every request the mock answers is logged here, in order (the recorder
    /// buffers each body, so a test that aborts a body must not set it).
    pub requests: Option<RequestLog>,
    /// `--parallel`: keep only the length of each chunk PUT, not its bytes (a 2 GiB fixture would otherwise be
    /// held whole in `chunk_bodies`).
    pub drop_chunk_bodies: bool,
}

pub type Shared = Arc<Mutex<Mock>>;

/// A fresh mock to configure before [`spawn`].
pub fn shared(mock: Mock) -> Shared {
    Arc::new(Mutex::new(mock))
}

fn gone_response(g: Option<Gone>) -> Response {
    match g.unwrap_or(Gone::NotFound) {
        Gone::NotFound => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Gone::BadRequestExpired => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "upload session is not writable: expired" })),
        )
            .into_response(),
    }
}

/// `answer` with every field of `extra` (an object) set on it.
fn merged(mut answer: serde_json::Value, extra: &Option<serde_json::Value>) -> serde_json::Value {
    if let (Some(fields), Some(serde_json::Value::Object(more))) = (answer.as_object_mut(), extra) {
        for (k, v) in more {
            fields.insert(k.clone(), v.clone());
        }
    }
    answer
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

fn router(state: Shared) -> Router {
    let log = state.lock().unwrap().requests.clone();
    let router = Router::new()
        .route(
            "/api/v1/uploads/init",
            post(
                |State(st): State<Shared>, Json(body): Json<serde_json::Value>| async move {
                    let file_id = body
                        .get("file_id")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    let mut m = st.lock().unwrap();
                    m.inits.push(file_id.clone());
                    m.init_bodies.push(body.clone());
                    m.events.push(format!("init:{file_id}"));
                    // The v2 init carries the encrypted name as `file_name`.
                    let name = body.get("file_name").and_then(|v| v.as_str()).unwrap_or("");
                    m.init_names.push(name.to_string());
                    if let Some((code, remaining)) = m.init_fail_then_succeed.get_mut(&file_id) {
                        if *remaining > 0 {
                            *remaining -= 1;
                            let status = StatusCode::from_u16(*code).unwrap();
                            return (status, Json(json!({ "error": "refused by mock" }))).into_response();
                        }
                    }
                    if let Some(code) = m.init_refuse.get(&file_id).copied() {
                        let status = StatusCode::from_u16(code).unwrap();
                        return (status, Json(json!({ "error": "refused by mock" }))).into_response();
                    }
                    let sid = Uuid::new_v4().to_string();
                    if !m.kill_new_sessions {
                        m.live.insert(sid.clone());
                    }
                    // L13: the session belongs to the id the server was SENT; a
                    // misbehaving server only answers a different one.
                    m.session_file.insert(sid.clone(), file_id.clone());
                    let echoed = m.init_echo.clone().unwrap_or_else(|| file_id.clone());
                    let answer = json!({
                        "upload_session_id": sid,
                        "file_id": echoed,
                        "chunk_size_bytes": body.get("chunk_size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                        "chunk_count": body.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0),
                    });
                    Json(merged(answer, &m.init_extra)).into_response()
                },
            ),
        )
        .route(
            "/api/v1/uploads/:session/chunks/:idx",
            put(
                |State(st): State<Shared>, Path((sid, idx)): Path<(String, u32)>, body: Body| async move {
                    let mut stream = body.into_data_stream();
                    let mut data = Vec::new();
                    while let Some(frame) = stream.next().await {
                        data.extend_from_slice(&frame.unwrap_or_default());
                    }
                    let n = data.len();
                    let mut m = st.lock().unwrap();
                    m.chunk_puts.push((sid.clone(), idx));
                    m.events.push(format!("chunk:{sid}:{idx}"));
                    if !m.drop_chunk_bodies {
                        m.chunk_bodies.insert((sid.clone(), idx), data);
                    }
                    if m.chunk_rate_limited_once.remove(&idx) {
                        return (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", m.retry_after_secs.to_string())],
                            Json(json!({ "error": "rate_limited" })),
                        )
                            .into_response();
                    }
                    if m.chunk_busy_once.remove(&idx) {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            [("retry-after", m.retry_after_secs.to_string())],
                            Json(json!({ "error": "storage_unavailable" })),
                        )
                            .into_response();
                    }
                    if let Some(&code) = m.chunk_status.get(&idx) {
                        let status = StatusCode::from_u16(code).unwrap();
                        return (status, Json(json!({ "error": "failed by mock" }))).into_response();
                    }
                    if !m.live.contains(&sid) {
                        return gone_response(m.gone);
                    }
                    if let Some(&limit) = m.dies_after_chunks.get(&sid) {
                        let count = *m.chunk_accept_count.get(&sid).unwrap_or(&0);
                        if count >= limit {
                            return gone_response(m.gone);
                        }
                        m.chunk_accept_count.insert(sid.clone(), count + 1);
                    }
                    Json(json!({ "index": idx, "size": n, "skipped": false })).into_response()
                },
            ),
        )
        .route(
            // Server task 1571: abandon ends the file's in-flight session.
            "/api/v1/files/:id/upload/abandon",
            post(|State(st): State<Shared>, Path(fid): Path<String>| async move {
                let mut m = st.lock().unwrap();
                m.abandons.push(fid.clone());
                m.events.push(format!("abandon:{fid}"));
                if let Some(code) = m.abandon_status {
                    // A transient failure: the session stays alive.
                    let status = StatusCode::from_u16(code).unwrap();
                    return (status, Json(json!({ "error": "abandon failed by mock" }))).into_response();
                }
                let ended: Vec<String> = m
                    .session_file
                    .iter()
                    .filter(|(_, f)| **f == fid)
                    .map(|(s, _)| s.clone())
                    .collect();
                for sid in ended {
                    m.live.remove(&sid);
                }
                Json(json!({ "id": fid, "status": "abandoned" })).into_response()
            }),
        )
        .route(
            "/api/v1/uploads/:session/complete",
            post(|State(st): State<Shared>, Path(sid): Path<String>| async move {
                let mut m = st.lock().unwrap();
                if !m.live.contains(&sid) || m.complete_gone.contains(&sid) {
                    return gone_response(m.gone);
                }
                m.completed.push(sid.clone());
                m.events.push(format!("complete:{sid}"));
                let fid = m
                    .complete_echo
                    .clone()
                    .or_else(|| m.session_file.get(&sid).cloned())
                    .unwrap_or_default();
                Json(merged(
                    json!({ "id": fid, "size_bytes": 0, "chunk_count": 0 }),
                    &m.complete_extra,
                ))
                .into_response()
            }),
        )
        .route(
            "/api/v1/files/folder",
            post(
                |State(st): State<Shared>, Json(body): Json<serde_json::Value>| async move {
                    let requested = body
                        .get("folder_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let mut m = st.lock().unwrap();
                    m.folder_requests.push(requested.clone());
                    m.folder_bodies.push(body.clone());
                    m.events.push(format!("folder:{requested}"));
                    let echoed = m.folder_echo.clone().unwrap_or(requested);
                    Json(json!({ "id": echoed })).into_response()
                },
            ),
        )
        // The listing: nothing is listed (a WebDAV path resolves as new).
        .route("/api/v1/files", get(|| async { not_found() }))
        .route(
            "/api/v1/file-requests",
            get(|State(st): State<Shared>| async move {
                let doc = st.lock().unwrap().requests_doc.clone();
                if doc.is_null() {
                    Json(json!({ "file_requests": [] })).into_response()
                } else {
                    Json(doc).into_response()
                }
            }),
        )
        .route(
            "/api/v1/files/:id",
            get(|State(st): State<Shared>, Path(fid): Path<String>| async move {
                let mut m = st.lock().unwrap();
                if std::mem::take(&mut m.file_rate_limited_once) {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", m.retry_after_secs.to_string())],
                        Json(json!({ "error": "rate_limited" })),
                    )
                        .into_response();
                }
                if let Some(code) = m.file_status {
                    let status = StatusCode::from_u16(code).unwrap();
                    return (status, Json(json!({ "error": "failed by mock" }))).into_response();
                }
                match m.files.get(&fid) {
                    Some(meta) => Json(meta.clone()).into_response(),
                    None => not_found(),
                }
            })
            .delete(|State(st): State<Shared>, Path(fid): Path<String>| async move {
                let mut m = st.lock().unwrap();
                m.trashed.push(fid.clone());
                m.events.push(format!("trash:{fid}"));
                Json(json!({ "id": fid, "is_trashed": true })).into_response()
            })
            .patch(
                |State(st): State<Shared>, Path(fid): Path<String>, Json(body): Json<serde_json::Value>| async move {
                    let mut m = st.lock().unwrap();
                    if let Some(status) = m.move_status {
                        return StatusCode::from_u16(status).unwrap().into_response();
                    }
                    m.move_bodies.push((fid.clone(), body));
                    m.events.push(format!("move:{fid}"));
                    Json(json!({ "id": fid })).into_response()
                },
            ),
        )
        .route(
            "/api/v1/files/:id/download",
            get(|State(st): State<Shared>, Path(fid): Path<String>| async move {
                match st.lock().unwrap().downloads.get(&fid) {
                    Some(body) => Body::from(body.clone()).into_response(),
                    None => not_found(),
                }
            }),
        )
        .with_state(state);
    match log {
        Some(log) => recorded(router, &log),
        None => router,
    }
}

// ── The request recorder (`--parallel` Task 1: the `--parallel 1` golden) ──────────────────────────────────

/// One request a mock answered: what the `--parallel 1` golden compares. The body is its length only: core seals
/// every chunk under a fresh nonce, so no two runs send the same bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggedRequest {
    pub method: String,
    /// The path with every id replaced by what it is (`{session}`, `{file}`, `{id}`); never a query string.
    pub path: String,
    pub status: u16,
    /// When the request arrived, in milliseconds since the log was made (the pause tests read it; a golden
    /// line does not carry it).
    pub at_ms: u64,
    /// `None` for the init: its body carries the sealed name, whose JSON array encoding varies in length with
    /// the nonce's digits (core `seal_name_envelope`), so there is nothing stable to pin.
    pub body_len: Option<u64>,
}

impl LoggedRequest {
    /// One line of a golden file: `METHOD path status body_len` (`-` for a length that is not recorded).
    pub fn line(&self) -> String {
        let body_len = self.body_len.map_or("-".to_string(), |n| n.to_string());
        format!("{} {} {} {body_len}", self.method, self.path, self.status)
    }
}

/// The requests a mock answered, in the order they were answered.
#[derive(Clone)]
pub struct RequestLog {
    entries: Arc<Mutex<Vec<LoggedRequest>>>,
    made: std::time::Instant,
}

impl Default for RequestLog {
    fn default() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            made: std::time::Instant::now(),
        }
    }
}

impl RequestLog {
    pub fn entries(&self) -> Vec<LoggedRequest> {
        self.entries.lock().unwrap().clone()
    }

    /// Milliseconds since the log was made: the clock `at_ms` is on.
    pub fn now_ms(&self) -> u64 {
        self.made.elapsed().as_millis() as u64
    }

    pub fn lines(&self) -> Vec<String> {
        self.entries().iter().map(LoggedRequest::line).collect()
    }
}

/// `path` with every UUID segment replaced by a placeholder for what it names: `{session}` under `uploads` and
/// under a storage bucket, `{file}` under `files`, `{id}` elsewhere.
pub fn placeholders(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    let mut out: Vec<String> = Vec::with_capacity(segments.len());
    for (i, segment) in segments.iter().enumerate() {
        if Uuid::parse_str(segment).is_err() {
            out.push((*segment).to_string());
            continue;
        }
        let under = i.checked_sub(1).map(|j| segments[j]).unwrap_or("");
        out.push(
            match under {
                "uploads" | "bb-test" => "{session}",
                "files" => "{file}",
                _ => "{id}",
            }
            .to_string(),
        );
    }
    out.join("/")
}

/// `router` with every request it answers logged to `log`: method, placeholder path, status and body length.
/// The body is read whole before the handler runs (so a handler that streams sees one frame).
pub fn recorded(router: Router, log: &RequestLog) -> Router {
    router.layer(axum::middleware::from_fn_with_state(log.clone(), record))
}

async fn record(
    State(log): State<RequestLog>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let at_ms = log.now_ms();
    let (parts, body) = request.into_parts();
    let method = parts.method.to_string();
    let path = placeholders(parts.uri.path());
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap_or_default();
    let body_len = (path != "/api/v1/uploads/init").then_some(bytes.len() as u64);
    let response = next
        .run(axum::extract::Request::from_parts(parts, Body::from(bytes)))
        .await;
    log.entries.lock().unwrap().push(LoggedRequest {
        method,
        path,
        status: response.status().as_u16(),
        at_ms,
        body_len,
    });
    response
}

/// Serve the mock on the CURRENT tokio runtime; returns its base URL.
pub async fn spawn(state: Shared) -> String {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Serve the mock on a thread and runtime of its own, for code under test that
/// drives its client with `block_on` (the FUSE filesystem), where the server
/// cannot share the caller's runtime. Returns its base URL.
#[cfg_attr(not(feature = "fuse"), allow(dead_code))] // only the FUSE tests drive a client with `block_on`
pub fn spawn_on_own_thread(state: Shared) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mock server runtime");
        rt.block_on(async move {
            let app = router(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mock server");
            tx.send(format!("http://{}", listener.local_addr().expect("mock addr")))
                .expect("hand back the mock address");
            let _ = axum::serve(listener, app).await;
        });
    });
    rx.recv().expect("mock server address")
}

/// A scratch dir with the resume sidecar (and the upload locks, which follow
/// the same seam) redirected into it. Holding the guard serialises every test
/// that touches the process-global env var.
pub struct Scratch {
    pub dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Scratch {
    pub fn new(tag: &str) -> Self {
        let guard = crate::resume::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("bb-1589-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: serialised by TEST_ENV_LOCK; no other thread reads it
        // concurrently outside that lock.
        unsafe {
            std::env::set_var("BB_PENDING_UPLOADS_PATH", dir.join("pending.json"));
        }
        Self { dir, _guard: guard }
    }

    /// `payload.bin` of `size` bytes in the scratch dir: (path, size, mtime_ns).
    pub fn file(&self, size: usize) -> (std::path::PathBuf, u64, u128) {
        let p = self.dir.join("payload.bin");
        std::fs::write(&p, vec![0x5Au8; size]).unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        (p, meta.len(), mtime)
    }
}

impl Scratch {
    /// `payload.bin` of `size` bytes, sparse (`set_len`, never written): the fixture for a chunk plan over 2 GiB
    /// costs no disk I/O. (path, size, mtime_ns), as [`file`](Self::file).
    pub fn sparse_file(&self, size: u64) -> (std::path::PathBuf, u64, u128) {
        let p = self.dir.join("payload.bin");
        std::fs::File::create(&p).unwrap().set_len(size).unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        (p, meta.len(), mtime)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("BB_PENDING_UPLOADS_PATH");
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
