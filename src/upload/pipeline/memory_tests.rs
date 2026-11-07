//! Task 5 of the `--parallel` plan (spec §4.5, Review Focus 1): the REAL heap peak of the whole pipeline, not
//! the shares, matches the budget table — and an uncharged chunk-sized buffer turns it red.
//!
//! The tracking allocator (`crate::test_alloc`) is process-global, so the measurement runs in a CHILD process
//! (`real_peak_matches_the_budget_table` re-runs this binary with just `real_peak_child --ignored`), where only
//! this one test allocates. A mock holds every chunk PUT, so the pipeline reaches its worst moment; a sparse
//! 2 GiB + 1 MiB file gives the table's chunk sizes with no disk I/O.

use std::sync::Arc;
use std::time::Duration;

use beebeeb_core::kdf::MasterKey;
use uuid::Uuid;

use super::{UploadSpec, stream_encrypt_upload};
use crate::api::ApiClient;
use crate::interrupt::Interrupt;
use crate::test_server::{ChunkHold, Mock, Scratch, shared, spawn};
use crate::upload::NoopProgress;
use crate::upload::limits::ChunkLimits;

const MIB: u64 = 1024 * 1024;
/// A sparse 2 GiB + 1 MiB file: 128 MiB chunks at N 1–3, down the ladder as N rises.
const LARGE: u64 = 2048 * MIB + MIB;

/// Spec §4.2's worst moment, in MiB, for a fresh upload of `LARGE` at this N.
fn expected_peak_mib(asked: u32) -> u64 {
    match asked {
        1 => 640,
        2 => 640,
        4 => 512,
        8 => 384,
        other => panic!("unhandled N={other}"),
    }
}

fn spec(path: &std::path::Path, asked: u32, limits: Arc<ChunkLimits>, interrupt: &Interrupt) -> UploadSpec {
    UploadSpec {
        path: path.to_path_buf(),
        file_name: "payload.bin".to_string(),
        file_id: Uuid::new_v4(),
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

/// The real heap peak of the whole pipeline, charged to NOTHING but this process (run isolated). `#[ignore]`d
/// because the tracking allocator is process-global; `real_peak_matches_the_budget_table` runs it in a child.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "process-global peak-alloc; run isolated via real_peak_matches_the_budget_table"]
async fn real_peak_child() {
    for asked in [1u32, 2, 4, 8] {
        let s = Scratch::new(&format!("mem-peak-{asked}"));
        let (path, _, _) = s.sparse_file(LARGE);
        let chunk_size = super::chunk_size_for(LARGE, asked, 1, None);
        let used = crate::upload::limits::used_in_flight(asked, chunk_size);
        let limits = ChunkLimits::new(asked);
        let interrupt = Interrupt::new();
        let hold = Arc::new(ChunkHold::default());
        let state = shared(Mock {
            drop_chunk_bodies: true,
            hold_chunk_puts: Some(Arc::clone(&hold)),
            ..Mock::default()
        });
        let base = spawn(state).await;

        let baseline = crate::test_alloc::live();
        crate::test_alloc::reset_peak();

        let spec = spec(&path, asked, Arc::clone(&limits), &interrupt);
        let task = tokio::spawn(async move {
            let api = ApiClient::new_for_test(base);
            let mk = Arc::new(MasterKey::from_bytes([7u8; 32]));
            stream_encrypt_upload(&api, mk, spec, &NoopProgress).await
        });

        // Wait until every worker is parked and the producer has done all the sealing the budget allows (held
        // bytes stable): the worst moment has been reached.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "N={asked}: only {} PUTs parked",
                hold.held()
            );
            if hold.held() as u32 >= used {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Wait for the worst moment itself: the real heap peak reaching the table's lower bound. The shares peak
        // when the producer ACQUIRES its 2 C, before the seal allocates the copy and the frame, so they are no
        // signal for the allocation; a fixed sleep is no signal on a loaded host. A mock that drained at once
        // (the prescribed mutant c) never gets here and fails at the deadline, the lower bound's red.
        let expected = expected_peak_mib(asked);
        let half_c = (chunk_size / MIB) / 2;
        while (crate::test_alloc::peak().saturating_sub(baseline) as u64) < (expected - half_c) * MIB {
            assert!(
                tokio::time::Instant::now() < deadline,
                "N={asked}: the real peak reached only {} MiB, never the lower bound {}",
                crate::test_alloc::peak().saturating_sub(baseline) as u64 / MIB,
                expected - half_c
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // A beat for anything that would overshoot (what the upper bound is there to catch).
        tokio::time::sleep(Duration::from_millis(150)).await;

        let measured_mib = (crate::test_alloc::peak().saturating_sub(baseline)) as u64 / MIB;
        println!("real_peak N={asked}: measured {measured_mib} MiB, expected {expected} MiB (± {half_c})");
        assert!(
            measured_mib + half_c >= expected && measured_mib <= expected + half_c,
            "N={asked}: measured {measured_mib} MiB, expected {expected} ± {half_c}"
        );

        interrupt.set();
        let _ = task.await.expect("task");
        assert_eq!(limits.held_bytes(), 0, "N={asked}: every budget share returned");
    }
}

/// Run `real_peak_child` in a child process (this binary) so the process-global allocator sees only it, and
/// assert the child reported one passing test.
#[test]
fn real_peak_matches_the_budget_table() {
    let exe = std::env::current_exe().expect("test binary path");
    let output = std::process::Command::new(exe)
        .args([
            "upload::pipeline::memory_tests::real_peak_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("BB_REFUSE_NON_LOOPBACK_API", "1")
        .output()
        .expect("run the child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the child's real-peak test did not pass:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}
