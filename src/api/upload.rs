//! `ApiClient` upload-session calls: init, chunk, complete, abandon.

use super::*;

/// Whether an init asks the server for a direct upload (task 2173).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InitTransport {
    /// Exactly the pre-2173 request: no capability header.
    Api,
    /// Advertise `object-upload-v1` on this call only and let the answer's
    /// `transport` decide what happens next.
    Direct,
}

impl ApiClient {
    /// V2 chunked upload init: `POST /api/v1/uploads/init`.
    ///
    /// Opens an upload *session* against the v2 route (`routes/uploads.rs`). The
    /// server derives the real chunk plan (`chunk_size_bytes` × `chunk_count`)
    /// and returns a per-attempt `upload_session_id` that the chunk PUTs and the
    /// complete call key off — distinct from the durable `file_id`.
    ///
    /// On a **replace**, pass `file_id` = the existing file's id and
    /// `base_version_number` = its current `version_number`; the server snapshots
    /// the prior version, bumps `version_number`, and UPDATEs the row in place
    /// (correct versioning by file_id — no `name_encrypted` byte-match). It
    /// returns a stale-version 409 if `base_version_number` no longer matches,
    /// and a 409 if the file already has an upload in progress. On a fresh push,
    /// pass `file_id = None` and `base_version_number = None`.
    ///
    /// `file_name` is the encrypted name blob (the v2 field is `file_name`);
    /// `size_bytes` is the **plaintext** byte count. `chunk_size_bytes` /
    /// `chunk_count` are the client's plan hint — sent paired (the server
    /// validates both-or-neither) so it can mirror the exact framing the CLI's
    /// `ChunkEncryptor` emits; the response's plan is authoritative.
    #[allow(clippy::too_many_arguments)]
    pub async fn upload_init(
        &self,
        file_id: Option<uuid::Uuid>,
        name_encrypted: &str,
        parent_id: Option<uuid::Uuid>,
        size_bytes: i64,
        chunk_size_bytes: i64,
        chunk_count: i32,
        is_media: bool,
        base_version_number: Option<i32>,
    ) -> Result<UploadInit, ApiError> {
        self.upload_init_typed(
            file_id,
            name_encrypted,
            parent_id,
            size_bytes,
            chunk_size_bytes,
            chunk_count,
            is_media,
            base_version_number,
        )
        .await
    }

    /// [`upload_init`](Self::upload_init), keeping the HTTP status + server
    /// code so the upload driver can decide whether a re-init after a swept
    /// session may fall back to a fresh `file_id` (task 1589).
    #[allow(clippy::too_many_arguments)]
    pub async fn upload_init_typed(
        &self,
        file_id: Option<uuid::Uuid>,
        name_encrypted: &str,
        parent_id: Option<uuid::Uuid>,
        size_bytes: i64,
        chunk_size_bytes: i64,
        chunk_count: i32,
        is_media: bool,
        base_version_number: Option<i32>,
    ) -> Result<UploadInit, ApiError> {
        self.upload_init_for_transport(
            file_id,
            name_encrypted,
            parent_id,
            size_bytes,
            chunk_size_bytes,
            chunk_count,
            is_media,
            base_version_number,
            InitTransport::Api,
        )
        .await
    }

    /// [`upload_init_typed`](Self::upload_init_typed), optionally asking for the direct upload
    /// (task 2173): with `direct` the request carries `X-Beebeeb-Capabilities: object-upload-v1`
    /// (on this call only, never globally) and the answer's `transport` says what the server chose.
    /// Without it the request is byte-identical to what it always was. Pass
    /// `InitTransport::Direct` to advertise the capability.
    #[allow(clippy::too_many_arguments)]
    pub async fn upload_init_for_transport(
        &self,
        file_id: Option<uuid::Uuid>,
        name_encrypted: &str,
        parent_id: Option<uuid::Uuid>,
        size_bytes: i64,
        chunk_size_bytes: i64,
        chunk_count: i32,
        is_media: bool,
        base_version_number: Option<i32>,
        transport: InitTransport,
    ) -> Result<UploadInit, ApiError> {
        let token = self.require_auth()?;
        // The v2 server (`routes/uploads.rs::parse_profile`) accepts only
        // web/mobile/desktop/backup_agent — there is no "cli" wire profile. The
        // CLI computes its own plan with `ChunkProfile::Cli` and sends explicit
        // `chunk_size_bytes` + `chunk_count`, which the server uses verbatim
        // (the profile-derived plan is the fallback for the both-omitted case
        // only), so the wire profile is non-load-bearing here. We send "desktop"
        // — the closest accepted profile (same 256 MiB chunk ceiling).
        let body = serde_json::json!({
            "file_id": file_id,
            "file_name": name_encrypted,
            "file_size_bytes": size_bytes,
            "parent_id": parent_id,
            "profile": "desktop",
            "chunk_size_bytes": chunk_size_bytes,
            "chunk_count": chunk_count,
            "is_media": is_media,
            "base_version_number": base_version_number,
        });
        for _ in 0..3 {
            // After a Ctrl-C no session is opened (a request the run would then abandon).
            if !pace_if_needed(&self.base_url).await {
                return Err(crate::upload::INTERRUPTED.to_string().into());
            }
            let mut request = self
                .client
                .post(self.url("/api/v1/uploads/init"))
                .bearer_auth(token)
                .json(&body);
            if transport == InitTransport::Direct {
                request = request.header(
                    beebeeb_core::direct_transfer::CAPABILITIES_HEADER,
                    beebeeb_core::direct_transfer::CAP_OBJECT_UPLOAD_V1,
                );
            }
            let resp = request.send().await.map_err(format_request_error)?;
            match parse_response_typed(resp).await {
                Err(e) if e.message == super::RATE_LIMITED_MESSAGE => continue,
                Err(e) => return Err(self.explain_account_refusal(e).await),
                Ok(v) => return UploadInit::from_value(&v),
            }
        }
        Err("rate limited after 3 retries".to_string().into())
    }

