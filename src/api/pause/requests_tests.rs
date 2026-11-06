//! Task 4 of the `--parallel` plan (spec §6.1, Review Focus 3), through the real API calls against the loopback
//! mock, in real time (a paused clock would run ahead while a request waits on the socket): one 429 or 503
//! pauses every request of the process, an unpaced caller still waits after its own 429, a 503 on a chunk is
//! retried within the chunk's attempts, and the push's interrupt ends a pause at once.
//!
//! These assert the gate's own [`pauses`] count and the recorded request timing, not captured notice lines: the
//! notice slot is process-global, so under the parallel suite any other test's 429/503 prints into whatever sink
//! is registered. The notice TEXT is pinned by the unit test `the_429_line_is_todays_and_the_503_line_has_no_...`
//! and its routing through a live sink by `upload::pipeline::live_stats_tests::rate_limit_pause_prints_...`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use uuid::Uuid;

use super::{PauseReason, bind_interrupt, extend, pauses, reason_for};
use crate::api::ApiClient;
use crate::interrupt::Interrupt;
use crate::test_server::{Mock, RequestLog, shared, spawn};

const MIB: usize = 1024 * 1024;

/// A mock with the recorder on and a live session; `(api, log, session id, the mock)`.
async fn client(mock: Mock) -> (ApiClient, RequestLog, String, crate::test_server::Shared) {
    let log = RequestLog::default();
    let sid = Uuid::new_v4().to_string();
    let mut mock = Mock {
        requests: Some(log.clone()),
        ..mock
    };
    mock.live.insert(sid.clone());
    let state = shared(mock);
    let base = spawn(state.clone()).await;
    (ApiClient::new_for_test(base), log, sid, state)
}

/// A 429 on a trigger request closes the gate; every worker that then begins waits for it. Asserted on the
/// recorded request timing and the gate's [`pauses`] count, not captured notices (spec §6.1: a pause costs at
/// most the requests in flight plus one per unpaced caller).
#[tokio::test]
async fn a_429_pauses_every_worker() {
    let mut mock = Mock::default();
    mock.chunk_rate_limited_once.insert(0);
    mock.retry_after_secs = 3;
    let (api, log, sid, _) = client(mock).await;
    let api = Arc::new(api);

    let origin = api.pause_origin().to_string();
    // The trigger's PUT 429s, which closes the gate. It then waits the 3 s out itself.
    let trigger = {
        let api = Arc::clone(&api);
        let sid = sid.clone();
        tokio::spawn(async move {
            api.upload_chunk_typed(sid.as_str(), 0, Bytes::from(vec![0u8; MIB]))
                .await
                .expect("chunk");
        })
    };
    // Wait until the gate is observed closed (`pauses` counts the open), not a wall-clock guess: under load the
    // 429's round-trip can take any amount of real time, and a worker that begins before the gate is closed would
    // not wait, which is not what this test is about.
    while pauses(&origin) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let workers: Vec<_> = (1..4u32)
        .map(|index| {
            let api = Arc::clone(&api);
            let sid = sid.clone();
            tokio::spawn(async move {
                api.upload_chunk_typed(sid.as_str(), index, Bytes::from(vec![0u8; MIB]))
                    .await
                    .expect("chunk");
            })
        })
        .collect();
    trigger.await.expect("trigger");
    for w in workers {
        w.await.expect("worker");
    }

    let entries = log.entries();
    let refused = entries.iter().find(|r| r.status == 429).expect("the 429 was answered");
    // The gate was closed before any worker began. Every worker's PUT, and the trigger's retry, must land only
    // once the gate reopens (`refused.at_ms` + ~3 s): they all wait for the one moment. Ordering, not an absolute
    // window, so the assertion holds whatever the machine's load.
    let after_429: Vec<&crate::test_server::LoggedRequest> = entries
        .iter()
        .filter(|r| r.status == 200 && r.method == "PUT")
        .collect();
    assert_eq!(
        after_429.len(),
        4,
        "the retried trigger and the three workers all land: {entries:?}"
    );
    assert!(
        after_429.iter().all(|r| r.at_ms >= refused.at_ms + 2_800),
        "every PUT waited for the gate to reopen: 429 at {}, PUTs at {:?}",
        refused.at_ms,
        after_429.iter().map(|r| r.at_ms).collect::<Vec<_>>()
    );
    assert_eq!(pauses(&origin), 1, "one pause for the whole run");
}

