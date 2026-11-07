//! Task 2333 (spec §9.3 P8–P11): what the download drivers tell the progress sink. A chunk counts after it decrypts,
//! never before; the chunks in flight stay under the route's limit; a direct download that falls back to the API
//! stream takes back what it counted; the file's storage country reaches the file's handle.
//!
//! Both routes run against loopback mocks: an API (discovery, download grants, the pinned per-chunk route, the full
//! stream) and a storage server whose answers a test can slow down or refuse per chunk. Invented values only
//! (`bucket-test`, `Testville`): `tests/no_city_copy.rs` scans this file whole.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use beebeeb_core::chunk_stream::ExpectedChunkFormat;
use beebeeb_core::encrypt::encrypt_chunk_raw;
use beebeeb_core::kdf::{MasterKey, derive_file_key};
use serde_json::{Value, json};

use super::{DownloadError, DownloadStats, stream_download_decrypt};
use crate::api::ApiClient;
use crate::transfer_stats::{Direction, PoolLabel, Summary};
use crate::transfer_view::LiveProgress;
use crate::transfer_view::sink::{Draw, Level, ViewOptions};
use crate::upload::{ChunkProgress, Recording};

const FILE_ID: &str = "3e15382b-1111-2222-3333-444455556666";
const VERSION: &str = "4d5e6f70-8192-4a3b-9c4d-5e6f708192a3";
const BUCKET: &str = "bucket-test";
const STRIDE: usize = 1024;
/// Three chunks: two full ones and a last one of 100 bytes.
const CHUNKS: usize = 3;
const FULL_FRAME: u64 = STRIDE as u64 + 28;
const LAST_FRAME: u64 = 100 + 28;
const TOTAL: u64 = 2 * FULL_FRAME + LAST_FRAME;

/// How the mocks answer.
#[derive(Default)]
struct Script {
    /// Discovery offers direct downloads; otherwise the file comes through the API stream.
    direct: bool,
    /// Storage waits this long before it answers a chunk.
    delay_ms: HashMap<u32, u64>,
    /// Storage refuses these chunks every time (an expired link).
    refused: Vec<u32>,
    /// The pinned per-chunk route answers one byte short for these chunks: a failure, not a chunk.
    api_short: Vec<u32>,
    /// The full stream ends after this many frames, before the one marked last.
    stream_frames: Option<usize>,
}

struct Mock {
    frames: Vec<Vec<u8>>,
    plain_len: usize,
    script: Script,
    storage_host: String,
    /// The chunks storage answered, in the order it answered them.
    served: Mutex<Vec<u32>>,
}

fn master_key() -> MasterKey {
    MasterKey::from_bytes([7u8; 32])
}

fn plaintext() -> Vec<u8> {
    (0..STRIDE * (CHUNKS - 1) + 100).map(|i| (i % 251) as u8).collect()
}

fn frames() -> Vec<Vec<u8>> {
    let key = derive_file_key(&master_key(), FILE_ID.as_bytes());
    plaintext()
        .chunks(STRIDE)
        .map(|c| encrypt_chunk_raw(&key, c).unwrap())
        .collect()
}

async fn discovery(State(m): State<Arc<Mock>>) -> Response {
    let offered: Vec<&str> = if m.script.direct {
        vec!["object-download-v1"]
    } else {
        vec![]
    };
    Json(json!({
        "download": offered,
        "upload": [],
        "storage_hosts": [m.storage_host],
        "storage_buckets": [{ "host": m.storage_host, "bucket": BUCKET }],
        "max_window": 32,
        "url_ttl_seconds": 300,
    }))
    .into_response()
}

async fn grants(State(m): State<Arc<Mock>>, Json(body): Json<Value>) -> Response {
    let n = m.frames.len();
    let start = body["start_index"].as_u64().unwrap_or(0) as usize;
    let end = (start + body["count"].as_u64().unwrap_or(0) as usize).min(n);
    let expires = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + 300, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let list: Vec<Value> = (start..end)
        .map(|i| {
            json!({
                "index": i,
                "method": "GET",
                "url": format!("http://{}/{BUCKET}/{FILE_ID}/{i}?X-Amz-Signature=SIG-{i}", m.storage_host),
                "length": m.frames[i].len(),
                "expires_at": expires,
            })
        })
        .collect();
    Json(json!({
        "transport": "object-download-v1",
        "file_id": FILE_ID,
        "object_version_id": VERSION,
        "chunk_count": n,
        "chunk_size": STRIDE,
        "encrypted_size": m.frames.iter().map(Vec::len).sum::<usize>(),
        "original_size": m.plain_len,
        "grants": list,
        "next_index": if end < n { json!(end) } else { Value::Null },
    }))
    .into_response()
}

