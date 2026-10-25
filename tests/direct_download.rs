//! `bb pull` over the direct S3 transport (task 2173, contract v1).
//!
//! Drives the real `bb` binary against two in-process mocks: an API (axum) that
//! speaks discovery, download grants, the pinned per-chunk route and today's
//! `/download`, and a STORAGE server written by hand on a raw TCP socket, so a
//! test can send exactly the bytes it wants: a short body, a wrong
//! `Content-Length`, a redirect, a tampered chunk. Every run has a scratch
//! `HOME` (never the developer's session) and `BB_NO_UPDATE=1`.
//!
//! The file is 21 chunks, so the transfer needs three windows of 8 grants.
//! Every scenario asserts the output is byte-identical to the plaintext, which
//! transport carried it (`bb pull --json` reports it), and what the mocks saw.
//! No scenario may print a signed link: `assert_clean` greps stdout and stderr
//! for the signature of every link the API issued.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use beebeeb_core::chunk_stream::ChunkEncryptor;
use beebeeb_core::kdf::MasterKey;
use serde_json::{Value, json};

const FILE_ID: &str = "7b0c1a52-3f4e-4d2a-9c61-2f8e5a1d0b37";
const VERSION: &str = "4d5e6f70-8192-4a3b-9c4d-5e6f708192a3";
const BUCKET: &str = "bb-test";
const STRIDE: u64 = 1024;
const PLAIN_LEN: usize = 20 * 1024 + 100; // 21 chunks, the last one 100 bytes
const SIG: &str = "SIGNATURE-DO-NOT-PRINT";

static SEQ: AtomicUsize = AtomicUsize::new(0);

// ─────────────────────────────── fixture ───────────────────────────────

struct Fixture {
    file_id: String,
    stride: u64,
    /// The account master key as `bb` reads it from its config (base64).
    key_b64: String,
    plaintext: Vec<u8>,
    frames: Vec<Vec<u8>>,
    name_encrypted: String,
}

/// 21 chunks of v1 frames under the all-zero test key.
fn fixture() -> Fixture {
    let plaintext: Vec<u8> = (0..PLAIN_LEN).map(|i| (i.wrapping_mul(31) % 251) as u8).collect();
    let mk = MasterKey::from_bytes([0u8; 32]);
    let mut enc = ChunkEncryptor::for_push_with_chunk_size(&mk, FILE_ID, plaintext.len() as u64, STRIDE).unwrap();
    let frames: Vec<Vec<u8>> = plaintext
        .chunks(STRIDE as usize)
        .map(|c| enc.push_chunk(c).unwrap().data)
        .collect();
    enc.finish().unwrap();
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(&mk, FILE_ID, "multi.bin", None).unwrap();
    Fixture {
        file_id: FILE_ID.to_string(),
        stride: STRIDE,
        key_b64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
        plaintext,
        frames,
        name_encrypted,
    }
}

/// The checked-in core v2 vector (4 chunks, position-bound AEAD) with its LAST
/// frame dropped: the manifest and the metadata both say 3 chunks, every frame
/// authenticates, but the stream never reaches the frame marked last.
fn fixture_v2_missing_last_frame() -> Fixture {
    let v: Value = serde_json::from_str(include_str!("fixtures/core_1908_vectors.json")).unwrap();
    let f = &v["v2_file"];
    let wire = hex_decode(f["wire_hex"].as_str().unwrap());
    let plain = hex_decode(f["plaintext_hex"].as_str().unwrap());
    let stride = f["chunk_size"].as_u64().unwrap();
    let frame_len = (stride + 28) as usize;
    let frames: Vec<Vec<u8>> = wire.chunks(frame_len).take(3).map(<[u8]>::to_vec).collect();
    assert!(
        frames.iter().all(|fr| fr.len() == frame_len),
        "the first three frames are full"
    );
    let key = hex_decode(f["master_key_hex"].as_str().unwrap());
    Fixture {
        file_id: f["file_id"].as_str().unwrap().to_string(),
        stride,
        key_b64: base64_std(&key),
        plaintext: plain[..3 * stride as usize].to_vec(),
        frames,
        name_encrypted: f["name_encrypted"].as_str().unwrap().to_string(),
    }
}

