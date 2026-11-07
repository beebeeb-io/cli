//! Task 2333 (plan Review Focus 5): Ctrl-C during `bb sync`'s upload phase. Sync uploads `--concurrency` files at
//! once through the real driver, with one shutdown flag and no interrupt; when it trips with both files' chunks in
//! flight, every chunk settles, and the sink's summary (taken before the "interrupted" line prints) stops its ticker.
//! (Sync's run-wide limit, `RunLimit` over each file's `FileSink`: `tests/transfer_stats_output.rs`
//! `sync_peak_is_at_most_the_concurrency` and `sync_limit_is_at_most_the_files_pending`.)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use beebeeb_core::kdf::MasterKey;
use uuid::Uuid;

use crate::api::ApiClient;
use crate::test_server::{ChunkHold, Mock, Scratch, shared, spawn};
use crate::transfer_view::LiveProgress;
use crate::upload::{CHUNKS_IN_FLIGHT_PER_FILE, ChunkProgress, UploadError, UploadSpec, stream_encrypt_upload};

const MIB: usize = 1024 * 1024;

/// Two files of three chunks uploading at once into `sink`, as sync's upload phase runs them; the flag trips once
/// both have a chunk parked at the server, then the server answers. Each file's result.
async fn interrupted_pass(sink: Arc<dyn ChunkProgress>) -> Vec<Result<(), UploadError>> {
    let hold = Arc::new(ChunkHold::default());
    let base = spawn(shared(Mock {
        hold_chunk_puts: Some(Arc::clone(&hold)),
        ..Mock::default()
    }))
    .await;
    let shutdown = Arc::new(AtomicBool::new(false));
    // One scratch dir (its lock is process-wide): the second file sits beside the first.
    let scratch = Scratch::new("2333-sync-ctrlc");
    let first = scratch.file(9 * MIB).0;
    let second = first.with_file_name("second.bin");
    std::fs::write(&second, vec![0x5Au8; 9 * MIB]).unwrap();
    let mut tasks = Vec::new();
    for path in [first, second] {
        let spec = UploadSpec {
            path,
            file_name: "payload.bin".to_string(),
            file_id: Uuid::new_v4(),
            resume_tag: None,
            base_version_number: None,
            replace_pin: None,
            replace_file_key: None,
            parent_id: None,
            concurrency: 2,
            unrecorded_resume_concurrency: 2,
            chunks_in_flight: CHUNKS_IN_FLIGHT_PER_FILE,
            limits: crate::upload::limits::ChunkLimits::single(),
            interrupt: None,
            shutdown: Arc::clone(&shutdown),
            read_row: None,
        };
        let (base, sink) = (base.clone(), Arc::clone(&sink));
        tasks.push(tokio::spawn(async move {
            let api = ApiClient::new_for_test(base);
            let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
            stream_encrypt_upload(&api, mk, spec, sink.as_ref()).await.map(|_| ())
        }));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while hold.held() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {} PUTs parked",
            hold.held()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    shutdown.store(true, Ordering::SeqCst);
    hold.release();
    let mut results = Vec::new();
    for task in tasks {
        let joined = tokio::time::timeout(Duration::from_secs(20), task).await;
        results.push(joined.expect("the upload returns").expect("no panic"));
    }
    results
}

#[tokio::test]
async fn sync_interrupt_settles_every_chunk_and_clears_the_lines() {
    let rec = crate::upload::Recording::default();
    let results = interrupted_pass(Arc::new(rec.clone())).await;
    assert!(
        results
            .iter()
            .all(|r| r.as_ref().is_err_and(UploadError::is_interrupted)),
        "both files end as interrupted: {results:?}"
    );
    assert_eq!(rec.peak(), 2, "both files had a chunk in flight: {:?}", rec.events());
    assert_eq!(rec.in_flight(), 0, "every chunk in flight settled: {:?}", rec.events());

    // The real sink, drawing through indicatif with its ticker: nothing left in flight, then the summary (which
    // sync takes before it prints "interrupted") stops the ticker and clears the lines.
    let live = Arc::new(LiveProgress::for_ticker_test(Duration::from_millis(1)));
    interrupted_pass(live.clone()).await;
    let stats = live.render_now().into_iter().find(|l| l.contains("in flight"));
    assert!(
        stats.as_deref().is_some_and(|l| l.contains(" 0/1 in flight")),
        "{stats:?}"
    );
    assert!(live.ticker_running());
    live.summary();
    assert!(!live.ticker_running(), "the summary joined the ticker");
}

/// One file's attempt through `sink`, as the driver reports it: `sent` of its `chunks` confirmed, then it ends.
fn attempt(sink: &dyn ChunkProgress, name: &str, bytes: u64, chunks: u32, sent: u32) {
    let file = sink.begin_file_chunks(name, bytes, chunks, bytes / chunks as u64);
    for index in 0..sent {
        file.chunk_started(index, crate::transfer_stats::Route::Api);
        file.chunk_done(index, bytes / chunks as u64, crate::transfer_stats::Held::Transferred);
        file.chunk_settled(index);
    }
    file.finish(sent == chunks);
}

/// Review P2 (#142): every pending upload of a pass settles in the run's numbers (spec §4.3). A file the write check
/// skips never reaches the driver: it leaves the totals. A file whose upload fails stays in them, and what it did
/// not send leaves the ETA. At the end, done plus given up is the total.
#[test]
fn every_pending_upload_of_a_pass_settles() {
    use super::UploadShare;
    use crate::transfer_view::sink::{Level, RunLimit};
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let direction = crate::transfer_stats::Direction::Upload;
    let (live, _) = LiveProgress::for_test(direction, Level::Default, 80, clock);
    let limit = RunLimit::new(4);
    let plans = [(1028, 1), (2056, 2), (3084, 3)];
    let [_, failed, uploaded] = plans;
    for (bytes, chunks) in plans {
        live.plan_file(bytes, chunks);
    }
    live.seal_plan();
    // Keys 0, 1, 2: the file the write check skips, the one whose upload fails, the one uploaded.
    let [skip, fail, ok] = [0u32, 1, 2].map(|key| UploadShare::new(&live, &limit, key, Some(plans[key as usize])));
    attempt(fail.driver_sink(), "failed.bin", failed.0, failed.1, 1);
    attempt(ok.driver_sink(), "uploaded.bin", uploaded.0, uploaded.1, uploaded.1);
    skip.end(&Err::<(), _>("kept read-only"));
    fail.end(&Err::<(), _>("upload failed"));
    ok.end(&Ok::<(), String>(()));

    let end = live.summary();
    let json = end.to_json();
    assert_eq!(
        (&json["files_total"], &json["bytes_total"], &json["chunks_total"]),
        (
            &serde_json::json!(2),
            &serde_json::json!(failed.0 + uploaded.0),
            &serde_json::json!(5)
        ),
        "the skipped file left the totals: {json}"
    );
    assert_eq!(end.files_done, 1, "{json}");
    assert_eq!(
        live.given_up_for_test(),
        failed.0 / 2,
        "the failed file's unsent chunk left the ETA"
    );
    assert_eq!(
        end.bytes_done + live.given_up_for_test(),
        end.bytes_total,
        "every byte settled: {json}"
    );
}

/// Review P2 (#142): an in-place replace with a resume record is planned at the record's chunk size, the one its
/// driver resumes at: the plan asks the sidecar with the row it versions, as the driver does.
#[test]
fn a_replace_with_a_resume_record_is_planned_as_the_driver_sends_it() {
    let scratch = Scratch::new("2333-sync-plan-replace");
    let (path, size, mtime) = scratch.file(9 * MIB);
    let row = Uuid::new_v4();
    crate::resume::record_upload(
        &path,
        crate::item_id::Proposed::Listed(row),
        None,
        Uuid::new_v4(),
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
        false,
        None,
        MIB as u64,
    );
    // What the driver of the in-place replace reads (`upload::replacing`: the row's id) and frames the file with.
    let driver = crate::resume::resumable_upload_for(&path, size, mtime, Some(row));
    assert!(driver.is_some(), "premise: the replace's driver continues the record");
    let sent =
        crate::upload::expected_at_chunk_size(size, crate::upload::planned_chunk_size(size, 4, 4, driver.as_ref()));
    assert_eq!(sent.1, 9, "premise: the record's 1 MiB chunks");
    let resume = crate::resume::pending_snapshot();
    assert_eq!(super::upload_plan(&resume, &path, size, 4, Some(row)), sent);
}

/// Review P3 (#142): the plan reads the sidecar once per pass. A record written after the pass took its snapshot is
/// not seen by it; a new snapshot sees it.
#[test]
fn the_plan_reads_the_sidecar_once() {
    let scratch = Scratch::new("2333-sync-plan-once");
    let (path, size, mtime) = scratch.file(MIB);
    let row = Uuid::new_v4();
    let resume = crate::resume::pending_snapshot();
    crate::resume::record_upload(
        &path,
        crate::item_id::Proposed::Listed(row),
        None,
        Uuid::new_v4(),
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
        false,
        None,
        MIB as u64 / 4,
    );
    assert!(
        resume.resumable_now(&path, Some(row)).is_none(),
        "read before the record"
    );
    assert!(
        crate::resume::pending_snapshot()
            .resumable_now(&path, Some(row))
            .is_some()
    );
}
