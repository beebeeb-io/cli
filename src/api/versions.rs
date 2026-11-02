//! `ApiClient` download and thumbnail calls.

use super::*;

impl ApiClient {
    /// Upload an encrypted thumbnail for a file.
    ///
    /// `PUT /api/v1/files/{file_id}/thumbnail` with the encrypted blob as the
    /// request body. The server stores it as a reserved chunk index and sets
    /// `has_thumbnail = true` on the file row.
    pub async fn upload_thumbnail(&self, file_id: &str, encrypted_data: Vec<u8>) -> Result<(), ApiError> {
        let token = self.require_auth()?;
        for _ in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .put(self.url(&format!("/api/v1/files/{file_id}/thumbnail")))
                .bearer_auth(token)
                .header("Content-Type", "application/octet-stream")
                .body(encrypted_data.clone())
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e.message == "__rate_limited__" => continue,
                Err(e) => return Err(format!("thumbnail upload: {e}").into()),
                Ok(_) => return Ok(()),
            }
        }
        Err("thumbnail upload rate limited after 3 retries".to_string().into())
    }

    /// Upload an encrypted large thumbnail for a file.
    pub async fn upload_thumbnail_large(&self, file_id: &str, encrypted_data: Vec<u8>) -> Result<(), ApiError> {
        let token = self.require_auth()?;
        for _ in 0..3 {
            pace_if_needed().await;
            let resp = self
                .client
                .put(self.url(&format!("/api/v1/files/{file_id}/thumbnail/large")))
                .bearer_auth(token)
                .header("Content-Type", "application/octet-stream")
                .body(encrypted_data.clone())
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e.message == "__rate_limited__" => continue,
                Err(e) => return Err(format!("large thumbnail upload: {e}").into()),
                Ok(_) => return Ok(()),
            }
        }
        Err("large thumbnail upload rate limited after 3 retries".to_string().into())
    }

    /// Open a streaming download of a file's encrypted bytes.
    ///
    /// Returns the live `Response` (headers + unread body) so the caller can
    /// read the body incrementally with `Response::chunk()` instead of
    /// buffering the whole payload. A 600s timeout overrides the client default
    /// (large files), matching `beebeeb-upload`.
    pub async fn download_stream(&self, file_id: &str) -> Result<reqwest::Response, ApiError> {
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
            return Err(format!("download failed ({status}): {body}").into());
        }
        Ok(resp)
    }

    /// Download the raw encrypted bytes for a file.
    pub async fn download_file(&self, file_id: &str) -> Result<Vec<u8>, ApiError> {
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
            return Err(format!("download failed ({status}): {body}").into());
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("failed to read response: {e}").into())
    }

    // ── Client device + session API ──────────────────────────────────────

}
