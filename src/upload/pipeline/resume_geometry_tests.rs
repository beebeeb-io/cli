//! Task 3 of the `--parallel` plan (spec §4.3, Review Focus 2): the chunk plan follows `--parallel`, a session's
//! does not. A resumed upload frames against the size its record names, whatever the run's concurrency; a record
//! from before this change (no size) is planned as its caller planned then (`bb push` 1, `bb sync` its own
//! `--concurrency`); a record claiming a size no CLI writes is not continued.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use beebeeb_core::kdf::MasterKey;
use uuid::Uuid;

use super::{UploadOutcome, UploadSpec, planned_chunk_size, stream_encrypt_upload};
use crate::api::ApiClient;
use crate::test_server::{Mock, RequestLog, Scratch, shared, spawn};
use crate::upload::{NoopProgress, UploadError};

const MIB: u64 = 1024 * 1024;
/// Over 2 GiB: a fresh plan at concurrency 1 is 128 MiB chunks.
const OVER_2_GIB: u64 = 2048 * MIB + MIB;
/// 3 GiB: 128 MiB chunks at concurrency 1 to 3, 64 MiB at 4 to 6, 32 MiB at 7 and 8.
const THREE_GIB: u64 = 3 * 1024 * MIB;

fn spec(path: &std::path::Path, file_id: Uuid, concurrency: u32, unrecorded: u32) -> UploadSpec {
    UploadSpec {
        path: path.to_path_buf(),
        file_name: "payload.bin".to_string(),
        file_id,
        resume_tag: None,
        base_version_number: None,
        replace_pin: None,
        replace_file_key: None,
        parent_id: None,
        concurrency,
        unrecorded_resume_concurrency: unrecorded,
        chunks_in_flight: 1,
        limits: crate::upload::limits::ChunkLimits::single(),
        interrupt: None,
        shutdown: Arc::new(AtomicBool::new(false)),
        read_row: None,
    }
}

/// A live API session for `path` in the mock, recorded in the sidecar with `chunk_size` (`None`: a record from
/// before this change).
fn resumed(mock: &mut Mock, path: &std::path::Path, size: u64, mtime: u128, chunk_size: Option<u64>) -> Uuid {
    let (file_id, sid) = (Uuid::new_v4(), Uuid::new_v4());
    let format = beebeeb_core::chunk_stream::ChunkFormat::V1Legacy;
    match chunk_size {
        Some(c) => crate::resume::record_with_chunk_size(path, file_id, sid, format, size, mtime, c),
        None => crate::resume::record(path, file_id, sid, format, size, mtime),
    }
    mock.live.insert(sid.to_string());
    mock.session_file.insert(sid.to_string(), file_id.to_string());
    file_id
}

async fn run(
    mock: Mock,
    spec: UploadSpec,
) -> (
    Result<UploadOutcome, UploadError>,
    RequestLog,
    crate::test_server::Shared,
) {
    let log = RequestLog::default();
    let state = shared(Mock {
        requests: Some(log.clone()),
        drop_chunk_bodies: true,
        ..mock
    });
    let base = spawn(state.clone()).await;
    let api = ApiClient::new_for_test(base);
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let out = stream_encrypt_upload(&api, mk, spec, &NoopProgress).await;
    (out, log, state)
}

/// The body lengths of the chunk PUTs, in order.
fn put_lengths(log: &RequestLog) -> Vec<u64> {
    log.entries()
        .iter()
        .filter(|r| r.method == "PUT" && r.path.contains("/chunks/"))
        .map(|r| r.body_len.expect("a chunk body is recorded"))
        .collect()
}

/// A record at 32 MiB, resumed at concurrency 1 (which would plan 128 MiB afresh): the encryptor and every PUT
/// frame 32 MiB + 28.
#[tokio::test]
async fn resume_keeps_recorded_chunk_size_across_parallel() {
    let s = Scratch::new("geometry-recorded-32");
    let (path, size, mtime) = s.sparse_file(OVER_2_GIB);
    let mut mock = Mock::default();
    let file_id = resumed(&mut mock, &path, size, mtime, Some(32 * MIB));

    let (out, log, state) = run(mock, spec(&path, file_id, 1, 1)).await;

    let out = out.expect("upload");
    let lengths = put_lengths(&log);
    assert_eq!(lengths.len(), 65, "65 chunks of 32 MiB, not 17 of 128 MiB: {lengths:?}");
    assert!(
        lengths[..64].iter().all(|&l| l == 32 * MIB + 28),
        "every full chunk frames 32 MiB + 28: {lengths:?}"
    );
    assert_eq!(lengths[64], MIB + 28, "the tail");
    assert!(
        state.lock().unwrap().inits.is_empty(),
        "the session was continued, not re-inited"
    );
    assert_eq!(out.ciphertext_bytes, size + 28 * 65);
}