    /// Task 1037: an account without a plan is refused at upload init and
    /// share create with `409 { error: "plan_required" | "account_lapsed" }`
    /// (older path: `413 quota_exceeded`, quota 0). Replace the display
    /// message with the account-state notice (choose a plan / trial ended +
    /// deletion date + where to subscribe); code and status are kept.
    ///
    /// Failure path only — a successful request never pays for this.
    /// `plan_required` needs no lookup; `account_lapsed` looks up the
    /// subscription for the deletion date (and still explains without it);
    /// `quota_exceeded` is only rewritten if the subscription says the
    /// account can't upload. The lookup runs at most once per client, so a
    /// `bb sync` pass over many files doesn't refetch per file.
    pub(super) async fn explain_account_refusal(&self, mut e: ApiError) -> ApiError {
        use crate::account_state::{AccountState, Refusal};

        let Some(refusal) = Refusal::classify(e.code.as_deref(), e.status) else {
            return e;
        };
        let fetched = if refusal.needs_subscription() {
            self.refusal_account_state
                .get_or_init(|| async {
                    let sub = self.get_subscription().await.ok()?;
                    Some(AccountState::from_subscription(&sub))
                })
                .await
                .as_ref()
        } else {
            None
        };
        let app_base = crate::web_url::resolve_web_app_base(std::env::var("APP_URL").ok().as_deref(), &self.base_url);
        if let Some(notice) = refusal.resolve(fetched).and_then(|state| state.notice(&app_base)) {
            e.message = notice;
        }
        e
    }

    /// Upload one chunk to a v2 session: `PUT /api/v1/uploads/{session}/chunks/{index}`.
    ///
    /// Retries the **same** index on transient transport failures (conn-reset,
    /// timeout, premature EOF) and on rate-limit pauses, with exponential
    /// backoff. Safe to re-PUT: the endpoint is idempotent — an already-stored
    /// chunk returns `{skipped: true}` without re-writing the blob — so a resumed
    /// upload simply re-PUTs every chunk and the server short-circuits the ones
    /// it already has. `data` is `Bytes` so each retry reuses the same allocation
    /// (a clone is just an `Arc` refcount bump).
    pub async fn upload_chunk(
        &self,
        upload_session_id: impl Into<DirectUploadId>,
        index: u32,
        data: Bytes,
    ) -> Result<Value, ApiError> {
        self.upload_chunk_typed(upload_session_id, index, data).await
    }

    /// [`upload_chunk`](Self::upload_chunk), keeping the HTTP status + server
    /// code so the upload driver can tell a swept session (404, or 400
    /// "not writable: expired" from an older server) apart from any other
    /// failure (task 1589). See [`is_upload_session_gone`].
    pub async fn upload_chunk_typed(
        &self,
        upload_session_id: impl Into<DirectUploadId>,
        index: u32,
        data: Bytes,
    ) -> Result<Value, ApiError> {
        self.upload_chunk_observed(upload_session_id, index, data, &|| {}).await
    }

