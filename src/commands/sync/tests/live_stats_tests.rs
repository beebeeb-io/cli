//! Task 2333 (plan Review Focus 5): Ctrl-C during `bb sync`'s upload phase. Sync uploads `--concurrency` files at
//! once through the real driver, with one shutdown flag and no interrupt; when it trips with both files' chunks in
//! flight, every chunk settles, the sink counts the run's limit as the files at once times each file's (the driver
//! sets one file's), and the sink's summary (taken before the "interrupted" line prints) stops its ticker.

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
    live.set_files_at_once(2);
    interrupted_pass(live.clone()).await;
    let stats = live.render_now().into_iter().find(|l| l.contains("in flight"));
    assert!(
        stats.as_deref().is_some_and(|l| l.contains(" 0/2 in flight")),
        "{stats:?}"
    );
    assert!(live.ticker_running());
    live.summary();
    assert!(!live.ticker_running(), "the summary joined the ticker");
}
