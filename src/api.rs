use reqwest::Client;
use serde_json::Value;

use crate::config::load_config;

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
            .map_err(|e| format!("request failed: {e}"))?;

        parse_response(resp).await
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<Value, String> {
        let resp = self
            .client
            .post(self.url("/api/v1/auth/login"))
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        parse_response(resp).await
    }

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
            .map_err(|e| format!("request failed: {e}"))?;

        parse_response(resp).await
    }

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
            .map_err(|e| format!("request failed: {e}"))?;

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
            .map_err(|e| format!("request failed: {e}"))?;

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
            .map_err(|e| format!("request failed: {e}"))?;

        parse_response(resp).await
    }

    pub async fn get_region(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/api/v1/region"))
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        parse_response(resp).await
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
            .map_err(|e| format!("request failed: {e}"))?;

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
            .map_err(|e| format!("request failed: {e}"))?;

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
            .map_err(|e| format!("request failed: {e}"))?;

        parse_response(resp).await
    }

    /// Create a share link for a file.
    pub async fn create_share(
        &self,
        file_id: &str,
        expires_in_hours: Option<u64>,
        max_opens: Option<u32>,
        passphrase: Option<&str>,
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
        let resp = self
            .client
            .post(self.url("/api/v1/shares"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

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
            .map_err(|e| format!("request failed: {e}"))?;

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
            .map_err(|e| format!("request failed: {e}"))?;

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
            .map_err(|e| format!("request failed: {e}"))?;
        parse_response(resp).await
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
            .map_err(|e| format!("request failed: {e}"))?;

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