fn hex_decode(h: &str) -> Vec<u8> {
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn base64_std(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ─────────────────────────────── mock behaviour ───────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Discovery {
    Offer,
    Bare404,
    Json404,
    Nothing,
    ServerError,
}

#[derive(Clone, Copy, PartialEq)]
enum Grants {
    Normal,
    DirectUnavailable,
    VersionGoneAfterFirstWindow,
    /// The first window is served, every later one is refused as ineligible.
    UnavailableAfterFirstWindow,
    /// The first window is served, every later one is a 200 whose body is not the contract's JSON.
    GarbageAfterFirstWindow,
    /// The very first grant request answers 409 `replace_in_progress` with `Retry-After: 1`.
    ReplaceInProgressOnce,
    /// Every grant request answers 409 `replace_in_progress` with no `Retry-After`: another device
    /// is still uploading the file.
    ReplaceInProgressNoRetryAfter,
    /// The first window after the first is refused as `version_unavailable` (a new version was
    /// published mid-download); every window after that is served normally.
    VersionGoneOnce,
    Bare404,
    Forbidden,
    /// Links name a host the discovery did not list.
    UnlistedHost,
    /// The manifest claims a different chunk count than the file's metadata.
    WrongChunkCount,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Fault {
    /// The first `n` requests for the chunk answer 403 (an expired link).
    Forbidden(usize),
    /// Always a flipped bit in the body.
    Tamper,
    /// Declares the full length, sends half, closes.
    ShortBody,
    /// Declares a `Content-Length` that is not the grant's `length`.
    WrongLength,
    /// 302 to a path on the API mock.
    Redirect,
    /// Chunked transfer-encoding: no `Content-Length` at all.
    Chunked,
    /// `206 Partial Content` with a correct `Content-Length`: only 200 is accepted.
    Status206,
}

struct Config {
    discovery: Discovery,
    grants: Grants,
    storage: HashMap<u32, Fault>,
    /// The pinned per-chunk API route serves a flipped bit for these chunks.
    api_chunk_tamper: Vec<u32>,
    /// The pinned per-chunk API route serves one byte too few for these chunks.
    api_chunk_short: Vec<u32>,
    /// The pinned per-chunk API route answers 409 `version_unavailable` for these chunks.
    api_chunk_gone: Vec<u32>,
}

#[derive(Default)]
struct Log {
    discovery_hits: usize,
    discovery_cap_headers: Vec<Option<String>>,
    grant_bodies: Vec<Value>,
    grant_cap_headers: Vec<Option<String>>,
    api_download_hits: usize,
    api_chunk_hits: Vec<u32>,
    leak_hits: usize,
    /// (chunk index, lower-cased header names) per storage request.
    storage: Vec<(u32, Vec<String>)>,
    /// Signatures of every link issued (so output can be checked for them).
    issued_links: Vec<String>,
}

struct Mock {
    fx: Fixture,
    cfg: Mutex<Config>,
    log: Mutex<Log>,
    storage_addr: SocketAddr,
    serial: AtomicUsize,
    replace_served: std::sync::atomic::AtomicBool,
    gone_served: std::sync::atomic::AtomicBool,
}

// ─────────────────────────────── storage mock (raw HTTP) ───────────────────────────────

fn spawn_storage(
    mock_cell: Arc<Mutex<Option<Arc<Mock>>>>,
    api_addr_cell: Arc<Mutex<Option<SocketAddr>>>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let attempts: Arc<Mutex<HashMap<u32, usize>>> = Arc::default();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mock = mock_cell.lock().unwrap().clone();
            let api_addr = *api_addr_cell.lock().unwrap();
            let attempts = attempts.clone();
            std::thread::spawn(move || {
                if let (Some(mock), Some(api_addr)) = (mock, api_addr) {
                    let _ = serve_storage(&mut stream, &mock, api_addr, &attempts);
                }
            });
        }
    });
    addr
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, Vec<(String, String)>)> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        buf.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Ok((request_line, headers))
}

fn serve_storage(
    stream: &mut TcpStream,
    mock: &Mock,
    api_addr: SocketAddr,
    attempts: &Mutex<HashMap<u32, usize>>,
) -> std::io::Result<()> {
    let (request_line, headers) = read_request(stream)?;
    // GET /bb-test/<file>/<index>?query HTTP/1.1
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let path = target.split('?').next().unwrap_or("");
    let index: u32 = path.rsplit('/').next().and_then(|s| s.parse().ok()).unwrap_or(u32::MAX);
    let names: Vec<String> = headers.iter().map(|(k, _)| k.clone()).collect();
    mock.log.lock().unwrap().storage.push((index, names));
    let Some(frame) = mock.fx.frames.get(index as usize) else {
        return stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let n = {
        let mut a = attempts.lock().unwrap();
        let c = a.entry(index).or_insert(0);
        *c += 1;
        *c
    };
    let fault = mock.cfg.lock().unwrap().storage.get(&index).copied();
    let mut body = frame.clone();
    let ok_head = |len: usize| format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n");
    match fault {
        Some(Fault::Forbidden(times)) if n <= times => {
            let xml = "<Error><Code>AccessDenied</Code><Message>Request has expired</Message></Error>";
            write!(
                stream,
                "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
                xml.len()
            )
        }
        Some(Fault::Tamper) => {
            body[20] ^= 0x01;
            stream.write_all(ok_head(body.len()).as_bytes())?;
            stream.write_all(&body)
        }
        Some(Fault::ShortBody) => {
            stream.write_all(ok_head(body.len()).as_bytes())?;
            stream.write_all(&body[..body.len() / 2])
        }
        Some(Fault::WrongLength) => {
            stream.write_all(ok_head(body.len() + 7).as_bytes())?;
            stream.write_all(&body)
        }
        Some(Fault::Redirect) => write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: http://{api_addr}/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        Some(Fault::Status206) => {
            write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {len}\r\nContent-Range: bytes 0-{last}/{len}\r\nConnection: close\r\n\r\n",
                len = body.len(),
                last = body.len() - 1
            )?;
            stream.write_all(&body)
        }
        Some(Fault::Chunked) => {
            stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")?;
            write!(stream, "{:x}\r\n", body.len())?;
            stream.write_all(&body)?;
            stream.write_all(b"\r\n0\r\n\r\n")
        }
        _ => {
            stream.write_all(ok_head(body.len()).as_bytes())?;
            stream.write_all(&body)
        }
    }
}

// ─────────────────────────────── API mock (axum) ───────────────────────────────

fn rfc3339_in(secs: i64) -> String {
    chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + secs, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn cap_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-beebeeb-capabilities")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn json_error(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({ "error": error }))).into_response()
}

async fn meta(State(m): State<Arc<Mock>>, AxPath(id): AxPath<String>) -> Response {
    if id != m.fx.file_id {
        return json_error(StatusCode::NOT_FOUND, "not found");
    }
    let wrong_count = m.cfg.lock().unwrap().grants == Grants::WrongChunkCount;
    let count = m.fx.frames.len() + usize::from(wrong_count);
    Json(json!({
        "id": m.fx.file_id,
        "name_encrypted": m.fx.name_encrypted,
        "is_folder": false,
        "chunk_count": count,
        "size_bytes": m.fx.plaintext.len(),
    }))
    .into_response()
}