async fn storage(
    State(m): State<Arc<Mock>>,
    AxPath((_bucket, _file, index)): AxPath<(String, String, u32)>,
) -> Response {
    if let Some(ms) = m.script.delay_ms.get(&index) {
        tokio::time::sleep(Duration::from_millis(*ms)).await;
    }
    if m.script.refused.contains(&index) {
        return StatusCode::FORBIDDEN.into_response();
    }
    m.served.lock().unwrap().push(index);
    m.frames[index as usize].clone().into_response()
}

async fn pinned_chunk(
    State(m): State<Arc<Mock>>,
    AxPath((_id, index)): AxPath<(String, u32)>,
    Query(_q): Query<HashMap<String, String>>,
) -> Response {
    let mut body = m.frames[index as usize].clone();
    if m.script.api_short.contains(&index) {
        body.pop();
    }
    ([(header::CONTENT_TYPE, "application/octet-stream")], body).into_response()
}

async fn full_stream(State(m): State<Arc<Mock>>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::HeaderName::from_static("x-chunk-count"),
                m.frames.len().to_string(),
            ),
            (
                header::HeaderName::from_static("x-original-size"),
                m.plain_len.to_string(),
            ),
            (header::HeaderName::from_static("x-chunk-size"), STRIDE.to_string()),
        ],
        m.frames[..m.script.stream_frames.unwrap_or(m.frames.len())].concat(),
    )
        .into_response()
}

/// `bb pull`'s driver call for the mock's file, into `sink`; the result, the file written, and the mock.
async fn pull(
    script: Script,
    sink: &dyn ChunkProgress,
    pool: Option<&PoolLabel>,
) -> (Result<DownloadStats, DownloadError>, Option<Vec<u8>>, Arc<Mock>) {
    let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let storage_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = Arc::new(Mock {
        frames: frames(),
        plain_len: plaintext().len(),
        script,
        storage_host: storage_listener.local_addr().unwrap().to_string(),
        served: Mutex::new(Vec::new()),
    });
    let api_app = Router::new()
        .route("/api/v1/transfer/capabilities", get(discovery))
        .route("/api/v1/files/:id/download-grants", post(grants))
        .route("/api/v1/files/:id/chunks/:index", get(pinned_chunk))
        .route("/api/v1/files/:id/download", get(full_stream))
        .with_state(mock.clone());
    let storage_app = Router::new()
        .route("/:bucket/:file/:index", get(storage))
        .with_state(mock.clone());
    let api_addr = api_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(api_listener, api_app).await;
    });
    tokio::spawn(async move {
        let _ = axum::serve(storage_listener, storage_app).await;
    });
    let api = ApiClient::new_for_test(format!("http://{api_addr}"));
    let dir = std::env::temp_dir().join(format!("bb-dl-2333-{}-{}", std::process::id(), rand::random::<u32>()));
    let out = dir.join("movie.mkv");
    let result = stream_download_decrypt(
        &api,
        &master_key(),
        FILE_ID,
        ExpectedChunkFormat::Unrecorded,
        CHUNKS as u32,
        &out,
        sink,
        "movie.mkv",
        pool,
    )
    .await;
    let written = std::fs::read(&out).ok();
    let _ = std::fs::remove_dir_all(&dir);
    (result, written, mock)
}

/// The same pull into the real sink (no drawing), and its summary.
async fn pull_summary(script: Script) -> Summary {
    let (live, _record) = LiveProgress::for_draw(
        Direction::Download,
        ViewOptions {
            level: Level::Verbose,
            draw: Draw::Off,
        },
    );
    live.plan_file(TOTAL, CHUNKS as u32);
    live.next_file(TOTAL, CHUNKS as u32);
    live.seal_plan();
    let (result, written, _) = pull(script, &live, None).await;
    result.expect("the download succeeds");
    assert_eq!(written.as_deref(), Some(plaintext().as_slice()));
    live.summary()
}

fn done_indices(rec: &Recording) -> Vec<u32> {
    rec.of("done")
        .iter()
        .map(|e| e.split(' ').nth(1).unwrap().parse().unwrap())
        .collect()
}

