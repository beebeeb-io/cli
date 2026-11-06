//! Task 1 of the `--parallel` plan (spec §11, Review Focus 6): at one chunk at a time a push makes today's
//! requests, in today's order. Each golden under `testdata/` was recorded at cli origin/main `27612df`
//! (before this change) with `BB_WRITE_REQUEST_LOGS=1`; the test compares what the mock answered with it,
//! `(method, path, status, body_len)` per request. Bodies are not compared: core seals every chunk under a
//! fresh nonce, so no two runs send the same bytes.
//!
//! The direct-route goldens live beside the direct mock: `direct_upload::chunk::tests::support::request_log`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use beebeeb_core::kdf::MasterKey;
use uuid::Uuid;

use super::stream_encrypt_upload;
use crate::api::ApiClient;
use crate::test_server::{Mock, RequestLog, Scratch, shared, spawn};
use crate::transfer_stats::Direction;
use crate::transfer_view::LiveProgress;
use crate::transfer_view::sink::Level;
use crate::upload::{ChunkProgress, NoopProgress, UploadSpec};

const MIB: u64 = 1024 * 1024;
/// 16 chunks of 4 MiB (core's ladder: `next_pow2(size / 32)`, floor 4 MiB).
pub(crate) const SMALL: u64 = 64 * MIB;
/// 17 chunks at one chunk at a time: 16 of 128 MiB and a 1 MiB tail. The size at which the chunk plan follows
/// `--parallel`, so the one that catches a wrong plan at 1.
pub(crate) const LARGE: u64 = 2048 * MIB + MIB;

/// The golden file `name` under `testdata/`.
pub(crate) fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/upload/pipeline/testdata")
        .join(format!("{name}.log"))
}

/// Compare `log` with the golden `name`, or (with `BB_WRITE_REQUEST_LOGS=1`, at origin/main only) write it.
pub(crate) fn check_golden(name: &str, log: &RequestLog) {
    let got = log.lines();
    assert!(!got.is_empty(), "the recorder saw nothing: the instrument is broken");
    let path = golden_path(name);
    if std::env::var_os("BB_WRITE_REQUEST_LOGS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{}\n", got.join("\n"))).unwrap();
        return;
    }
    let want: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("golden {}: {e}", path.display()))
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(
        got, want,
        "{name}: the requests of this run (left) differ from origin/main's (right)"
    );
}

fn spec(path: &std::path::Path) -> UploadSpec {
    UploadSpec {
        path: path.to_path_buf(),
        file_name: "payload.bin".to_string(),
        file_id: Uuid::new_v4(),
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

async fn push(mock: Mock, path: &std::path::Path, progress: &dyn ChunkProgress) -> RequestLog {
    let log = RequestLog::default();
    let state = shared(Mock {
        requests: Some(log.clone()),
        drop_chunk_bodies: true,
        ..mock
    });
    let base = spawn(state.clone()).await;
    let api = ApiClient::new_for_test(base);
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    stream_encrypt_upload(&api, mk, spec(path), progress)
        .await
        .expect("upload");
    log
}

#[tokio::test]
async fn parallel_1_matches_main_request_log_api_small() {
    let s = Scratch::new("golden-api-small");
    let (path, size, _) = s.file(SMALL as usize);
    assert_eq!(super::expected_chunks_for(size, 1), 16, "premise: 16 chunks of 4 MiB");
    let log = push(Mock::default(), &path, &NoopProgress).await;
    check_golden("push_parallel1_api_small", &log);
}

#[tokio::test]
async fn parallel_1_matches_main_request_log_api_large() {
    let s = Scratch::new("golden-api-large");
    let (path, size, _) = s.sparse_file(LARGE);
    assert_eq!(
        super::expected_chunks_for(size, 1),
        17,
        "premise: 16 x 128 MiB and a 1 MiB tail"
    );
    let log = push(Mock::default(), &path, &NoopProgress).await;
    check_golden("push_parallel1_api_large", &log);
}

/// A 429 on one chunk: the log holds the refusal and the retry, and the pause line is today's, byte for byte.
#[tokio::test]
async fn parallel_1_matches_main_request_log_api_429() {
    let s = Scratch::new("golden-api-429");
    let (path, _, _) = s.file(SMALL as usize);
    let _notices = crate::transfer_view::notice::TEST_SINK_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut mock = Mock::default();
    mock.chunk_rate_limited_once.insert(1);
    mock.retry_after_secs = 1;
    colored::control::set_override(false);
    let (live, record) = LiveProgress::for_test(Direction::Upload, Level::Default, 80, Arc::new(AtomicU64::new(0)));
    live.register_notices_for_test();

    let log = push(mock, &path, &live).await;

    let _ = live.summary();
    let printed = record.lock().unwrap().clone();
    // The lock keeps the other capturing test off the slot, but a non-capturing test's 429/503 may still print
    // into this sink under the parallel suite, so assert today's line is PRESENT, not that it is the only line.
    // `contains`, not `==`: `colored`'s override is process-global and a sibling test sets it; the line's exact
    // bytes are pinned by `api::pause::tests::the_429_line_is_todays_and_the_503_line_has_no_glyph_or_colour`.
    assert!(
        printed
            .iter()
            .any(|l| l.starts_with("err:") && l.contains("rate limited \u{2014} pausing 1s")),
        "today's 429 line, through the sink, on stderr: {printed:?}"
    );
    check_golden("push_parallel1_api_429", &log);
}