/// `get_file` does not await the pacer. Meeting `429 Retry-After: 2`, it still returns after the pause, not at
/// once, and a paced call started meanwhile waits for the same moment.
#[tokio::test]
async fn an_unpaced_caller_still_waits_after_its_429() {
    let mock = Mock {
        file_rate_limited_once: true,
        retry_after_secs: 2,
        ..Mock::default()
    };
    let (api, log, sid, _) = client(mock).await;
    let api = Arc::new(api);
    let origin = api.pause_origin().to_string();
    let start = Instant::now();

    let unpaced = {
        let api = Arc::clone(&api);
        tokio::spawn(async move { api.get_file(Uuid::new_v4().to_string()).await })
    };
    // Start the paced call only once the unpaced one's 429 has closed the gate (load-independent, not a sleep).
    while pauses(&origin) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let paced = {
        let api = Arc::clone(&api);
        let sid = sid.clone();
        tokio::spawn(async move {
            api.upload_chunk_typed(sid.as_str(), 0, Bytes::from(vec![0u8; 1024]))
                .await
                .expect("chunk")
        })
    };
    let answer = unpaced.await.expect("task");
    let unpaced_back = start.elapsed();
    paced.await.expect("task");

    assert!(answer.is_err(), "the 429 is the caller's answer, after the pause");
    assert!(
        unpaced_back >= Duration::from_millis(1_900),
        "returned after {unpaced_back:?}, not at once"
    );
    let entries = log.entries();
    let refused = entries.iter().find(|r| r.status == 429).expect("the 429");
    let put = entries.iter().find(|r| r.method == "PUT").expect("the paced PUT");
    assert!(
        put.at_ms >= refused.at_ms + 1_900,
        "the paced call waited for the gate: 429 at {}, PUT at {}",
        refused.at_ms,
        put.at_ms
    );
}

/// One 503 on chunk 1, then 200: the chunk lands on its second attempt, one pause counted. (The 503 line's text
/// is pinned by the unit test `the_429_line_is_todays_and_the_503_line_has_no_glyph_or_colour`.)
#[tokio::test]
async fn a_503_on_a_chunk_pauses_and_retries() {
    let mut mock = Mock::default();
    mock.chunk_busy_once.insert(1);
    mock.retry_after_secs = 1;
    let (api, log, sid, _) = client(mock).await;
    let origin = api.pause_origin().to_string();

    api.upload_chunk_typed(sid.as_str(), 1, Bytes::from(vec![0u8; 1024]))
        .await
        .expect("the chunk lands on the second attempt");

    let statuses: Vec<u16> = log.entries().iter().map(|r| r.status).collect();
    assert_eq!(statuses, [503, 200]);
    assert_eq!(pauses(&origin), 1);
}

/// A 503 every time spends the chunk's attempts (5 on the API route) and fails it: the pause never loops.
#[tokio::test]
async fn five_503s_fail_the_chunk() {
    let mut mock = Mock::default();
    mock.chunk_status.insert(1, 503);
    let (api, log, sid, _) = client(mock).await;

    let err = api
        .upload_chunk_typed(sid.as_str(), 1, Bytes::from(vec![0u8; 1024]))
        .await
        .expect_err("the attempts run out");

    assert!(
        err.message.contains("after 5 attempts") && err.message.contains("HTTP 503"),
        "{}",
        err.message
    );
    assert_eq!(log.entries().iter().filter(|r| r.status == 503).count(), 5);
}

/// A 60 s pause and the push's interrupt bound to the gate, set after one second: a chunk PUT waiting in the
/// pacer and an unpaced call waiting inside its own 429 branch both return at once.
#[tokio::test]
async fn ctrl_c_ends_a_shared_pause_through_the_pacer() {
    let mock = Mock {
        file_rate_limited_once: true,
        retry_after_secs: 60,
        ..Mock::default()
    };
    let (api, _, sid, _) = client(mock).await;
    let api = Arc::new(api);
    let origin = api.pause_origin().to_string();
    let interrupt = Interrupt::new();
    bind_interrupt(&origin, interrupt.clone());

    // The unpaced call meets its 429 first and waits inside the 429 branch.
    let in_branch = {
        let api = Arc::clone(&api);
        tokio::spawn(async move { api.get_file(Uuid::new_v4().to_string()).await })
    };
    // Start the pacer-bound call only once the gate is closed (load-independent), then let it reach the pacer.
    while pauses(&origin) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let in_pacer = {
        let api = Arc::clone(&api);
        let sid = sid.clone();
        tokio::spawn(async move {
            api.upload_chunk_typed(sid.as_str(), 0, Bytes::from(vec![0u8; 1024]))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let set_at = Instant::now();
    interrupt.set();

    let branch = tokio::time::timeout(Duration::from_secs(2), in_branch)
        .await
        .expect("the 429 branch returns within a tick of the interrupt")
        .expect("task");
    let pacer = tokio::time::timeout(Duration::from_secs(2), in_pacer)
        .await
        .expect("the pacer returns within a tick of the interrupt")
        .expect("task");
    assert!(set_at.elapsed() < Duration::from_secs(2));
    assert_eq!(
        branch.unwrap_err().message,
        crate::upload::INTERRUPTED,
        "an interrupted 429 wait is the interrupt, not a rate-limit retry"
    );
    assert_eq!(pacer.unwrap_err().message, crate::upload::INTERRUPTED);
}

/// `reason_for`: the two statuses that pause, and nothing else.
#[test]
fn only_a_429_or_503_pauses() {
    assert_eq!(reason_for(429), Some(PauseReason::RateLimited { remaining: None }));
    assert_eq!(reason_for(503), Some(PauseReason::Busy503));
    for status in [0, 200, 400, 404, 409, 500, 502, 504] {
        assert_eq!(reason_for(status), None, "{status}");
    }
    // A gate keyed by a base URL with a trailing slash is the gate of every URL under it.
    extend("http://127.0.0.1:7/", Duration::from_secs(60), PauseReason::Busy503);
    assert!(
        pauses("http://127.0.0.1:7/api/v1/files/x") >= 1,
        "one gate per origin, however the key is spelled"
    );
}