const SYNC_FOLDER_ID: &str = "22222222-2222-4222-8222-222222222222";

/// `GET /api/v1/files`: the account root holds the folder `SyncUp`; it holds the fixture file.
async fn list(State(m): State<Arc<Mock>>, Query(q): Query<HashMap<String, String>>) -> Response {
    match q.get("parent_id").map(String::as_str) {
        None => Json(json!({ "files": [
            { "id": SYNC_FOLDER_ID, "is_folder": true, "name_encrypted": "SyncUp" }
        ] }))
        .into_response(),
        Some(SYNC_FOLDER_ID) => Json(json!({ "files": [{
            "id": m.fx.file_id,
            "name_encrypted": m.fx.name_encrypted,
            "is_folder": false,
            "size_bytes": m.fx.plaintext.len(),
            "chunk_count": m.fx.frames.len(),
            "updated_at": "2020-01-01T00:00:00+00:00",
        }] }))
        .into_response(),
        Some(_) => Json(json!({ "files": [] })).into_response(),
    }
}

async fn download(State(m): State<Arc<Mock>>, AxPath(_id): AxPath<String>) -> Response {
    m.log.lock().unwrap().api_download_hits += 1;
    let body: Vec<u8> = m.fx.frames.concat();
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::HeaderName::from_static("x-chunk-count"),
                m.fx.frames.len().to_string(),
            ),
            (
                header::HeaderName::from_static("x-original-size"),
                m.fx.plaintext.len().to_string(),
            ),
            (header::HeaderName::from_static("x-chunk-size"), m.fx.stride.to_string()),
        ],
        body,
    )
        .into_response()
}

async fn h_discovery(State(m): State<Arc<Mock>>, headers: HeaderMap) -> Response {
    {
        let mut log = m.log.lock().unwrap();
        log.discovery_hits += 1;
        log.discovery_cap_headers.push(cap_header(&headers));
    }
    let mode = m.cfg.lock().unwrap().discovery;
    let host = m.storage_addr.to_string();
    match mode {
        Discovery::Offer => Json(json!({
            "download": ["object-download-v1"],
            "upload": [],
            "storage_hosts": [host],
            "storage_buckets": [{ "host": host, "bucket": BUCKET }],
            "max_window": 32,
            "url_ttl_seconds": 300,
        }))
        .into_response(),
        Discovery::Nothing => Json(json!({
            "download": [], "upload": [], "storage_hosts": [], "storage_buckets": [],
            "max_window": 32, "url_ttl_seconds": 300,
        }))
        .into_response(),
        Discovery::Bare404 => StatusCode::NOT_FOUND.into_response(),
        Discovery::Json404 => json_error(StatusCode::NOT_FOUND, "not found"),
        Discovery::ServerError => json_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
}

async fn h_grants(
    State(m): State<Arc<Mock>>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    {
        let mut log = m.log.lock().unwrap();
        log.grant_bodies.push(body.clone());
        log.grant_cap_headers.push(cap_header(&headers));
    }
    if id != m.fx.file_id {
        return json_error(StatusCode::NOT_FOUND, "not found");
    }
    let mode = m.cfg.lock().unwrap().grants;
    let start = body["start_index"].as_u64().unwrap_or(0) as usize;
    let count = body["count"].as_u64().unwrap_or(0) as usize;
    let pinned = body["object_version_id"].as_str();
    match mode {
        Grants::Bare404 => return StatusCode::NOT_FOUND.into_response(),
        Grants::Forbidden => return json_error(StatusCode::FORBIDDEN, "forbidden"),
        Grants::DirectUnavailable => {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "direct_unavailable", "reason": "disabled" })),
            )
                .into_response();
        }
        Grants::VersionGoneAfterFirstWindow if start > 0 => {
            return json_error(StatusCode::CONFLICT, "version_unavailable");
        }
        Grants::GarbageAfterFirstWindow if start > 0 => {
            return Json(json!({ "nope": true })).into_response();
        }
        Grants::ReplaceInProgressOnce if !m.replace_served.swap(true, Ordering::SeqCst) => {
            return (
                StatusCode::CONFLICT,
                [(header::RETRY_AFTER, "1")],
                Json(json!({ "error": "replace_in_progress" })),
            )
                .into_response();
        }
        Grants::ReplaceInProgressNoRetryAfter => {
            return json_error(StatusCode::CONFLICT, "replace_in_progress");
        }
        Grants::VersionGoneOnce if start > 0 && !m.gone_served.swap(true, Ordering::SeqCst) => {
            return json_error(StatusCode::CONFLICT, "version_unavailable");
        }
        Grants::UnavailableAfterFirstWindow if start > 0 => {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "direct_unavailable", "reason": "budget_unavailable" })),
            )
                .into_response();
        }
        _ => {}
    }
    if cap_header(&headers).as_deref() != Some("object-download-v1") {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "direct_unavailable", "reason": "capability_missing" })),
        )
            .into_response();
    }
    if pinned.is_some_and(|p| p != VERSION) || (pinned.is_none() && start != 0) {
        return json_error(StatusCode::CONFLICT, "version_unavailable");
    }
    let n = m.fx.frames.len();
    let end = (start + count).min(n);
    let host = if mode == Grants::UnlistedHost {
        "127.0.0.1:9".to_string()
    } else {
        m.storage_addr.to_string()
    };
    let mut links = Vec::new();
    let mut list = Vec::new();
    for i in start..end {
        let serial = m.serial.fetch_add(1, Ordering::SeqCst);
        let sig = format!("{SIG}-{i}-{serial}");
        links.push(sig.clone());
        list.push(json!({
            "index": i,
            "method": "GET",
            "url": format!("http://{host}/{BUCKET}/{}/{i}?X-Amz-Signature={sig}&X-Amz-Expires=300", m.fx.file_id),
            "length": m.fx.frames[i].len(),
            "expires_at": rfc3339_in(300),
        }));
    }
    m.log.lock().unwrap().issued_links.extend(links);
    let enc: usize = m.fx.frames.iter().map(Vec::len).sum();
    let chunk_count = n; // the manifest is honest; `WrongChunkCount` only skews the file's metadata
    Json(json!({
        "transport": "object-download-v1",
        "file_id": m.fx.file_id,
        "object_version_id": VERSION,
        "chunk_count": chunk_count,
        "chunk_size": m.fx.stride,
        "encrypted_size": enc,
        "original_size": m.fx.plaintext.len(),
        "grants": list,
        "next_index": if end < n { json!(end) } else { Value::Null },
    }))
    .into_response()
}

