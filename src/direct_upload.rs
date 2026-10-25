//! The direct upload of one chunk (task 2173): grant, `PUT` to the storage link, commit.
//!
//! The DECISIONS live in `beebeeb_core::direct_transfer` (shared with every client): what an answer
//! means, whether a link may be followed, how many links and how much waiting one chunk gets before
//! the file goes back to the API. This file makes the requests and reports what it saw. It prints no
//! link: the HTTP stack's errors never leave `ApiClient::put_to_storage`.
//!
//! A session's transport never changes after init (spec, "Transport exclusivity"), so the only
//! fallback is for the whole file: [`DirectUploadError::UseApi`] tells the upload driver to abandon
//! the session and init again without the capability token.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use beebeeb_core::direct_transfer as dt;
use bytes::Bytes;

use crate::api::{ApiClient, UploadCall};

/// Why a chunk did not go through the direct route.
#[derive(Debug)]
pub(crate) enum DirectUploadError {
    /// This session cannot finish direct: abandon it and init again without the token.
    UseApi,
    /// The upload session is gone; the driver's session recovery owns it.
    SessionGone(String),
    /// An authorization answer: do not fall back (the API would refuse too).
    Terminal { status: u16, message: String },
    /// The run was interrupted while the chunk waited.
    Interrupted,
}

/// How often a waiting chunk looks at the shutdown flag.
const WAIT_SLICE: Duration = Duration::from_millis(250);

/// Sleep `total`, giving up early (`false`) when the run is being shut down.
async fn wait(total: Duration, shutdown: &AtomicBool) -> bool {
    let mut left = total;
    while !left.is_zero() {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        let slice = left.min(WAIT_SLICE);
        tokio::time::sleep(slice).await;
        left -= slice;
    }
    !shutdown.load(Ordering::Relaxed)
}

