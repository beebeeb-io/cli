use reqwest::Client;
use serde_json::Value;
use std::path::Path;

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

    pub async fn upload_file(
        &self,
        file_path: &Path,
        parent_id: Option<&str>,
    ) -> Result<Value, String> {
        let token = self.require_auth()?;

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let file_bytes = std::fs::read(file_path)
            .map_err(|e| format!("failed to read file: {e}"))?;

        let file_part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name)
            .mime_str("application/octet-stream")
            .map_err(|e| format!("mime error: {e}"))?;

        let mut form = reqwest::multipart::Form::new().part("file", file_part);

        if let Some(pid) = parent_id {
            form = form.text("parent_id", pid.to_string());
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