async fn api_chunk(
    State(m): State<Arc<Mock>>,
    AxPath((id, index)): AxPath<(String, u32)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if id != m.fx.file_id {
        return json_error(StatusCode::NOT_FOUND, "not found");
    }
    m.log.lock().unwrap().api_chunk_hits.push(index);
    if q.get("object_version_id").map(String::as_str) != Some(VERSION) {
        return json_error(StatusCode::CONFLICT, "version_unavailable");
    }
    let Some(frame) = m.fx.frames.get(index as usize) else {
        return json_error(StatusCode::BAD_REQUEST, "out of range");
    };
    let mut body = frame.clone();
    let cfg = m.cfg.lock().unwrap();
    if cfg.api_chunk_gone.contains(&index) {
        return json_error(StatusCode::CONFLICT, "version_unavailable");
    }
    if cfg.api_chunk_tamper.contains(&index) {
        body[20] ^= 0x01;
    }
    if cfg.api_chunk_short.contains(&index) {
        body.pop();
    }
    ([(header::CONTENT_TYPE, "application/octet-stream")], body).into_response()
}

async fn leak(State(m): State<Arc<Mock>>) -> Response {
    m.log.lock().unwrap().leak_hits += 1;
    StatusCode::OK.into_response()
}

// ─────────────────────────────── harness ───────────────────────────────

struct Env {
    mock: Arc<Mock>,
    api_url: String,
    home: PathBuf,
    cwd: PathBuf,
}

fn setup(discovery: Discovery, grants: Grants, storage: &[(u32, Fault)], api_chunk_tamper: &[u32]) -> Env {
    setup_with(fixture(), discovery, grants, storage, api_chunk_tamper)
}

fn setup_with(
    fx: Fixture,
    discovery: Discovery,
    grants: Grants,
    storage: &[(u32, Fault)],
    api_chunk_tamper: &[u32],
) -> Env {
    let mock_cell: Arc<Mutex<Option<Arc<Mock>>>> = Arc::default();
    let api_addr_cell: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
    let storage_addr = spawn_storage(mock_cell.clone(), api_addr_cell.clone());
    let mock = Arc::new(Mock {
        fx,
        cfg: Mutex::new(Config {
            discovery,
            grants,
            storage: storage.iter().copied().collect(),
            api_chunk_tamper: api_chunk_tamper.to_vec(),
            api_chunk_short: Vec::new(),
            api_chunk_gone: Vec::new(),
        }),
        log: Mutex::default(),
        storage_addr,
        serial: AtomicUsize::new(0),
        replace_served: std::sync::atomic::AtomicBool::new(false),
        gone_served: std::sync::atomic::AtomicBool::new(false),
    });
    *mock_cell.lock().unwrap() = Some(mock.clone());

    let (tx, rx) = std::sync::mpsc::channel();
    let app_mock = mock.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let app = Router::new()
                .route("/api/v1/transfer/capabilities", get(h_discovery))
                .route("/api/v1/files", get(list))
                .route("/api/v1/files/:id", get(meta))
                .route("/api/v1/files/:id/download", get(download))
                .route("/api/v1/files/:id/download-grants", post(h_grants))
                .route("/api/v1/files/:id/chunks/:index", get(api_chunk))
                .route("/leak", get(leak))
                .fallback(|| async { json_error(StatusCode::NOT_FOUND, "not found") })
                .with_state(app_mock);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = axum::serve(listener, app).await;
        });
    });
    let api_addr = rx.recv().unwrap();
    *api_addr_cell.lock().unwrap() = Some(api_addr);

    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("direct-download-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    let cwd = dir.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let api_url = format!("http://{api_addr}");
    let config = json!({
        "api_url": api_url,
        "session_token": "test-session-token",
        "email": "direct@beebeeb.io",
        "master_key": mock.fx.key_b64,
    });
    for d in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    }
    Env {
        mock,
        api_url,
        home,
        cwd,
    }
}

fn bb(env: &Env, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.args(args)
        .current_dir(&env.cwd)
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", env.home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("run bb")
}

