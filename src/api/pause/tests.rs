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
    let at = start.elapsed();
    assert!(
        (Duration::from_millis(2100)..=Duration::from_millis(2350)).contains(&at),
        "the latest end, plus a lone waiter's 0-250 ms: {at:?}"
    );

    assert_eq!(extend(origin, Duration::from_secs(1), RATE), Extension::Opened);
    assert_eq!(pauses(origin), 2, "a pause after the gate opened is a new one");
}

#[tokio::test(start_paused = true)]
async fn four_waiters_are_released_spread_over_at_least_500_ms() {
    let origin = "http://pause-all-workers";
    let start = Instant::now();
    extend(origin, Duration::from_secs(2), RATE);
    let waiters: Vec<_> = (0..4)
        .map(|_| tokio::spawn(async move { (wait_open(origin).await, start.elapsed()) }))
        .collect();
    let mut resumed = Vec::new();
    for waiter in waiters {
        let (open, at) = waiter.await.expect("task");
        assert!(open);
        assert!(
            (Duration::from_secs(2)..=Duration::from_secs(3)).contains(&at),
            "the pause end, plus at most the 1 s release window: {at:?}"
        );
        resumed.push(at);
    }
    resumed.sort();
    let spread = resumed[3] - resumed[0];
    assert!(spread >= Duration::from_millis(500), "first to last: {spread:?}");
    for pair in resumed.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(gap >= Duration::from_millis(187), "no two go together: {gap:?}");
    }
}

#[test]
fn the_release_line_is_an_even_step_per_place_plus_a_quarter_step_of_jitter() {
    assert_eq!(release_offset_ms(0, 4, 0), 0);
    assert_eq!(release_offset_ms(3, 4, 0), 750);
    assert_eq!(release_offset_ms(3, 4, 1_000), 812);
    assert_eq!(release_offset_ms(0, 1, 1_000), 250, "one waiter alone: 0-250 ms");
    assert_eq!(release_offset_ms(0, 0, 0), 0, "no division by zero");
    for waiters in 1..=64 {
        assert!(
            release_offset_ms(waiters - 1, waiters, 1_000) <= 1_000,
            "inside the window"
        );
    }
    // The least spread of 4: the last with no jitter, the first with all of it.
    assert!(release_offset_ms(3, 4, 0) - release_offset_ms(0, 4, 1_000) >= 500);
}

#[tokio::test(start_paused = true)]
async fn a_new_pause_starts_a_new_release_line() {
    let origin = "http://pause-next-line";
    extend(origin, Duration::from_secs(1), RATE);
    assert!(wait_open(origin).await);
    extend(origin, Duration::from_secs(1), RATE);
    let start = Instant::now();
    assert!(wait_open(origin).await);
    let at = start.elapsed();
    assert!(
        (Duration::from_secs(1)..=Duration::from_millis(1250)).contains(&at),
        "alone in the second pause's line: {at:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_refused_chunk_closes_the_gate_by_the_pause_rule_and_its_retry_waits_on_it() {
    let origin = "http://pause-chunk";
    // A 429 with a 1 s hint: the 2 s floor, plus at most 25 %.
    assert_eq!(
        chunk_refused(origin, 429, Duration::from_secs(1)),
        Some(Extension::Opened)
    );
    let closed = closed_for(origin);
    assert!(
        (Duration::from_secs(2)..=Duration::from_millis(2500)).contains(&closed),
        "the floor, not the hint: {closed:?}"
    );
    let start = Instant::now();
    assert!(wait_open(origin).await);
    assert!(
        start.elapsed() >= Duration::from_secs(2),
        "the retry waited on the gate"
    );

    // The next 429 in a row takes the next rung (4 s); a longer hint wins over the rung.
    chunk_refused(origin, 429, Duration::from_millis(500));
    let closed = closed_for(origin);
    assert!(
        (Duration::from_secs(4)..=Duration::from_secs(5)).contains(&closed),
        "the second rung: {closed:?}"
    );
    assert!(wait_open(origin).await);
    answered(origin);
    chunk_refused(origin, 429, Duration::from_secs(30));
    let closed = closed_for(origin);
    assert!(
        (Duration::from_secs(30)..=Duration::from_millis(37_500)).contains(&closed),
        "the hint: {closed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_503_chunk_pauses_for_the_policy_wait_and_any_other_status_pauses_nothing() {
    let origin = "http://pause-chunk-503";
    assert_eq!(chunk_refused(origin, 403, Duration::from_secs(9)), None);
    assert_eq!(chunk_refused(origin, 0, Duration::from_secs(9)), None);
    assert_eq!(pauses(origin), 0);
    assert_eq!(
        chunk_refused(origin, 503, Duration::from_secs(3)),
        Some(Extension::Opened)
    );
    assert_eq!(closed_for(origin), Duration::from_secs(3));
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