fn germany() -> PoolLabel {
    PoolLabel::from_answer(&json!({
        "storage_location": { "country": "Germany", "country_code": "DE", "city": "Testville" }
    }))
    .expect("a file row with a country")
}

/// P10: the API stream reads one frame at a time, and counts each after it decrypts.
#[tokio::test]
async fn api_stream_is_one_at_a_time() {
    let rec = Recording::default();
    let (result, written, _) = pull(Script::default(), &rec, Some(&germany())).await;
    assert_eq!(result.expect("the download succeeds").transferred_bytes, TOTAL);
    assert_eq!(written.as_deref(), Some(plaintext().as_slice()));
    assert_eq!(rec.of("limit"), ["limit 1"]);
    assert_eq!(rec.of("begin"), [format!("begin movie.mkv {TOTAL} 3 {STRIDE}")]);
    assert_eq!(rec.of("start"), ["start 0 api", "start 1 api", "start 2 api"]);
    assert_eq!(
        rec.of("done"),
        [
            format!("done 0 {FULL_FRAME} transferred"),
            format!("done 1 {FULL_FRAME} transferred"),
            format!("done 2 {LAST_FRAME} transferred"),
        ]
    );
    assert_eq!(rec.peak(), 1, "{:?}", rec.events());
    assert_eq!(rec.in_flight(), 0);
    assert_eq!(rec.of("finish"), ["finish true"]);
    assert!(
        rec.of("confirmed").is_empty() && rec.of("begin-legacy").is_empty(),
        "{:?}",
        rec.events()
    );
}

/// P11, the download half: a file row names its storage country only; the file's handle gets that country.
#[tokio::test]
async fn the_file_rows_country_reaches_the_files_handle_on_both_routes() {
    for direct in [false, true] {
        let rec = Recording::default();
        let script = Script {
            direct,
            ..Script::default()
        };
        let (result, _, _) = pull(script, &rec, Some(&germany())).await;
        result.expect("the download succeeds");
        assert_eq!(rec.of("pool"), ["pool Germany"], "direct: {direct}");
        let rec = Recording::default();
        let script = Script {
            direct,
            ..Script::default()
        };
        let (result, _, _) = pull(script, &rec, None).await;
        result.expect("the download succeeds");
        assert!(rec.of("pool").is_empty(), "no country, no pool: {:?}", rec.events());
    }
}

/// P8: chunk 1 arrives first, then 0, then 2. Each counts only once it decrypts, so in order; every fetch of the
/// window goes out at once, under the route's limit of 4.
#[tokio::test]
async fn direct_download_counts_after_decrypt() {
    let rec = Recording::default();
    let script = Script {
        direct: true,
        delay_ms: HashMap::from([(0, 300), (2, 600)]),
        ..Script::default()
    };
    let (result, written, mock) = pull(script, &rec, None).await;
    let stats = result.expect("the download succeeds");
    assert_eq!(stats.transport, "direct");
    assert_eq!(stats.transferred_bytes, TOTAL);
    assert_eq!(written.as_deref(), Some(plaintext().as_slice()));
    assert_eq!(*mock.served.lock().unwrap(), [1, 0, 2], "the order storage answered in");
    assert_eq!(done_indices(&rec), [0, 1, 2], "{:?}", rec.events());
    assert_eq!(rec.of("limit"), ["limit 4"]);
    assert_eq!(rec.of("begin"), [format!("begin movie.mkv {TOTAL} 3 {STRIDE}")]);
    assert_eq!(rec.of("start"), ["start 0 direct", "start 1 direct", "start 2 direct"]);
    assert!(rec.peak() <= 4, "{:?}", rec.events());
    assert_eq!(
        rec.peak(),
        3,
        "the three fetches of one window were out together: {:?}",
        rec.events()
    );
    assert_eq!(rec.in_flight(), 0);
    assert_eq!(rec.of("finish"), ["finish true"]);
    assert!(
        rec.of("confirmed").is_empty() && rec.of("begin-legacy").is_empty(),
        "{:?}",
        rec.events()
    );

    // Through the real sink: every chunk on the direct route, each counted once.
    let s = pull_summary(Script {
        direct: true,
        delay_ms: HashMap::from([(0, 300), (2, 600)]),
        ..Script::default()
    })
    .await;
    assert_eq!((s.direct, s.api), (3, 0));
    assert_eq!((s.chunks_done, s.chunks_total), (3, 3));
    assert_eq!((s.bytes_done, s.bytes_transferred), (TOTAL, TOTAL));
    assert_eq!((s.peak, s.max_in_flight), (3, 4));
    assert_eq!(s.files_done, 1);
}

