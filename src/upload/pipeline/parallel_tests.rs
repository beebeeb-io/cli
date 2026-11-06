//! Task 5 of the `--parallel` plan (spec §4.2, §4.4, §5, §6.2): up to `used` chunks of one file at once, the
//! lowered-N line, the interrupt dropping the chunks in flight, out-of-order resume, and a chunk that fails for
//! good. Peak in flight is read from 2333's recorder; the budget from `ChunkLimits`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use beebeeb_core::chunk_stream::ChunkFormat;
use beebeeb_core::kdf::MasterKey;
use uuid::Uuid;

use super::{UploadOutcome, UploadSpec, stream_encrypt_upload};
use crate::api::ApiClient;
use crate::interrupt::Interrupt;
use crate::test_server::{ChunkHold, Mock, Scratch, Shared, shared, spawn};
use crate::upload::limits::ChunkLimits;
use crate::upload::{Recording, UploadError};

const MIB: u64 = 1024 * 1024;

/// A `bb push --parallel` spec: `asked` chunks of this file at once, under `limits`, with `interrupt` (whose flag
/// is the shutdown the producer and direct route read).
fn spec(
    path: &std::path::Path,
    file_id: Uuid,
    asked: u32,
    limits: Arc<ChunkLimits>,
    interrupt: &Interrupt,
) -> UploadSpec {
    UploadSpec {
        path: path.to_path_buf(),
        file_name: "payload.bin".to_string(),
        file_id,
        resume_tag: None,
        base_version_number: None,
        replace_pin: None,
        replace_file_key: None,
        parent_id: None,
        concurrency: asked,
        unrecorded_resume_concurrency: 1,
        chunks_in_flight: asked,
        limits,
        interrupt: Some(interrupt.clone()),
        shutdown: Arc::clone(interrupt.flag()),
        read_row: None,
    }
}

async fn run(state: &Shared, spec: UploadSpec, rec: &Recording) -> Result<UploadOutcome, UploadError> {
    let base = spawn(state.clone()).await;
    let api = ApiClient::new_for_test(base);
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    stream_encrypt_upload(&api, mk, spec, rec).await
}

/// An API session for `path` recorded in the sidecar at `chunk_size`, live in the mock.
fn resumed(mock: &mut Mock, path: &std::path::Path, size: u64, mtime: u128, chunk_size: u64) -> Uuid {
    let (file_id, sid) = (Uuid::new_v4(), Uuid::new_v4());
    crate::resume::record_with_chunk_size(path, file_id, sid, ChunkFormat::V1Legacy, size, mtime, chunk_size);
    mock.live.insert(sid.to_string());
    mock.session_file.insert(sid.to_string(), file_id.to_string());
    file_id
}

