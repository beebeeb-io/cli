//! `ApiClient` share and file-request calls.

use super::*;

impl ApiClient {
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
        share: &crate::commands::share::ShareMaterial,
    ) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        // Single-file, double-encrypted, owner-recoverable create — the same
        // body the web share dialog sends (see commands/share.rs "Share wire
        // format"). The raw share token is client-minted so it can be wrapped
        // under the master key; the server stores only its hash.
        let mut body = serde_json::json!({
            "file_id": file_id,
            "wrapped_file_key": share.wrapped_file_key,
            "token": share.token,
            "owner_wrapped_key": share.owner_wrapped_key,
            "owner_wrapped_token": share.owner_wrapped_token,
        });
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
            .map_err(format_request_error)?;

        // Task 1037: a plan-less / lapsed account can't create shares either
        // (409 plan_required / account_lapsed) — say why and where to fix it.
        match parse_response_typed(resp).await {
            Ok(v) => Ok(v),
            Err(e) => Err(self.explain_account_refusal(e).await),
        }
    }

    pub async fn list_shares(&self) -> Result<Value, ApiError> {
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
    pub async fn delete_share(&self, share_id: impl Into<ShareId>) -> Result<Value, ApiError> {
        let share_id = share_id.into();
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

    /// Every invite this account has sent: `GET /api/v1/shares/invites/sent`,
    /// `{ "invites": [...] }`. The CLI's one reader of this list. A server
    /// without the route (404/405/501) answers `Value::Null`, and each caller
    /// says what that means for its question (no invite to open a key from;
    /// for a push, nobody it can say is invited, so unknown); any other failure
    /// is an error, never "no invites".
    pub async fn list_sent_invites(&self) -> Result<Value, ApiError> {
        self.sent_invites(&[404, 405, 501]).await
    }

    /// [`Self::list_sent_invites`] for key resolution (task 2334): there only 405 or 501 say the route is absent.
    /// A 404 can come from a proxy or a misrouted node, and reading it as "no invite" would make the rows of a
    /// legacy-invite folder unopenable for good, so a push would pass over them and make duplicates: it is an
    /// error, which the resolver counts as a read that did not answer.
    pub async fn list_sent_invites_for_keys(&self) -> Result<Value, ApiError> {
        self.sent_invites(&[405, 501]).await
    }

    async fn sent_invites(&self, absent: &[u16]) -> Result<Value, ApiError> {
        self.get_feature_listing("/api/v1/shares/invites/sent", absent).await
    }

    /// This account's live folder links, all folders:
    /// `GET /api/v1/shares/folder-links`, `{ "links": [...] }`. A server without
    /// folder links, or with shared folders switched off for the account
    /// (`501 shared_folders_v1`), answers `Value::Null`; any other failure is
    /// an error, never "no links".
    pub async fn list_links_to_folders(&self) -> Result<Value, ApiError> {
        self.get_feature_listing("/api/v1/shares/folder-links", &[404, 405, 501])
            .await
    }

    /// A GET whose route a server may not have: a status in `absent` (404/405/501 unless the caller says
    /// otherwise, the same reading as [`ApiClient::get_shared_folder`]) is `Value::Null`.
    async fn get_feature_listing(&self, path: &str, absent: &[u16]) -> Result<Value, ApiError> {
        let resp = self
            .client
            .get(self.url(path))
            .bearer_auth(self.require_auth()?)
            .send()
            .await
            .map_err(format_request_error)?;
        if absent.contains(&resp.status().as_u16()) {
            return Ok(Value::Null);
        }
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
    ) -> Result<Value, ApiError> {
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
    pub async fn list_file_requests(&self) -> Result<Value, ApiError> {
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
    pub async fn close_file_request(&self, id: impl Into<ShareId>) -> Result<Value, ApiError> {
        let id = id.into();
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
    pub async fn delete_file_request(&self, id: impl Into<ShareId>) -> Result<Value, ApiError> {
        let id = id.into();
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
    ) -> Result<Value, ApiError> {
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
}