/// P9: storage never serves chunk 2 and the pinned route answers it short, so the API stream takes the file from
/// the start. The two chunks the direct route decrypted are taken back once, and crossed the wire twice.
#[tokio::test]
async fn direct_download_fallback_rolls_back() {
    let script = || Script {
        direct: true,
        refused: vec![2],
        api_short: vec![2],
        ..Script::default()
    };
    let rec = Recording::default();
    let (result, written, _) = pull(script(), &rec, None).await;
    let stats = result.expect("the API stream serves the file");
    assert_eq!(stats.transport, "api");
    assert_eq!(
        stats.transferred_bytes,
        TOTAL + 2 * FULL_FRAME,
        "the two direct chunks, then the whole stream"
    );
    assert_eq!(written.as_deref(), Some(plaintext().as_slice()));
    assert_eq!(rec.of("rollback"), [format!("rollback {} 2 0", 2 * FULL_FRAME)]);
    assert_eq!(rec.of("limit"), ["limit 4", "limit 1"]);
    assert_eq!(rec.of("finish"), ["finish false", "finish true"]);
    assert!(
        !rec.of("retry").is_empty(),
        "storage's refusals were retried: {:?}",
        rec.events()
    );
    assert_eq!(rec.net_chunks(), CHUNKS as i64);
    assert_eq!(rec.in_flight(), 0, "{:?}", rec.events());
    assert!(rec.of("rollback-bytes").is_empty(), "{:?}", rec.events());

    let s = pull_summary(script()).await;
    assert_eq!((s.chunks_done, s.chunks_total), (3, 3));
    assert_eq!(s.bytes_done, TOTAL);
    assert_eq!(s.bytes_transferred, TOTAL + 2 * FULL_FRAME);
    assert_eq!((s.direct, s.api), (2, 3));
    assert_eq!(s.files_done, 1);
}

/// A download dropped mid-way (Ctrl-C, a timeout) leaves nothing open on the sink: every chunk in flight settles, what
/// was counted is taken back (without the line of a move to the API: #142 review P3), and the file closes as failed.
#[tokio::test]
async fn a_dropped_direct_download_settles_and_takes_back_what_it_counted() {
    let rec = Recording::default();
    let script = Script {
        direct: true,
        delay_ms: HashMap::from([(1, 30_000)]),
        ..Script::default()
    };
    let dropped = tokio::time::timeout(Duration::from_secs(2), pull(script, &rec, None)).await;
    assert!(
        dropped.is_err(),
        "chunk 1 never came, so the download was still running"
    );
    assert_eq!(done_indices(&rec), [0], "{:?}", rec.events());
    assert_eq!(rec.in_flight(), 0, "{:?}", rec.events());
    assert_eq!(rec.of("rollback-failed"), [format!("rollback-failed {FULL_FRAME} 1 0")]);
    assert!(rec.of("rollback").is_empty(), "{:?}", rec.events());
    assert_eq!(rec.of("finish"), ["finish false"]);
}

/// Review P3 (#142): an API stream that fails takes back what it counted, as a failed direct attempt does: here it
/// ends after two of its three frames, so the file fails with nothing left counted.
#[tokio::test]
async fn a_failed_api_stream_takes_back_what_it_counted() {
    let (live, _record) = LiveProgress::for_draw(
        Direction::Download,
        ViewOptions {
            level: Level::Verbose,
            draw: Draw::Off,
        },
    );
    live.plan_file(TOTAL, CHUNKS as u32);
    live.next_file(TOTAL, CHUNKS as u32);
    live.seal_plan();
    let script = Script {
        stream_frames: Some(2),
        ..Script::default()
    };
    let (result, written, _) = pull(script, &live, None).await;
    assert!(
        matches!(result, Err(DownloadError::Unreadable(_))),
        "the cut stream fails as unreadable"
    );
    assert_eq!(written, None, "nothing was published");
    let end = live.summary();
    assert_eq!((end.chunks_done, end.bytes_done), (0, 0), "{end:?}");
    assert_eq!(
        end.bytes_transferred,
        2 * FULL_FRAME,
        "the wire still moved them: {end:?}"
    );
}
