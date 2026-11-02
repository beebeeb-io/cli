mod compat;
mod fallback;
mod sync;
mod transport;
mod version;

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
#[allow(deprecated)] // fixture: a fixed id and chunk size, which the NewItemId/WriteTarget writers cannot express
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

/// A pre-`bb repair` file: the same 21 chunks, but each frame is sealed under the file key derived
/// from the 16-byte BINARY uuid. The streaming reader (string-UUID key) cannot open frame 0; only the
/// API path's buffered fallback tries the binary key.
fn fixture_legacy_binary_uuid() -> Fixture {
    let mut fx = fixture();
    let mk = MasterKey::from_bytes([0u8; 32]);
    let uuid: uuid::Uuid = FILE_ID.parse().unwrap();
    let key = beebeeb_core::kdf::derive_file_key(&mk, uuid.as_bytes());
    fx.frames = fx
        .plaintext
        .chunks(STRIDE as usize)
        .map(|c| beebeeb_core::encrypt::encrypt_chunk_raw(&key, c).unwrap())
        .collect();
    fx
}

/// The checked-in core v2 vector (4 chunks, position-bound AEAD) with its LAST
/// frame dropped: the manifest and the metadata both say 3 chunks, every frame
/// authenticates, but the stream never reaches the frame marked last.
fn fixture_v2_missing_last_frame() -> Fixture {
    let v: Value = serde_json::from_str(include_str!("../../fixtures/core_1908_vectors.json")).unwrap();
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
    /// The very first grant request answers 409 `replace_in_progress` with `Retry-After: 1`. The
    /// contract allows a stated wait; today's server sends none (see `ReplaceInProgressNoRetryAfter`).
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
    if id == SYNC_FOLDER_ID {
        return Json(json!({ "id": id, "name_encrypted": "SyncUp", "is_folder": true, "chunk_count": 0 }))
            .into_response();
    }
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

/// The body of the server's pinned per-chunk route for a version that is gone.
const GONE_VERSION: &str = "download version unavailable; restart download from fresh metadata";

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
        Discovery::ServerError => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong."),
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
    // The real server (routes/version_download.rs): a version that is gone is a 409 whose body
    // is this sentence, NOT the grant route's `version_unavailable` code.
    if q.get("object_version_id").map(String::as_str) != Some(VERSION) {
        return json_error(StatusCode::CONFLICT, GONE_VERSION);
    }
    let Some(frame) = m.fx.frames.get(index as usize) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "chunk index {index} out of range (version has {} chunk(s))",
                m.fx.frames.len()
            ),
        );
    };
    let mut body = frame.clone();
    let cfg = m.cfg.lock().unwrap();
    if cfg.api_chunk_gone.contains(&index) {
        return json_error(StatusCode::CONFLICT, GONE_VERSION);
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
                .route(
                    "/api/v1/auth/me",
                    get(|| async { Json(json!({"id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"})) }),
                )
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
    cmd.env("BB_REFUSE_NON_LOOPBACK_API", "1");
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

/// Run `bb sync <cwd>/syncdir /SyncUp` the way a daemon does (no `--once`) until it reaches watch
/// mode, then stop it. Returns (reached watch mode, stdout, stderr). The child is the one process
/// this starts, and it is the only one stopped.
fn sync_until_watching(env: &Env) -> (bool, String, String) {
    use std::io::{BufRead, BufReader};
    let dir = env.cwd.join("syncdir");
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bb"))
        .env("BB_REFUSE_NON_LOOPBACK_API", "1")
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

fn sync_left_nothing(dir: &Path) {
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != ".bb-sync.json")
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}
