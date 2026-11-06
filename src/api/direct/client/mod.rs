//! The requests themselves: discovery, direct download grants, the pinned
//! per-chunk route and the upload-session calls, plus their tests.
//!
//! Owns: the `impl ApiClient` blocks for direct transfer, `UploadCall`,
//! `upload_session_gone`, `failed_upload_call`.
//! Must not: decide what an answer means (see `direct::state`) or build a
//! storage client (see `direct::http`).
//! Tests: `mod tests` at the bottom of this file covers discovery caching,
//! offer/refusal text, the pinned route and the upload-session fallbacks.

use super::*;
use crate::api::DirectUploadId;

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

    /// The storage client for PUTs (no read timeout, see [`build_upload_storage_client`]).
    pub(crate) fn upload_storage_client(&self) -> Option<&Client> {
        self.upload_storage_client
            .get_or_init(|| build_upload_storage_client(&self.base_url))
            .as_ref()
    }

    /// A direct upload had to be given up: start no new one, in this process and for this API base, until the
    /// trust period has passed (measured on the monotonic clock).
    pub(crate) fn note_direct_upload_failed(&self) {
        self.direct.note_upload_failed(tokio::time::Instant::now());
    }

    /// Was a direct upload given up in this process, for this API base, within the trust period? Shared by every
    /// client of the base ([`DirectState::shared`]).
    pub(crate) fn direct_upload_failed_recently(&self) -> bool {
        self.direct.upload_failed_within_hold(tokio::time::Instant::now())
    }

    /// Does this server offer direct downloads right now? Asks the discovery
    /// route lazily and remembers the answer (R1.6); `None` means "use the API
    /// for this file", for any reason: forced with `--transport api`, an old
    /// server, a node without the route, a network failure, a server that
    /// offers nothing. An authorization answer (401/403) is NOT handled here:
    /// the API path that follows reports it in its own words.
    pub(crate) async fn direct_download_offer(&self) -> Option<dt::Discovery> {
        self.direct_offer(dt::Discovery::offers_download).await.discovery()
    }

    /// [`direct_download_offer`](Self::direct_download_offer) for the upload direction (task 2173):
    /// the same lazily asked, cached discovery, filtered on `object-upload-v1`. The answer keeps "the server said
    /// no" apart from "nobody answered" ([`DirectOffer`]): a recorded direct session is given up on the first only.
    pub(crate) async fn direct_upload_offer(&self) -> DirectOffer {
        self.direct_offer(dt::Discovery::offers_upload).await
    }

    pub(super) async fn direct_offer(&self, wanted: fn(&dt::Discovery) -> bool) -> DirectOffer {
        // The user's choice is a definite answer.
        if crate::transport::api_forced() {
            return DirectOffer::NotOffered;
        }
        let Ok(token) = self.require_auth() else {
            return DirectOffer::Unauthorized(Refusal::NoSession);
        };
        let login = login_id(token);
        let verdict = |discovery: dt::Discovery| {
            if wanted(&discovery) {
                DirectOffer::Offered(discovery)
            } else {
                DirectOffer::NotOffered
            }
        };
        let mut state = self.direct.discovery.lock().await;
        // This login was refused a moment ago: say so without asking again (a revoked session must not cost one
        // request per file), unless the hold is over or another login is asking.
        if let Some(refusal) = state.refusal_for(login, tokio::time::Instant::now(), self.direct.refusal_hold) {
            return DirectOffer::Unauthorized(refusal);
        }
        match state.lookup(now_ms()) {
            dt::Lookup::Cached(discovery) => verdict(discovery),
            // The cache gave up: because the server said no, or because asking kept failing.
            dt::Lookup::UseApi if state.said_no() => DirectOffer::NotOffered,
            dt::Lookup::UseApi => DirectOffer::Unknown,
            dt::Lookup::Fetch => {
                let (outcome, discovery) = self.fetch_discovery().await;
                // A refusal for want of a valid login says nothing about the server: it is remembered for THIS login
                // only, for a while (see [`DiscoveryState`]).
                if let dt::Outcome::Terminal(t) = &outcome
                    && matches!(t.status, 401 | 403)
                {
                    let refusal = Refusal::Refused {
                        status: t.status,
                        code: t.error.clone(),
                    };
                    state.record_refusal(login, refusal.clone(), tokio::time::Instant::now());
                    return DirectOffer::Unauthorized(refusal);
                }
                state.record(&outcome, discovery.clone(), now_ms());
                match (outcome, discovery) {
                    (_, Some(discovery)) => verdict(discovery),
                    (dt::Outcome::NotSupported, None) => DirectOffer::NotOffered,
                    _ => DirectOffer::Unknown,
                }
            }
        }
    }

    pub(super) async fn fetch_discovery(&self) -> (dt::Outcome, Option<dt::Discovery>) {
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

    pub(super) async fn download_chunk_pinned_with(
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

    pub(super) async fn pinned_attempt(
        &self,
        idle: Duration,
        file_id: &str,
        index: u32,
        version: &str,
        expected_len: u64,
    ) -> Result<Vec<u8>, PinnedFailure> {
        let token = self
            .require_auth()
            .map_err(|m| PinnedFailure::Final(ChunkApiError::Terminal(m.to_string())))?;
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

/// What a direct-upload request came to.
pub(crate) enum UploadCall<T> {
    /// A good answer, validated by core.
    Ok(T),
    /// The classifier's verdict on a failure, and the HTTP status it was given (0: no answer). A 2xx whose
    /// body broke the contract is `ApiForThisFile`. The status is what the pause (`api::pause`) reads: core's
    /// outcome does not keep it (a 429 and a 5xx with a stated wait are one `Backoff`).
    Failed { outcome: dt::Outcome, status: u16 },
    /// The upload session is gone (a JSON 404, or a 400 "not writable: expired"): the caller's own
    /// session recovery re-inits it, exactly as for a proxy chunk PUT.
    SessionGone(String),
}

/// Is this failed answer "the session is gone"? The one rule of the proxy routes
/// (`upload_session_gone` in `api`), with one difference that is a fact about the
/// route: here a BARE 404 is a node without the route, not a swept session, so only a JSON 404 counts.
pub(super) fn upload_session_gone(seen: &Seen) -> bool {
    crate::api::upload_session_gone(
        seen.answer.status,
        seen.answer.has_json_error(),
        seen.answer.error_code().unwrap_or(""),
    )
}

impl ApiClient {
    /// `POST /api/v1/uploads/{session}/chunks/{index}/grant {"size"}`. Sends no capability header: the
    /// session's transport was chosen at init and never changes.
    pub(crate) async fn upload_grant(
        &self,
        session: impl Into<DirectUploadId>,
        index: u32,
        size: u64,
        discovery: &dt::Discovery,
    ) -> UploadCall<dt::UploadGrant> {
        let session = session.into();
        let seen = self
            .upload_call(
                &dt::upload_grant_path(session.as_str(), index),
                &dt::UploadGrantRequest { size },
            )
            .await;
        match dt::classify(dt::Call::UploadGrant, &seen.answer) {
            dt::Outcome::Proceed => match dt::UploadGrant::from_answer(&seen.body, index, discovery, &self.base_url) {
                Ok(grant) => UploadCall::Ok(grant),
                // The answer broke the contract (a link to a host nobody listed, a header we must not
                // send): this session cannot go direct.
                Err(_) => UploadCall::Failed {
                    outcome: dt::Outcome::ApiForThisFile,
                    status: seen.answer.status,
                },
            },
            other => failed_upload_call(&seen, other),
        }
    }

    /// `POST /api/v1/uploads/{session}/chunks/{index}/commit {"size"}`.
    pub(crate) async fn upload_commit(
        &self,
        session: impl Into<DirectUploadId>,
        index: u32,
        size: u64,
    ) -> UploadCall<dt::UploadCommitted> {
        let session = session.into();
        let seen = self
            .upload_call(
                &dt::upload_commit_path(session.as_str(), index),
                &dt::UploadCommitRequest { size },
            )
            .await;
        match dt::classify(dt::Call::UploadCommit, &seen.answer) {
            dt::Outcome::Proceed => match dt::UploadCommitted::from_answer(&seen.body, index, size) {
                Ok(done) => UploadCall::Ok(done),
                Err(_) => UploadCall::Failed {
                    outcome: dt::Outcome::ApiForThisFile,
                    status: seen.answer.status,
                },
            },
            other => failed_upload_call(&seen, other),
        }
    }

    pub(super) async fn upload_call(&self, path: &str, body: &impl serde::Serialize) -> Seen {
        let Ok(token) = self.require_auth() else {
            return Seen::no_answer();
        };
        super::pace_if_needed(&self.base_url).await;
        let sent = self
            .client
            .post(self.url(path))
            .bearer_auth(token)
            .json(body)
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
        let Some(client) = self.upload_storage_client() else {
            return 0;
        };
        let len = data.len() as u64;
        let timeout = put_timeout(STORAGE_PUT_BASE, len);
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

pub(super) fn failed_upload_call<T>(seen: &Seen, outcome: dt::Outcome) -> UploadCall<T> {
    if upload_session_gone(seen) {
        return UploadCall::SessionGone(format!("upload session is gone (HTTP {})", seen.answer.status));
    }
    UploadCall::Failed {
        outcome,
        status: seen.answer.status,
    }
}

#[cfg(test)]
mod tests;
