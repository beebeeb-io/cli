mod compatibility;
mod direct_session;
mod fallback;
mod hold;
mod live_stats;
mod resume;
mod retry;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use beebeeb_core::kdf::MasterKey;
use futures_util::StreamExt;
use serde_json::json;
use uuid::Uuid;

use crate::upload::{ChunkProgress, FileProgress, NoopProgress, UploadSpec, stream_encrypt_upload};

const CAP: &str = "object-upload-v1";

/// How the mock answers `GET /api/v1/transfer/capabilities`.
#[derive(Clone, Copy)]
enum Discovery {
    Offers,
    /// An old server: the route is unknown (empty 404).
    Unknown,
    /// A new server with uploads off.
    UploadOff,
    /// The capabilities route is down (a 503): nobody answers.
    Fails,
    /// The server says, in JSON, that it has no direct transport (a JSON 404).
    NoDirect,
    /// The session is not valid: the route answers 401.
    Unauthorized,
    /// The route answers 403.
    Forbidden,
}

/// One scripted answer of the grant route.
#[derive(Clone)]
enum Grant {
    /// A link to the mock's storage server (the normal case).
    Link,
    /// A link to this host, which discovery does not list.
    LinkTo(&'static str),
    /// A status and JSON body, with an optional `Retry-After`.
    Reply(u16, serde_json::Value, Option<&'static str>),
    /// An empty-bodied status (a node without the route).
    Bare(u16),
}

/// One scripted answer of the commit route (`None` = the default, success).
#[derive(Clone)]
enum Commit {
    Reply(u16, serde_json::Value, Option<&'static str>),
}

#[derive(Default)]
struct Mock {
    storage: String,
    discovery_hits: usize,
    offers: Option<Discovery>,
    /// Does init answer `transport: object-upload-v1` when asked for it?
    select_direct: bool,
    grant_script: VecDeque<Grant>,
    storage_script: VecDeque<u16>,
    commit_script: VecDeque<Commit>,
    /// One entry per init: `Some(status)` refuses it with that status, `None` answers it.
    init_script: VecDeque<Option<u16>>,
    /// What the init answers as `transport`, instead of what the capability header selected.
    init_transport: Option<&'static str>,
    // observations
    init_headers: Vec<Option<String>>,
    inits: Vec<String>,
    grants: Vec<(String, u32, u64)>,
    storage_puts: Vec<StoragePut>,
    commits: Vec<(String, u32, u64)>,
    proxy_puts: Vec<(String, u32, usize)>,
    completed: Vec<String>,
    abandons: Vec<String>,
    // sessions: id -> (file id, direct, committed chunk indexes)
    sessions: HashMap<String, (String, bool, HashSet<u32>)>,
    live: HashSet<String>,
}

struct StoragePut {
    path: String,
    len: usize,
    content_type: Option<String>,
    had_authorization: bool,
    had_cookie: bool,
}

type Shared = Arc<Mutex<Mock>>;

fn bare(status: u16) -> Response {
    StatusCode::from_u16(status).unwrap().into_response()
}

fn reply(status: u16, body: serde_json::Value, retry_after: Option<&str>) -> Response {
    let mut response = (StatusCode::from_u16(status).unwrap(), Json(body)).into_response();
    if let Some(v) = retry_after {
        response.headers_mut().insert("retry-after", v.parse().unwrap());
    }
    response
}

async fn spawn(state: Shared) -> String {
    // The storage server first: its address goes into the links and into discovery.
    let storage_app = Router::new()
        .route(
            "/bb-test/*rest",
            put(
                |State(st): State<Shared>, Path(rest): Path<String>, headers: HeaderMap, body: Body| async move {
                    let mut stream = body.into_data_stream();
                    let mut len = 0usize;
                    while let Some(frame) = stream.next().await {
                        len += frame.map(|b| b.len()).unwrap_or(0);
                    }
                    let mut m = st.lock().unwrap();
                    m.storage_puts.push(StoragePut {
                        path: rest,
                        len,
                        content_type: headers
                            .get("content-type")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from),
                        had_authorization: headers.contains_key("authorization"),
                        had_cookie: headers.contains_key("cookie"),
                    });
                    let status = m.storage_script.pop_front().unwrap_or(200);
                    bare(status)
                },
            ),
        )
        .with_state(state.clone());
    let storage_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let storage_addr = storage_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(storage_listener, storage_app).await;
    });
    state.lock().unwrap().storage = format!("127.0.0.1:{}", storage_addr.port());

