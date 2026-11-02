//! `ApiClient` file-tree calls: listing, lookup, folders, trash, index blobs and conflict checks.

use super::*;

impl ApiClient {
    /// Fetch ONE page of `GET /api/v1/files`. `cursor` is the OPAQUE keyset
    /// token returned by the previous page (server task 0739) — passed back
    /// verbatim, never parsed. The server emits it base64url (URL_SAFE_NO_PAD,
    /// so no percent-escaping is needed in the query string). Returns the page's
    /// `files` rows plus `next_cursor` (`None` on the last page — the server
    /// returns it null once a short page is served). `parent_id` / `trashed`
    /// select the listing branch and MUST stay constant across a walk (the
    /// cursor is branch-shaped; a mismatched cursor is treated as first page).
    async fn list_files_page(
        &self,
        parent_id: Option<&str>,
        trashed: bool,
        cursor: Option<&str>,
    ) -> Result<(Vec<Value>, Option<String>), ApiError> {
        let token = self.require_auth()?;
        let mut params: Vec<String> = Vec::new();
        if let Some(pid) = parent_id {
            params.push(format!("parent_id={pid}"));
        }
        if trashed {
            params.push("trashed=true".to_string());
        }
        if let Some(c) = cursor {
            params.push(format!("cursor={c}"));
        }
        let mut url = self.url("/api/v1/files");
        if !params.is_empty() {
            url = format!("{url}?{}", params.join("&"));
        }
        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        let body = parse_response(resp).await?;
        let files = body
            .get("files")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let next_cursor = body.get("next_cursor").and_then(|v| v.as_str()).map(|s| s.to_string());
        Ok((files, next_cursor))
    }

    /// Enumerate EVERY entry of a listing branch by following the server's
    /// `next_cursor` until exhausted (server task 0739 keyset pagination), then
    /// return the SAME `{ "files": [...] }` shape every caller already expects.
    /// This replaces the old single-page call that silently truncated at the
    /// server's default page size (~200) — so `bb ls` / `bb pull` / sync / etc.
    /// now see the whole folder. Bounded by `FILE_LIST_HARD_CAP` as a safety
    /// valve (mirrors the web client's `FILE_LIST_HARD_CAP`, task 0755).
    async fn list_all_files(&self, parent_id: Option<&str>, trashed: bool) -> Result<Value, ApiError> {
        self.list_all_files_bounded(parent_id, trashed, FILE_LIST_HARD_CAP)
            .await
    }

    pub async fn list_files_bounded(&self, parent_id: impl Into<FolderId>, cap: usize) -> Result<Value, ApiError> {
        let parent_id = parent_id.into();
        self.list_all_files_bounded(Some(parent_id.as_str()), false, cap.min(FILE_LIST_HARD_CAP))
            .await
    }