fn describe(out: &Output) -> String {
    format!(
        "rc={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Nothing a run printed may contain a signed link or any part of its signature.
fn assert_clean(env: &Env, out: &Output) {
    let all = describe(out);
    // With BB_TRANSPORT_TRACE on, the planner must never have stalled or erred: a safety net
    // that quietly falls back to the API would otherwise hide a missing planner call.
    for needle in ["planner stalled", "planner error"] {
        assert!(!all.contains(needle), "the planner lost track of a request:\n{all}");
    }
    for needle in ["X-Amz-Signature", "x-amz-signature", SIG, "X-Amz-Expires"] {
        assert!(!all.contains(needle), "output leaks {needle}:\n{all}");
    }
    for link in env.mock.log.lock().unwrap().issued_links.iter() {
        assert!(!all.contains(link.as_str()), "output leaks an issued link:\n{all}");
    }
}

/// `bb pull <id> --json -o out.bin`: returns the output and the JSON it printed.
fn pull(env: &Env, extra: &[&str], extra_env: &[(&str, &str)]) -> (Output, Value) {
    let id = env.mock.fx.file_id.clone();
    let mut args = vec!["pull", id.as_str(), "--json", "-o", "out.bin"];
    args.extend_from_slice(extra);
    let out = bb(env, &args, extra_env);
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out, json)
}

fn assert_identical(env: &Env, out: &Output) {
    let got = std::fs::read(env.cwd.join("out.bin")).unwrap_or_default();
    assert!(
        got == env.mock.fx.plaintext,
        "output differs from the plaintext\n{}",
        describe(out)
    );
    assert!(
        !env.cwd.join("out.bbtmp").exists() && !env.cwd.join("out.bin.bbtmp").exists(),
        "a temp file was left behind"
    );
}

fn assert_nothing_written(env: &Env, out: &Output) {
    assert!(
        !env.cwd.join("out.bin").exists(),
        "a file was published\n{}",
        describe(out)
    );
    let leftovers: Vec<_> = std::fs::read_dir(&env.cwd)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        leftovers.is_empty(),
        "files left in the output directory: {leftovers:?}"
    );
}

fn storage_hits(env: &Env, index: u32) -> usize {
    env.mock
        .log
        .lock()
        .unwrap()
        .storage
        .iter()
        .filter(|(i, _)| *i == index)
        .count()
}

// ─────────────────────────────── scenarios ───────────────────────────────

#[test]
fn direct_download_is_byte_identical_and_never_touches_the_api_byte_route() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    assert_clean(&env, &out);

    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.api_download_hits, 0, "the API streamed bytes");
    assert!(log.api_chunk_hits.is_empty());
    assert_eq!(log.discovery_hits, 1);
    assert_eq!(
        log.discovery_cap_headers,
        vec![None],
        "the capability header is for grants only"
    );
    // Three windows: 0..8, 8..16, 16..21; the first unpinned, the rest pinned.
    let starts: Vec<(u64, u64)> = log
        .grant_bodies
        .iter()
        .map(|b| (b["start_index"].as_u64().unwrap(), b["count"].as_u64().unwrap()))
        .collect();
    assert_eq!(starts, vec![(0, 8), (8, 8), (16, 5)]);
    assert_eq!(log.grant_bodies[0]["object_version_id"], Value::Null);
    assert_eq!(log.grant_bodies[1]["object_version_id"], VERSION);
    assert!(
        log.grant_cap_headers
            .iter()
            .all(|h| h.as_deref() == Some("object-download-v1"))
    );
    // 21 storage requests, one per chunk, none of them carrying the account.
    assert_eq!(log.storage.len(), 21);
    for (index, names) in &log.storage {
        for banned in ["authorization", "cookie", "referer"] {
            assert!(
                !names.iter().any(|n| n == banned),
                "chunk {index} sent {banned}: {names:?}"
            );
        }
        assert!(
            !names.iter().any(|n| n.starts_with("x-beebeeb")),
            "chunk {index} sent a Beebeeb header: {names:?}"
        );
    }
}

#[test]
fn forcing_the_api_transport_by_flag_and_by_environment() {
    for (args, envs) in [
        (vec!["--transport", "api"], vec![]),
        (vec![], vec![("BB_TRANSPORT", "api")]),
    ] {
        let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
        let (out, json) = pull(&env, &args, &envs);
        assert!(out.status.success(), "{}", describe(&out));
        assert_identical(&env, &out);
        assert_eq!(json["transport"], "api", "{}", describe(&out));
        let log = env.mock.log.lock().unwrap();
        assert_eq!(log.discovery_hits, 0, "a forced API run must not even ask discovery");
        assert!(log.grant_bodies.is_empty() && log.storage.is_empty());
        assert_eq!(log.api_download_hits, 1);
    }
}

#[test]
fn the_flag_beats_the_environment_and_auto_means_direct() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &["--transport", "auto"], &[("BB_TRANSPORT", "api")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
}

#[test]
fn servers_without_direct_transport_are_served_over_the_api() {
    // An old server's unknown route, a node without the route, a server error, a
    // server that offers nothing: all end on the API, byte-identical, no error text.
    for mode in [
        Discovery::Bare404,
        Discovery::Json404,
        Discovery::ServerError,
        Discovery::Nothing,
    ] {
        let env = setup(mode, Grants::Normal, &[], &[]);
        let (out, json) = pull(&env, &[], &[]);
        assert!(out.status.success(), "{}", describe(&out));
        assert_identical(&env, &out);
        assert_eq!(json["transport"], "api", "{}", describe(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).trim().is_empty(),
            "{}",
            describe(&out)
        );
        let log = env.mock.log.lock().unwrap();
        assert_eq!(log.discovery_hits, 1, "one discovery request for one file");
        assert!(log.grant_bodies.is_empty());
        assert_eq!(log.api_download_hits, 1);
    }
}

