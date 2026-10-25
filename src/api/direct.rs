//! The API calls of direct S3 transfers v1 (task 2173): discovery, download
//! grants and the pinned per-chunk fallback route, plus the HTTP client that
//! fetches presigned storage links.
//!
//! The DECISIONS (what an answer means, which links may be followed, when to
//! renew or fall back) live in `beebeeb_core::direct_transfer`, shared with
//! every other client; this file only makes the requests and reports what it
//! saw. Nothing here prints a URL: errors from the HTTP stack are run through
//! `dt::scrub` before they leave this module.

use std::time::Duration;

use beebeeb_core::direct_transfer as dt;
use reqwest::Client;

use super::ApiClient;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const GRANT_TIMEOUT: Duration = Duration::from_secs(60);
const CHUNK_API_TIMEOUT: Duration = Duration::from_secs(600);
/// A storage transfer may take as long as the link needs, but it must keep moving: no byte for
/// this long and the request fails (and the chunk is renewed or taken over by the API).
const STORAGE_READ_TIMEOUT: Duration = Duration::from_secs(60);
const STORAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Unix milliseconds: the clock the planner and the discovery cache are given.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The answer to a grant request.
pub(crate) enum GrantReply {
    Window(dt::DownloadGrantResponse),
    /// A 2xx whose body is not the contract's JSON.
    Malformed,
    /// Anything but a window: the classifier's verdict on the answer.
    Failed(dt::Outcome),
}

/// Why the pinned per-chunk API route did not give a chunk.
pub(crate) enum ChunkApiError {
    /// 409 `version_unavailable`: the pinned version is gone.
    Restart,
    /// 401/403 or a real not-found: do not retry.
    Terminal(String),
    /// Anything else (network, 5xx, wrong length).
    Failed(String),
}

/// How hard the pinned per-chunk route is tried before the chunk, and with it the file, goes to the
/// API stream. Grants and this route share the per-user rate limiter server-side, and a download that
/// has fallen back to it fetches four chunks at a time, so one 429 is likely and must not cost the
/// whole transfer.
#[derive(Clone, Copy)]
struct PinnedRetry {
    /// Attempts in all, the first one included.
    attempts: u32,
    /// The most the retries may wait in total; a wait that would pass it ends them. It must hold the
    /// longest wait a server can state (`MAX_RETRY_AFTER`, 120 s) plus a margin: the server's real 429
    /// on these routes says `Retry-After: 60`, and a budget of 60 s would give up on the first stated
    /// wait that follows any earlier one, or on a second 429, restarting the whole file.
    budget: Duration,
    /// The wait before the second attempt when the server named none; it doubles each time.
    base: Duration,
    /// An attempt that hears nothing for this long (no answer, or no body byte) has failed. Without
    /// it a stalled connection would hold an attempt for the request's 600 s ceiling.
    idle: Duration,
}

const PINNED_RETRY: PinnedRetry = PinnedRetry {
    attempts: 4,
    budget: Duration::from_secs(150),
    base: Duration::from_secs(1),
    idle: Duration::from_secs(60),
};

/// Whether to wait `wait` and try again after attempt number `attempt` (1-based) failed, with `waited`
/// already spent waiting: `None` when the attempts or the time budget are used up.
fn next_wait(retry: PinnedRetry, attempt: u32, waited: Duration, wait: Duration) -> Option<Duration> {
    if attempt >= retry.attempts || waited.saturating_add(wait) > retry.budget {
        None
    } else {
        Some(wait)
    }
}

/// One attempt of the pinned route that did not give the chunk.
enum PinnedFailure {
    /// Not worth another try (an authorization answer, a gone version, a refusal that will repeat).
    Final(ChunkApiError),
    /// Could be gone in a moment: a 429, a 5xx, a held file, a network blip, a bad read.
    /// `wait` is what the server asked for, if it did.
    Again { wait: Option<Duration>, message: String },
}

/// The wait a server names with `Retry-After: <seconds>`, clamped exactly like a 429's: core's
/// classifier decides the bounds, asked about a 429 that carries this header.
fn stated_wait(secs: Option<u64>) -> Option<Duration> {
    let raw = secs?.to_string();
    match dt::classify(
        dt::Call::DownloadGrant,
        &dt::ApiAnswer::from_parts(429, None, b"", Some(&raw)),
    ) {
        dt::Outcome::Backoff(wait) => Some(wait),
        _ => None,
    }
}