    async fn list_all_files_bounded(
        &self,
        parent_id: Option<&str>,
        trashed: bool,
        cap: usize,
    ) -> Result<Value, ApiError> {
        let mut all: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        // Set when the cap cut the listing short. Callers that conclude
        // "deleted" from absence (bb sync) must treat such a listing as
        // incomplete (task 1915).
        let mut truncated = false;
        loop {
            let (mut page, next) = self.list_files_page(parent_id, trashed, cursor.as_deref()).await?;
            all.append(&mut page);
            if let Some(cut) = listing_cap_reached(&mut all, cap, next.is_some()) {
                truncated = cut;
                if cut {
                    eprintln!("warning: file listing reached the {cap}-entry safety cap; some entries may be omitted");
                }
                break;
            }
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(serde_json::json!({ "files": all, "truncated": truncated }))
    }

    /// List a folder's (or the root's) active files. Follows the server's
    /// keyset pagination internally so the result is the COMPLETE listing, not a
    /// silently-truncated first page. Returns `{ "files": [...] }`.
    pub async fn list_files(&self, parent_id: Option<&str>) -> Result<Value, ApiError> {
        self.list_all_files(parent_id, false).await
    }

    /// Fetch the whole-vault file index in ONE request (task 0810). Returns the
    /// flat array of every non-trashed file with `id`, `parent_id`,
    /// `name_encrypted`, `is_folder`, `size_bytes`, `created_at` — the client
    /// builds the tree. Used by `bb search` to avoid an HTTP request per folder.
    pub async fn files_index(&self) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/files/index"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// Get file metadata by ID.
    pub async fn get_file(&self, file_id: impl Into<FileId>) -> Result<Value, ApiError> {
        let file_id = file_id.into();
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

    /// Missing custody is explicit; network/auth/malformed responses never mean absent.
    pub async fn get_shared_folder(&self, id: impl Into<FolderId>) -> Result<Value, ApiError> {
        let id = id.into();
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/shared-folders/{id}")))
            .bearer_auth(self.require_auth()?)
            .send()
            .await
            .map_err(format_request_error)?;
        if matches!(resp.status().as_u16(), 404 | 405 | 501) {
            return Ok(Value::Null);
        }
        parse_response(resp).await
    }

    pub async fn get_shared_folder_keys(&self, id: impl Into<FolderId>, cursor: Option<&str>) -> Result<Value, ApiError> {
        let id = id.into();
        let mut request = self
            .client
            .get(self.url(&format!("/api/v1/shared-folders/{id}/keys")))
            .bearer_auth(self.require_auth()?)
            .query(&[("limit", "500")]);
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        parse_response(request.send().await.map_err(format_request_error)?).await
    }

    pub async fn adopt_owner_key(&self, id: impl Into<FileId>, wrap: &str) -> Result<bool, ApiError> {
        let id = id.into();
        let response = self
            .client
            .post(self.url(&format!("/api/v1/files/{id}/owner-key")))
            .bearer_auth(self.require_auth()?)
            .json(&serde_json::json!({"owner_wrapped_file_key": wrap}))
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(response).await?["adopted"]
            .as_bool()
            .ok_or_else(|| "missing adoption result".into())
    }

    /// Return all `{id, name_encrypted}` pairs in a folder so the caller can
    /// decrypt names locally and detect filename conflicts before uploading.
    /// `parent_id = None` queries the root folder.
    pub async fn check_conflict(&self, parent_id: Option<uuid::Uuid>) -> Result<Value, ApiError> {
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

    pub async fn get_file_count(&self) -> Result<Value, ApiError> {
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

    pub async fn get_usage(&self) -> Result<Value, ApiError> {
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

    pub async fn create_folder(
        &self,
        name_encrypted: &str,
        parent_id: Option<uuid::Uuid>,
        folder_id: Option<uuid::Uuid>,
    ) -> Result<Value, ApiError> {
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

    /// Rename and/or move a file or folder. `new_parent_id`: None leaves the
    /// parent unchanged, Some(None) moves to root, Some(Some(id)) to a folder.
    pub async fn move_file(
        &self,
        file_id: &str,
        new_name_encrypted: Option<&str>,
        new_parent_id: Option<Option<uuid::Uuid>>,
    ) -> Result<Value, ApiError> {
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
    pub async fn trash_file(&self, file_id: impl Into<FileId>) -> Result<Value, ApiError> {
        let file_id = file_id.into();
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

    /// Bulk-trash files/folders via `POST /api/v1/files/trash` with `{ids:[...]}`.
    /// The server cascades the trashed flag to folder contents. Returns
    /// `{trashed, already_trashed, missing}` (each a list of ids). Max 500 ids
    /// per call — the caller batches.
    pub async fn bulk_trash(&self, ids: &[String]) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/files/trash"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "ids": ids }))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// Bulk permanently delete via `POST /api/v1/files/permanent` with `{ids}`
    /// and one step-up `X-Confirm-Token`. The server erases only items that are
    /// both owned and already trashed (others come back in `skipped_not_trashed`
    /// / `missing`); irreversible. Returns `{deleted, skipped_not_trashed, missing}`.
    pub async fn bulk_permanent_delete(&self, ids: &[String], confirm_token: &str) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/files/permanent"))
            .bearer_auth(token)
            .header("X-Confirm-Token", confirm_token)
            .json(&serde_json::json!({ "ids": ids }))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// Permanently delete a file/folder via `DELETE /api/v1/files/{id}/permanent`.
    /// Irreversible — erases the row and its blobs. Requires a step-up
    /// `X-Confirm-Token` (minted by `confirm_password`); the server's
    /// `ConfirmedTrashAction` extractor refuses the call without it.
    /// Not called by any current command — reserved API surface.
    #[allow(dead_code)]
    pub async fn permanent_delete(&self, file_id: impl Into<FileId>, confirm_token: &str) -> Result<Value, ApiError> {
        let file_id = file_id.into();
        let token = self.require_auth()?;
        let resp = self
            .client
            .delete(self.url(&format!("/api/v1/files/{file_id}/permanent")))
            .bearer_auth(token)
            .header("X-Confirm-Token", confirm_token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// Restore a trashed file/folder via `POST /api/v1/files/{id}/restore`.
    pub async fn restore_file(&self, file_id: impl Into<FileId>) -> Result<Value, ApiError> {
        let file_id = file_id.into();
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url(&format!("/api/v1/files/{file_id}/restore")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// List trashed entries via `GET /api/v1/files?trashed=true`. Same shape as
    /// the active listing (`{ "files": [...] }`). Used by `bb trash list`,
    /// `bb restore <name>` (trashed-name match), and `bb ls -a`. Follows the
    /// server's keyset pagination internally so a trash with >200 entries is
    /// fully enumerated (task 0755).
    pub async fn list_trashed(&self) -> Result<Value, ApiError> {
        self.list_all_files(None, true).await
    }

    /// Generic authenticated GET that returns parsed JSON.
    #[allow(dead_code)]
    pub async fn get_json(&self, path: &str) -> Result<Value, ApiError> {
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

    /// Index pages are opaque, bounded blobs. Only the shared store has CAS.
    pub async fn index_blob(&self, path: &str) -> Result<Vec<u8>, ApiError> {
        let mut response = self
            .client
            .get(self.url(path))
            .bearer_auth(self.require_auth()?)
            .send()
            .await
            .map_err(format_request_error)?;
        if !response.status().is_success() {
            return Err(format!("index read: {}", response.status()).into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(format_request_error)? {
            if bytes.len() + chunk.len() > 160 * 1024 {
                return Err("index page exceeds limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub async fn write_index_page(
        &self,
        path: &str,
        blob: Option<Vec<u8>>,
        version: Option<u64>,
    ) -> Result<bool, ApiError> {
        let mut request = if let Some(blob) = blob {
            if blob.len() > 160 * 1024 {
                return Err("index page exceeds limit".into());
            }
            self.client
                .put(self.url(path))
                .header("Content-Type", "application/octet-stream")
                .body(blob)
        } else {
            self.client.delete(self.url(path))
        };
        if let Some(version) = version {
            request = request.query(&[("if_version", version)]);
        }
        let response = request
            .bearer_auth(self.require_auth()?)
            .send()
            .await
            .map_err(format_request_error)?;
        if response.status() == reqwest::StatusCode::CONFLICT && version.is_some() {
            let body: Value = response.json().await.map_err(|e| e.to_string())?;
            if body["error"] == "index_version_conflict" {
                return Ok(false);
            }
            return Err("index write conflict".into());
        }
        if !response.status().is_success() {
            return Err(format!("index write: {}", response.status()).into());
        }
        Ok(true)
    }

    /// Search all user files (root + one level of subfolders) for a file
    /// whose UUID starts with the given hex prefix. Returns the full UUID if
    /// exactly one match is found, `None` for zero matches, and an error for
    /// ambiguous (multiple) matches.
    pub async fn find_file_by_id_prefix(&self, prefix: &str) -> Result<Option<String>, ApiError> {
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
            ).into()),
        }
    }

}