    let app = Router::new()
        .route(
            "/api/v1/transfer/capabilities",
            get(|State(st): State<Shared>| async move {
                let mut m = st.lock().unwrap();
                m.discovery_hits += 1;
                let host = m.storage.clone();
                match m.offers.unwrap_or(Discovery::Offers) {
                    Discovery::Unknown => bare(404),
                    Discovery::Fails => bare(503),
                    Discovery::NoDirect => reply(404, json!({"error": "not found"}), None),
                    Discovery::Unauthorized => reply(401, json!({"error": "unauthorized"}), None),
                    Discovery::Forbidden => reply(403, json!({"error": "forbidden"}), None),
                    Discovery::UploadOff => reply(
                        200,
                        json!({"download": [], "upload": [], "storage_hosts": [], "storage_buckets": []}),
                        None,
                    ),
                    Discovery::Offers => reply(
                        200,
                        json!({
                            "download": [],
                            "upload": [CAP],
                            "storage_hosts": [host],
                            "storage_buckets": [{"host": host, "bucket": "bb-test"}],
                            "max_window": 8,
                            "url_ttl_seconds": 300,
                        }),
                        None,
                    ),
                }
            }),
        )
        .route(
            "/api/v1/uploads/init",
            post(
                |State(st): State<Shared>, headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                    let file_id = body
                        .get("file_id")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    let asked = headers
                        .get("x-beebeeb-capabilities")
                        .and_then(|v| v.to_str().ok())
                        .map(String::from);
                    let mut m = st.lock().unwrap();
                    m.init_headers.push(asked.clone());
                    m.inits.push(file_id.clone());
                    if let Some(Some(status)) = m.init_script.pop_front() {
                        return reply(status, json!({"error": "forbidden"}), None);
                    }
                    let direct = asked.as_deref() == Some(CAP) && m.select_direct;
                    let sid = Uuid::new_v4().to_string();
                    m.sessions.insert(sid.clone(), (file_id.clone(), direct, HashSet::new()));
                    m.live.insert(sid.clone());
                    let mut answer = json!({
                        "upload_session_id": sid,
                        "file_id": file_id,
                        "chunk_size_bytes": body.get("chunk_size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                        "chunk_count": body.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0),
                    });
                    if let Some(transport) = m.init_transport {
                        answer["transport"] = json!(transport);
                    } else if direct {
                        answer["transport"] = json!(CAP);
                    }
                    (StatusCode::CREATED, Json(answer)).into_response()
                },
            ),
        )
        .route(
            "/api/v1/uploads/:session/chunks/:idx",
            put(
                |State(st): State<Shared>, Path((sid, idx)): Path<(String, u32)>, body: Body| async move {
                    let mut stream = body.into_data_stream();
                    let mut n = 0usize;
                    while let Some(frame) = stream.next().await {
                        n += frame.map(|b| b.len()).unwrap_or(0);
                    }
                    let mut m = st.lock().unwrap();
                    m.proxy_puts.push((sid.clone(), idx, n));
                    match m.sessions.get(&sid) {
                        None => reply(404, json!({"error": "not found"}), None),
                        Some((_, true, _)) => reply(409, json!({"error": "transport_mismatch"}), None),
                        Some(_) => reply(200, json!({"index": idx, "size": n, "skipped": false}), None),
                    }
                },
            ),
        )
        .route(
            "/api/v1/uploads/:session/chunks/:idx/grant",
            post(
                |State(st): State<Shared>,
                 Path((sid, idx)): Path<(String, u32)>,
                 Json(body): Json<serde_json::Value>| async move {
                    let size = body["size"].as_u64().unwrap_or(0);
                    let mut m = st.lock().unwrap();
                    m.grants.push((sid.clone(), idx, size));
                    let Some((_, direct, committed)) = m.sessions.get(&sid).cloned() else {
                        return reply(404, json!({"error": "not found"}), None);
                    };
                    if !m.live.contains(&sid) {
                        return reply(404, json!({"error": "not found"}), None);
                    }
                    if !direct {
                        return reply(409, json!({"error": "transport_mismatch"}), None);
                    }
                    if committed.contains(&idx) {
                        return reply(200, json!({"transport": CAP, "index": idx, "skipped": true}), None);
                    }
                    let n = m.grants.len();
                    let host = m.storage.clone();
                    let link = |host: &str| {
                        reply(
                            200,
                            json!({
                                "transport": CAP,
                                "index": idx,
                                "method": "PUT",
                                "url": format!("http://{host}/bb-test/{sid}/{idx}/{n}.bin?X-Amz-Signature=SIGSECRET"),
                                "headers": {"content-type": "application/octet-stream"},
                                "expires_at": "2099-01-01T00:00:00Z",
                            }),
                            None,
                        )
                    };
                    match m.grant_script.pop_front().unwrap_or(Grant::Link) {
                        Grant::Link => link(&host),
                        Grant::LinkTo(other) => link(other),
                        Grant::Reply(status, body, retry_after) => reply(status, body, retry_after),
                        Grant::Bare(status) => bare(status),
                    }
                },
            ),
        )
        .route(
            "/api/v1/uploads/:session/chunks/:idx/commit",
            post(
                |State(st): State<Shared>,
                 Path((sid, idx)): Path<(String, u32)>,
                 Json(body): Json<serde_json::Value>| async move {
                    let size = body["size"].as_u64().unwrap_or(0);
                    let mut m = st.lock().unwrap();
                    m.commits.push((sid.clone(), idx, size));
                    if let Some(Commit::Reply(status, body, retry_after)) = m.commit_script.pop_front() {
                        return reply(status, body, retry_after);
                    }
                    if let Some((_, _, committed)) = m.sessions.get_mut(&sid) {
                        committed.insert(idx);
                    }
                    reply(
                        200,
                        json!({"index": idx, "size": size, "skipped": false, "object_key": "tenants/t/files/f/versions/v/x.bin"}),
                        None,
                    )
                },
            ),
        )
        .route(
            "/api/v1/files/:id/upload/abandon",
            post(|State(st): State<Shared>, Path(fid): Path<String>| async move {
                let mut m = st.lock().unwrap();
                m.abandons.push(fid.clone());
                let ended: Vec<String> = m
                    .sessions
                    .iter()
                    .filter(|(_, (f, _, _))| *f == fid)
                    .map(|(s, _)| s.clone())
                    .collect();
                for sid in ended {
                    m.live.remove(&sid);
                }
                Json(json!({"id": fid, "status": "abandoned"})).into_response()
            }),
        )
        .route(
            "/api/v1/uploads/:session/complete",
            post(|State(st): State<Shared>, Path(sid): Path<String>| async move {
                let mut m = st.lock().unwrap();
                if !m.live.contains(&sid) {
                    return reply(404, json!({"error": "not found"}), None);
                }
                m.completed.push(sid.clone());
                let fid = m.sessions.get(&sid).map(|s| s.0.clone()).unwrap_or_default();
                Json(json!({"id": fid, "size_bytes": 0, "chunk_count": 0})).into_response()
            }),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// A scratch dir with the resume sidecar redirected into it; the guard serialises every test that
/// touches the process-global variable.
struct Scratch {
    dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let guard = crate::resume::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("bb-2173u-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: serialised by TEST_ENV_LOCK.
        unsafe {
            std::env::set_var("BB_PENDING_UPLOADS_PATH", dir.join("pending.json"));
        }
        Self { dir, _guard: guard }
    }

    /// A file of `size` bytes, its size and mtime (what the resume record keys on).
    fn file(&self, size: usize) -> (std::path::PathBuf, u64, u128) {
        let p = self.dir.join("payload.bin");
        let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, bytes).unwrap();
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

/// 3 chunks of ~4 MiB: concurrency 32 gives the small chunk ladder.
const FILE_BYTES: usize = 10 * 1024 * 1024;
const CONCURRENCY: u32 = 32;

fn chunk_count(size: u64) -> u32 {
    beebeeb_types::plan_chunks_concurrent(size, beebeeb_types::ChunkProfile::Cli, CONCURRENCY).chunk_count as u32
}

fn spec(path: &std::path::Path, file_id: Uuid) -> UploadSpec {
    UploadSpec {
        path: path.to_path_buf(),
        file_name: "payload.bin".to_string(),
        file_id,
        resume_tag: None,
        base_version_number: None,
        replace_pin: None,
        replace_file_key: None,
        parent_id: None,
        concurrency: CONCURRENCY,
        shutdown: Arc::new(AtomicBool::new(false)),
        read_row: None,
    }
}

fn shared(setup: impl FnOnce(&mut Mock)) -> Shared {
    let mut mock = Mock {
        select_direct: true,
        ..Mock::default()
    };
    setup(&mut mock);
    Arc::new(Mutex::new(mock))
}

async fn run(state: &Shared, spec: UploadSpec) -> Result<crate::upload::UploadOutcome, crate::upload::UploadError> {
    run_with(state, spec, &NoopProgress).await
}

async fn run_with(
    state: &Shared,
    spec: UploadSpec,
    progress: &dyn ChunkProgress,
) -> Result<crate::upload::UploadOutcome, crate::upload::UploadError> {
    let base = spawn(state.clone()).await;
    let api = crate::api::ApiClient::new_for_test(base);
    let master_key = Arc::new(MasterKey::from_bytes([9u8; 32]));
    stream_encrypt_upload(&api, master_key, spec, progress).await
}

/// Sums what `chunk_confirmed` reported, net of rollbacks.
#[derive(Default)]
struct Counting(Arc<Mutex<i64>>);

struct CountingFile(Arc<Mutex<i64>>);

impl FileProgress for CountingFile {
    fn chunk_confirmed(&self, n: u64) {
        *self.0.lock().unwrap() += n as i64;
    }
    fn finish(self: Box<Self>, _: bool) {}
}

impl ChunkProgress for Counting {
    fn begin_file(&self, _: &str, _: u64) -> Box<dyn FileProgress> {
        Box::new(CountingFile(self.0.clone()))
    }
    fn rollback(&self, n: u64) {
        *self.0.lock().unwrap() -= n as i64;
    }
}

fn snapshot<T>(state: &Shared, f: impl FnOnce(&Mock) -> T) -> T {
    f(&state.lock().unwrap())
}

// ───────────────────────────── the happy path ─────────────────────────────

/// The whole file goes the API way: the session is abandoned once, init is repeated WITHOUT the
/// token, every chunk goes to the proxy route, and the progress counts the file once.
async fn assert_fell_back(state: &Shared, size: u64, progress: &Counting, outcome: &crate::upload::UploadOutcome) {
    snapshot(state, |m| {
        assert_eq!(m.abandons.len(), 1, "the direct session was ended once");
        assert_eq!(m.init_headers.len(), 2);
        assert_eq!(m.init_headers[0].as_deref(), Some(CAP));
        assert_eq!(
            m.init_headers[1], None,
            "the second init does not ask for the direct upload"
        );
        assert_eq!(
            m.proxy_puts.len() as u32,
            chunk_count(size),
            "every chunk goes through the API"
        );
        assert_eq!(m.completed.len(), 1);
    });
    assert_eq!(
        *progress.0.lock().unwrap() as u64,
        outcome.ciphertext_bytes,
        "the progress counts the file once, not twice"
    );
    assert_eq!(
        outcome.transport, "api",
        "the route that finished the file is the one reported"
    );
}

/// A session this run did not open: in the mock, and in the resume sidecar.
fn resumed_session(
    state: &Shared,
    path: &std::path::Path,
    size: u64,
    mtime: u128,
    direct: bool,
    committed: &[u32],
) -> (Uuid, Uuid) {
    let file_id = Uuid::new_v4();
    let sid = Uuid::new_v4();
    {
        let mut m = state.lock().unwrap();
        m.sessions.insert(
            sid.to_string(),
            (file_id.to_string(), direct, committed.iter().copied().collect()),
        );
        m.live.insert(sid.to_string());
    }
    crate::resume::record_with_transport(
        path,
        crate::item_id::Proposed::Minted(file_id),
        None,
        sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
        direct,
    );
    (file_id, sid)
}

/// A recorded direct session the server has since swept: the first grant (or commit) is a JSON 404.
/// The driver's recovery for a resumed session starts the file again, and the new session is direct.
fn swept_resumed_session(state: &Shared, path: &std::path::Path, size: u64, mtime: u128) -> Uuid {
    let (file_id, sid) = resumed_session(state, path, size, mtime, true, &[]);
    state.lock().unwrap().live.remove(&sid.to_string());
    file_id
}

/// A resumed direct session, run once with `api` and the capabilities `offers`; returns the error and checks that
/// nothing was ended, sent or recorded away.
async fn kept_resume_error(tag: &str, offers: Discovery, logged_out: bool) -> String {
    let scratch = Scratch::new(tag);
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.offers = Some(offers));
    let (file_id, sid) = resumed_session(&state, &path, size, mtime, true, &[0]);
    let base = spawn(state.clone()).await;
    let api = if logged_out {
        crate::api::ApiClient::new_for_test_logged_out(base)
    } else {
        crate::api::ApiClient::new_for_test(base)
    };
    let key = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let err = stream_encrypt_upload(&api, key, spec(&path, file_id), &NoopProgress)
        .await
        .unwrap_err();
    snapshot(&state, |m| {
        assert!(
            m.abandons.is_empty(),
            "the session is NOT ended: it holds committed chunks"
        );
        assert!(m.inits.is_empty() && m.grants.is_empty() && m.proxy_puts.is_empty());
        assert!(m.completed.is_empty());
    });
    let record = crate::resume::resumable_upload_now(&path).expect("the record is kept");
    assert!(record.direct && record.session_id == sid, "{record:?}");
    err.to_string()
}
