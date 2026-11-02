//! Storage-side HTTP: the per-route timeouts, capped/exact body readers, and
//! the reqwest clients built for presigned storage URLs (https-only unless the
//! API origin is loopback http).
//!
//! Owns: the timeout budgets, `read_capped`/`read_exact_len`, the storage
//! client builders and the PUT base/throughput constants.
//! Must not: interpret an answer or decide a fallback (see `direct::retry`,
//! `direct::client`).

use super::*;

pub(super) const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) const GRANT_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) const CHUNK_API_TIMEOUT: Duration = Duration::from_secs(600);

/// A storage transfer may take as long as the link needs, but it must keep moving: no byte for
/// this long and the request fails (and the chunk is renewed or taken over by the API).
pub(super) const STORAGE_READ_TIMEOUT_RELEASE: Duration = Duration::from_secs(60);

/// What the code uses: the release value, except in this crate's own tests, which compress the two storage budgets
/// (this and [`STORAGE_PUT_BASE`]) so a test that must outlive them does not wait a minute. The `_RELEASE` constants
/// are pinned by `the_storage_budgets_of_a_release_build_are_what_the_docs_say`; the arm that SHIPS, which a test build
/// never takes, is pinned by the `const _` block below, which fails the build of anything that is not a test.
pub(super) const STORAGE_READ_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(3)
} else {
    STORAGE_READ_TIMEOUT_RELEASE
};

pub(super) const STORAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Unix milliseconds: the clock the planner and the discovery cache are given.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Read a body incrementally, giving up (`None`) when it passes `max` bytes.
pub(super) async fn read_capped(resp: &mut reqwest::Response, max: usize) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(piece) = resp.chunk().await.ok()? {
        if body.len() + piece.len() > max {
            return None;
        }
        body.extend_from_slice(&piece);
    }
    Some(body)
}

/// What reading a body that must be exactly `expected` bytes long came to.
pub(super) enum Exact {
    Body(Vec<u8>),
    /// More or fewer bytes than the manifest states: asking again will not change that.
    WrongLength,
    /// The connection broke while reading: another try may well work.
    Broken,
}

pub(super) async fn read_exact_len(resp: &mut reqwest::Response, expected: u64, idle: Duration) -> Exact {
    let mut body = Vec::new();
    loop {
        match tokio::time::timeout(idle, resp.chunk()).await {
            Err(_) | Ok(Err(_)) => return Exact::Broken,
            Ok(Ok(None)) => break,
            Ok(Ok(Some(piece))) => {
                if (body.len() as u64).saturating_add(piece.len() as u64) > expected {
                    return Exact::WrongLength;
                }
                body.extend_from_slice(&piece);
            }
        }
    }
    if body.len() as u64 == expected {
        Exact::Body(body)
    } else {
        Exact::WrongLength
    }
}

/// Turn a finished response into what the classifier needs, reading at most
/// `max` body bytes.
pub(super) async fn answer_of(mut resp: reqwest::Response, max: usize) -> Seen {
    super::update_rate_state(resp.headers());
    let status = resp.status().as_u16();
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let content_type = header("content-type");
    let retry_after = header("retry-after");
    let body = read_capped(&mut resp, max).await.unwrap_or_default();
    let answer = dt::ApiAnswer::from_parts(status, content_type.as_deref(), &body, retry_after.as_deref());
    Seen {
        answer,
        body,
        retry_after_sent: retry_after.is_some(),
        retry_after_secs: retry_after.as_deref().and_then(|v| v.trim().parse::<u64>().ok()),
    }
}

/// An API answer as the classifier sees it, plus what it cannot tell us.
pub(super) struct Seen {
    pub(super) answer: dt::ApiAnswer,
    pub(super) body: Vec<u8>,
    /// Did the server send a `Retry-After` at all? (The classifier substitutes a default when not.)
    pub(super) retry_after_sent: bool,
    /// Its value when it is a number of seconds.
    pub(super) retry_after_secs: Option<u64>,
}

