use reqwest::Client;
use serde_json::Value;
use std::error::Error;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::config::load_config;

static RATE_REMAINING: AtomicI64 = AtomicI64::new(-1);
static RATE_LIMIT: AtomicI64 = AtomicI64::new(-1);
static RATE_RESET: AtomicI64 = AtomicI64::new(0);

fn update_rate_state(headers: &reqwest::header::HeaderMap) {
    if let Some(v) = headers.get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()) {
        if let Ok(n) = v.parse::<i64>() {
            RATE_REMAINING.store(n, Ordering::Relaxed);
        }
    }
    if let Some(v) = headers.get("x-ratelimit-limit").and_then(|v| v.to_str().ok()) {
        if let Ok(n) = v.parse::<i64>() {
            RATE_LIMIT.store(n, Ordering::Relaxed);
        }
    }
    if let Some(v) = headers.get("x-ratelimit-reset").and_then(|v| v.to_str().ok()) {
        if let Ok(n) = v.parse::<i64>() {
            RATE_RESET.store(n, Ordering::Relaxed);
        }
    }
}

async fn pace_if_needed() {
    let remaining = RATE_REMAINING.load(Ordering::Relaxed);
    let limit = RATE_LIMIT.load(Ordering::Relaxed);
    let reset = RATE_RESET.load(Ordering::Relaxed);
    if remaining < 0 || limit <= 0 {
        return;
    }
    let ratio = remaining as f64 / limit as f64;
    if ratio > 0.2 {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let secs_until_reset = (reset - now).max(1);
    let requests_left = remaining.max(1);
    let pace_ms = ((secs_until_reset as f64 / requests_left as f64) * 1000.0).min(5000.0) as u64;
    if pace_ms > 50 {
        tokio::time::sleep(std::time::Duration::from_millis(pace_ms)).await;
    }
}

fn format_request_error(error: reqwest::Error) -> String {
    let mut message = format!("request failed: {error}");
    let mut source = error.source();
    while let Some(err) = source {
        message.push_str(&format!(": {err}"));
        source = err.source();
    }
    message
}

pub struct ApiClient {
    client: Client,
    base_url: String,
    token: Option<String>,
}

impl ApiClient {
    pub fn from_config() -> Self {
        let config = load_config();
        // 300s per-request timeout, matching beebeeb-upload, so a stalled chunk
        // PUT fails instead of hanging forever. Long-lived calls that need more
        // (the SSE sync stream) override this per-request with their own
        // `.timeout(...)`, so this default is safe for them.
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            base_url: config.api_url,
            token: config.session_token,
        }
    }

    pub fn require_auth(&self) -> Result<&str, String> {
        self.token
            .as_deref()
            .ok_or_else(|| "Not logged in. Run `bb login` first.".to_string())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    #[allow(dead_code)]
    pub async fn signup(&self, email: &str, password: &str) -> Result<Value, String> {
        let resp = self
            .client
            .post(self.url("/api/v1/auth/signup"))
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    #[allow(dead_code)]
    pub async fn opaque_login_start(&self, email: &str, client_message_b64: &str) -> Result<Value, String> {
        let resp = self
            .client
            .post(self.url("/api/v1/opaque/login-start"))
            .json(&serde_json::json!({
                "email": email,
                "client_message": client_message_b64,
            }))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    #[allow(dead_code)]
    pub async fn opaque_login_finish(
        &self,
        email: &str,
        client_message_b64: &str,
        server_state_b64: &str,
    ) -> Result<Value, String> {
        let resp = self
            .client
            .post(self.url("/api/v1/opaque/login-finish"))
            .json(&serde_json::json!({
                "email": email,
                "client_message": client_message_b64,
                "server_state": server_state_b64,
            }))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    pub async fn logout(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/auth/logout"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    pub async fn get_me(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/auth/me"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    pub async fn get_region(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/api/v1/region"))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    pub async fn get_my_region(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/me/region"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Hit the health endpoint and return the round-trip latency in milliseconds.
    pub async fn ping_health(&self) -> Result<u128, String> {
        let start = std::time::Instant::now();
        let resp = self
            .client
            .get(self.url("/api/v1/health"))
            .send()
            .await
            .map_err(format_request_error)?;
        let latency_ms = start.elapsed().as_millis();
        if !resp.status().is_success() {
            return Err(format!("health check failed: {}", resp.status()));
        }
        Ok(latency_ms)
    }

    pub async fn list_files(&self, parent_id: Option<&str>) -> Result<Value, String> {
        let token = self.require_auth()?;
        let mut url = self.url("/api/v1/files");
        if let Some(pid) = parent_id {
            url = format!("{url}?parent_id={pid}");
        }
        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// Get file metadata by ID.
    pub async fn get_file(&self, file_id: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/files/{file_id}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// Upload an encrypted file via multipart.
    ///
    /// The server expects:
    /// - A "metadata" text field containing JSON with name_encrypted, parent_id, mime_type, size_bytes
    /// - One or more "chunk_N" binary fields containing the encrypted chunk data
    pub async fn upload_encrypted(
        &self,
        metadata_json: &str,
        encrypted_chunks: &[(u32, Vec<u8>)],
    ) -> Result<Value, String> {
        let token = self.require_auth()?;

        let mut form = reqwest::multipart::Form::new().text("metadata", metadata_json.to_string());

        for (idx, data) in encrypted_chunks {
            let part = reqwest::multipart::Part::bytes(data.clone())
                .mime_str("application/octet-stream")
                .map_err(|e| format!("mime error: {e}"))?;
            form = form.part(format!("chunk_{idx}"), part);
        }

        let resp = self
            .client
            .post(self.url("/api/v1/files/upload"))
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// V2 chunked upload: init → upload chunks → complete.
    /// Stores chunk_size in object_versions so downloads know the exact layout.
    pub async fn upload_init(
        &self,
        file_id: Option<uuid::Uuid>,
        name_encrypted: &str,
        parent_id: Option<uuid::Uuid>,
        size_bytes: i64,
        chunk_count: i32,
        is_media: bool,
    ) -> Result<Value, String> {
        let token = self.require_auth()?;
        let body = serde_json::json!({
            "file_id": file_id,
            "name_encrypted": name_encrypted,
            "parent_id": parent_id,
            "size_bytes": size_bytes,
            "chunk_count": chunk_count,
            "is_media": is_media,
        });
        for _ in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .post(self.url("/api/v1/files/upload/init"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e == "__rate_limited__" => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string())
    }

    pub async fn upload_chunk(&self, file_id: &str, index: u32, data: Vec<u8>) -> Result<Value, String> {
        let token = self.require_auth()?;
        for attempt in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .put(self.url(&format!("/api/v1/files/{file_id}/chunks/{index}")))
                .bearer_auth(&token)
                .header("Content-Type", "application/octet-stream")
                .body(if attempt == 0 { data.clone() } else { data.clone() })
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e == "__rate_limited__" => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string())
    }

    pub async fn upload_complete(&self, file_id: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        for _ in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .post(self.url(&format!("/api/v1/files/{file_id}/upload/complete")))
                .bearer_auth(&token)
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e == "__rate_limited__" => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string())
    }

    /// Create a share link for a file.
    ///
    /// `wrapped_file_key` — if provided, enables double-encrypted mode where
    /// the server stores an opaque blob it cannot unwrap. The client key K_c
    /// used to produce this blob goes in the URL fragment.
    pub async fn create_share(
        &self,
        file_id: &str,
        expires_in_hours: Option<u64>,
        max_opens: Option<u32>,
        passphrase: Option<&str>,
        wrapped_file_key: Option<String>,
    ) -> Result<Value, String> {
        let token = self.require_auth()?;
        let mut body = serde_json::json!({ "file_id": file_id });
        if let Some(h) = expires_in_hours {
            body["expires_in_hours"] = serde_json::json!(h);
        }
        if let Some(n) = max_opens {
            body["max_opens"] = serde_json::json!(n);
        }
        if let Some(p) = passphrase {
            body["passphrase"] = serde_json::json!(p);
        }
        if let Some(wfk) = wrapped_file_key {
            body["wrapped_file_key"] = serde_json::json!(wfk);
        }
        let resp = self
            .client
            .post(self.url("/api/v1/shares"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// List the current user's shares.
    pub async fn list_shares(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/shares/mine"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// Revoke a share by ID.
    pub async fn delete_share(&self, share_id: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .delete(self.url(&format!("/api/v1/shares/{share_id}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    // ── File requests ────────────────────────────────────────────────────
    // The inverse of sharing: an account-less link anyone can use to upload an
    // encrypted file *into* the owner's vault. See `commands::request`.

    /// Create a file request. `wrapped_private_key` + `wrap_nonce` are the
    /// owner's X25519 request private key sealed under the master key (base64);
    /// the server stores them as opaque bytes and never unwraps them.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_file_request(
        &self,
        title: &str,
        description: Option<&str>,
        target_folder_id: Option<uuid::Uuid>,
        max_files: Option<u32>,
        max_total_bytes: Option<i64>,
        expires_in_secs: Option<i64>,
        wrapped_private_key_b64: &str,
        wrap_nonce_b64: &str,
    ) -> Result<Value, String> {
        let token = self.require_auth()?;
        let mut body = serde_json::json!({
            "title": title,
            "wrapped_private_key": wrapped_private_key_b64,
            "wrap_nonce": wrap_nonce_b64,
        });
        if let Some(d) = description {
            body["description"] = serde_json::json!(d);
        }
        if let Some(f) = target_folder_id {
            body["target_folder_id"] = serde_json::json!(f);
        }
        if let Some(n) = max_files {
            body["max_files"] = serde_json::json!(n);
        }
        if let Some(b) = max_total_bytes {
            body["max_total_bytes"] = serde_json::json!(b);
        }
        if let Some(s) = expires_in_secs {
            body["expires_in_secs"] = serde_json::json!(s);
        }
        let resp = self
            .client
            .post(self.url("/api/v1/file-requests"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// List the owner's file requests (includes wrapped_private_key + wrap_nonce
    /// so the client can rebuild each link by deriving R_pub).
    pub async fn list_file_requests(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/file-requests"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Close a file request (stop accepting uploads). Idempotent.
    pub async fn close_file_request(&self, id: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url(&format!("/api/v1/file-requests/{id}/close")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Hard-delete a file request row. NOTE: the server does not yet expose a
    /// DELETE route for file requests (only `/close`); this is wired for
    /// forward-compatibility and will surface a clear error until it lands.
    pub async fn delete_file_request(&self, id: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .delete(self.url(&format!("/api/v1/file-requests/{id}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Upload an encrypted file to a file request via the PUBLIC, account-less
    /// endpoint (`POST /api/v1/r/:token/upload`). No bearer auth — the upload is
    /// authorised purely by possession of the token + sealing to the request's
    /// public key. `metadata_json` carries name_encrypted, size_bytes,
    /// sender_ephemeral_pubkey, and wrapped_key.
    pub async fn upload_to_file_request(
        &self,
        token: &str,
        metadata_json: &str,
        encrypted_chunks: &[(u32, Vec<u8>)],
    ) -> Result<Value, String> {
        let mut form = reqwest::multipart::Form::new().text("metadata", metadata_json.to_string());
        for (idx, data) in encrypted_chunks {
            let part = reqwest::multipart::Part::bytes(data.clone())
                .mime_str("application/octet-stream")
                .map_err(|e| format!("mime error: {e}"))?;
            form = form.part(format!("chunk_{idx}"), part);
        }
        let resp = self
            .client
            .post(self.url(&format!("/api/v1/r/{token}/upload")))
            .multipart(form)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    pub async fn get_subscription(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/billing/subscription"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// GET /api/v1/account/security-score (note: under /account, not /auth/account)
    pub async fn security_score(&self) -> Result<serde_json::Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/account/security-score"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// GET /api/v1/account/sessions (newer shape with device_kind, country_code).
    pub async fn list_sessions_v2(&self) -> Result<serde_json::Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/account/sessions"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// GET /api/v1/auth/passkeys
    pub async fn list_passkeys(&self) -> Result<serde_json::Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/auth/passkeys"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Step-up re-auth: POST /api/v1/auth/confirm.
    /// Returns the raw confirmation token. Caller is responsible for attaching
    /// it as `X-Confirm-Token` on the protected call within 5 minutes.
    pub async fn confirm_password(&self, password: &str) -> Result<String, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/auth/confirm"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "password": password }))
            .send()
            .await
            .map_err(format_request_error)?;
        let body = parse_response(resp).await?;
        body.get("confirmation_token")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| "server did not return a confirmation_token".to_string())
    }

    /// Alias of `get_subscription` for use from `bb billing show`. The Spec 2
    /// billing tree will move to a dedicated `/api/v1/billing/*` namespace, so
    /// keep the call sites pointed at a billing-named function from day one.
    pub async fn get_billing_subscription(&self) -> Result<Value, String> {
        self.get_subscription().await
    }

    /// Same idea — billing-named alias so `bb billing show` doesn't reach into
    /// the files API directly.
    pub async fn get_billing_usage(&self) -> Result<Value, String> {
        self.get_usage().await
    }

    /// Return all `{id, name_encrypted}` pairs in a folder so the caller can
    /// decrypt names locally and detect filename conflicts before uploading.
    /// `parent_id = None` queries the root folder.
    pub async fn check_conflict(&self, parent_id: Option<uuid::Uuid>) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/files/check-conflict"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "parent_id": parent_id }))
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    pub async fn get_file_count(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/files/count"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    pub async fn get_usage(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/files/usage"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    pub async fn get_sessions(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/auth/sessions"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    pub async fn create_folder(
        &self,
        name_encrypted: &str,
        parent_id: Option<uuid::Uuid>,
        folder_id: Option<uuid::Uuid>,
    ) -> Result<Value, String> {
        let token = self.require_auth()?;
        let mut body = serde_json::json!({ "name_encrypted": name_encrypted });
        if let Some(pid) = parent_id {
            body["parent_id"] = serde_json::json!(pid);
        }
        if let Some(fid) = folder_id {
            body["folder_id"] = serde_json::json!(fid);
        }
        let resp = self
            .client
            .post(self.url("/api/v1/files/folder"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Rename and/or move a file.  Pass `None` to leave a field unchanged.
    pub async fn move_file(
        &self,
        file_id: &str,
        new_name_encrypted: Option<&str>,
        new_parent_id: Option<uuid::Uuid>,
    ) -> Result<Value, String> {
        let token = self.require_auth()?;
        let mut body = serde_json::json!({});
        if let Some(name) = new_name_encrypted {
            body["name_encrypted"] = serde_json::json!(name);
        }
        if let Some(pid) = new_parent_id {
            body["parent_id"] = serde_json::json!(pid);
        }
        let resp = self
            .client
            .patch(self.url(&format!("/api/v1/files/{file_id}")))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Soft-delete (trash) a file by ID.
    pub async fn trash_file(&self, file_id: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .delete(self.url(&format!("/api/v1/files/{file_id}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// Mint a short-lived (~1h) bearer token for the SSE sync stream. The
    /// stream cannot use the main session token because SSE auth lives in
    /// the URL query string (browsers can't set headers on EventSource).
    pub async fn create_stream_token(&self) -> Result<String, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/sync/stream-token"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        let body = parse_response(resp).await?;
        body.get("stream_token")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| "server did not return a stream_token".to_string())
    }

    /// Build the absolute URL of the SSE endpoint for a given stream token.
    pub fn stream_url(&self, stream_token: &str) -> String {
        // Use a query-string token because SSE doesn't allow custom headers.
        // URL-encoding is unnecessary — the server-issued token is base64url
        // (no `+`, `/`, `=`, or other reserved characters).
        format!("{}/api/v1/sync/stream?token={stream_token}", self.base_url)
    }

    /// Fetch sync ops with `seq_id > since` so we can catch up after an SSE
    /// reconnect. The server caps `limit` at 5000.
    pub async fn list_ops_since(&self, since: i64) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/sync/ops?since={since}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Build a streaming SSE response for the sync endpoint. Caller is
    /// responsible for parsing event frames out of the byte stream.
    pub async fn open_sync_stream(&self, stream_token: &str) -> Result<reqwest::Response, String> {
        let resp = self
            .client
            .get(self.stream_url(stream_token))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            // SSE connections are long-lived; disable reqwest's default
            // request timeout for this call only.
            .timeout(std::time::Duration::from_secs(60 * 60 * 24))
            .send()
            .await
            .map_err(|e| format!("sse connect failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("sse handshake failed ({status}): {body}"));
        }
        Ok(resp)
    }

    /// Generic authenticated GET that returns parsed JSON.
    #[allow(dead_code)]
    pub async fn get_json(&self, path: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url(path))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Upload a raw blob to the speedtest endpoint.
    pub async fn speedtest_upload(&self, data: &[u8]) -> Result<(), String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/speedtest"))
            .bearer_auth(token)
            .body(data.to_vec())
            .send()
            .await
            .map_err(format_request_error)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("speedtest upload ({status}): {body}"));
        }
        Ok(())
    }

    /// Download `size` random bytes from the speedtest endpoint.
    pub async fn speedtest_download(&self, size: usize) -> Result<Vec<u8>, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/speedtest?size={size}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("speedtest download ({status}): {body}"));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("speedtest read: {e}"))
    }

    /// Search all user files (root + one level of subfolders) for a file
    /// whose UUID starts with the given hex prefix. Returns the full UUID if
    /// exactly one match is found, `None` for zero matches, and an error for
    /// ambiguous (multiple) matches.
    pub async fn find_file_by_id_prefix(&self, prefix: &str) -> Result<Option<String>, String> {
        let root = self.list_files(None).await?;
        let root_files = root.get("files").and_then(|v| v.as_array());

        let mut all_ids: Vec<String> = Vec::new();
        if let Some(files) = root_files {
            for f in files {
                if let Some(id) = f.get("id").and_then(|v| v.as_str()) {
                    all_ids.push(id.to_string());
                    let is_folder = f.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
                    if is_folder {
                        if let Ok(children) = self.list_files(Some(id)).await {
                            if let Some(child_files) = children.get("files").and_then(|v| v.as_array()) {
                                for cf in child_files {
                                    if let Some(cid) = cf.get("id").and_then(|v| v.as_str()) {
                                        all_ids.push(cid.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let matches: Vec<&String> = all_ids.iter().filter(|id| id.starts_with(prefix)).collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches[0].clone())),
            _ => Err(format!(
                "ambiguous prefix '{}' matches {} files — use more characters",
                prefix,
                matches.len()
            )),
        }
    }

    /// Upload an encrypted thumbnail for a file.
    ///
    /// `PUT /api/v1/files/{file_id}/thumbnail` with the encrypted blob as the
    /// request body. The server stores it as a reserved chunk index and sets
    /// `has_thumbnail = true` on the file row.
    pub async fn upload_thumbnail(&self, file_id: &str, encrypted_data: Vec<u8>) -> Result<(), String> {
        let token = self.require_auth()?;
        for _ in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .put(self.url(&format!("/api/v1/files/{file_id}/thumbnail")))
                .bearer_auth(&token)
                .header("Content-Type", "application/octet-stream")
                .body(encrypted_data.clone())
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e == "__rate_limited__" => continue,
                Err(e) => return Err(format!("thumbnail upload: {e}")),
                Ok(_) => return Ok(()),
            }
        }
        Err("thumbnail upload rate limited after 3 retries".to_string())
    }

    /// Upload an encrypted large thumbnail for a file.
    pub async fn upload_thumbnail_large(&self, file_id: &str, encrypted_data: Vec<u8>) -> Result<(), String> {
        let token = self.require_auth()?;
        for _ in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .put(self.url(&format!("/api/v1/files/{file_id}/thumbnail/large")))
                .bearer_auth(&token)
                .header("Content-Type", "application/octet-stream")
                .body(encrypted_data.clone())
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e == "__rate_limited__" => continue,
                Err(e) => return Err(format!("large thumbnail upload: {e}")),
                Ok(_) => return Ok(()),
            }
        }
        Err("large thumbnail upload rate limited after 3 retries".to_string())
    }

    /// Open a streaming download of a file's encrypted bytes.
    ///
    /// Returns the live `Response` (headers + unread body) so the caller can
    /// read the body incrementally with `Response::chunk()` instead of
    /// buffering the whole payload. A 600s timeout overrides the client default
    /// (large files), matching `beebeeb-upload`.
    pub async fn download_stream(&self, file_id: &str) -> Result<reqwest::Response, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/files/{file_id}/download")))
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(600))
            .send()
            .await
            .map_err(format_request_error)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("download failed ({status}): {body}"));
        }
        Ok(resp)
    }

    /// Download the raw encrypted bytes for a file.
    pub async fn download_file(&self, file_id: &str) -> Result<Vec<u8>, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/files/{file_id}/download")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("download failed ({status}): {body}"));
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("failed to read response: {e}"))
    }

    // ── Client device + session API ──────────────────────────────────────

    /// Register (upsert) a client device with the server.
    pub async fn register_device(&self, hostname: &str, platform: &str, bb_version: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        let body = serde_json::json!({
            "hostname": hostname,
            "platform": platform,
            "bb_version": bb_version,
        });
        for _ in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .post(self.url("/api/v1/clients/devices"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e == "__rate_limited__" => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string())
    }

    /// Create a new client session (sync, mount, etc.).
    pub async fn create_client_session(
        &self,
        device_id: &str,
        name: &str,
        session_type: &str,
        local_path: Option<&str>,
        remote_path: &str,
    ) -> Result<Value, String> {
        let token = self.require_auth()?;
        let body = serde_json::json!({
            "device_id": device_id,
            "name": name,
            "session_type": session_type,
            "local_path": local_path,
            "remote_path": remote_path,
        });
        for _ in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .post(self.url("/api/v1/clients/sessions"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e == "__rate_limited__" => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string())
    }

    /// Fire-and-forget heartbeat — no retry on rate limit.
    pub async fn send_heartbeat(
        &self,
        session_id: &str,
        status: &str,
        files_synced: Option<u64>,
        files_total: Option<u64>,
        bytes_synced: Option<u64>,
        bytes_total: Option<u64>,
        current_file: Option<&str>,
        speed_bps: Option<u64>,
    ) -> Result<Value, String> {
        let token = self.require_auth()?;
        let body = serde_json::json!({
            "status": status,
            "files_synced": files_synced,
            "files_total": files_total,
            "bytes_synced": bytes_synced,
            "bytes_total": bytes_total,
            "current_file": current_file,
            "speed_bps": speed_bps,
        });
        // Heartbeats are fire-and-forget — don't retry
        pace_if_needed().await;
        let resp = self
            .client
            .post(self.url(&format!("/api/v1/clients/sessions/{session_id}/heartbeat")))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// List all client sessions for the authenticated user.
    pub async fn list_client_sessions(&self) -> Result<Value, String> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/clients/sessions"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Stop a client session by setting its status to "stopped".
    pub async fn stop_client_session(&self, session_id: &str) -> Result<Value, String> {
        let token = self.require_auth()?;
        let body = serde_json::json!({ "status": "stopped" });
        let resp = self
            .client
            .patch(self.url(&format!("/api/v1/clients/sessions/{session_id}")))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }
}

async fn parse_response(resp: reqwest::Response) -> Result<Value, String> {
    let status = resp.status();
    update_rate_state(resp.headers());

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(5);
        let remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        if !crate::ui::is_quiet() {
            use colored::Colorize;
            eprintln!(
                "  {} rate limited — pausing {}s{}",
                "⏸".custom_color(crate::colors::AMBER),
                retry_after,
                remaining.map_or(String::new(), |r| format!(" ({r} requests remaining)")),
            );
        }

        tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
        return Err("__rate_limited__".to_string());
    }

    let body = resp.text().await.map_err(|e| format!("failed to read response: {e}"))?;

    if !status.is_success() {
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
            .unwrap_or_else(|| format!("{status}: {body}"));
        return Err(msg);
    }

    serde_json::from_str(&body).map_err(|e| format!("invalid JSON: {e}"))
}
