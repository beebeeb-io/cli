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
