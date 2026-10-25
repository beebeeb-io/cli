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
const STORAGE_TIMEOUT: Duration = Duration::from_secs(600);
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

/// Turn a finished response into what the classifier needs, reading at most
/// `max` body bytes.
async fn answer_of(mut resp: reqwest::Response, max: usize) -> (dt::ApiAnswer, Vec<u8>) {
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
    (answer, body)
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
    let dev = reqwest::Url::parse(api_base)
        .map(|u| u.scheme() == "http" && matches!(u.host_str(), Some("127.0.0.1" | "localhost")))
        .unwrap_or(false);
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!dev)
        .connect_timeout(STORAGE_CONNECT_TIMEOUT)
        .timeout(STORAGE_TIMEOUT)
        .build()
        .ok()
}

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
        if crate::transport::api_forced() || self.require_auth().is_err() {
            return None;
        }
        let mut cache = self.direct_discovery.lock().await;
        match cache.lookup(now_ms()) {
            dt::Lookup::Cached(discovery) => discovery.offers_download().then_some(discovery),
            dt::Lookup::UseApi => None,
            dt::Lookup::Fetch => {
                let (outcome, discovery) = self.fetch_discovery().await;
                cache.record(&outcome, discovery.clone(), now_ms());
                discovery.filter(dt::Discovery::offers_download)
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
        let (answer, body) = match sent {
            Ok(resp) => answer_of(resp, dt::wire::MAX_DISCOVERY_BYTES).await,
            Err(_) => (dt::ApiAnswer::no_answer(), Vec::new()),
        };
        match dt::classify(dt::Call::Discovery, &answer) {
            dt::Outcome::Proceed => match dt::Discovery::from_json(&body) {
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
        let (answer, body) = match sent {
            Ok(resp) => answer_of(resp, dt::wire::MAX_GRANT_BODY_BYTES).await,
            Err(_) => (dt::ApiAnswer::no_answer(), Vec::new()),
        };
        match dt::classify(dt::Call::DownloadGrant, &answer) {
            dt::Outcome::Proceed => match dt::DownloadGrantResponse::from_json(&body) {
                Ok(window) => GrantReply::Window(window),
                Err(_) => GrantReply::Malformed,
            },
            other => GrantReply::Failed(other),
        }
    }

    /// `GET /api/v1/files/{id}/chunks/{index}?object_version_id=<pinned>`: one
    /// chunk of the pinned version through the API, the fallback for a chunk
    /// storage could not give (and the re-fetch of one that failed to
    /// authenticate). `expected_len` is the stored length the grant manifest
    /// states for this chunk.
    pub(crate) async fn download_chunk_pinned(
        &self,
        file_id: &str,
        index: u32,
        version: &str,
        expected_len: u64,
    ) -> Result<Vec<u8>, ChunkApiError> {
        let token = self.require_auth().map_err(ChunkApiError::Terminal)?;
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/files/{file_id}/chunks/{index}")))
            .query(&[("object_version_id", version)])
            .bearer_auth(token)
            .timeout(CHUNK_API_TIMEOUT)
            .send()
            .await
            .map_err(|e| ChunkApiError::Failed(dt::scrub(&e.to_string())))?;
        let status = resp.status();
        if !status.is_success() {
            let (answer, _) = answer_of(resp, 64 * 1024).await;
            return Err(match (status.as_u16(), answer.error_code()) {
                (401, _) => ChunkApiError::Terminal(super::SESSION_EXPIRED_MESSAGE.to_string()),
                (403, _) => ChunkApiError::Terminal(format!("download failed ({status})")),
                (404, Some(_)) => ChunkApiError::Terminal(format!("download failed ({status})")),
                (409, Some("version_unavailable")) => ChunkApiError::Restart,
                _ => ChunkApiError::Failed(format!("download chunk {index} failed ({status})")),
            });
        }
        // The body must be exactly the manifest's length: read at most that much and check it.
        let mut resp = resp;
        match read_capped(&mut resp, usize::try_from(expected_len).unwrap_or(usize::MAX)).await {
            Some(bytes) if bytes.len() as u64 == expected_len => Ok(bytes),
            _ => Err(ChunkApiError::Failed(format!(
                "download chunk {index}: the body is not the manifest's length"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::build_storage_client;
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
}