#[test]
fn a_refusal_to_serve_this_file_directly_falls_back_to_the_api() {
    for mode in [
        Grants::DirectUnavailable,
        Grants::Bare404,
        Grants::UnlistedHost,
        Grants::WrongChunkCount,
    ] {
        let env = setup(Discovery::Offer, mode, &[], &[]);
        let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
        if mode == Grants::WrongChunkCount {
            // The metadata says 22 chunks, the manifest 21: the direct path steps aside
            // without fetching anything, and the API path's own checks decide the file
            // (its stream has 21 frames, so it refuses). Nothing is published either way.
            assert!(!out.status.success(), "{}", describe(&out));
            assert_nothing_written(&env, &out);
            assert_clean(&env, &out);
            assert_eq!(
                env.mock.log.lock().unwrap().storage.len(),
                0,
                "nothing fetched from storage"
            );
            assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 1);
            let _ = json;
            continue;
        }
        assert!(out.status.success(), "{}", describe(&out));
        assert_identical(&env, &out);
        assert_eq!(json["transport"], "api", "{}", describe(&out));
        assert_clean(&env, &out);
        let log = env.mock.log.lock().unwrap();
        assert_eq!(log.api_download_hits, 1);
        assert_eq!(log.storage.len(), 0, "no storage request for a refused file");
    }
}

#[test]
fn an_authorization_refusal_is_terminal_and_never_falls_back() {
    let env = setup(Discovery::Offer, Grants::Forbidden, &[], &[]);
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("403"),
        "{}",
        describe(&out)
    );
    assert_eq!(
        env.mock.log.lock().unwrap().api_download_hits,
        0,
        "403 must not be routed around"
    );
}

#[test]
fn a_version_that_goes_away_mid_transfer_restarts_and_publishes_nothing() {
    let env = setup(Discovery::Offer, Grants::VersionGoneAfterFirstWindow, &[], &[]);
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("changed again"),
        "{}",
        describe(&out)
    );
    assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 0, "never mix versions");
}

#[test]
fn a_version_published_mid_download_is_read_again_from_fresh_metadata_once() {
    let env = setup(Discovery::Offer, Grants::VersionGoneOnce, &[], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("starting again"),
        "{}",
        describe(&out)
    );
    let starts: Vec<u64> = env
        .mock
        .log
        .lock()
        .unwrap()
        .grant_bodies
        .iter()
        .map(|b| b["start_index"].as_u64().unwrap())
        .collect();
    // The first attempt: window 0, window 8 (refused: the version is gone). The second starts over.
    assert_eq!(starts, vec![0, 8, 0, 8, 16], "{starts:?}");
}

#[test]
fn a_file_another_device_is_still_uploading_goes_straight_to_the_api() {
    // 409 replace_in_progress with no Retry-After: not a moment's hold. The API path answers it at
    // once; five waits of five seconds per file would make every sync pass slower.
    let env = setup(Discovery::Offer, Grants::ReplaceInProgressNoRetryAfter, &[], &[]);
    let started = std::time::Instant::now();
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(started.elapsed() < std::time::Duration::from_secs(4), "it did not wait");
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "api", "{}", describe(&out));
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.grant_bodies.len(), 1, "one grant request, then the API");
    assert_eq!(log.api_download_hits, 1);
}

#[test]
fn the_trace_is_off_when_the_variable_says_zero_and_an_unknown_transport_warns_once() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, _) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "0")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("transport:"),
        "{}",
        describe(&out)
    );
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, _) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("transport:"),
        "{}",
        describe(&out)
    );

    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT", "bogus")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        json["transport"],
        "direct",
        "an unknown value means auto: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.matches("ignoring BB_TRANSPORT").count(), 1, "{}", describe(&out));
}

// ─────────────────────────────── bb sync --once ───────────────────────────────

/// `bb sync --once <cwd>/syncdir /SyncUp`: the remote folder holds the fixture file.
fn sync_once(env: &Env, extra_env: &[(&str, &str)]) -> (Output, PathBuf) {
    let dir = env.cwd.join("syncdir");
    std::fs::create_dir_all(&dir).unwrap();
    let out = bb(
        env,
        &[
            "--api",
            &env.api_url,
            "sync",
            "--once",
            dir.to_str().unwrap(),
            "/SyncUp",
        ],
        extra_env,
    );
    (out, dir)
}

#[test]
fn a_sync_pass_downloads_over_the_direct_transport() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, dir) = sync_once(&env, &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        std::fs::read(dir.join("multi.bin")).unwrap_or_default(),
        env.mock.fx.plaintext
    );
    let log = env.mock.log.lock().unwrap();
    assert!(
        !log.grant_bodies.is_empty() && log.api_download_hits == 0,
        "it went direct"
    );
}

/// Run `bb sync <cwd>/syncdir /SyncUp` the way a daemon does (no `--once`) until it reaches watch
/// mode, then stop it. Returns (reached watch mode, stdout, stderr). The child is the one process
/// this starts, and it is the only one stopped.
fn sync_until_watching(env: &Env) -> (bool, String, String) {
    use std::io::{BufRead, BufReader};
    let dir = env.cwd.join("syncdir");
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(["--api", &env.api_url, "sync", dir.to_str().unwrap(), "/SyncUp"])
        .current_dir(&env.cwd)
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", env.home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bb sync");
    let out = Arc::new(Mutex::new(String::new()));
    let err = Arc::new(Mutex::new(String::new()));
    let mut readers = Vec::new();
    for (pipe, sink) in [
        (
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            out.clone(),
        ),
        (
            Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
            err.clone(),
        ),
    ] {
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let mut sink = sink.lock().unwrap();
                sink.push_str(&line);
                sink.push('\n');
            }
        }));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut watching = false;
    while std::time::Instant::now() < deadline {
        if out.lock().unwrap().contains("synced \u{00b7} watching") {
            watching = true;
            break;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    for r in readers {
        let _ = r.join();
    }
    let (o, e) = (out.lock().unwrap().clone(), err.lock().unwrap().clone());
    (watching, o, e)
}

/// Stops whatever `bb sync --daemon` started in this scratch HOME, even when a test fails.
struct StopDaemons<'a>(&'a Env);

