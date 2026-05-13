use reqwest::Client;
use serde_json::Value;
use std::error::Error;

use crate::config::load_config;

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
        Self {
            client: Client::new(),
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
    pub async fn opaque_login_start(
        &self,
        email: &str,
        client_message_b64: &str,
    ) -> Result<Value, String> {
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

        let mut form = reqwest::multipart::Form::new()
            .text("metadata", metadata_json.to_string());

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

    /// Return all `{id, name_encrypted}` pairs in a folder so the caller can
    /// decrypt names locally and detect filename conflicts before uploading.
    /// `parent_id = None` queries the root folder.
    pub async fn check_conflict(
        &self,
        parent_id: Option<uuid::Uuid>,
    ) -> Result<Value, String> {
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
    pub async fn open_sync_stream(
        &self,
        stream_token: &str,
    ) -> Result<reqwest::Response, String> {
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
}

async fn parse_response(resp: reqwest::Response) -> Result<Value, String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("failed to read response: {e}"))?;

    if !status.is_success() {
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
            .unwrap_or_else(|| format!("{status}: {body}"));
        return Err(msg);
    }

    serde_json::from_str(&body).map_err(|e| format!("invalid JSON: {e}"))
}
