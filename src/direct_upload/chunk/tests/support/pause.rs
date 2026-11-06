//! Task 4 of the `--parallel` plan (spec §6.1) on the direct route: a 429 on a grant and a 503 from storage
//! pause every request of the process for the wait core's policy ordered; any other failure keeps the chunk's
//! own jittered wait.

use super::*;
use crate::api::pause;
use crate::test_server::RequestLog;

async fn push_recorded(setup: impl FnOnce(&mut Mock)) -> (RequestLog, String, crate::upload::UploadOutcome) {
    let scratch = Scratch::new("direct-pause");
    let (path, _, _) = scratch.file(FILE_BYTES);
    let log = RequestLog::default();
    let state = shared(|m| {
        m.requests = Some(log.clone());
        setup(m);
    });
    let base = spawn(state.clone()).await;
    let api = crate::api::ApiClient::new_for_test(base);
    let origin = api.pause_origin().to_string();
    let key = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let out = stream_encrypt_upload(&api, key, spec(&path, Uuid::new_v4()), &NoopProgress)
        .await
        .expect("the file completes");
    (log, origin, out)
}

/// The gap, in ms, between the answer with `status` and the next request the mock saw.
fn gap_after(log: &RequestLog, status: u16) -> u64 {
    let entries = log.entries();
    let at = entries.iter().position(|r| r.status == status).expect("the answer");
    entries[at + 1].at_ms - entries[at].at_ms
}

#[tokio::test]
async fn a_429_on_a_direct_grant_pauses_every_request() {
    let (log, origin, out) = push_recorded(|m| {
        m.grant_script = VecDeque::from([Grant::Reply(429, json!({"error": "rate_limited"}), Some("1"))]);
    })
    .await;

    assert_eq!(out.transport, "direct");
    assert_eq!(pause::pauses(&origin), 1, "one pause");
    assert!(
        gap_after(&log, 429) >= 950,
        "the next request waited for the gate: {} ms",
        gap_after(&log, 429)
    );
}

#[tokio::test]
async fn a_503_from_storage_pauses_every_request() {
    let (log, origin, out) = push_recorded(|m| {
        m.storage_script = VecDeque::from([503]);
    })
    .await;

    assert_eq!(out.transport, "direct");
    assert_eq!(pause::pauses(&origin), 1, "one pause");
    // Core's first wait is 1 s doubling with equal jitter: at least half a second.
    assert!(
        gap_after(&log, 503) >= 450,
        "the next request waited for the gate: {} ms",
        gap_after(&log, 503)
    );
}

/// A failed storage PUT with any other status keeps the chunk's private wait: nothing is paused.
#[tokio::test]
async fn another_storage_failure_pauses_nothing() {
    let (_, origin, out) = push_recorded(|m| {
        m.storage_script = VecDeque::from([403]);
    })
    .await;

    assert_eq!(out.transport, "direct");
    assert_eq!(pause::pauses(&origin), 0, "a 403 is the chunk's own retry");
}