/// Hold every chunk PUT, run the upload in a task, wait until `expect_held` are parked, then return the hold and
/// the task handle so the caller can assert and interrupt.
async fn run_holding(
    state: Shared,
    spec: UploadSpec,
    rec: Recording,
    expect_held: usize,
) -> (
    Arc<ChunkHold>,
    tokio::task::JoinHandle<Result<UploadOutcome, UploadError>>,
) {
    let hold = Arc::new(ChunkHold::default());
    state.lock().unwrap().hold_chunk_puts = Some(Arc::clone(&hold));
    let base = spawn(state).await;
    let task = tokio::spawn(async move {
        let api = ApiClient::new_for_test(base);
        let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
        stream_encrypt_upload(&api, mk, spec, &rec).await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while hold.held() < expect_held {
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {} of {expect_held} PUTs parked",
            hold.held()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (hold, task)
}

/// A 1 GiB file at N = 4 (32 chunks of 32 MiB) runs exactly 4 at once.
#[tokio::test]
async fn peak_in_flight_equals_used() {
    let s = Scratch::new("parallel-peak-4");
    let (path, size, _) = s.sparse_file(1024 * MIB);
    assert_eq!(super::expected_chunks_for(size, 4), 32, "premise: 32 chunks of 32 MiB");
    let limits = ChunkLimits::new(4);
    let interrupt = Interrupt::new();
    let rec = Recording::default();
    let state = shared(Mock {
        drop_chunk_bodies: true,
        ..Mock::default()
    });

    let (hold, task) = run_holding(
        state,
        spec(&path, Uuid::new_v4(), 4, Arc::clone(&limits), &interrupt),
        rec.clone(),
        4,
    )
    .await;
    // Give any 5th PUT a chance to appear (it must not): the budget fits 17, so `used` is the only cap.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hold.held(), 4, "exactly 4 chunks are in flight, never a 5th");
    assert_eq!(rec.peak(), 4, "the recorder's peak is 4");
    interrupt.set();
    let out = task.await.expect("task");
    assert!(out.unwrap_err().is_interrupted());
    assert_eq!(limits.held_bytes(), 0, "every budget share returned");
}

/// A 128 MiB-chunk session resumed at N = 8 runs only 2 (the budget lowers it), and says so once.
#[tokio::test]
async fn resumed_large_session_lowers_used() {
    let s = Scratch::new("parallel-resume-lower");
    let (path, size, mtime) = s.sparse_file(2048 * MIB + MIB);
    let mut mock = Mock {
        drop_chunk_bodies: true,
        ..Mock::default()
    };
    let file_id = resumed(&mut mock, &path, size, mtime, 128 * MIB);
    let limits = ChunkLimits::new(8);
    let interrupt = Interrupt::new();
    let rec = Recording::default();

    let (hold, task) = run_holding(
        shared(mock),
        spec(&path, file_id, 8, Arc::clone(&limits), &interrupt),
        rec.clone(),
        2,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hold.held(), 2, "128 MiB chunks: only 2 fit at once");
    assert_eq!(rec.peak(), 2);
    assert!(
        rec.of("note").iter().any(|l| l
            == "note err     payload.bin: 2 chunks at once, not 8: its 128 MiB chunks fill the 640 MiB upload buffer"),
        "the lowered-N line, verbatim: {:?}",
        rec.of("note")
    );
    interrupt.set();
    assert!(task.await.expect("task").unwrap_err().is_interrupted());
}

/// The one lowered FRESH case (spec §4.2): N = 3 over 2 GiB plans 128 MiB, so it runs 2.
#[tokio::test]
async fn parallel_3_on_a_big_fresh_file_runs_2() {
    let s = Scratch::new("parallel-3-runs-2");
    let (path, size, _) = s.sparse_file(2048 * MIB + MIB);
    assert_eq!(
        super::expected_chunks_for(size, 3),
        17,
        "premise: 128 MiB chunks at N=3"
    );
    let limits = ChunkLimits::new(3);
    let interrupt = Interrupt::new();
    let rec = Recording::default();
    let state = shared(Mock {
        drop_chunk_bodies: true,
        ..Mock::default()
    });

    let (hold, task) = run_holding(
        state,
        spec(&path, Uuid::new_v4(), 3, Arc::clone(&limits), &interrupt),
        rec.clone(),
        2,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hold.held(), 2);
    assert_eq!(rec.peak(), 2);
    assert!(
        rec.of("note").iter().any(|l| l
            == "note err     payload.bin: 2 chunks at once, not 3: its 128 MiB chunks fill the 640 MiB upload buffer"),
        "{:?}",
        rec.of("note")
    );
    interrupt.set();
    assert!(task.await.expect("task").unwrap_err().is_interrupted());
}

/// The interrupt drops every PUT in flight: the mock sees 4 bodies aborted, the run returns interrupted within a
/// second of real time, and the budget is empty after.
#[tokio::test]
async fn interrupt_drops_in_flight_puts() {
    let s = Scratch::new("parallel-interrupt");
    let (path, size, _) = s.sparse_file(1024 * MIB);
    let chunks = super::expected_chunks_for(size, 4);
    let limits = ChunkLimits::new(4);
    let interrupt = Interrupt::new();
    let rec = Recording::default();
    let state = shared(Mock {
        drop_chunk_bodies: true,
        ..Mock::default()
    });

    let (_hold, task) = run_holding(
        state,
        spec(&path, Uuid::new_v4(), 4, Arc::clone(&limits), &interrupt),
        rec.clone(),
        4,
    )
    .await;
    assert_eq!(rec.in_flight(), 4, "4 chunks in flight, fewer than the file's {chunks}");
    let set_at = tokio::time::Instant::now();
    interrupt.set();
    let out = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("the pipeline returns within a second of the interrupt")
        .expect("task");
    assert!(out.unwrap_err().is_interrupted());
    assert_eq!(rec.in_flight(), 0, "every in-flight chunk settled");
    assert_eq!(limits.held_bytes(), 0, "every budget share returned");
    assert!(set_at.elapsed() < Duration::from_secs(1));
}

/// A chunk that fails for good stops the file: no chunk beyond the failed one is confirmed, `complete` is never
/// called, the record is kept, and the budget is empty after.
#[tokio::test]
async fn one_permanent_failure_stops_the_file() {
    let s = Scratch::new("parallel-perm-fail");
    let (path, size, mtime) = s.file(64 * MIB as usize);
    let mut mock = Mock {
        drop_chunk_bodies: true,
        ..Mock::default()
    };
    // Resume so the record exists and is expected to survive the failure.
    let file_id = resumed(&mut mock, &path, size, mtime, 4 * MIB);
    mock.chunk_status.insert(5, 400);
    let state = shared(mock);
    let limits = ChunkLimits::new(4);
    let interrupt = Interrupt::new();
    let rec = Recording::default();

    let err = run(&state, spec(&path, file_id, 4, Arc::clone(&limits), &interrupt), &rec)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("chunk 5 upload"), "{err}");
    assert!(
        state.lock().unwrap().completed.is_empty(),
        "complete is never called for a failed file"
    );
    assert_eq!(rec.of("finish"), ["finish false"]);
    assert_eq!(rec.in_flight(), 0, "every in-flight chunk settled");
    assert_eq!(limits.held_bytes(), 0);
    assert!(
        crate::resume::resumable_upload(&path, size, mtime).is_some(),
        "the record is kept for the next run"
    );
}

/// A 409 on one chunk while others are in flight ends the file the same way (Review Focus 5).
#[tokio::test]
async fn final_answer_on_one_chunk_drops_the_rest() {
    let s = Scratch::new("parallel-409");
    let (path, _, _) = s.file(64 * MIB as usize);
    let mut mock = Mock {
        drop_chunk_bodies: true,
        ..Mock::default()
    };
    mock.chunk_status.insert(2, 409);
    let state = shared(mock);
    let limits = ChunkLimits::new(4);
    let interrupt = Interrupt::new();
    let rec = Recording::default();

    let err = run(
        &state,
        spec(&path, Uuid::new_v4(), 4, Arc::clone(&limits), &interrupt),
        &rec,
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("chunk 2 upload"), "{err}");
    assert!(state.lock().unwrap().completed.is_empty());
    assert_eq!(limits.held_bytes(), 0);
}

/// Out-of-order resume: a session with chunks 0, 2, 3 already stored re-PUTs every index, the server answers the
/// held ones `skipped`, and only the missing chunk 1 is stored anew. Peak stays within `used`.
#[tokio::test]
async fn out_of_order_completion_resumes_only_missing() {
    let s = Scratch::new("parallel-out-of-order");
    let (path, size, mtime) = s.file(16 * MIB as usize); // 4 chunks of 4 MiB
    assert_eq!(super::expected_chunks_for(size, 4), 4, "premise: 4 chunks");
    let mut mock = Mock {
        drop_chunk_bodies: true,
        ..Mock::default()
    };
    let file_id = resumed(&mut mock, &path, size, mtime, 4 * MIB);
    // Mark the session as already holding chunks 0, 2, 3: those PUTs answer `skipped`.
    let sid = mock
        .session_file
        .iter()
        .find(|(_, f)| **f == file_id.to_string())
        .map(|(s, _)| s.clone())
        .unwrap();
    mock.skip_chunks.insert((sid.clone(), 0));
    mock.skip_chunks.insert((sid.clone(), 2));
    mock.skip_chunks.insert((sid, 3));
    let state = shared(mock);
    let limits = ChunkLimits::new(4);
    let interrupt = Interrupt::new();
    let rec = Recording::default();

    run(&state, spec(&path, file_id, 4, Arc::clone(&limits), &interrupt), &rec)
        .await
        .expect("the resume completes");

    let m = state.lock().unwrap();
    let stored_fresh = m.chunk_stored_fresh.len();
    assert_eq!(
        stored_fresh, 1,
        "only the missing chunk is stored anew: {:?}",
        m.chunk_stored_fresh
    );
    assert!(m.chunk_stored_fresh.contains(&1), "and it is chunk 1");
    assert!(
        rec.peak() >= 1 && rec.peak() <= 4,
        "peak within used, was {}",
        rec.peak()
    );
}

/// Review P1: `bb sync` has no interrupt, only its shutdown flag. With the flag set while a PUT is in flight,
/// and that PUT then finishing, nothing is in flight and nothing may be taken: the file ends as interrupted. It
/// must not leave every `select!` arm disabled (a tokio panic that took the whole sync down).
#[tokio::test]
async fn shutdown_flag_without_an_interrupt_ends_the_file() {
    let s = Scratch::new("parallel-shutdown-flag");
    let (path, _, _) = s.file(16 * MIB as usize); // 4 chunks of 4 MiB
    let hold = Arc::new(ChunkHold::default());
    let state = shared(Mock {
        hold_chunk_puts: Some(Arc::clone(&hold)),
        ..Mock::default()
    });
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut spec = spec(&path, Uuid::new_v4(), 1, ChunkLimits::single(), &Interrupt::new());
    spec.interrupt = None;
    spec.shutdown = Arc::clone(&shutdown);
    let base = spawn(state).await;
    let task = tokio::spawn(async move {
        let api = ApiClient::new_for_test(base);
        let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
        stream_encrypt_upload(&api, mk, spec, &Recording::default()).await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while hold.held() < 1 {
        assert!(tokio::time::Instant::now() < deadline, "no PUT parked");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    shutdown.store(true, Ordering::SeqCst);
    hold.release();

    let out = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .expect("the pipeline returns")
        .expect("the task joined: no panic");
    assert!(out.unwrap_err().is_interrupted(), "ended as interrupted, not failed");
}
