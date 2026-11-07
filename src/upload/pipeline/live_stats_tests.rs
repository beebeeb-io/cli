//! Task 2333 (spec §9.3): what the upload driver tells its progress sink, through the real
//! `stream_encrypt_upload` against the loopback mock: chunks in flight, retries, routes, pools,
//! and what a failed attempt takes back.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use beebeeb_core::kdf::MasterKey;
use serde_json::json;
use uuid::Uuid;

use super::{CHUNKS_IN_FLIGHT_PER_FILE, UploadOutcome, UploadSpec, stream_encrypt_upload};
use crate::api::ApiClient;
use crate::test_server::{Mock, Scratch, Shared, shared, spawn};
use crate::transfer_stats::Direction;
use crate::transfer_view::LiveProgress;
use crate::transfer_view::sink::Level;
use crate::upload::{ChunkProgress, Recording, UploadError};

const MIB: usize = 1024 * 1024;
/// One full chunk on the wire: 4 MiB of plaintext plus nonce and tag.
const FULL_CHUNK: u64 = 4 * MIB as u64 + 28;

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
        concurrency: 1,
        unrecorded_resume_concurrency: 1,
        chunks_in_flight: 1,
        limits: crate::upload::limits::ChunkLimits::single(),
        interrupt: None,
        shutdown: Arc::new(AtomicBool::new(false)),
        read_row: None,
    }
}

async fn run(state: &Shared, spec: UploadSpec, progress: &dyn ChunkProgress) -> Result<UploadOutcome, UploadError> {
    let base = spawn(state.clone()).await;
    let api = ApiClient::new_for_test(base);
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    stream_encrypt_upload(&api, mk, spec, progress).await
}

fn chunks_of(size: u64) -> u32 {
    beebeeb_types::plan_chunks_concurrent(size, beebeeb_types::ChunkProfile::Cli, 1).chunk_count as u32
}

/// The start, done and settle lines, in order.
fn flow(rec: &Recording) -> Vec<String> {
    rec.events()
        .into_iter()
        .filter(|e| matches!(e.split(' ').next(), Some("start" | "done" | "settle")))
        .collect()
}