/// A sizeless record (a CLI before `--parallel`) met by `bb push` at N = 8: the session was planned at 1, so it
/// is framed at 128 MiB, whatever N is now.
#[tokio::test]
async fn old_push_record_resumes_at_concurrency_1() {
    let s = Scratch::new("geometry-old-push");
    let (path, size, mtime) = s.sparse_file(THREE_GIB);
    let mut mock = Mock::default();
    let file_id = resumed(&mut mock, &path, size, mtime, None);
    // Chunk 1 is refused for good, so the run ends after two PUTs: the first one's length is the proof.
    mock.chunk_status.insert(1, 400);

    let (out, log, _) = run(mock, spec(&path, file_id, 8, 1)).await;

    let err = out.expect_err("chunk 1 fails the file");
    assert!(err.to_string().contains("chunk 1 upload"), "{err}");
    let lengths = put_lengths(&log);
    assert_eq!(
        lengths,
        [128 * MIB + 28, 128 * MIB + 28],
        "128 MiB frames, not 32 MiB (N = 8)"
    );
}

/// A sizeless record met by `bb sync --concurrency 4`: planned at 4 then, so 64 MiB (a fallback of 1 would PUT
/// 128 MiB bodies into a 64 MiB session).
#[tokio::test]
async fn old_sync_record_resumes_at_sync_concurrency() {
    let s = Scratch::new("geometry-old-sync");
    let (path, size, mtime) = s.sparse_file(THREE_GIB);
    let mut mock = Mock::default();
    let file_id = resumed(&mut mock, &path, size, mtime, None);
    mock.chunk_status.insert(1, 400);

    let (out, log, _) = run(mock, spec(&path, file_id, 4, 4)).await;

    assert!(out.is_err());
    let lengths = put_lengths(&log);
    assert_eq!(
        lengths,
        [64 * MIB + 28, 64 * MIB + 28],
        "64 MiB frames: today's sync plan at 4"
    );
}

/// A record claiming 256 MiB (above core's 128 MiB Cli cap, so no CLI wrote it) is ended through the
/// unresumable path and the file starts again under this run's id.
#[tokio::test]
async fn oversized_recorded_chunk_is_not_continued() {
    let s = Scratch::new("geometry-oversized");
    let (path, size, mtime) = s.file(64 * MIB as usize);
    let mut mock = Mock::default();
    let recorded_id = resumed(&mut mock, &path, size, mtime, Some(256 * MIB));
    let fresh_id = Uuid::new_v4();

    let (out, log, state) = run(mock, spec(&path, fresh_id, 1, 1)).await;

    out.expect("the file is uploaded afresh");
    let m = state.lock().unwrap();
    assert_eq!(
        m.abandons,
        [recorded_id.to_string()],
        "the recorded session is ended first"
    );
    assert_eq!(m.inits, [fresh_id.to_string()], "then this run's id is inited");
    assert_eq!(put_lengths(&log).len(), 16, "16 chunks of 4 MiB into the new session");
    drop(m);
    assert!(
        crate::resume::resumable_upload(&path, size, mtime).is_none(),
        "the record is cleared with the upload"
    );
}

/// The one rule, as `push_report` plans with it: a new session at the run's concurrency, a recorded size kept, a
/// sizeless record at the caller's pre-`--parallel` concurrency.
#[test]
fn planned_chunk_size_follows_the_record_then_the_caller() {
    let pending = |chunk_size_bytes| crate::resume::PendingUpload {
        file_id: Uuid::nil(),
        session_id: Uuid::nil(),
        chunk_format: None,
        direct: false,
        resume_tag: None,
        destination: None,
        chunk_size_bytes,
    };
    assert_eq!(
        planned_chunk_size(THREE_GIB, 8, 1, None),
        32 * MIB,
        "a new session at N = 8"
    );
    assert_eq!(
        planned_chunk_size(THREE_GIB, 1, 1, None),
        128 * MIB,
        "a new session at N = 1"
    );
    assert_eq!(
        planned_chunk_size(THREE_GIB, 8, 1, Some(&pending(Some(64 * MIB)))),
        64 * MIB,
        "a recorded size is kept"
    );
    assert_eq!(
        planned_chunk_size(THREE_GIB, 8, 1, Some(&pending(None))),
        128 * MIB,
        "a sizeless record: the push planned at 1"
    );
    assert_eq!(
        planned_chunk_size(THREE_GIB, 4, 4, Some(&pending(None))),
        64 * MIB,
        "a sizeless record: the sync planned at its concurrency"
    );
}
