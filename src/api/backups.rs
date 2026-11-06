//! `ApiClient` calls for the Backups root (LD-0113, spec §5.2).

use super::*;

impl ApiClient {
    /// `PUT /api/v1/backups/root`: find, claim or create the account's Backups
    /// root in one call. `folder_id` is the folder to claim (a live root-level
    /// folder) or, with `name_encrypted`, the client-minted id of a new root.
    /// Answers `{ id, outcome: "existing" | "claimed" | "created" }`; a lost
    /// race answers `existing` with the winner's id.
    pub async fn put_backups_root(&self, folder_id: Uuid, name_encrypted: Option<&str>) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let mut body = serde_json::json!({ "folder_id": folder_id });
        if let Some(name) = name_encrypted {
            body["name_encrypted"] = serde_json::json!(name);
        }
        let resp = self
            .client
            .put(self.url("/api/v1/backups/root"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }
}