/// An interrupted API session for `path`, recorded in the sidecar and held by the mock.
fn resumed(mock: &mut Mock, path: &std::path::Path, size: u64, mtime: u128) -> (Uuid, Uuid) {
    let (file_id, sid) = (Uuid::new_v4(), Uuid::new_v4());
    crate::resume::record(
        path,
        file_id,
        sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    mock.live.insert(sid.to_string());
    // An honest server's completion names the file the session belongs to.
    mock.session_file.insert(sid.to_string(), file_id.to_string());
    (file_id, sid)
}

/// P1: `bb push` sends one chunk of a file at a time, and the constant says so.
#[tokio::test]
async fn push_sends_one_chunk_at_a_time() {
    let s = Scratch::new("2333-one-at-a-time");
    let (path, size, _) = s.file(9 * MIB);
    assert_eq!(chunks_of(size), 3, "premise: 3 chunks of 4 MiB");
    assert_eq!(
        super::expected_chunks_for(size, 1),
        3,
        "a command plans what the driver sends"
    );
    let state = shared(Mock::default());
    let rec = Recording::default();

    let out = run(&state, spec(&path, Uuid::new_v4()), &rec).await.expect("upload");

    let last = (MIB + 28).to_string();
    assert_eq!(
        flow(&rec),
        [
            "start 0 api".to_string(),
            format!("done 0 {FULL_CHUNK} transferred"),
            "settle 0".to_string(),
            "start 1 api".to_string(),
            format!("done 1 {FULL_CHUNK} transferred"),
            "settle 1".to_string(),
            "start 2 api".to_string(),
            format!("done 2 {last} transferred"),
            "settle 2".to_string(),
        ]
    );
    assert_eq!(rec.peak(), CHUNKS_IN_FLIGHT_PER_FILE as i64);
    assert_eq!(rec.in_flight(), 0);
    assert_eq!(
        rec.of("begin"),
        [format!("begin payload.bin {} 3 {}", out.ciphertext_bytes, 4 * MIB)]
    );
    assert_eq!(rec.of("finish"), ["finish true"]);
    assert!(
        rec.of("confirmed").is_empty() && rec.of("begin-legacy").is_empty(),
        "the driver uses the chunk methods, not the byte ones: {:?}",
        rec.events()
    );
    assert_eq!(out.transferred_bytes, out.ciphertext_bytes);
}

/// P2: a session swept after one chunk takes that chunk back; the retry counts the file once.
#[tokio::test]
async fn swept_mid_pipeline_rolls_back_chunks() {
    let s = Scratch::new("2333-midchunk");
    let (path, size, mtime) = s.file(5 * MIB);
    let mut mock = Mock::default();
    let (file_id, dead_sid) = resumed(&mut mock, &path, size, mtime);
    mock.dies_after_chunks.insert(dead_sid.to_string(), 1);
    let state = shared(mock);
    let rec = Recording::default();

    let out = run(&state, spec(&path, file_id), &rec)
        .await
        .expect("upload must recover");

    assert_eq!(rec.of("rollback"), [format!("rollback {FULL_CHUNK} 1 0")]);
    assert!(rec.of("rollback-bytes").is_empty());
    assert_eq!(rec.net_chunks(), chunks_of(size) as i64);
    assert_eq!(rec.in_flight(), 0);
    assert_eq!(
        out.transferred_bytes,
        out.ciphertext_bytes + FULL_CHUNK,
        "chunk 0 crossed the wire twice: speed counts it twice, progress once"
    );
    assert_eq!(
        rec.of("note"),
        ["note err   payload.bin: the interrupted upload expired on the server; starting it again"],
        "the driver's notice goes through the sink, with the same text"
    );
}

/// P3: `complete` finding the session gone takes back every chunk of the attempt.
#[tokio::test]
async fn complete_gone_rolls_back_every_chunk() {
    let s = Scratch::new("2333-complete-gone");
    let (path, size, mtime) = s.file(9 * MIB);
    let mut mock = Mock::default();
    let (file_id, sid) = resumed(&mut mock, &path, size, mtime);
    mock.complete_gone.insert(sid.to_string());
    let state = shared(mock);
    let rec = Recording::default();

    let out = run(&state, spec(&path, file_id), &rec)
        .await
        .expect("upload must recover");

    assert_eq!(rec.of("rollback"), [format!("rollback {} 3 0", out.ciphertext_bytes)]);
    assert_eq!(rec.net_chunks(), 3);
    assert_eq!(rec.of("finish"), ["finish false", "finish true"]);
}

/// P5: a chunk that fails for good still leaves the in-flight count.
#[tokio::test]
async fn failed_chunk_still_settles() {
    let s = Scratch::new("2333-failed-chunk");
    let (path, _, _) = s.file(9 * MIB);
    let mut mock = Mock::default();
    mock.chunk_status.insert(1, 500);
    let state = shared(mock);
    let rec = Recording::default();

    let err = run(&state, spec(&path, Uuid::new_v4()), &rec).await.unwrap_err();

    assert!(err.to_string().contains("chunk 1 upload"), "{err}");
    assert_eq!(rec.of("start").len(), 2, "{:?}", rec.events());
    assert_eq!(rec.of("settle").len(), 2, "{:?}", rec.events());
    assert_eq!(rec.in_flight(), 0);
    assert_eq!(rec.of("done").len(), 1);
    assert_eq!(rec.of("finish"), ["finish false"]);
    assert!(
        rec.of("rollback").is_empty(),
        "a failed file keeps its confirmed chunks: the server holds them for a resume"
    );
}

/// P6: the API chunk route retrying a transient refusal reports one retry, and the chunk counts once.
#[tokio::test]
async fn api_chunk_retry_is_reported() {
    let s = Scratch::new("2333-api-retry");
    let (path, _, _) = s.file(9 * MIB);
    let mut mock = Mock::default();
    mock.chunk_rate_limited_once.insert(1);
    let state = shared(mock);
    let rec = Recording::default();

    run(&state, spec(&path, Uuid::new_v4()), &rec).await.expect("upload");

    assert_eq!(rec.of("retry"), ["retry 1"]);
    let puts_of_1 = state.lock().unwrap().chunk_puts.iter().filter(|(_, i)| *i == 1).count();
    assert_eq!(puts_of_1, 2, "premise: chunk 1 was sent twice");
    let done_1: Vec<String> = rec
        .of("done")
        .into_iter()
        .filter(|e| e.starts_with("done 1 "))
        .collect();
    assert_eq!(done_1.len(), 1, "counted once: {done_1:?}");
    assert_eq!(rec.of("start").len(), 3, "a retry is not a new chunk in flight");
}

/// P11 (upload half): a new session's pool comes from its init answer, a resumed one's from its
/// completion. The answers also carry a datacenter code and a city, which are never kept.
#[tokio::test]
async fn pool_label_from_init_and_complete() {
    let init_answer = json!({
        "storage_pool_id": "3e15382b-0000-4000-8000-000000000001",
        "region": "zz9",
        "storage_location": {"city": "Testville", "country": "Germany", "country_code": "DE"},
    });
    let complete_answer = json!({
        "storage_pool_id": "4f26493c-0000-4000-8000-000000000002",
        "region": "zz9",
        "storage_location": {"city": "Testville", "country": "Finland", "country_code": "FI"},
    });

    // A new session: init names the pool; the completion of a session this run opened is not read.
    {
        let s = Scratch::new("2333-pool-init");
        let (path, _, _) = s.file(64 * 1024);
        let state = shared(Mock {
            init_extra: Some(init_answer.clone()),
            complete_extra: Some(complete_answer.clone()),
            ..Mock::default()
        });
        let rec = Recording::default();
        run(&state, spec(&path, Uuid::new_v4()), &rec).await.expect("upload");
        assert_eq!(rec.of("pool"), ["pool Germany"]);
    }

    // A resumed session skips init: its completion names the pool.
    {
        let s = Scratch::new("2333-pool-complete");
        let (path, size, mtime) = s.file(64 * 1024);
        let mut mock = Mock {
            init_extra: Some(init_answer),
            complete_extra: Some(complete_answer),
            ..Mock::default()
        };
        let (file_id, _) = resumed(&mut mock, &path, size, mtime);
        let state = shared(mock);
        let rec = Recording::default();
        run(&state, spec(&path, file_id), &rec).await.expect("upload");
        assert!(
            state.lock().unwrap().inits.is_empty(),
            "premise: the session was continued"
        );
        assert_eq!(rec.of("pool"), ["pool Finland"]);
    }
}

/// Spec §6.7: the API layer's rate-limit pause is printed through the live sink (around its lines),
/// never past it; once the run's sink is done, such a line goes straight to stderr again.
#[tokio::test]
async fn rate_limit_pause_prints_through_the_live_sink() {
    let _notices = crate::transfer_view::notice::TEST_SINK_LOCK.lock().await;
    let s = Scratch::new("2333-pause-line");
    let (path, _, _) = s.file(9 * MIB);
    let mut mock = Mock::default();
    mock.chunk_rate_limited_once.insert(1);
    // 4 s: no other test's 429 waits this long, so the line below is this pipeline's own.
    mock.retry_after_secs = 4;
    let state = shared(mock);
    let (live, record) = LiveProgress::for_test(Direction::Upload, Level::Default, 80, Arc::new(AtomicU64::new(0)));
    live.register_notices_for_test();

    run(&state, spec(&path, Uuid::new_v4()), &live).await.expect("upload");

    let printed = record.lock().unwrap().clone();
    let pauses: Vec<&String> = printed
        .iter()
        .filter(|l| l.contains("rate limited \u{2014} pausing 4s"))
        .collect();
    assert!(
        !pauses.is_empty() && pauses.iter().all(|l| l.starts_with("err:")),
        "the pause line goes through the sink, on stderr: {printed:?}"
    );
    let _ = live.summary();
    crate::transfer_view::notice::stderr("  after the run");
    assert!(
        !record.lock().unwrap().iter().any(|l| l.contains("after the run")),
        "a finished sink lets go of the notices"
    );
}