impl Drop for StopDaemons<'_> {
    fn drop(&mut self) {
        let _ = bb(self.0, &["--api", &self.0.api_url, "sync", "--stop-all"], &[]);
    }
}

#[test]
fn a_forced_api_transport_reaches_the_sync_daemon() {
    // `--transport api` is a flag of this invocation; the detached child is started with its own
    // command line, so the flag has to be handed on (BB_TRANSPORT travels in the environment).
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let _stop = StopDaemons(&env);
    let dir = env.cwd.join("syncdir");
    std::fs::create_dir_all(&dir).unwrap();
    let out = bb(
        &env,
        &[
            "--transport",
            "api",
            "--api",
            &env.api_url,
            "sync",
            "--daemon",
            dir.to_str().unwrap(),
            "/SyncUp",
        ],
        &[],
    );
    assert!(out.status.success(), "{}", describe(&out));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline
        && std::fs::read(dir.join("multi.bin")).unwrap_or_default() != env.mock.fx.plaintext
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(
        std::fs::read(dir.join("multi.bin")).unwrap_or_default(),
        env.mock.fx.plaintext,
        "the daemon downloaded the file"
    );
    let log = env.mock.log.lock().unwrap();
    assert_eq!(
        log.discovery_hits, 0,
        "a daemon started with --transport api must not ask discovery"
    );
    assert!(log.grant_bodies.is_empty() && log.storage.is_empty());
    assert_eq!(log.api_download_hits, 1);
}

fn sync_left_nothing(dir: &Path) {
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != ".bb-sync.json")
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}

#[test]
fn a_sync_pass_survives_a_file_that_never_authenticates() {
    // The chunk fails AEAD from storage and again from the pinned API route: the direct driver's
    // `Corrupt`. The API path treats a file it cannot decrypt as "unreadable, retry next pass"
    // (task 1916); the direct path must too. If this ended the pass, launchd and systemd would
    // restart the daemon every 30 s for ever and the files after it would never arrive.
    let env = setup(Discovery::Offer, Grants::Normal, &[(2, Fault::Tamper)], &[2]);
    let (watching, stdout, stderr) = sync_until_watching(&env);
    assert!(
        watching,
        "the daemon must reach watch mode\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let dir = env.cwd.join("syncdir");
    assert!(
        !dir.join("multi.bin").exists(),
        "nothing is published for a file that did not authenticate"
    );
    sync_left_nothing(&dir);
    assert!(
        stderr.contains("did not authenticate") && stderr.contains("next pass"),
        "{stderr}"
    );
    assert!(!stderr.contains("sync incomplete"), "{stderr}");

    // The one-shot form reports the incomplete pass with its own exit code (3), not as an error (1).
    let env = setup(Discovery::Offer, Grants::Normal, &[(2, Fault::Tamper)], &[2]);
    let (out, dir) = sync_once(&env, &[]);
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 failed"),
        "{}",
        describe(&out)
    );
    assert!(!dir.join("multi.bin").exists());
    assert_clean(&env, &out);
}