/// Read a body incrementally, giving up (`None`) when it passes `max` bytes.
async fn read_capped(resp: &mut reqwest::Response, max: usize) -> Option<Vec<u8>> {
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
enum Exact {
    Body(Vec<u8>),
    /// More or fewer bytes than the manifest states: asking again will not change that.
    WrongLength,
    /// The connection broke while reading: another try may well work.
    Broken,
}

async fn read_exact_len(resp: &mut reqwest::Response, expected: u64, idle: Duration) -> Exact {
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
async fn answer_of(mut resp: reqwest::Response, max: usize) -> Seen {
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
struct Seen {
    answer: dt::ApiAnswer,
    body: Vec<u8>,
    /// Did the server send a `Retry-After` at all? (The classifier substitutes a default when not.)
    retry_after_sent: bool,
    /// Its value when it is a number of seconds.
    retry_after_secs: Option<u64>,
}

impl Seen {
    fn no_answer() -> Self {
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
fn build_storage_client(api_base: &str) -> Option<Client> {
    build_storage_client_with(api_base, STORAGE_READ_TIMEOUT)
}

/// [`build_storage_client`] with an explicit idle (no-byte) timeout. There is no total timeout: a
/// large chunk on a slow link is fine as long as bytes keep arriving.
fn build_storage_client_with(api_base: &str, read_timeout: Duration) -> Option<Client> {
    let dev = reqwest::Url::parse(api_base)
        .map(|u| u.scheme() == "http" && matches!(u.host_str(), Some("127.0.0.1" | "localhost")))
        .unwrap_or(false);
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!dev)
        .connect_timeout(STORAGE_CONNECT_TIMEOUT)
        .read_timeout(read_timeout)
        .build()
        .ok()
}

/// The discovery cache. Core's cache already does what a long-running process needs (a sync daemon
/// that runs for weeks and meets one bad quarter hour): it forgets a failed discovery AND a JSON 404
/// "no direct transport" once the trust period has passed (core 2aa1f39), so the CLI keeps no rule of
/// its own on top of it. (Until the core pin moved past 3235dc15 this was a wrapper that re-implemented
/// the failure half and pinned the 404 half forever; the 404 half is now core's decision, see the
/// cache vectors in core's `test-vectors/direct_transfer.json`.)
pub(crate) type DiscoveryState = dt::DiscoveryCache;

impl ApiClient {
    /// The API origin this client talks to (the authority check needs it).
    pub(crate) fn api_base(&self) -> &str {
        &self.base_url
    }

    /// The storage-link client, or `None` when it could not be built.
    pub(crate) fn storage_client(&self) -> Option<&Client> {
        self.storage_client
            .get_or_init(|| build_storage_client(&self.base_url))
            .as_ref()
    }

    /// Does this server offer direct downloads right now? Asks the discovery
    /// route lazily and remembers the answer (R1.6); `None` means "use the API
    /// for this file", for any reason: forced with `--transport api`, an old
    /// server, a node without the route, a network failure, a server that
    /// offers nothing. An authorization answer (401/403) is NOT handled here:
    /// the API path that follows reports it in its own words.
    pub(crate) async fn direct_download_offer(&self) -> Option<dt::Discovery> {
        self.direct_offer(dt::Discovery::offers_download).await
    }

    /// [`direct_download_offer`](Self::direct_download_offer) for the upload direction (task 2173):
    /// the same lazily asked, cached discovery, filtered on `object-upload-v1`.
    pub(crate) async fn direct_upload_offer(&self) -> Option<dt::Discovery> {
        self.direct_offer(dt::Discovery::offers_upload).await
    }

    async fn direct_offer(&self, wanted: fn(&dt::Discovery) -> bool) -> Option<dt::Discovery> {
        if crate::transport::api_forced() || self.require_auth().is_err() {
            return None;
        }
        let mut state = self.direct_discovery.lock().await;
        match state.lookup(now_ms()) {
            dt::Lookup::Cached(discovery) => wanted(&discovery).then_some(discovery),
            dt::Lookup::UseApi => None,
            dt::Lookup::Fetch => {
                let (outcome, discovery) = self.fetch_discovery().await;
                state.record(&outcome, discovery.clone(), now_ms());
                discovery.filter(|d| wanted(d))
            }
        }
    }

    async fn fetch_discovery(&self) -> (dt::Outcome, Option<dt::Discovery>) {
        let Ok(token) = self.require_auth() else {
            return (dt::Outcome::UseApiNow, None);
        };
        // The capabilities header is NOT sent here: only grant requests carry it.
        let sent = self
            .client
            .get(self.url(dt::DISCOVERY_PATH))
            .bearer_auth(token)
            .timeout(DISCOVERY_TIMEOUT)
            .send()
            .await;
        let seen = match sent {
            Ok(resp) => answer_of(resp, dt::wire::MAX_DISCOVERY_BYTES).await,
            Err(_) => Seen::no_answer(),
        };
        match dt::classify(dt::Call::Discovery, &seen.answer) {
            dt::Outcome::Proceed => match dt::Discovery::from_json(&seen.body) {
                Ok(discovery) => (dt::Outcome::Proceed, Some(discovery)),
                Err(_) => (dt::Outcome::UseApiNow, None),
            },
            other => (other, None),
        }
    }

    /// `POST /api/v1/files/{id}/download-grants` with the capability header.
    pub(crate) async fn download_grants(&self, file_id: &str, request: &dt::DownloadGrantRequest) -> GrantReply {
        let Ok(token) = self.require_auth() else {
            return GrantReply::Failed(dt::Outcome::UseApiNow);
        };
        let sent = self
            .client
            .post(self.url(&dt::download_grants_path(file_id)))
            .bearer_auth(token)
            .header(dt::CAPABILITIES_HEADER, dt::CAP_OBJECT_DOWNLOAD_V1)
            .json(request)
            .timeout(GRANT_TIMEOUT)
            .send()
            .await;
        let seen = match sent {
            Ok(resp) => answer_of(resp, dt::wire::MAX_GRANT_BODY_BYTES).await,
            Err(_) => Seen::no_answer(),
        };
        match dt::classify(dt::Call::DownloadGrant, &seen.answer) {
            dt::Outcome::Proceed => match dt::DownloadGrantResponse::from_json(&seen.body) {
                Ok(window) => GrantReply::Window(window),
                Err(_) => GrantReply::Malformed,
            },
            // A 409 "held" with no Retry-After is not a moment's hold: another device is still
            // uploading this file. The API path answers that at once (a sync pass defers the
            // file); waiting here would cost every such file 5 x 5 s in every pass.
            dt::Outcome::Backoff(_) if seen.answer.status == 409 && !seen.retry_after_sent => {
                GrantReply::Failed(dt::Outcome::ApiForThisFile)
            }
            other => GrantReply::Failed(other),
        }
    }

    /// `GET /api/v1/files/{id}/chunks/{index}?object_version_id=<pinned>`: one
    /// chunk of the pinned version through the API, the fallback for a chunk
    /// storage could not give (and the re-fetch of one that failed to
    /// authenticate). `expected_len` is the stored length the grant manifest
    /// states for this chunk.
    ///
    /// A transient failure (429, 5xx, a held file, a network blip) is retried a few times, honouring
    /// `Retry-After`; only when that is spent does the caller give the file to the API stream.
    pub(crate) async fn download_chunk_pinned(
        &self,
        file_id: &str,
        index: u32,
        version: &str,
        expected_len: u64,
    ) -> Result<Vec<u8>, ChunkApiError> {
        self.download_chunk_pinned_with(PINNED_RETRY, file_id, index, version, expected_len)
            .await
    }

    async fn download_chunk_pinned_with(
        &self,
        retry: PinnedRetry,
        file_id: &str,
        index: u32,
        version: &str,
        expected_len: u64,
    ) -> Result<Vec<u8>, ChunkApiError> {
        let mut waited = Duration::ZERO;
        let mut attempt: u32 = 1;
        loop {
            let (wait, message) = match self
                .pinned_attempt(retry.idle, file_id, index, version, expected_len)
                .await
            {
                Ok(bytes) => return Ok(bytes),
                Err(PinnedFailure::Final(e)) => return Err(e),
                Err(PinnedFailure::Again { wait, message }) => {
                    let doubling = 2u32.saturating_pow(attempt - 1);
                    (wait.unwrap_or_else(|| retry.base.saturating_mul(doubling)), message)
                }
            };
            let Some(wait) = next_wait(retry, attempt, waited, wait) else {
                return Err(ChunkApiError::Failed(message));
            };
            tokio::time::sleep(wait).await;
            waited += wait;
            attempt += 1;
        }
    }

    async fn pinned_attempt(
        &self,
        idle: Duration,
        file_id: &str,
        index: u32,
        version: &str,
        expected_len: u64,
    ) -> Result<Vec<u8>, PinnedFailure> {
        let token = self
            .require_auth()
            .map_err(|m| PinnedFailure::Final(ChunkApiError::Terminal(m)))?;
        let sending = self
            .client
            .get(self.url(&format!("/api/v1/files/{file_id}/chunks/{index}")))
            .query(&[("object_version_id", version)])
            .bearer_auth(token)
            .timeout(CHUNK_API_TIMEOUT)
            .send();
        let resp = match tokio::time::timeout(idle, sending).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                return Err(PinnedFailure::Again {
                    wait: None,
                    message: dt::scrub(&e.to_string()),
                });
            }
            Err(_) => {
                return Err(PinnedFailure::Again {
                    wait: None,
                    message: format!("download chunk {index}: no answer within {} s", idle.as_secs()),
                });
            }
        };
        let status = resp.status();
        if !status.is_success() {
            let seen = answer_of(resp, 64 * 1024).await;
            // On this route a 409 means the pinned version is gone. The server's body is the
            // sentence "download version unavailable; restart download from fresh metadata" (not
            // the grant route's `version_unavailable` code), so the status decides, whatever the
            // text says. Only "another operation holds the file" is a wait.
            if seen.answer.status == 409
                && !matches!(
                    seen.answer.error_code(),
                    Some("replace_in_progress" | "commit_in_progress")
                )
            {
                return Err(PinnedFailure::Final(ChunkApiError::Restart));
            }
            // Otherwise the same questions as for a grant: authorization is final, "held" and
            // rate limits wait, a server hiccup is tried again.
            let failed = format!("download chunk {index} failed ({status})");
            return Err(match dt::classify(dt::Call::DownloadGrant, &seen.answer) {
                dt::Outcome::Terminal(t) if t.status == 401 => {
                    PinnedFailure::Final(ChunkApiError::Terminal(super::SESSION_EXPIRED_MESSAGE.to_string()))
                }
                dt::Outcome::Terminal(_) => {
                    PinnedFailure::Final(ChunkApiError::Terminal(format!("download failed ({status})")))
                }
                dt::Outcome::Restart => PinnedFailure::Final(ChunkApiError::Restart),
                dt::Outcome::Backoff(wait) => PinnedFailure::Again {
                    wait: Some(wait),
                    message: failed,
                },
                // A 5xx may carry its own `Retry-After` (the server's 503 sends 5): honour it, with
                // the same bounds as any stated wait.
                dt::Outcome::UseApiNow => PinnedFailure::Again {
                    wait: if (500..=599).contains(&seen.answer.status) {
                        stated_wait(seen.retry_after_secs)
                    } else {
                        None
                    },
                    message: failed,
                },
                _ => PinnedFailure::Final(ChunkApiError::Failed(failed)),
            });
        }
        // The body must be exactly the manifest's length: read at most that much and check it.
        let mut resp = resp;
        match read_exact_len(&mut resp, expected_len, idle).await {
            Exact::Body(bytes) => Ok(bytes),
            Exact::WrongLength => Err(PinnedFailure::Final(ChunkApiError::Failed(format!(
                "download chunk {index}: the body is not the manifest's length"
            )))),
            Exact::Broken => Err(PinnedFailure::Again {
                wait: None,
                message: format!("download chunk {index}: the connection broke while reading"),
            }),
        }
    }
}

// ───────────────────────────── direct uploads (task 2173) ─────────────────────────────

/// What a direct-upload request came to.
pub(crate) enum UploadCall<T> {
    /// A good answer, validated by core.
    Ok(T),
    /// The classifier's verdict on a failure. A 2xx whose body broke the contract is `ApiForThisFile`.
    Failed(dt::Outcome),
    /// The upload session is gone (a JSON 404, or a 400 "not writable: expired"): the caller's own
    /// session recovery re-inits it, exactly as for a proxy chunk PUT.
    SessionGone(String),
}

/// The text of the 400 the server sends for a session that expired (the same rule as the proxy route's,
/// `is_upload_session_gone`).
const SESSION_EXPIRED: &str = "upload session is not writable: expired";

/// Is this failed answer "the session is gone"? A JSON 404 (the session is not the caller's, or was swept)
/// and the expired-session 400; a BARE 404 is a node without the route, which is not that.
fn upload_session_gone(seen: &Seen) -> bool {
    match seen.answer.status {
        404 => seen.answer.has_json_error(),
        400 => seen.answer.error_code().is_some_and(|c| c.contains(SESSION_EXPIRED)),
        _ => false,
    }
}

/// A storage PUT may take as long as the link needs, but must keep moving: the budget is a minute plus the
/// chunk at this speed. The server's own fence assumes a minimum uplink of the same order.
const STORAGE_PUT_BASE: Duration = Duration::from_secs(60);
const STORAGE_PUT_MIN_BYTES_PER_SEC: u64 = 128 * 1024;

impl ApiClient {
    /// `POST /api/v1/uploads/{session}/chunks/{index}/grant {"size"}`. Sends no capability header: the
    /// session's transport was chosen at init and never changes.
    pub(crate) async fn upload_grant(
        &self,
        session: &str,
        index: u32,
        size: u64,
        discovery: &dt::Discovery,
    ) -> UploadCall<dt::UploadGrant> {
        let seen = self.upload_call(&dt::upload_grant_path(session, index), size).await;
        match dt::classify(dt::Call::UploadGrant, &seen.answer) {
            dt::Outcome::Proceed => match dt::UploadGrant::from_answer(&seen.body, index, discovery, &self.base_url) {
                Ok(grant) => UploadCall::Ok(grant),
                // The answer broke the contract (a link to a host nobody listed, a header we must not
                // send): this session cannot go direct.
                Err(_) => UploadCall::Failed(dt::Outcome::ApiForThisFile),
            },
            other => failed_upload_call(&seen, other),
        }
    }

    /// `POST /api/v1/uploads/{session}/chunks/{index}/commit {"size"}`.
    pub(crate) async fn upload_commit(&self, session: &str, index: u32, size: u64) -> UploadCall<dt::UploadCommitted> {
        let seen = self.upload_call(&dt::upload_commit_path(session, index), size).await;
        match dt::classify(dt::Call::UploadCommit, &seen.answer) {
            dt::Outcome::Proceed => match dt::UploadCommitted::from_answer(&seen.body, index, size) {
                Ok(done) => UploadCall::Ok(done),
                Err(_) => UploadCall::Failed(dt::Outcome::ApiForThisFile),
            },
            other => failed_upload_call(&seen, other),
        }
    }

    async fn upload_call(&self, path: &str, size: u64) -> Seen {
        let Ok(token) = self.require_auth() else {
            return Seen::no_answer();
        };
        super::pace_if_needed().await;
        let sent = self
            .client
            .post(self.url(path))
            .bearer_auth(token)
            .json(&dt::UploadGrantRequest { size })
            .timeout(GRANT_TIMEOUT)
            .send()
            .await;
        match sent {
            Ok(resp) => answer_of(resp, dt::wire::MAX_UPLOAD_BODY_BYTES).await,
            Err(_) => Seen::no_answer(),
        }
    }

    /// `PUT` one chunk's ciphertext to a granted link, on the storage client: the one header the grant
    /// allows, no Authorization, no cookies, no redirects. Returns the HTTP status, 0 when there was no
    /// answer (or no safe client). The link and the transport error (which can carry the link) never
    /// leave this function.
    pub(crate) async fn put_to_storage(&self, link: &dt::UploadLink, data: bytes::Bytes) -> u16 {
        let Some(client) = self.storage_client() else {
            return 0;
        };
        let len = data.len() as u64;
        let timeout = STORAGE_PUT_BASE + Duration::from_secs(len / STORAGE_PUT_MIN_BYTES_PER_SEC);
        let sent = client
            .put(link.url.as_str())
            .header(reqwest::header::CONTENT_TYPE, link.content_type())
            .body(data)
            .timeout(timeout)
            .send()
            .await;
        match sent {
            Ok(resp) => resp.status().as_u16(),
            Err(_) => 0,
        }
    }
}

fn failed_upload_call<T>(seen: &Seen, outcome: dt::Outcome) -> UploadCall<T> {
    if upload_session_gone(seen) {
        return UploadCall::SessionGone(format!("upload session is gone (HTTP {})", seen.answer.status));
    }
    UploadCall::Failed(outcome)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use std::time::{Duration, Instant};

    use super::{
        ChunkApiError, DiscoveryState, GrantReply, PinnedRetry, build_storage_client, build_storage_client_with, dt,
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
        assert_eq!(state.lookup(TTL - 1), dt::Lookup::UseApi, "an answer holds for the trust period");
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
}
