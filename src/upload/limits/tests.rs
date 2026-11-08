//! Task 2 of the `--parallel` plan (spec §4.2): the chunks-at-once rule and the byte budget.

use std::sync::Arc;
use std::time::Duration;

use super::*;

const MIB: u64 = 1024 * 1024;

#[test]
fn used_follows_the_budget_table() {
    assert_eq!(used_in_flight(1, 128 * MIB), 1);
    assert_eq!(used_in_flight(2, 128 * MIB), 2);
    assert_eq!(
        used_in_flight(3, 128 * MIB),
        2,
        "3 x 128 MiB beside a read buffer and a seal does not fit"
    );
    assert_eq!(used_in_flight(4, 64 * MIB), 4);
    assert_eq!(used_in_flight(6, 64 * MIB), 6);
    assert_eq!(used_in_flight(8, 32 * MIB), 8);
    assert_eq!(used_in_flight(8, 128 * MIB), 2);
    assert_eq!(used_in_flight(8, 4 * MIB), 8);
    assert_eq!(used_in_flight(1, 0), 1, "an empty file counts as the 4 MiB floor");
}

#[tokio::test(start_paused = true)]
async fn shares_never_exceed_the_budget() {
    let limits = ChunkLimits::new(8);
    let tasks: Vec<_> = (0..32)
        .map(|_| {
            let limits = Arc::clone(&limits);
            tokio::spawn(async move {
                let _share = limits.share(64 * MIB).await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            })
        })
        .collect();
    for task in tasks {
        task.await.expect("task");
    }
    assert!(
        limits.peak_held_bytes() <= UPLOAD_BUFFER_BUDGET,
        "peak {}",
        limits.peak_held_bytes()
    );
    assert_eq!(
        limits.peak_held_bytes(),
        UPLOAD_BUFFER_BUDGET,
        "the budget was reached, so the bound is exercised"
    );
    assert_eq!(limits.held_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn shrink_returns_the_rest() {
    let limits = ChunkLimits::new(4);
    let mut seal = limits.share(128 * MIB).await;
    let rest = limits.share(512 * MIB).await;
    assert_eq!(limits.held_bytes(), UPLOAD_BUFFER_BUDGET);

    let waiter = {
        let limits = Arc::clone(&limits);
        tokio::spawn(async move { limits.share(64 * MIB).await })
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(100), async {
            while !waiter.is_finished() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .is_err(),
        "the budget is full: the waiting share is not granted"
    );

    seal.shrink_to(64 * MIB);
    let granted = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("a shrunk share frees the budget for the waiter")
        .expect("task");
    assert_eq!(limits.held_bytes(), 64 * MIB + 512 * MIB + 64 * MIB);
    drop((granted, rest, seal));
    assert_eq!(limits.held_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_share_larger_than_the_budget_is_capped_not_stuck() {
    let limits = ChunkLimits::new(1);
    let share = limits.share(UPLOAD_BUFFER_BUDGET * 2).await;
    assert_eq!(limits.held_bytes(), UPLOAD_BUFFER_BUDGET);
    drop(share);
}

/// Spec §7: an admitted attempt holds its read buffer and one seal (R + 2 C) until it ends; its seals come from the
/// pool while the pool has them, and from the reserve (not charged twice) when it does not.
#[tokio::test(start_paused = true)]
async fn admission_holds_the_read_buffer_and_one_seal() {
    let limits = ChunkLimits::new(4);
    let admission = limits.admit(64 * MIB).await;
    assert_eq!(limits.held_bytes(), 192 * MIB, "R + 2 C");

    let rest = limits.share(UPLOAD_BUFFER_BUDGET - 192 * MIB).await;
    let mut lent = tokio::time::timeout(Duration::from_secs(1), admission.seal_share(&limits, 128 * MIB))
        .await
        .expect("the pool is empty: the seal comes from the reserve");
    assert_eq!(
        limits.held_bytes(),
        UPLOAD_BUFFER_BUDGET,
        "a lent seal is not charged twice"
    );
    lent.shrink_to(64 * MIB);
    assert_eq!(limits.held_bytes(), UPLOAD_BUFFER_BUDGET);
    drop(rest);
    drop(admission);
    assert_eq!(
        limits.held_bytes(),
        128 * MIB,
        "the read buffer is back; the reserve (2 C) stays charged while a frame it lent is alive"
    );
    drop(lent);
    assert_eq!(limits.held_bytes(), 0);
}

/// Spec §7: four files of 128 MiB chunks (a resumed record's size) at N = 8 share the 640 MiB budget without a
/// deadlock: each admitted attempt can always seal its next chunk. Each simulated file seals 17 chunks, holds at
/// most 2 in flight (its `used`) and one of the push's 8 chunk slots per PUT, each PUT 10 ms of paused time.
#[tokio::test(start_paused = true)]
async fn big_files_admission_never_deadlocks() {
    const C: u64 = 128 * MIB;
    let limits = ChunkLimits::new(8);
    let files: Vec<_> = (0..4)
        .map(|_| {
            let limits = Arc::clone(&limits);
            tokio::spawn(async move {
                let admission = limits.admit(C).await;
                let used = Arc::new(tokio::sync::Semaphore::new(used_in_flight(8, C) as usize));
                let mut puts = Vec::new();
                for _ in 0..17 {
                    let mut frame = admission.seal_share(&limits, 2 * C).await;
                    frame.shrink_to(C);
                    let own = Arc::clone(&used).acquire_owned().await.expect("open");
                    let slot = limits.chunk_slot().await;
                    puts.push(tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        drop((frame, own, slot));
                    }));
                }
                for put in puts {
                    put.await.expect("put");
                }
            })
        })
        .collect();
    let all = async {
        for file in files {
            file.await.expect("file");
        }
    };
    tokio::time::timeout(Duration::from_secs(3600), all)
        .await
        .expect("every file finished: no deadlock on the budget");
    assert!(
        limits.peak_held_bytes() <= UPLOAD_BUFFER_BUDGET,
        "peak {}",
        limits.peak_held_bytes()
    );
    assert_eq!(limits.held_bytes(), 0);
}

/// Spec §7: at most `asked` chunk slots are out at once, across every holder.
#[tokio::test(start_paused = true)]
async fn chunk_slots_cap_the_push() {
    let limits = ChunkLimits::new(3);
    let held: Vec<_> = futures_util::future::join_all((0..3).map(|_| limits.chunk_slot())).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), limits.chunk_slot())
            .await
            .is_err(),
        "a fourth waits"
    );
    drop(held);
    let _granted = tokio::time::timeout(Duration::from_millis(100), limits.chunk_slot())
        .await
        .expect("a freed slot is granted");
}