#[test]
fn a_sync_pass_defers_a_file_whose_version_changed_under_it() {
    let env = setup(Discovery::Offer, Grants::VersionGoneAfterFirstWindow, &[], &[]);
    let (watching, stdout, stderr) = sync_until_watching(&env);
    assert!(
        watching,
        "the daemon must reach watch mode\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let dir = env.cwd.join("syncdir");
    assert!(!dir.join("multi.bin").exists());
    sync_left_nothing(&dir);
    assert!(stderr.contains("changed") && stderr.contains("next pass"), "{stderr}");
    assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 0, "never mix versions");

    // Deferred is not a failure: a one-shot pass that only deferred it is complete.
    let env = setup(Discovery::Offer, Grants::VersionGoneAfterFirstWindow, &[], &[]);
    let (out, dir) = sync_once(&env, &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(!dir.join("multi.bin").exists());
}

#[test]
fn an_expired_link_is_renewed_once_and_the_file_stays_direct() {
    let env = setup(Discovery::Offer, Grants::Normal, &[(3, Fault::Forbidden(1))], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    assert_clean(&env, &out);
    assert_eq!(storage_hits(&env, 3), 2, "one failure, one retry on a fresh link");
    let log = env.mock.log.lock().unwrap();
    assert!(
        log.grant_bodies
            .iter()
            .any(|b| b["renewal"] == true && b["start_index"] == 3 && b["count"] == 1),
        "a renewal for chunk 3 was requested: {:?}",
        log.grant_bodies
    );
    assert!(log.api_chunk_hits.is_empty() && log.api_download_hits == 0);
}

#[test]
fn a_chunk_storage_never_serves_goes_to_the_pinned_api_route() {
    let env = setup(Discovery::Offer, Grants::Normal, &[(5, Fault::Forbidden(99))], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_clean(&env, &out);
    assert_eq!(storage_hits(&env, 5), 4, "the first attempt plus three renewals");
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.api_chunk_hits, vec![5], "only that chunk, through the pinned route");
    assert_eq!(log.api_download_hits, 0, "never the full-file stream");
}

#[test]
fn broken_storage_responses_are_handled_like_any_storage_failure() {
    // A short body, a Content-Length that is not the grant's, a body with no
    // Content-Length: each chunk is retried, then fetched through the API.
    let env = setup(
        Discovery::Offer,
        Grants::Normal,
        &[
            (0, Fault::ShortBody),
            (4, Fault::WrongLength),
            (9, Fault::Chunked),
            (12, Fault::Status206),
        ],
        &[],
    );
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_clean(&env, &out);
    let mut api = env.mock.log.lock().unwrap().api_chunk_hits.clone();
    api.sort_unstable();
    assert_eq!(api, vec![0, 4, 9, 12]);
}

#[test]
fn a_tampered_storage_chunk_is_refetched_through_the_api_and_a_double_tamper_is_fatal() {
    let env = setup(Discovery::Offer, Grants::Normal, &[(2, Fault::Tamper)], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_eq!(env.mock.log.lock().unwrap().api_chunk_hits, vec![2]);

    let env = setup(Discovery::Offer, Grants::Normal, &[(2, Fault::Tamper)], &[2]);
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert_clean(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("authenticate"),
        "{}",
        describe(&out)
    );
}

#[test]
fn a_storage_redirect_is_never_followed() {
    let env = setup(Discovery::Offer, Grants::Normal, &[(1, Fault::Redirect)], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_clean(&env, &out);
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.leak_hits, 0, "the client followed a redirect off the storage host");
    assert_eq!(log.api_chunk_hits, vec![1]);
}

#[test]
fn the_pull_json_and_help_stay_compatible() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    for key in [
        "id",
        "name",
        "size_bytes",
        "download_speed_bps",
        "elapsed_ms",
        "output",
        "transport",
    ] {
        assert!(json.get(key).is_some(), "missing {key} in {json}");
    }
    assert_eq!(json["size_bytes"], PLAIN_LEN);
    let help = bb(&env, &["pull", "--help"], &[]);
    assert!(help.status.success(), "{}", describe(&help));
    assert!(
        !String::from_utf8_lossy(&help.stdout).contains("--transport"),
        "the debug switch is hidden from --help:\n{}",
        describe(&help)
    );
    let _ = &env.api_url;
}

#[test]
fn a_window_the_server_refuses_after_the_first_moves_the_rest_to_the_pinned_api_route() {
    // The mixed-node rollout case (R1.6): the first window came from a node with the route, a later
    // one from a node without it. What was written stays; the chunks without a grant come through
    // the pinned per-chunk route; the full-file stream is never used and no transport is mixed
    // WITHIN a chunk.
    let env = setup(Discovery::Offer, Grants::UnavailableAfterFirstWindow, &[], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_clean(&env, &out);
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.api_download_hits, 0, "no full-file stream");
    let mut api = log.api_chunk_hits.clone();
    api.sort_unstable();
    assert_eq!(
        api,
        (8..21).collect::<Vec<u32>>(),
        "exactly the chunks the first window did not cover"
    );
    let storage: std::collections::BTreeSet<u32> = log.storage.iter().map(|(i, _)| *i).collect();
    assert_eq!(storage, (0..8).collect(), "storage served exactly the granted chunks");
}

#[test]
fn an_undecodable_window_after_the_first_is_handled_like_any_window_that_cannot_be_served() {
    let env = setup(Discovery::Offer, Grants::GarbageAfterFirstWindow, &[], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.api_download_hits, 0);
    assert_eq!(log.api_chunk_hits.len(), 13, "chunks 8..=20 through the pinned route");
}

#[test]
fn a_file_held_by_another_operation_is_waited_for_and_stays_direct() {
    // 409 replace_in_progress with Retry-After: 1 on the first grant request (R1.13): wait, ask again.
    let env = setup(Discovery::Offer, Grants::ReplaceInProgressOnce, &[], &[]);
    let started = std::time::Instant::now();
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(900),
        "it waited for Retry-After"
    );
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    let log = env.mock.log.lock().unwrap();
    assert_eq!(
        log.grant_bodies[0], log.grant_bodies[1],
        "the same first window was asked for again"
    );
    assert_eq!(log.api_download_hits, 0);
}

#[test]
fn a_v2_stream_that_ends_before_its_last_frame_is_never_published() {
    let env = setup_with(
        fixture_v2_missing_last_frame(),
        Discovery::Offer,
        Grants::Normal,
        &[],
        &[],
    );
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert_clean(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("incomplete"),
        "{}",
        describe(&out)
    );
    // Control: the same fixture served whole would have decrypted; the failure above is the
    // missing last frame, not a broken fixture.
    assert_eq!(env.mock.log.lock().unwrap().storage.len(), 3);
}

#[test]
fn the_api_fallback_route_is_held_to_the_manifest_length_and_the_version_pin() {
    // Storage never serves chunk 5, so it goes through the pinned API route. A body one byte
    // short there is a failure, not a chunk: nothing per chunk is left to try, so the file
    // ends on the full API stream, from the start, with the partial output discarded.
    let env = setup(Discovery::Offer, Grants::Normal, &[(5, Fault::Forbidden(99))], &[]);
    env.mock.cfg.lock().unwrap().api_chunk_short.push(5);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "api", "{}", describe(&out));
    assert_clean(&env, &out);
    {
        let log = env.mock.log.lock().unwrap();
        assert!(
            log.api_chunk_hits.contains(&5),
            "the pinned route was tried for chunk 5"
        );
        assert_eq!(
            log.api_download_hits, 1,
            "then the whole file came through the API stream"
        );
    }

    // The pinned version gone on that route: abort, nothing written, no full-file stream.
    let env = setup(Discovery::Offer, Grants::Normal, &[(5, Fault::Forbidden(99))], &[]);
    env.mock.cfg.lock().unwrap().api_chunk_gone.push(5);
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("changed"),
        "{}",
        describe(&out)
    );
    assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 0);
}