impl Seen {
    pub(super) fn no_answer() -> Self {
        Self {
            answer: dt::ApiAnswer::no_answer(),
            body: Vec::new(),
            retry_after_sent: false,
            retry_after_secs: None,
        }
    }
}

/// The client for presigned storage links (R1.11). Built once per `ApiClient`.
///
/// It is NOT `ApiClient::client`: that one sends `X-Beebeeb-*` headers on every
/// request, and a storage host must learn nothing about the account or the
/// client. This one has no default headers, no cookie store, never follows a
/// redirect, and refuses plain http unless the API itself is on loopback http
/// (local development). `None` if the client cannot be built: the caller then
/// stays on the API rather than use a less restricted client.
pub(super) fn build_storage_client(api_base: &str) -> Option<Client> {
    build_storage_client_with(api_base, STORAGE_READ_TIMEOUT)
}

/// [`build_storage_client`] with an explicit idle (no-byte) timeout. There is no total timeout: a
/// large chunk on a slow link is fine as long as bytes keep arriving.
pub(super) fn build_storage_client_with(api_base: &str, read_timeout: Duration) -> Option<Client> {
    storage_client_builder(api_base).read_timeout(read_timeout).build().ok()
}

/// The part of the storage clients both share: nothing about the account, no redirect, https only unless the
/// API itself is on loopback http (local development).
pub(super) fn storage_client_builder(api_base: &str) -> reqwest::ClientBuilder {
    let dev = reqwest::Url::parse(api_base)
        .map(|u| u.scheme() == "http" && matches!(u.host_str(), Some("127.0.0.1" | "localhost")))
        .unwrap_or(false);
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!dev)
        .connect_timeout(STORAGE_CONNECT_TIMEOUT)
}

/// The client for storage PUTs: [`build_storage_client`] WITHOUT the read timeout.
///
/// reqwest's `read_timeout` is one timer that starts when the request is sent and is not reset while the request
/// BODY goes out; it only restarts per read once the response is arriving. A PUT of a 128 MiB chunk therefore
/// died at 60 s on any uplink under about 18 Mbit/s, and the file went back to the API after four such links.
/// A GET (the download client) sends no body, so its timer does what its name says: it resets per read, and a
/// transfer that keeps moving is never cut off (pinned by
/// `a_storage_transfer_that_keeps_moving_is_never_cut_off_but_one_that_stalls_is`). A PUT is bounded
/// by the total timeout `put_to_storage` sets per request, sized to the chunk.
pub(super) fn build_upload_storage_client(api_base: &str) -> Option<Client> {
    storage_client_builder(api_base).build().ok()
}

/// A storage PUT is bounded by one total timeout: a minute plus the chunk at this speed (about 1 Mbit/s). The
/// server's own fence assumes a minimum uplink of the same order. The upload client has no read timeout (see
/// [`build_upload_storage_client`]), so this is the only bound.
pub(super) const STORAGE_PUT_BASE_RELEASE: Duration = Duration::from_secs(60);

pub(super) const STORAGE_PUT_BASE: Duration = if cfg!(test) {
    Duration::from_secs(8)
} else {
    STORAGE_PUT_BASE_RELEASE
};

/// What ships, pinned where it is built: a test build takes the compressed arms above, so only a build that is not a
/// test can see (and refuse) an edit of the other arm.
#[cfg(not(test))]
const _: () = {
    assert!(STORAGE_READ_TIMEOUT.as_millis() == 60_000);
    assert!(STORAGE_PUT_BASE.as_millis() == 60_000);
};

pub(super) const STORAGE_PUT_MIN_BYTES_PER_SEC: u64 = 128 * 1024;

/// The total time one PUT of `len` bytes may take.
pub(super) fn put_timeout(base: Duration, len: u64) -> Duration {
    base + Duration::from_secs(len / STORAGE_PUT_MIN_BYTES_PER_SEC)
}