    /// [`upload_chunk_typed`](Self::upload_chunk_typed), calling `on_retry` once before every
    /// re-attempt of the PUT (a rate limit, or a transient transport error), so the upload driver can
    /// count retries per chunk (task 2333). Never called for the first attempt, nor after the last.
    pub async fn upload_chunk_observed(
        &self,
        upload_session_id: impl Into<DirectUploadId>,
        index: u32,
        data: Bytes,
        on_retry: &(dyn Fn() + Sync),
    ) -> Result<Value, ApiError> {
        let upload_session_id = upload_session_id.into();
        let token = self.require_auth()?;
        let url = self.url(&format!("/api/v1/uploads/{upload_session_id}/chunks/{index}"));
        let mut last_err = String::new();
        for attempt in 1..=CHUNK_RETRY.max_attempts {
            if attempt > 1 {
                on_retry();
            }
            if !pace_if_needed(&self.base_url).await {
                return Err(crate::upload::INTERRUPTED.to_string().into());
            }
            let send = self
                .client
                .put(&url)
                .bearer_auth(token)
                .header("Content-Type", "application/octet-stream")
                .body(data.clone())
                .send()
                .await;
            match send {
                // A storage pool that is briefly unavailable (spec §6.1): one pause for every request of the
                // process, then this chunk's next attempt. It spends one of the chunk's attempts, so it never loops.
                Ok(resp) if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE => {
                    let by = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map_or_else(
                            || {
                                std::time::Duration::from_millis(CHUNK_RETRY.delay_ms(attempt))
                                    .max(std::time::Duration::from_secs(1))
                            },
                            std::time::Duration::from_secs,
                        );
                    last_err = "server busy (HTTP 503)".to_string();
                    super::pause::extend(&self.base_url, by, super::pause::PauseReason::Busy503);
                    continue;
                }
                Ok(resp) => match parse_response_typed(resp).await {
                    // parse_response already slept for Retry-After; just re-try.
                    Err(e) if e.message == super::RATE_LIMITED_MESSAGE => {
                        last_err = "rate limited".to_string();
                        continue;
                    }
                    // Success, or a definitive HTTP error (4xx/5xx) — don't retry.
                    other => return other,
                },
                Err(err) if is_transient_transport_error(&err) => {
                    last_err = format_request_error(err);
                    let wait = CHUNK_RETRY.delay_ms(attempt);
                    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                    continue;
                }
                // Permanent transport error (TLS/builder/etc.) — fail fast.
                Err(err) => return Err(format_request_error(err).into()),
            }
        }
        Err(format!(
            "chunk {index} failed after {} attempts: {last_err}",
            CHUNK_RETRY.max_attempts
        )
        .into())
    }

    /// Finalise a v2 upload session: `POST /api/v1/uploads/{session}/complete`.
    /// Verifies all chunks landed, flips the file to the new version, and returns
    /// the full file metadata. Idempotent (a second call on a completed session
    /// returns `{already_completed: true}`).
    pub async fn upload_complete(&self, upload_session_id: impl Into<DirectUploadId>) -> Result<Value, ApiError> {
        self.upload_complete_typed(upload_session_id).await
    }

    /// [`upload_complete`](Self::upload_complete), keeping the HTTP status +
    /// server code (task 1589; see [`upload_chunk_typed`](Self::upload_chunk_typed)).
    pub async fn upload_complete_typed(&self, upload_session_id: impl Into<DirectUploadId>) -> Result<Value, ApiError> {
        let upload_session_id = upload_session_id.into();
        let token = self.require_auth()?;
        for _ in 0..3 {
            if !pace_if_needed(&self.base_url).await {
                return Err(crate::upload::INTERRUPTED.to_string().into());
            }
            let resp = self
                .client
                .post(self.url(&format!("/api/v1/uploads/{upload_session_id}/complete")))
                .bearer_auth(token)
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response_typed(resp).await {
                Err(e) if e.message == super::RATE_LIMITED_MESSAGE => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string().into())
    }

    /// Abandon a file's in-flight upload session (`POST
    /// /api/v1/files/{id}/upload/abandon`, server task 1571): a replace reverts
    /// to the previous version, a never-completed first upload is deleted. Core's
    /// resume rule (task 1908) needs it before it re-inits a file whose old
    /// session it may not continue. A 404 means there is nothing to abandon.
    pub async fn upload_abandon(&self, file_id: impl Into<FileId>) -> Result<Value, ApiError> {
        let file_id = file_id.into();
        let token = self.require_auth()?;
        for _ in 0..3 {
            pace_if_needed(&self.base_url).await;
            let resp = self
                .client
                .post(self.url(&format!("/api/v1/files/{file_id}/upload/abandon")))
                .bearer_auth(token)
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response_typed(resp).await {
                Err(e) if e.message == super::RATE_LIMITED_MESSAGE => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string().into())
    }
}
