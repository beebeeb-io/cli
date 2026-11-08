//! The direct-upload client against a scripted API: the grant, the PUT and the
//! commit as the server really speaks them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use std::time::{Duration, Instant};

use super::{
    ChunkApiError, DirectState, DiscoveryState, GrantReply, PinnedRetry, REFUSAL_HOLD, Refusal, STORAGE_PUT_BASE,
    STORAGE_PUT_BASE_RELEASE, STORAGE_PUT_MIN_BYTES_PER_SEC, STORAGE_READ_TIMEOUT, STORAGE_READ_TIMEOUT_RELEASE, Said,
    build_storage_client, build_storage_client_with, dt, put_timeout,
};
use crate::api::ApiClient;

/// A mock API whose discovery route counts its hits and offers (or not) direct downloads.
async fn mock_discovery(offers: bool) -> (ApiClient, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().route(
        "/api/v1/transfer/capabilities",
        axum::routing::get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                axum::Json(serde_json::json!({
                    "download": if offers { vec!["object-download-v1"] } else { vec![] },
                    "upload": [],
                    "storage_hosts": [],
                    "storage_buckets": [],
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (ApiClient::new_for_test(format!("http://{addr}")), hits)
}

#[tokio::test]
async fn discovery_is_asked_once_and_its_answer_is_honoured_every_time() {
    // A server that offers nothing: no direct transport, from the cache as from the wire.
    let (api, hits) = mock_discovery(false).await;
    for round in 1..=3 {
        assert!(api.direct_download_offer().await.is_none(), "round {round}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one request, then the cache");
    // A server that offers it: the same, and the answer is the offer.
    let (api, hits) = mock_discovery(true).await;
    for round in 1..=3 {
        let offer = api.direct_download_offer().await.expect("offered");
        assert!(offer.offers_download(), "round {round}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_storage_client_is_https_only_unless_the_api_is_loopback_http() {
    // Production API: a plain-http storage URL is refused before any connection.
    let client = build_storage_client("https://api.beebeeb.test").expect("client");
    let err = client.get("http://storage.test/b/k").send().await.unwrap_err();
    assert!(err.is_builder(), "plain http must be refused up front, got: {err}");
    // Development against a loopback http API: http is allowed (this one then fails to connect).
    for api in ["http://127.0.0.1:3001", "http://localhost:3001"] {
        let dev = build_storage_client(api).expect("client");
        let err = dev.get("http://127.0.0.1:9/b/k").send().await.unwrap_err();
        assert!(!err.is_builder(), "{api}: http must reach the connector, got: {err}");
    }
    // A look-alike is not loopback.
    let fake = build_storage_client("http://localhost.evil.test:3001").expect("client");
    let err = fake.get("http://127.0.0.1:9/b/k").send().await.unwrap_err();
    assert!(err.is_builder(), "a look-alike API base must not enable plain http");
}

// ── the discovery state of a long-running process ──

const TTL: i64 = dt::DISCOVERY_TTL_MS;

fn offers() -> dt::Discovery {
    dt::Discovery {
        download: vec![dt::CAP_OBJECT_DOWNLOAD_V1.to_string()],
        ..Default::default()
    }
}

#[test]
fn a_discovery_that_gave_up_asking_asks_again_once_the_trust_period_has_passed() {
    let mut state = DiscoveryState::new();
    let t0 = 1_000_000;
    // Two failed attempts: core stops asking.
    assert_eq!(state.lookup(t0), dt::Lookup::Fetch);
    state.record(&dt::Outcome::UseApiNow, None, t0);
    assert_eq!(state.lookup(t0 + 1), dt::Lookup::Fetch, "one retry per command");
    state.record(&dt::Outcome::UseApiNow, None, t0 + 1);
    assert_eq!(state.lookup(t0 + 2), dt::Lookup::UseApi, "given up");
    // Still given up just before the trust period (counted from the last failure) ends ...
    assert_eq!(state.lookup(t0 + 1 + TTL - 1), dt::Lookup::UseApi);
    // ... and asking again once it has, with a fresh allowance.
    assert_eq!(state.lookup(t0 + 1 + TTL), dt::Lookup::Fetch);
    state.record(&dt::Outcome::Proceed, Some(offers()), t0 + 2 + TTL);
    assert!(matches!(state.lookup(t0 + 3 + TTL), dt::Lookup::Cached(d) if d.offers_download()));

    // The fresh start is a fresh allowance, not the old strike count: after the trust period
    // a failed answer is retried once more before the cache gives up again.
    let mut state = DiscoveryState::new();
    for t in [t0, t0 + 1] {
        state.record(&dt::Outcome::UseApiNow, None, t);
    }
    assert_eq!(state.lookup(t0 + 2), dt::Lookup::UseApi);
    let later = t0 + 1 + TTL;
    assert_eq!(state.lookup(later), dt::Lookup::Fetch);
    state.record(&dt::Outcome::UseApiNow, None, later);
    assert_eq!(
        state.lookup(later + 1),
        dt::Lookup::Fetch,
        "one more try, as for a new command"
    );
}

#[test]
fn a_server_that_said_it_has_no_direct_transport_is_asked_again_after_the_trust_period() {
    // Core's rule (2aa1f39): the JSON 404 is remembered for the trust period, not for ever, so a
    // daemon notices the day the server gains the transport (a deploy turns the flag on).
    let mut state = DiscoveryState::new();
    state.record(&dt::Outcome::NotSupported, None, 0);
    assert_eq!(state.lookup(1), dt::Lookup::UseApi);
    assert_eq!(
        state.lookup(TTL - 1),
        dt::Lookup::UseApi,
        "an answer holds for the trust period"
    );
    assert_eq!(state.lookup(TTL), dt::Lookup::Fetch, "and is asked again after it");
}

#[test]
fn a_clock_that_goes_backwards_does_not_keep_a_discovery_parked() {
    let mut state = DiscoveryState::new();
    for t in [5_000_000, 5_000_001] {
        assert_eq!(state.lookup(t), dt::Lookup::Fetch);
        state.record(&dt::Outcome::UseApiNow, None, t);
    }
    assert_eq!(state.lookup(5_000_002), dt::Lookup::UseApi);
    assert_eq!(state.lookup(4_000_000), dt::Lookup::Fetch);
}

// ── the hold after a direct upload was given up ──

#[tokio::test(start_paused = true)]
async fn a_given_up_direct_upload_holds_for_the_trust_period_on_the_monotonic_clock() {
    // The tokio clock is paused, so only `advance` moves time: a hold read off the wall clock would never end
    // here (and a wall-clock step in production must neither end it early nor stretch it).
    let api = ApiClient::new_for_test("http://127.0.0.1:3001".to_string());
    assert!(!api.direct_upload_failed_recently(), "nothing was given up yet");
    api.note_direct_upload_failed();
    assert!(api.direct_upload_failed_recently());
    let ttl = Duration::from_millis(TTL as u64);
    tokio::time::advance(ttl - Duration::from_millis(1)).await;
    assert!(
        api.direct_upload_failed_recently(),
        "held until the trust period is over"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(
        !api.direct_upload_failed_recently(),
        "and not a moment longer: direct is tried again"
    );
    // Giving up again starts a new hold.
    api.note_direct_upload_failed();
    assert!(api.direct_upload_failed_recently());
}

#[test]
fn a_hold_is_counted_from_when_the_direct_upload_was_given_up_not_from_when_it_is_asked() {
    let state = DirectState::with_holds(Duration::from_secs(10), REFUSAL_HOLD);
    let t0 = tokio::time::Instant::now();
    assert!(!state.upload_failed_within_hold(t0), "never given up");
    state.note_upload_failed(t0);
    assert!(state.upload_failed_within_hold(t0));
    assert!(state.upload_failed_within_hold(t0 + Duration::from_millis(9_999)));
    assert!(!state.upload_failed_within_hold(t0 + Duration::from_secs(10)));
    // A zero hold never holds (the test constructor that tells the per-file bit from this memory).
    let none = DirectState::with_holds(Duration::ZERO, REFUSAL_HOLD);
    none.note_upload_failed(t0);
    assert!(!none.upload_failed_within_hold(t0));
}

// ── the storage client keeps moving, or stops ──

/// A storage server that answers `200` with `len` bytes, `pieces` pieces `gap` apart (the last
/// piece never comes when `stall` is set).
fn slow_storage(len: usize, pieces: usize, gap: Duration, stall: bool) -> std::net::SocketAddr {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                let mut byte = [0u8; 1];
                while !seen.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    seen.push(byte[0]);
                }
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n");
                let piece = vec![7u8; len / pieces];
                for n in 0..pieces {
                    if stall && n == pieces - 1 {
                        std::thread::sleep(Duration::from_secs(30));
                        return;
                    }
                    if stream.write_all(&piece).is_err() {
                        return;
                    }
                    let _ = stream.flush();
                    std::thread::sleep(gap);
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn a_storage_transfer_that_keeps_moving_is_never_cut_off_but_one_that_stalls_is() {
    let idle = Duration::from_millis(400);
    let client = build_storage_client_with("http://127.0.0.1:3001", idle).expect("client");
    // Five pieces 200 ms apart: a second in all, far past the idle limit, but never idle for long.
    let addr = slow_storage(5_000, 5, Duration::from_millis(200), false);
    let started = Instant::now();
    let body = client
        .get(format!("http://{addr}/b/k"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .expect("a slow but steady body arrives");
    assert_eq!(body.len(), 5_000);
    assert!(started.elapsed() > idle, "the transfer outlived the idle limit");
    // The last piece never comes: the request fails after the idle limit, not after minutes.
    let addr = slow_storage(5_000, 5, Duration::from_millis(50), true);
    let started = Instant::now();
    let result = client
        .get(format!("http://{addr}/b/k"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await;
    assert!(result.is_err(), "a stalled body must fail");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

// ── the pinned per-chunk route and the grant route, against a scripted API ──

#[derive(Clone)]
enum Reply {
    Status(u16),
    /// A JSON error body, optionally with a `Retry-After` (seconds).
    Json(u16, &'static str, Option<u32>),
    Status429(u32),
    /// A status with a `Retry-After` of this many seconds (the server's 503 sends 5).
    StatusRetry(u16, u32),
    Body(Vec<u8>),
}

fn reply_of(step: &Reply) -> axum::response::Response {
    use axum::response::IntoResponse;
    match step {
        Reply::Status(code) => axum::http::StatusCode::from_u16(*code).unwrap().into_response(),
        // The server's real 429 on /api/v1/files/* (rate_limit.rs `too_many_requests`): the
        // limiter's JSON body and a Retry-After (60 in practice).
        Reply::Status429(secs) => (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", secs.to_string())],
            axum::Json(serde_json::json!({
                "error": "rate_limited",
                "message": format!("Too many requests. Try again in {secs} seconds."),
                "retry_after": secs,
            })),
        )
            .into_response(),
        Reply::StatusRetry(code, secs) => (
            axum::http::StatusCode::from_u16(*code).unwrap(),
            [("retry-after", secs.to_string())],
        )
            .into_response(),
        Reply::Json(code, error, retry) => {
            let mut r = (
                axum::http::StatusCode::from_u16(*code).unwrap(),
                axum::Json(serde_json::json!({ "error": error })),
            )
                .into_response();
            if let Some(secs) = retry {
                r.headers_mut().insert("retry-after", secs.to_string().parse().unwrap());
            }
            r
        }
        Reply::Body(bytes) => bytes.clone().into_response(),
    }
}

/// A mock API that answers the chunk route (and the grant route) from a script, one entry per
/// request; the last entry repeats.
async fn scripted_api(script: Vec<Reply>) -> (ApiClient, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let (counter, steps) = (hits.clone(), Arc::new(script));
    let handler = move || {
        let (counter, steps) = (counter.clone(), steps.clone());
        async move {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            reply_of(&steps[n.min(steps.len() - 1)])
        }
    };
    let app = axum::Router::new()
        .route("/api/v1/files/:id/chunks/:index", axum::routing::get(handler.clone()))
        .route("/api/v1/files/:id/download-grants", axum::routing::post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (ApiClient::new_for_test(format!("http://{addr}")), hits)
}

/// What the server's pinned chunk route says for a version that is gone (its
/// `version_download.rs::unavailable`), as `{"error": <this>}`.
const GONE_VERSION: &str = "download version unavailable; restart download from fresh metadata";

const FAST: PinnedRetry = PinnedRetry {
    attempts: 4,
    budget: Duration::from_secs(5),
    base: Duration::from_millis(10),
    idle: Duration::from_millis(400),
};

#[test]
fn the_real_budget_holds_the_servers_own_429_wait_and_a_ban() {
    use super::{PINNED_RETRY, next_wait};
    let s = Duration::from_secs;
    // The server's real 429 says Retry-After: 60. First failure, nothing waited yet: wait it.
    assert_eq!(next_wait(PINNED_RETRY, 1, s(0), s(60)), Some(s(60)));
    // ... also when an earlier hiccup already cost a second (attempt 2, 1 s waited) ...
    assert_eq!(next_wait(PINNED_RETRY, 2, s(1), s(60)), Some(s(60)));
    // ... and a second 429 right after the first is waited for as well.
    assert_eq!(next_wait(PINNED_RETRY, 2, s(60), s(60)), Some(s(60)));
    // A ban is clamped to 120 s by core and is still waited for once.
    assert_eq!(next_wait(PINNED_RETRY, 1, s(0), s(120)), Some(s(120)));
    // What is not bounded away: the attempts, and the time budget, however the waits are named.
    // The budget is taken up to and including its last second, not beyond.
    assert_eq!(next_wait(PINNED_RETRY, 2, s(90), s(60)), Some(s(60)));
    assert_eq!(next_wait(PINNED_RETRY, 2, s(91), s(60)), None);
    assert_eq!(
        next_wait(PINNED_RETRY, 4, s(0), s(1)),
        None,
        "the fourth attempt was the last"
    );
    assert_eq!(
        next_wait(PINNED_RETRY, 3, s(120), s(60)),
        None,
        "180 s is past the budget"
    );
    assert!(PINNED_RETRY.budget >= dt::classify::MAX_RETRY_AFTER + s(30));
}

async fn pinned(api: &ApiClient, retry: PinnedRetry, len: u64) -> Result<Vec<u8>, ChunkApiError> {
    api.download_chunk_pinned_with(retry, "f", 3, "v", len).await
}

fn failed(e: ChunkApiError) -> String {
    match e {
        ChunkApiError::Failed(m) => m,
        ChunkApiError::Terminal(m) => panic!("terminal: {m}"),
        ChunkApiError::Restart => panic!("restart"),
    }
}

#[tokio::test]
async fn the_pinned_route_retries_what_may_pass_and_gives_the_chunk() {
    // A 5xx, then a 5xx, then the chunk.
    let (api, hits) = scripted_api(vec![Reply::Status(503), Reply::Status(500), Reply::Body(vec![9; 5])]).await;
    assert_eq!(pinned(&api, FAST, 5).await.ok(), Some(vec![9; 5]));
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    // A 429 with Retry-After: 1 waits that second, then the chunk.
    let (api, hits) = scripted_api(vec![Reply::Status429(1), Reply::Body(vec![9; 5])]).await;
    let started = Instant::now();
    assert_eq!(pinned(&api, FAST, 5).await.ok(), Some(vec![9; 5]));
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "Retry-After was honoured"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // A file held by another operation (409, Retry-After: 1) is waited for the same way.
    let (api, hits) = scripted_api(vec![
        Reply::Json(409, "replace_in_progress", Some(1)),
        Reply::Body(vec![9; 5]),
    ])
    .await;
    let started = Instant::now();
    assert_eq!(pinned(&api, FAST, 5).await.ok(), Some(vec![9; 5]));
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_server_error_with_a_retry_after_is_waited_for_as_told() {
    // The server's 503 says `Retry-After: 5` (error.rs): not "retry at once", not "retry whenever".
    let (api, hits) = scripted_api(vec![Reply::StatusRetry(503, 1), Reply::Body(vec![9; 5])]).await;
    let started = Instant::now();
    assert_eq!(pinned(&api, FAST, 5).await.ok(), Some(vec![9; 5]));
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // Too long for the budget: not waited for, and not a hang either.
    let (api, hits) = scripted_api(vec![Reply::StatusRetry(503, 90)]).await;
    let started = Instant::now();
    failed(pinned(&api, FAST, 5).await.unwrap_err());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_stated_wait_is_core_clamped_and_only_a_server_error_has_one() {
    // `Retry-After: 0` on a 503 means "at least a second", like a 429's.
    let (api, hits) = scripted_api(vec![Reply::StatusRetry(503, 0), Reply::Body(vec![9; 5])]).await;
    let started = Instant::now();
    assert_eq!(pinned(&api, FAST, 5).await.ok(), Some(vec![9; 5]));
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // A 404 (a node without the route) that happens to carry a Retry-After is the client's own
    // backoff, not a stated wait: the header is read for a server error only.
    let (api, hits) = scripted_api(vec![Reply::StatusRetry(404, 3), Reply::Body(vec![9; 5])]).await;
    let started = Instant::now();
    assert_eq!(pinned(&api, FAST, 5).await.ok(), Some(vec![9; 5]));
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_attempt_that_hears_nothing_fails_after_the_idle_limit_and_is_tried_again() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let seen = connections.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let n = seen.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    request.push(byte[0]);
                }
                match n {
                    // Accepts the request and never answers.
                    0 => std::thread::sleep(Duration::from_secs(30)),
                    // Answers, sends two of five bytes, goes quiet.
                    1 => {
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n");
                        let _ = stream.write_all(&[9, 9]);
                        let _ = stream.flush();
                        std::thread::sleep(Duration::from_secs(30));
                    }
                    _ => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(&[9; 5]);
                    }
                }
            });
        }
    });
    let api = ApiClient::new_for_test(format!("http://{addr}"));
    let started = Instant::now();
    assert_eq!(pinned(&api, FAST, 5).await.ok(), Some(vec![9; 5]));
    assert_eq!(connections.load(Ordering::SeqCst), 3);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(700),
        "two idle limits of 400 ms: {took:?}"
    );
    assert!(
        took < Duration::from_secs(6),
        "not the request's ten-minute ceiling: {took:?}"
    );
}

#[tokio::test]
async fn the_pinned_route_gives_up_after_its_attempts_or_when_a_wait_passes_the_budget() {
    // A server that keeps failing: exactly `attempts` requests, then the failure.
    let (api, hits) = scripted_api(vec![Reply::Status(500)]).await;
    let message = failed(pinned(&api, FAST, 5).await.unwrap_err());
    assert!(message.contains("500"), "{message}");
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    // With nothing named by the server the waits double: base, 2 x base, 4 x base.
    let slow = PinnedRetry {
        base: Duration::from_millis(100),
        ..FAST
    };
    let (api, _) = scripted_api(vec![Reply::Status(500)]).await;
    let started = Instant::now();
    failed(pinned(&api, slow, 5).await.unwrap_err());
    assert!(
        started.elapsed() >= Duration::from_millis(650),
        "{:?}",
        started.elapsed()
    );
    // A Retry-After longer than the whole budget: one request, no waiting at all.
    let (api, hits) = scripted_api(vec![Reply::Status429(30)]).await;
    let started = Instant::now();
    failed(pinned(&api, FAST, 5).await.unwrap_err());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn the_pinned_route_does_not_retry_what_a_retry_cannot_change() {
    for (reply, want) in [
        (Reply::Status(401), "session"),
        (Reply::Status(403), "403"),
        (Reply::Json(404, "not found", None), "404"),
    ] {
        let (api, hits) = scripted_api(vec![reply]).await;
        match pinned(&api, FAST, 5).await {
            Err(ChunkApiError::Terminal(m)) => {
                assert!(m.to_lowercase().contains(want), "{m} should mention {want}")
            }
            other => panic!("expected terminal, got {:?}", other.err().map(failed)),
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1, "{want}");
    }
    // A gone pin: the server's real body (`routes/version_download.rs`), a bare 409, and any
    // other 409 text all restart the file. `version_unavailable` is the GRANT route's code.
    for reply in [
        Reply::Json(409, GONE_VERSION, None),
        Reply::Status(409),
        Reply::Json(409, "something the server may say tomorrow", None),
    ] {
        let (api, hits) = scripted_api(vec![reply]).await;
        assert!(matches!(pinned(&api, FAST, 5).await, Err(ChunkApiError::Restart)));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
    // A refusal that will repeat (a malformed request).
    let (api, hits) = scripted_api(vec![Reply::Status(400)]).await;
    failed(pinned(&api, FAST, 5).await.unwrap_err());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // A body of the wrong length (short or long) is a disagreement with the manifest.
    for body in [vec![9u8; 4], vec![9u8; 6]] {
        let (api, hits) = scripted_api(vec![Reply::Body(body)]).await;
        let message = failed(pinned(&api, FAST, 5).await.unwrap_err());
        assert!(message.contains("manifest"), "{message}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn a_connection_that_breaks_mid_body_is_tried_again() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let seen = connections.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let n = seen.fetch_add(1, Ordering::SeqCst);
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                request.push(byte[0]);
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n"
            );
            // The first connection dies after two of five bytes.
            let _ = stream.write_all(if n == 0 { &[9, 9] } else { &[9, 9, 9, 9, 9] });
        }
    });
    let api = ApiClient::new_for_test(format!("http://{addr}"));
    assert_eq!(pinned(&api, FAST, 5).await.ok(), Some(vec![9; 5]));
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

fn grant_request() -> dt::DownloadGrantRequest {
    dt::DownloadGrantRequest {
        object_version_id: None,
        start_index: 0,
        count: 8,
        renewal: false,
    }
}

#[tokio::test]
async fn a_held_file_with_no_retry_after_goes_straight_to_the_api_but_a_stated_wait_is_kept() {
    // Another device is still uploading it: no waiting, the API path answers at once.
    let (api, hits) = scripted_api(vec![Reply::Json(409, "replace_in_progress", None)]).await;
    assert!(matches!(
        api.download_grants("f", &grant_request()).await,
        GrantReply::Failed(dt::Outcome::ApiForThisFile)
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // The server named a wait: wait that long.
    let (api, _) = scripted_api(vec![Reply::Json(409, "replace_in_progress", Some(7))]).await;
    assert!(matches!(
        api.download_grants("f", &grant_request()).await,
        GrantReply::Failed(dt::Outcome::Backoff(d)) if d == Duration::from_secs(7)
    ));
    // A rate limit is a wait with or without a stated time.
    let (api, _) = scripted_api(vec![Reply::Status(429)]).await;
    assert!(matches!(
        api.download_grants("f", &grant_request()).await,
        GrantReply::Failed(dt::Outcome::Backoff(_))
    ));
}

// ── review round: the PUT client, the process-wide direct state ──

#[test]
fn the_direct_state_is_one_per_api_base_for_the_whole_process() {
    let a = super::DirectState::shared("http://127.0.0.1:41001");
    assert!(
        std::sync::Arc::ptr_eq(&a, &super::DirectState::shared("http://127.0.0.1:41001")),
        "same base, same state"
    );
    assert!(
        std::sync::Arc::ptr_eq(&a, &super::DirectState::shared("http://127.0.0.1:41001/")),
        "a trailing slash is the same base"
    );
    assert!(
        !std::sync::Arc::ptr_eq(&a, &super::DirectState::shared("http://127.0.0.1:41002")),
        "another base, another state"
    );
    // The production constructor shares it: a daemon's per-resync clients are one client as far as discovery goes.
    let config = |url: &str| crate::config::Config {
        api_url: url.to_string(),
        ..crate::config::Config::default()
    };
    let one = ApiClient::with_config(config("http://127.0.0.1:41003"));
    let two = ApiClient::with_config(config("http://127.0.0.1:41003"));
    assert!(
        std::sync::Arc::ptr_eq(&one.direct, &two.direct),
        "two clients of one process and base share it"
    );
    assert!(!std::sync::Arc::ptr_eq(
        &one.direct,
        &ApiClient::with_config(config("http://127.0.0.1:41004")).direct
    ));
    // A test client has a state of its own: tests on different mock servers must not share a cache.
    let x = ApiClient::new_for_test("http://127.0.0.1:41001".to_string());
    let y = ApiClient::new_for_test("http://127.0.0.1:41001".to_string());
    assert!(!std::sync::Arc::ptr_eq(&x.direct, &y.direct));
}

/// A storage server that reads a PUT body slowly (`total` in all) and only then answers 200.
async fn slow_put_storage(total: Duration) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if sock.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&head).to_lowercase();
                let len: usize = text
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                let mut buf = vec![0u8; 1 << 16];
                let mut left = len;
                while left > 0 {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let n = n.min(left);
                    left -= n;
                    // `total` for the whole body, in proportion to what was read
                    tokio::time::sleep(total.mul_f64(n as f64 / len as f64)).await;
                }
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
            });
        }
    });
    (addr, task)
}

fn link_to(addr: std::net::SocketAddr) -> dt::UploadLink {
    let discovery = dt::Discovery {
        upload: vec![dt::CAP_OBJECT_UPLOAD_V1.to_string()],
        storage_hosts: vec![format!("127.0.0.1:{}", addr.port())],
        storage_buckets: vec![dt::StorageBucket {
            host: format!("127.0.0.1:{}", addr.port()),
            bucket: "bb".to_string(),
        }],
        ..Default::default()
    };
    let body = format!(
        r#"{{"transport":"object-upload-v1","index":0,"method":"PUT","url":"http://127.0.0.1:{}/bb/k?X-Amz-Signature=SIG","headers":{{"content-type":"application/octet-stream"}},"expires_at":"2099-01-01T00:00:00Z"}}"#,
        addr.port()
    );
    match dt::UploadGrant::from_answer(body.as_bytes(), 0, &discovery, "http://127.0.0.1:3001").expect("a grant") {
        dt::UploadGrant::Link(link) => link,
        other => panic!("{other:?}"),
    }
}

// These two run in real time (seconds each, in parallel with the rest of the suite): reqwest's timers and real
// sockets do not mix with a paused clock. The test build compresses the two storage budgets (3 s read timeout,
// 8 s PUT base instead of 60 s), and the release values are pinned by their own test.

#[test]
fn the_storage_budgets_of_a_release_build_are_what_the_docs_say() {
    assert_eq!(STORAGE_READ_TIMEOUT_RELEASE, Duration::from_secs(60));
    assert_eq!(STORAGE_PUT_BASE_RELEASE, Duration::from_secs(60));
    assert_eq!(STORAGE_PUT_MIN_BYTES_PER_SEC, 128 * 1024);
    // A minute plus the chunk at 128 KiB/s: 92 s for a 4 MiB chunk, 60 s for nothing.
    assert_eq!(
        put_timeout(STORAGE_PUT_BASE_RELEASE, 4 * 1024 * 1024),
        Duration::from_secs(92)
    );
    assert_eq!(put_timeout(STORAGE_PUT_BASE_RELEASE, 0), Duration::from_secs(60));
    // The test build really is the compressed one (a test below is only meaningful if it is).
    assert!(STORAGE_READ_TIMEOUT < STORAGE_READ_TIMEOUT_RELEASE && STORAGE_PUT_BASE < STORAGE_PUT_BASE_RELEASE);
}

#[tokio::test]
async fn a_storage_put_that_takes_longer_than_the_read_timeout_to_be_answered_still_succeeds() {
    // A 4 MiB chunk on a slow uplink: the storage server takes 4 s to read it, longer than the download client's
    // read timeout (3 s in a test build, 60 s in a release). reqwest's read timeout is one timer from the moment
    // the request is sent, not reset while the request BODY goes out, so a client that has one fails this PUT.
    // The upload client has none; the per-request total timeout (the PUT base + the chunk at 128 KiB/s = 40 s
    // here) is the bound.
    let (addr, server) = slow_put_storage(STORAGE_READ_TIMEOUT + Duration::from_secs(1)).await;
    let link = link_to(addr);
    let data = bytes::Bytes::from(vec![7u8; 4 * 1024 * 1024]);
    let api = ApiClient::new_for_test("http://127.0.0.1:3001".to_string());
    let download_client = build_storage_client("http://127.0.0.1:3001").expect("client");
    let (upload, download) = tokio::join!(
        api.put_to_storage(&link, data.clone()),
        download_client
            .put(link.url.as_str())
            .body(data)
            .timeout(put_timeout(STORAGE_PUT_BASE, 4 * 1024 * 1024))
            .send()
    );
    assert_eq!(upload, 200, "the upload client waits for the answer");
    let err = download.expect_err("the download client's read timer cuts the same PUT off");
    assert!(err.is_timeout(), "{err}");
    server.abort();
}

#[tokio::test]
async fn a_storage_put_is_still_bounded_by_its_total_timeout() {
    // A server that never answers: the PUT ends at the PUT base + len / 128 KiB/s (the base for 1 KiB), not never.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hold = tokio::spawn(async move {
        let mut kept = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            kept.push(sock);
        }
    });
    let link = link_to(addr);
    let api = ApiClient::new_for_test("http://127.0.0.1:3001".to_string());
    let started = std::time::Instant::now();
    let status = tokio::time::timeout(
        STORAGE_PUT_BASE * 3,
        api.put_to_storage(&link, bytes::Bytes::from(vec![7u8; 1024])),
    )
    .await
    .expect("the PUT must end by itself");
    assert_eq!(status, 0, "no answer is status 0");
    let waited = started.elapsed();
    assert!(
        waited >= STORAGE_PUT_BASE - Duration::from_secs(1) && waited <= STORAGE_PUT_BASE + Duration::from_secs(10),
        "{waited:?}"
    );
    hold.abort();
}

/// A mock API whose capabilities route always answers `status` with `body` and counts its hits.
async fn mock_discovery_answering(status: u16, body: &'static str) -> (ApiClient, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().route(
        "/api/v1/transfer/capabilities",
        axum::routing::get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    [("content-type", "application/json")],
                    body,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (ApiClient::new_for_test(format!("http://{addr}")), hits)
}

#[tokio::test]
async fn a_servers_no_and_nobody_answering_are_told_apart_and_both_are_cached() {
    use super::DirectOffer;
    // The server says, in JSON, that it has no direct transport: a definite no, asked once, from the cache after.
    let (api, hits) = mock_discovery_answering(404, r#"{"error":"not found"}"#).await;
    for round in 1..=3 {
        assert!(
            matches!(api.direct_upload_offer().await, DirectOffer::NotOffered),
            "round {round}"
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one request, then the cache");

    // A discovery that lists no upload capability: also a definite no.
    let (api, _) = mock_discovery(true).await;
    assert!(
        matches!(api.direct_upload_offer().await, DirectOffer::NotOffered),
        "downloads only"
    );

    // Nobody answers (a 503): not a no. Asked twice (core's allowance), then the cache stops asking, still not a no.
    let (api, hits) = mock_discovery_answering(503, "{}").await;
    for round in 1..=4 {
        assert!(
            matches!(api.direct_upload_offer().await, DirectOffer::Unknown),
            "round {round}"
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2, "two attempts, then the cache gave up");

    // A node without the route (a bare 404) is not a JSON no either.
    let app = axum::Router::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let api = ApiClient::new_for_test(format!("http://{addr}"));
    assert!(
        matches!(api.direct_upload_offer().await, DirectOffer::Unknown),
        "a bare 404"
    );
}

/// A mock capabilities route whose status the test can change, counting its hits.
async fn mock_discovery_switch(
    initial: u16,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::atomic::AtomicU16>,
    Arc<AtomicUsize>,
) {
    let status = Arc::new(std::sync::atomic::AtomicU16::new(initial));
    let current = status.clone();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().route(
        "/api/v1/transfer/capabilities",
        axum::routing::get(move || {
            let code = current.load(Ordering::SeqCst);
            counter.fetch_add(1, Ordering::SeqCst);
            async move {
                if code == 200 {
                    (
                        axum::http::StatusCode::OK,
                        [("content-type", "application/json")],
                        r#"{"download":[],"upload":["object-upload-v1"],"storage_hosts":["127.0.0.1:1"],"storage_buckets":[{"host":"127.0.0.1:1","bucket":"b"}]}"#,
                    )
                } else {
                    (
                        axum::http::StatusCode::from_u16(code).unwrap(),
                        [("content-type", "application/json")],
                        r#"{"error":"account_suspended"}"#,
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, status, hits)
}

#[tokio::test]
async fn a_login_that_is_not_valid_is_told_apart_from_a_server_nobody_could_ask_and_is_asked_about_once() {
    use super::DirectOffer;
    // No session at all: nothing is sent, and the answer is not "unknown".
    let api = ApiClient::new_for_test_logged_out("http://127.0.0.1:9".to_string());
    assert!(
        matches!(
            api.direct_upload_offer().await,
            DirectOffer::Unauthorized(Refusal::NoSession)
        ),
        "not logged in"
    );
    // A 401 or a 403 on the capabilities route: the same, with its status and the server's code. The server is
    // asked once; the refusal is then answered from memory, so a revoked session does not cost one extra request
    // per file.
    for status in [401u16, 403] {
        let (addr, _, hits) = mock_discovery_switch(status).await;
        let api = ApiClient::new_for_test(format!("http://{addr}"));
        for round in 1..=4 {
            match api.direct_upload_offer().await {
                DirectOffer::Unauthorized(Refusal::Refused { status: got, code }) => {
                    assert_eq!(
                        (got, code.as_deref()),
                        (status, Some("account_suspended")),
                        "round {round}"
                    );
                }
                other => panic!("{status}, round {round}: {other:?}"),
            }
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "{status}: asked once, then answered from memory"
        );
        // Downloads do not care which kind of nothing it is.
        assert!(api.direct_download_offer().await.is_none());
    }
}

#[tokio::test]
async fn a_refusal_is_remembered_for_that_login_and_for_the_hold_only() {
    use super::DirectOffer;
    let (addr, status, hits) = mock_discovery_switch(401).await;
    let base = format!("http://{addr}");
    // One state for every client of the base, as the production constructor shares it; a short hold.
    let hold = Duration::from_millis(400);
    let state = Arc::new(DirectState::with_holds(Duration::from_secs(600), hold));
    let client = |token: &str| ApiClient::new_for_test_with(base.clone(), Some(token), state.clone());
    let revoked = client("revoked-token");
    for round in 1..=3 {
        assert!(
            matches!(revoked.direct_upload_offer().await, DirectOffer::Unauthorized(_)),
            "round {round}"
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the second and third answers came from memory"
    );
    // The server is fine again; the refused login is still answered from memory, ...
    status.store(200, Ordering::SeqCst);
    assert!(matches!(
        revoked.direct_upload_offer().await,
        DirectOffer::Unauthorized(_)
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // ... a login with another token (a `bb login` since) is asked at once and gets direct, ...
    assert!(
        matches!(
            client("fresh-token").direct_upload_offer().await,
            DirectOffer::Offered(_)
        ),
        "the next login is not held by the last one's refusal"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // ... and the first login is asked again once the hold is over: a transient refusal does not park it.
    let (addr, status, hits) = mock_discovery_switch(403).await;
    let state = Arc::new(DirectState::with_holds(Duration::from_secs(600), hold));
    let api = ApiClient::new_for_test_with(format!("http://{addr}"), Some("t"), state);
    assert!(matches!(api.direct_upload_offer().await, DirectOffer::Unauthorized(_)));
    // refused again after the hold: twice is what would park core's cache for ten minutes, were it told
    tokio::time::sleep(hold + Duration::from_millis(150)).await;
    assert!(matches!(api.direct_upload_offer().await, DirectOffer::Unauthorized(_)));
    assert_eq!(hits.load(Ordering::SeqCst), 2, "asked again once the hold was over");
    status.store(200, Ordering::SeqCst);
    tokio::time::sleep(hold + Duration::from_millis(150)).await;
    assert!(
        matches!(api.direct_upload_offer().await, DirectOffer::Offered(_)),
        "asked again after the hold, and not parked by the two refusals before it"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[test]
fn a_refusal_the_server_said_no_and_asking_failed_are_one_state_not_three_flags() {
    let now = tokio::time::Instant::now();
    let refusal = Refusal::Refused {
        status: 401,
        code: None,
    };
    let mut state = DiscoveryState::new();
    assert!(!state.said_no() && state.refusal_for(7, now, REFUSAL_HOLD).is_none());
    // a refusal, then the server's JSON no: the no wins, the refusal is gone
    state.record_refusal(7, refusal.clone(), now);
    assert!(!state.said_no() && state.refusal_for(7, now, REFUSAL_HOLD).is_some());
    state.record(&dt::Outcome::NotSupported, None, 0);
    assert!(state.said_no() && state.refusal_for(7, now, REFUSAL_HOLD).is_none());
    // the server's no, then a refusal of the login after the no expired: the no must not outlive it
    state.record_refusal(7, refusal.clone(), now);
    assert!(
        !state.said_no(),
        "a stale no must not turn a refusal into 'the server offers nothing'"
    );
    // a refusal, then a failure to ask: the failure says nothing about the login
    state.record(&dt::Outcome::UseApiNow, None, 1);
    assert!(!state.said_no() && state.refusal_for(7, now, REFUSAL_HOLD).is_none());
    // a refusal, then a good answer
    state.record_refusal(7, refusal.clone(), now);
    state.record(&dt::Outcome::Proceed, Some(offers()), 2);
    assert!(state.refusal_for(7, now, REFUSAL_HOLD).is_none());
    // only the refused login, only inside the hold
    state.record_refusal(7, refusal.clone(), now);
    assert!(state.refusal_for(8, now, REFUSAL_HOLD).is_none(), "another login");
    let before_end = now + REFUSAL_HOLD - Duration::from_millis(1);
    assert!(state.refusal_for(7, before_end, REFUSAL_HOLD).is_some());
    assert!(
        state.refusal_for(7, now + REFUSAL_HOLD, REFUSAL_HOLD).is_none(),
        "the hold is over"
    );
    let _ = Said::Nothing;
}

#[test]
fn a_refusal_says_what_it_was_in_the_words_the_cli_uses_for_it() {
    let refused = |status: u16, code: Option<&str>| Refusal::Refused {
        status,
        code: code.map(str::to_string),
    };
    assert_eq!(
        Refusal::NoSession.resume_message("a.bin"),
        crate::api::NOT_LOGGED_IN_MESSAGE
    );
    assert_eq!(
        refused(401, Some("unauthorized")).resume_message("a.bin"),
        crate::api::SESSION_EXPIRED_MESSAGE
    );
    let forbidden = refused(403, Some("account_suspended")).resume_message("a.bin");
    assert!(
        forbidden.starts_with("a.bin:") && forbidden.contains("account_suspended, HTTP 403"),
        "{forbidden}"
    );
    assert!(
        !forbidden.contains("bb login") && !forbidden.contains("--transport"),
        "{forbidden}"
    );
    assert!(refused(403, None).resume_message("a.bin").contains("(HTTP 403)"));
}
