//! Task 4 of the `--parallel` plan (spec §6.1): the process-wide pause, at the gate itself. The requests
//! it pauses are tested through the real API calls in `api/pause_requests_tests.rs`.

use std::time::Duration;

use tokio::time::Instant;

use super::*;
use crate::interrupt::Interrupt;

const RATE: PauseReason = PauseReason::RateLimited { remaining: None };

#[tokio::test(start_paused = true)]
async fn overlapping_pauses_are_counted_once_and_the_gate_opens_at_the_latest_end() {
    let origin = "http://pause-overlap";
    let start = Instant::now();
    assert_eq!(extend(origin, Duration::from_secs(2), RATE), Extension::Opened);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(extend(origin, Duration::from_secs(2), RATE), Extension::Extended);
    assert_eq!(extend(origin, Duration::from_secs(1), RATE), Extension::Within);
    assert_eq!(pauses(origin), 1, "pauses, not extensions");

    assert!(wait_open(origin).await);
    assert_eq!(start.elapsed(), Duration::from_millis(2100));

    assert_eq!(extend(origin, Duration::from_secs(1), RATE), Extension::Opened);
    assert_eq!(pauses(origin), 2, "a pause after the gate opened is a new one");
}

#[tokio::test(start_paused = true)]
async fn every_waiter_resumes_at_the_same_moment() {
    let origin = "http://pause-all-workers";
    let start = Instant::now();
    extend(origin, Duration::from_secs(2), RATE);
    let waiters: Vec<_> = (0..4)
        .map(|_| tokio::spawn(async move { (wait_open(origin).await, start.elapsed()) }))
        .collect();
    for waiter in waiters {
        let (open, at) = waiter.await.expect("task");
        assert!(open);
        assert_eq!(at, Duration::from_secs(2));
    }
}

#[tokio::test(start_paused = true)]
async fn a_gate_belongs_to_its_origin() {
    extend("http://pause-origin-a", Duration::from_secs(60), RATE);
    let start = Instant::now();
    assert!(wait_open("http://pause-origin-b").await);
    assert_eq!(
        start.elapsed(),
        Duration::ZERO,
        "another API's pause does not hold this one"
    );
    assert_eq!(
        origin_of("http://127.0.0.1:9/api/v1/uploads/x/chunks/1?a=b"),
        origin_of("http://127.0.0.1:9")
    );
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_ends_a_shared_pause() {
    let origin = "http://pause-interrupt";
    let interrupt = Interrupt::new();
    bind_interrupt(origin, interrupt.clone());
    extend(origin, Duration::from_secs(60), RATE);
    let start = Instant::now();
    let waiter = tokio::spawn(async move { wait_open(origin).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    interrupt.set();
    assert!(!waiter.await.expect("task"), "false: the interrupt fired first");
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn the_429_line_is_todays_and_the_503_line_has_no_glyph_or_colour() {
    colored::control::set_override(false);
    assert_eq!(
        notice(Duration::from_secs(5), PauseReason::RateLimited { remaining: Some(3) }),
        "  ⏸ rate limited — pausing 5s (3 requests remaining)"
    );
    assert_eq!(notice(Duration::from_secs(5), RATE), "  ⏸ rate limited — pausing 5s");
    let busy = notice(Duration::from_secs(7), PauseReason::Busy503);
    assert_eq!(busy, "  server busy (HTTP 503), pausing 7s");
    assert!(busy.is_ascii(), "no glyph, no colour: plain ASCII bytes only");
}