/// Send one chunk through grant -> PUT -> commit, following the core policy after every failure.
/// `Ok(())` means the server holds the chunk (committed now, or already: `skipped`).
pub(crate) async fn upload_chunk(
    api: &ApiClient,
    discovery: &dt::Discovery,
    session: &str,
    index: u32,
    data: Bytes,
    shutdown: &Arc<AtomicBool>,
) -> Result<(), DirectUploadError> {
    let size = data.len() as u64;
    // One random seed per chunk: the equal jitter of its backoff, so chunks that fail together do not retry together.
    let mut state = dt::UploadChunkState::with_seed(dt::UploadChunkPolicy::default(), rand::random());
    let mut call = dt::Call::UploadGrant;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Err(DirectUploadError::Interrupted);
        }
        let failure = match call {
            dt::Call::UploadGrant => match api.upload_grant(session, index, size, discovery).await {
                UploadCall::Ok(dt::UploadGrant::Skipped) => return Ok(()),
                UploadCall::Ok(dt::UploadGrant::Link(link)) => {
                    state.link_issued();
                    let status = api.put_to_storage(&link, data.clone()).await;
                    if dt::put_stored(status) {
                        call = dt::Call::UploadCommit;
                        continue;
                    }
                    dt::UploadFailure::Put { status }
                }
                UploadCall::Failed(outcome) => dt::UploadFailure::Grant(outcome),
                UploadCall::SessionGone(message) => return Err(DirectUploadError::SessionGone(message)),
            },
            dt::Call::UploadCommit => match api.upload_commit(session, index, size).await {
                UploadCall::Ok(_) => return Ok(()),
                UploadCall::Failed(outcome) => dt::UploadFailure::Commit(outcome),
                UploadCall::SessionGone(message) => return Err(DirectUploadError::SessionGone(message)),
            },
            // The policy only ever names these two calls.
            _ => return Err(DirectUploadError::UseApi),
        };
        match state.failed(&failure) {
            dt::UploadNext::Retry { after, call: next } => {
                if !wait(after, shutdown).await {
                    return Err(DirectUploadError::Interrupted);
                }
                call = next;
            }
            dt::UploadNext::UseApi => return Err(DirectUploadError::UseApi),
            dt::UploadNext::Terminal(t) => {
                // A dead session reads as it does on the proxy route and on a download: log in again.
                let message = match (t.status, t.error.as_deref()) {
                    (401, _) => crate::api::SESSION_EXPIRED_MESSAGE.to_string(),
                    (status, Some(code)) => format!("{code} (HTTP {status})"),
                    (status, None) => format!("HTTP {status}"),
                };
                return Err(DirectUploadError::Terminal {
                    status: t.status,
                    message,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! The upload driver against a scripted API and a storage server of its own, through the real
    //! `stream_encrypt_upload`: the wire the server really speaks (bodies as in server #331), what the
    //! storage server sees, and what the driver does after each failure.

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
            base_version_number: None,
            replace_pin: None,
            parent_id: None,
            concurrency: CONCURRENCY,
            shutdown: Arc::new(AtomicBool::new(false)),
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

    async fn run(state: &Shared, spec: UploadSpec) -> Result<crate::upload::UploadOutcome, String> {
        run_with(state, spec, &NoopProgress).await
    }

    async fn run_with(
        state: &Shared,
        spec: UploadSpec,
        progress: &dyn ChunkProgress,
    ) -> Result<crate::upload::UploadOutcome, String> {
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

    #[tokio::test]
    async fn a_direct_session_sends_every_chunk_to_storage_and_commits_it() {
        let scratch = Scratch::new("happy");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        assert!(chunks >= 2, "the test needs several chunks, got {chunks}");
        let state = shared(|_| {});
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
        snapshot(&state, |m| {
            assert_eq!(
                m.init_headers,
                vec![Some(CAP.to_string())],
                "init asks for the direct upload"
            );
            assert_eq!(m.grants.len() as u32, chunks);
            assert_eq!(m.storage_puts.len() as u32, chunks);
            assert_eq!(m.commits.len() as u32, chunks);
            assert!(
                m.proxy_puts.is_empty(),
                "no chunk goes through the API: {:?}",
                m.proxy_puts
            );
            assert_eq!(m.completed.len(), 1);
            // Exactly the chunk lengths the grant was asked for were sent to storage and committed.
            let granted: u64 = m.grants.iter().map(|g| g.2).sum();
            let stored: u64 = m.storage_puts.iter().map(|p| p.len as u64).sum();
            let committed: u64 = m.commits.iter().map(|c| c.2).sum();
            assert_eq!(
                (granted, stored, committed),
                (
                    outcome.ciphertext_bytes,
                    outcome.ciphertext_bytes,
                    outcome.ciphertext_bytes
                )
            );
            for put in &m.storage_puts {
                assert_eq!(put.content_type.as_deref(), Some("application/octet-stream"));
                assert!(
                    !put.had_authorization && !put.had_cookie,
                    "storage learns nothing about the account"
                );
            }
            // Every chunk index reached storage, under the session it belongs to (the mock's link path is
            // `<session>/<index>/<n>.bin`).
            let indexes: std::collections::BTreeSet<u32> = m
                .storage_puts
                .iter()
                .map(|p| p.path.split('/').nth(1).unwrap().parse().unwrap())
                .collect();
            assert_eq!(indexes, (0..chunks).collect());
        });
        assert_eq!(*progress.0.lock().unwrap() as u64, outcome.ciphertext_bytes);
        assert_eq!(
            outcome.transport, "direct",
            "the outcome (and `bb push --json`) says so"
        );
        assert_eq!(
            crate::resume::resumable_upload_now(&path),
            None,
            "a finished upload leaves no record"
        );
    }

    // ───────────────────────────── compatibility ─────────────────────────────

    #[tokio::test]
    async fn an_old_server_without_discovery_gets_todays_upload_and_no_capability_header() {
        let scratch = Scratch::new("old");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.offers = Some(Discovery::Unknown));
        let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        assert_eq!(outcome.transport, "api");
        snapshot(&state, |m| {
            assert_eq!(m.init_headers, vec![None], "the init is byte-identical to today's");
            assert!(m.grants.is_empty() && m.storage_puts.is_empty() && m.commits.is_empty());
            assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
            assert_eq!(m.completed.len(), 1);
        });
    }

    #[tokio::test]
    async fn a_server_with_uploads_off_gets_todays_upload() {
        let scratch = Scratch::new("off");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.offers = Some(Discovery::UploadOff));
        let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        assert_eq!(outcome.transport, "api");
        snapshot(&state, |m| {
            assert_eq!(m.init_headers, vec![None]);
            assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
            assert!(m.grants.is_empty());
        });
    }

    #[tokio::test]
    async fn a_server_that_does_not_select_the_direct_upload_gets_proxy_puts() {
        let scratch = Scratch::new("notselected");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.select_direct = false);
        let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        assert_eq!(outcome.transport, "api");
        snapshot(&state, |m| {
            assert_eq!(
                m.init_headers,
                vec![Some(CAP.to_string())],
                "asked, and answered without a transport"
            );
            assert!(m.grants.is_empty() && m.storage_puts.is_empty());
            assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
            assert_eq!(m.completed.len(), 1);
        });
    }

    // ───────────────────────────── retries ─────────────────────────────

    #[tokio::test]
    async fn a_failed_storage_put_is_granted_again_and_the_chunk_still_lands_once() {
        let scratch = Scratch::new("putfail");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        let state = shared(|m| m.storage_script = VecDeque::from([403]));
        run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        snapshot(&state, |m| {
            assert_eq!(
                m.grants.len() as u32,
                chunks + 1,
                "one extra link for the chunk whose PUT failed"
            );
            assert_eq!(m.storage_puts.len() as u32, chunks + 1);
            assert_eq!(m.commits.len() as u32, chunks, "only the PUT that worked was committed");
            assert!(m.proxy_puts.is_empty());
            assert!(m.abandons.is_empty());
        });
    }

    #[tokio::test]
    async fn a_chunk_that_was_not_stored_is_granted_again() {
        let scratch = Scratch::new("notstored");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        let state = shared(|m| {
            m.commit_script = VecDeque::from([Commit::Reply(409, json!({"error": "chunk_not_stored"}), None)]);
        });
        run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        snapshot(&state, |m| {
            assert_eq!(m.grants.len() as u32, chunks + 1);
            assert_eq!(m.storage_puts.len() as u32, chunks + 1);
            assert_eq!(m.commits.len() as u32, chunks + 1);
            assert!(m.abandons.is_empty() && m.proxy_puts.is_empty());
        });
    }

    #[tokio::test]
    async fn a_busy_commit_waits_the_stated_time_and_commits_the_same_chunk_again() {
        let scratch = Scratch::new("busy");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        let state = shared(|m| {
            m.commit_script = VecDeque::from([Commit::Reply(409, json!({"error": "commit_in_progress"}), Some("1"))]);
        });
        let started = Instant::now();
        run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(1),
            "it waited the stated second"
        );
        snapshot(&state, |m| {
            assert_eq!(
                m.grants.len() as u32,
                chunks,
                "no new link: the staged chunk is still there"
            );
            assert_eq!(m.storage_puts.len() as u32, chunks);
            assert_eq!(m.commits.len() as u32, chunks + 1);
        });
    }

    #[tokio::test]
    async fn a_rate_limited_grant_waits_what_the_server_says() {
        let scratch = Scratch::new("429");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        let limited =
            json!({"error": "rate_limited", "message": "Too many requests. Try again in 1 seconds.", "retry_after": 1});
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::Reply(429, limited, Some("1"))]));
        let started = Instant::now();
        run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
        snapshot(&state, |m| {
            assert_eq!(
                m.grants.len() as u32,
                chunks + 1,
                "the refused grant is asked for again"
            );
            assert_eq!(m.storage_puts.len() as u32, chunks);
            assert!(m.abandons.is_empty());
        });
    }

    #[tokio::test]
    async fn a_chunk_that_is_waiting_for_a_rate_limit_stops_when_the_run_is_interrupted() {
        let scratch = Scratch::new("interrupt");
        let (path, _size, _) = scratch.file(FILE_BYTES);
        let limited = json!({"error": "rate_limited", "message": "Too many requests. Try again in 30 seconds.", "retry_after": 30});
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::Reply(429, limited, Some("30"))]));
        let spec = spec(&path, Uuid::new_v4());
        let flag = spec.shutdown.clone();
        let watched = state.clone();
        tokio::spawn(async move {
            // Interrupt once the first grant has been refused and the chunk is waiting (encrypting a chunk
            // takes a machine-dependent time, so a fixed delay could fire before the first request).
            loop {
                let asked = watched.lock().unwrap().grants.len();
                if asked >= 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = run(&state, spec).await;
        // The stated wait is 30 s; an interrupt must not sit it out (Ctrl-C has to feel immediate).
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "waited {:?}",
            started.elapsed()
        );
        assert_eq!(result.unwrap_err(), crate::upload::INTERRUPTED);
        snapshot(&state, |m| {
            assert_eq!(m.grants.len(), 1, "one grant, refused, then the wait");
            assert!(m.storage_puts.is_empty() && m.commits.is_empty() && m.completed.is_empty());
        });
        // The interrupted session stays resumable, and the record says it is a direct one: a proxy PUT
        // would be refused on it.
        let record = crate::resume::resumable_upload_now(&path).expect("an interrupted upload leaves its record");
        assert!(record.direct, "{record:?}");
    }

    // ───────────────────────────── falling back ─────────────────────────────

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

    #[tokio::test]
    async fn direct_unavailable_on_a_grant_sends_the_whole_file_through_the_api() {
        let scratch = Scratch::new("du");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let unavailable = json!({"error": "direct_unavailable", "reason": "disabled"});
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::Reply(409, unavailable, None)]));
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
        assert_fell_back(&state, size, &progress, &outcome).await;
    }

    #[tokio::test]
    async fn a_bare_404_on_the_grant_route_sends_the_whole_file_through_the_api() {
        let scratch = Scratch::new("bare404");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::Bare(404)]));
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
        assert_fell_back(&state, size, &progress, &outcome).await;
    }

    #[tokio::test]
    async fn four_failed_links_for_one_chunk_send_the_whole_file_through_the_api() {
        let scratch = Scratch::new("fourlinks");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.storage_script = VecDeque::from([500, 500, 500, 500]));
        let progress = Counting::default();
        let started = Instant::now();
        let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "1 s + 2 s + 4 s of waiting, not more"
        );
        snapshot(&state, |m| {
            assert_eq!(
                m.grants.len(),
                4,
                "the server issues four links per chunk and the client asks for no fifth"
            );
            assert_eq!(m.storage_puts.len(), 4);
            assert!(m.commits.is_empty());
        });
        assert_fell_back(&state, size, &progress, &outcome).await;
    }

    #[tokio::test]
    async fn a_link_to_a_host_nobody_listed_is_never_followed() {
        let scratch = Scratch::new("evil");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::LinkTo("evil.example")]));
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
        // The first link named a host discovery does not list: refused, nothing sent to it (the mock's storage
        // server saw no PUT of the refused link; the API path that followed sends no byte to storage at all).
        snapshot(&state, |m| {
            assert_eq!(m.storage_puts.len(), 0, "no byte was sent to storage");
        });
        assert_fell_back(&state, size, &progress, &outcome).await;
    }

    // ───────────────────────────── terminal answers ─────────────────────────────

    #[tokio::test]
    async fn an_authorization_answer_on_commit_is_not_papered_over_and_prints_no_link() {
        let scratch = Scratch::new("403");
        let (path, _, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| {
            m.commit_script = VecDeque::from([Commit::Reply(403, json!({"error": "forbidden"}), None)]);
        });
        let error = run(&state, spec(&path, Uuid::new_v4())).await.unwrap_err();
        assert!(error.contains("forbidden"), "{error}");
        assert!(
            !error.contains("X-Amz") && !error.contains("SIGSECRET") && !error.contains("127.0.0.1"),
            "{error}"
        );
        snapshot(&state, |m| {
            assert!(m.abandons.is_empty(), "no fallback after an authorization answer");
            assert!(m.proxy_puts.is_empty());
            assert!(m.completed.is_empty());
        });
    }

    // ───────────────────────────── resume ─────────────────────────────

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
            file_id,
            sid,
            beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
            size,
            mtime,
            direct,
        );
        (file_id, sid)
    }

    #[tokio::test]
    async fn a_resumed_direct_session_continues_direct_and_skips_what_the_server_holds() {
        let scratch = Scratch::new("resume-direct");
        let (path, size, mtime) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        let state = shared(|_| {});
        let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[0]);
        run(&state, spec(&path, file_id)).await.unwrap();
        snapshot(&state, |m| {
            assert!(m.inits.is_empty(), "the recorded session is reused, not re-opened");
            assert_eq!(m.grants.len() as u32, chunks, "every chunk asks for a grant");
            assert_eq!(
                m.storage_puts.len() as u32,
                chunks - 1,
                "chunk 0 is skipped: nothing is sent for it"
            );
            assert_eq!(m.commits.len() as u32, chunks - 1);
            assert!(m.proxy_puts.is_empty() && m.abandons.is_empty());
            assert_eq!(m.completed.len(), 1);
        });
    }

    #[tokio::test]
    async fn a_resumed_api_session_stays_on_the_api_even_when_the_server_now_offers_direct() {
        let scratch = Scratch::new("resume-api");
        let (path, size, mtime) = scratch.file(FILE_BYTES);
        let state = shared(|_| {});
        let (file_id, _) = resumed_session(&state, &path, size, mtime, false, &[]);
        run(&state, spec(&path, file_id)).await.unwrap();
        snapshot(&state, |m| {
            assert!(m.inits.is_empty());
            assert_eq!(
                m.discovery_hits, 0,
                "an API session has no use for the capabilities, so the route is not asked"
            );
            assert!(
                m.grants.is_empty() && m.storage_puts.is_empty(),
                "an API session takes no grant"
            );
            assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
            assert_eq!(m.completed.len(), 1);
        });
    }

    #[tokio::test]
    async fn a_resumed_direct_session_is_given_up_when_direct_is_no_longer_offered() {
        let scratch = Scratch::new("resume-off");
        let (path, size, mtime) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.offers = Some(Discovery::UploadOff));
        let (file_id, old_sid) = resumed_session(&state, &path, size, mtime, true, &[0]);
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, file_id), &progress).await.unwrap();
        snapshot(&state, |m| {
            assert_eq!(
                m.abandons,
                vec![file_id.to_string()],
                "the direct session is ended, not PUT to"
            );
            assert_eq!(m.inits.len(), 1, "a fresh API session is opened");
            assert_eq!(m.init_headers, vec![None]);
            assert!(m.proxy_puts.iter().all(|(sid, _, _)| *sid != old_sid.to_string()));
            assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
            assert_eq!(m.completed.len(), 1);
        });
        assert_eq!(*progress.0.lock().unwrap() as u64, outcome.ciphertext_bytes);
    }

    /// A recorded direct session the server has since swept: the first grant (or commit) is a JSON 404.
    /// The driver's recovery for a resumed session starts the file again, and the new session is direct.
    fn swept_resumed_session(state: &Shared, path: &std::path::Path, size: u64, mtime: u128) -> Uuid {
        let (file_id, sid) = resumed_session(state, path, size, mtime, true, &[]);
        state.lock().unwrap().live.remove(&sid.to_string());
        file_id
    }

    #[tokio::test]
    async fn a_resumed_direct_session_the_server_swept_is_started_again_as_a_direct_one() {
        let scratch = Scratch::new("swept-grant");
        let (path, size, mtime) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        let state = shared(|_| {});
        let file_id = swept_resumed_session(&state, &path, size, mtime);
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, file_id), &progress).await.unwrap();
        assert_eq!(outcome.transport, "direct");
        snapshot(&state, |m| {
            assert_eq!(m.inits.len(), 1, "one new session for the file");
            assert_eq!(
                m.init_headers,
                vec![Some(CAP.to_string())],
                "and it asks for the direct upload again"
            );
            assert_eq!(
                m.grants.len() as u32,
                chunks + 1,
                "the refused grant, then one per chunk"
            );
            assert_eq!(m.storage_puts.len() as u32, chunks);
            assert_eq!(m.commits.len() as u32, chunks);
            assert!(m.proxy_puts.is_empty() && m.abandons.is_empty());
            assert_eq!(m.completed.len(), 1);
        });
        assert_eq!(
            *progress.0.lock().unwrap() as u64,
            outcome.ciphertext_bytes,
            "the file is counted once"
        );
    }

    #[tokio::test]
    async fn a_commit_that_finds_the_session_gone_starts_the_file_again_too() {
        let scratch = Scratch::new("swept-commit");
        let (path, size, mtime) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        let gone = json!({"error": "not found"});
        let state = shared(|m| m.commit_script = VecDeque::from([Commit::Reply(404, gone, None)]));
        let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[]);
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, file_id), &progress).await.unwrap();
        assert_eq!(outcome.transport, "direct");
        snapshot(&state, |m| {
            assert_eq!(
                m.inits.len(),
                1,
                "a gone session is not committed to again: the file starts over"
            );
            assert_eq!(m.storage_puts.len() as u32, chunks + 1, "chunk 0 went to storage twice");
            assert_eq!(
                m.commits.len() as u32,
                chunks + 1,
                "the refused commit, then one per chunk"
            );
            assert_eq!(m.completed.len(), 1);
        });
        assert_eq!(
            *progress.0.lock().unwrap() as u64,
            outcome.ciphertext_bytes,
            "the file is counted once"
        );
    }

    #[tokio::test]
    async fn the_servers_expired_session_400_is_a_gone_session_not_a_reason_to_leave_the_direct_upload() {
        let scratch = Scratch::new("expired");
        let (path, size, mtime) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        // The server's text for a session past its lease (`routes/direct_transfer.rs:579`).
        let expired = json!({"error": "upload session is not writable: expired"});
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::Reply(400, expired, None)]));
        let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[]);
        let outcome = run(&state, spec(&path, file_id)).await.unwrap();
        assert_eq!(outcome.transport, "direct", "a new direct session, not the API");
        snapshot(&state, |m| {
            assert_eq!(m.inits.len(), 1);
            assert_eq!(m.init_headers, vec![Some(CAP.to_string())]);
            assert!(m.abandons.is_empty() && m.proxy_puts.is_empty());
            assert_eq!(m.storage_puts.len() as u32, chunks);
            assert_eq!(m.completed.len(), 1);
        });
    }

    #[tokio::test]
    async fn a_direct_session_given_up_after_some_chunks_took_their_progress_back() {
        let scratch = Scratch::new("midgiveup");
        let (path, size, _) = scratch.file(FILE_BYTES);
        // Chunk 0 is stored and committed; chunk 1's four links all fail at storage.
        let state = shared(|m| m.storage_script = VecDeque::from([200, 500, 500, 500, 500]));
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
        snapshot(&state, |m| {
            assert_eq!(m.commits.len(), 1, "chunk 0 was committed before the file was given up");
            assert_eq!(m.storage_puts.len(), 5);
        });
        // The file is counted once: what the dead session confirmed came back off before the retry.
        assert_fell_back(&state, size, &progress, &outcome).await;
    }

    #[tokio::test]
    async fn a_direct_session_that_was_given_up_leaves_no_resume_record_even_if_the_new_init_fails() {
        let scratch = Scratch::new("giveup-record");
        let (path, _size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| {
            m.grant_script = VecDeque::from([Grant::Bare(404)]);
            m.init_script = VecDeque::from([None, Some(403)]);
        });
        let result = run(&state, spec(&path, Uuid::new_v4())).await;
        assert!(result.is_err(), "the second init is refused");
        snapshot(&state, |m| {
            assert_eq!(m.abandons.len(), 1, "the direct session was ended first")
        });
        assert_eq!(
            crate::resume::resumable_upload_now(&path),
            None,
            "a record that points at the abandoned direct session would be resumed into a dead end"
        );
    }

    #[tokio::test]
    async fn a_transport_the_cli_does_not_know_is_todays_upload() {
        let scratch = Scratch::new("unknown-transport");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| {
            // A server that answers with a transport this CLI has never heard of; the session itself is the
            // plain one (the mock only accepts proxy PUTs on a session that was not opened direct).
            m.select_direct = false;
            m.init_transport = Some("object-upload-v2");
        });
        let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        assert_eq!(outcome.transport, "api");
        snapshot(&state, |m| {
            assert!(m.grants.is_empty() && m.storage_puts.is_empty() && m.commits.is_empty());
            assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
            assert!(m.abandons.is_empty());
        });
    }

    #[tokio::test]
    async fn a_commit_held_for_longer_than_the_failure_cap_is_waited_out_not_given_up() {
        let scratch = Scratch::new("held");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let chunks = chunk_count(size);
        // After a copy that failed the server keeps the chunk's commit lock for minutes. Nine one-second holds are more
        // than the eight failures the policy rides out: a hold is not a failure, so the chunk waits and commits.
        let held = json!({"error": "commit_in_progress"});
        let state = shared(|m| m.commit_script = (0..9).map(|_| Commit::Reply(409, held.clone(), Some("1"))).collect());
        let started = Instant::now();
        let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(9),
            "the nine holds were waited out"
        );
        assert_eq!(outcome.transport, "direct", "and the session stayed direct");
        snapshot(&state, |m| {
            assert_eq!(
                m.commits.len() as u32,
                chunks + 9,
                "nine refused commits, then one per chunk"
            );
            assert_eq!(
                m.storage_puts.len() as u32,
                chunks,
                "the staged chunk was not sent again"
            );
            assert!(m.abandons.is_empty() && m.proxy_puts.is_empty());
            assert_eq!(m.completed.len(), 1);
        });
    }

    // ── review round (cli#105): a resume is not given up on a lookup that nobody answered ──

    #[tokio::test]
    async fn a_resumed_direct_session_is_kept_when_the_servers_capabilities_cannot_be_read() {
        let scratch = Scratch::new("resume-unknown");
        let (path, size, mtime) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.offers = Some(Discovery::Fails));
        let (file_id, sid) = resumed_session(&state, &path, size, mtime, true, &[0]);
        let err = run(&state, spec(&path, file_id)).await.unwrap_err();
        assert!(err.contains("capabilities") && err.contains("--transport api"), "{err}");
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
    }

    #[tokio::test]
    async fn a_resumed_direct_session_is_given_up_when_the_server_says_it_has_no_direct_transport() {
        let scratch = Scratch::new("resume-nodirect");
        let (path, size, mtime) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.offers = Some(Discovery::NoDirect));
        let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[0]);
        let progress = Counting::default();
        let outcome = run_with(&state, spec(&path, file_id), &progress).await.unwrap();
        assert_eq!(outcome.transport, "api");
        snapshot(&state, |m| {
            assert_eq!(m.abandons.len(), 1, "a definite no ends the direct session");
            assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
            assert_eq!(m.completed.len(), 1);
        });
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
        err
    }

    #[tokio::test]
    async fn a_resumed_direct_session_is_kept_and_asks_for_a_login_when_the_session_is_not_valid() {
        // A 401: exactly the words every other expired session uses, and not the advice to leave the direct route
        // (that would end the session and lose the chunks it holds).
        let err = kept_resume_error("resume-401", Discovery::Unauthorized, false).await;
        assert_eq!(err, crate::api::SESSION_EXPIRED_MESSAGE);
        assert!(!err.contains("--transport"), "{err}");
    }

    #[tokio::test]
    async fn a_resumed_direct_session_is_kept_and_reports_the_status_when_the_lookup_is_forbidden() {
        // A 403 is not an expired session (every other 403 in the CLI says its status): the real status, and no
        // advice to log in again.
        let err = kept_resume_error("resume-403", Discovery::Forbidden, false).await;
        assert!(
            err.contains("forbidden, HTTP 403") && err.contains("progress is kept"),
            "{err}"
        );
        assert!(
            !err.contains("--transport") && !err.contains(crate::api::SESSION_EXPIRED_MESSAGE),
            "{err}"
        );
        assert!(!err.contains("bb login"), "a 403 is not an expired session: {err}");
    }

    #[tokio::test]
    async fn a_resumed_direct_session_is_kept_and_says_not_logged_in_when_there_is_no_session_at_all() {
        let err = kept_resume_error("resume-logged-out", Discovery::Offers, true).await;
        assert_eq!(err, crate::api::NOT_LOGGED_IN_MESSAGE);
        assert!(err.contains("Not logged in"), "{err}");
    }

    // ── review round (cli#105): direct broken server-wide costs one file, not every file ──

    #[tokio::test]
    async fn after_one_file_gave_up_the_direct_upload_the_next_one_does_not_try() {
        let scratch = Scratch::new("second-file");
        let (first, size, _) = scratch.file(FILE_BYTES);
        let second = scratch.dir.join("second.bin");
        std::fs::copy(&first, &second).unwrap();
        // The first grant is a bare 404: a node without the route. That file goes back to the API.
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::Bare(404)]));
        let base = spawn(state.clone()).await;
        let api = crate::api::ApiClient::new_for_test(base);
        let key = Arc::new(MasterKey::from_bytes([9u8; 32]));
        let one = stream_encrypt_upload(&api, key.clone(), spec(&first, Uuid::new_v4()), &NoopProgress)
            .await
            .unwrap();
        assert_eq!(one.transport, "api");
        let two = stream_encrypt_upload(&api, key, spec(&second, Uuid::new_v4()), &NoopProgress)
            .await
            .unwrap();
        assert_eq!(two.transport, "api");
        snapshot(&state, |m| {
            assert_eq!(
                m.init_headers,
                vec![Some(CAP.to_string()), None, None],
                "the first file asks, is refused and starts again; the second file does not ask"
            );
            assert_eq!(
                m.abandons.len(),
                1,
                "one abandon (and one FileDeleted to the other devices), not one per file"
            );
            assert_eq!(m.proxy_puts.len() as u32, 2 * chunk_count(size));
            assert_eq!(m.completed.len(), 2);
        });
    }

    // ── review round 3 (cli#105): the file's own bit and the remembered failure are two mechanisms ──

    #[tokio::test]
    async fn a_file_that_gave_up_the_direct_upload_does_not_ask_again_even_when_nothing_is_remembered() {
        // A hold of zero never holds, so only the per-file bit can keep this file's re-init off the direct route
        // (the remembered failure is a wall-clock-free memory with an expiry, and must not be what bounds the loop).
        let scratch = Scratch::new("own-bit");
        let (path, size, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::Bare(404)]));
        let base = spawn(state.clone()).await;
        let api = crate::api::ApiClient::new_for_test_with_failure_hold(base, std::time::Duration::ZERO);
        let key = Arc::new(MasterKey::from_bytes([9u8; 32]));
        let outcome = stream_encrypt_upload(&api, key, spec(&path, Uuid::new_v4()), &NoopProgress)
            .await
            .unwrap();
        assert_eq!(outcome.transport, "api");
        snapshot(&state, |m| {
            assert_eq!(
                m.init_headers,
                vec![Some(CAP.to_string()), None],
                "asked once, refused, started again WITHOUT asking"
            );
            assert_eq!(m.abandons.len(), 1);
            assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
        });
    }

    #[tokio::test]
    async fn a_direct_upload_that_was_given_up_is_tried_again_once_the_hold_has_passed() {
        let scratch = Scratch::new("hold-over");
        let (first, size, _) = scratch.file(FILE_BYTES);
        let second = scratch.dir.join("second.bin");
        std::fs::copy(&first, &second).unwrap();
        let state = shared(|m| m.grant_script = VecDeque::from([Grant::Bare(404)]));
        let base = spawn(state.clone()).await;
        // The hold is over by the time the second file starts.
        let api = crate::api::ApiClient::new_for_test_with_failure_hold(base, std::time::Duration::ZERO);
        let key = Arc::new(MasterKey::from_bytes([9u8; 32]));
        let one = stream_encrypt_upload(&api, key.clone(), spec(&first, Uuid::new_v4()), &NoopProgress)
            .await
            .unwrap();
        assert_eq!(one.transport, "api");
        let two = stream_encrypt_upload(&api, key, spec(&second, Uuid::new_v4()), &NoopProgress)
            .await
            .unwrap();
        assert_eq!(
            two.transport, "direct",
            "the server is asked again, and this time it works"
        );
        snapshot(&state, |m| {
            assert_eq!(
                m.init_headers,
                vec![Some(CAP.to_string()), None, Some(CAP.to_string())],
                "the second file asks again once nothing is held"
            );
            assert_eq!(
                m.proxy_puts.len() as u32,
                chunk_count(size),
                "only the first file went through the API"
            );
        });
    }

    #[tokio::test]
    async fn a_dead_session_on_a_direct_commit_says_to_log_in_again_like_the_proxy_route() {
        let scratch = Scratch::new("401");
        let (path, _, _) = scratch.file(FILE_BYTES);
        let state = shared(|m| {
            m.commit_script = VecDeque::from([Commit::Reply(401, json!({"error": "unauthorized"}), None)]);
        });
        let error = run(&state, spec(&path, Uuid::new_v4())).await.unwrap_err();
        assert!(error.contains(crate::api::SESSION_EXPIRED_MESSAGE), "{error}");
        snapshot(&state, |m| assert!(m.abandons.is_empty() && m.completed.is_empty()));
    }
}
