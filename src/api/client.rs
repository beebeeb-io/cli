//! `ApiClient` constructors, auth, account, billing, sessions and device calls.

use super::*;

impl ApiClient {
    pub fn from_config() -> Self {
        Self::with_config(load_config())
    }

    /// [`from_config`](Self::from_config) for a config already in hand.
    pub(super) fn with_config(config: crate::config::Config) -> Self {
        // 300s per-request timeout, matching beebeeb-upload, so a stalled chunk
        // PUT fails instead of hanging forever. Long-lived calls that need more
        // (the SSE sync stream) override this per-request with their own
        // `.timeout(...)`, so this default is safe for them.
        let client = build_client(std::time::Duration::from_secs(300));
        // The direct-transfer state is the process's, per API base: a daemon builds a new client for every resync,
        // and what discovery said (and that a direct upload was given up on) must outlive each of them.
        let direct = direct::DirectState::shared(&config.api_url);
        Self {
            client,
            base_url: config.api_url,
            token: config.session_token,
            refusal_account_state: tokio::sync::OnceCell::new(),
            direct,
            storage_client: std::sync::OnceLock::new(),
            upload_storage_client: std::sync::OnceLock::new(),
        }
    }

    /// Test-only constructor: point the client at an arbitrary base URL (e.g. a
    /// mock server) with a dummy auth token, bypassing the on-disk config. Uses
    /// the same `build_client` as production so tests exercise the real
    /// header-setting path, not a hand-rolled stand-in.
    #[cfg(test)]
    pub(crate) fn new_for_test(base_url: String) -> Self {
        Self::new_for_test_with(
            base_url,
            Some("test-token"),
            std::sync::Arc::new(direct::DirectState::new()),
        )
    }

    /// [`new_for_test`](Self::new_for_test) with no session at all (`bb login` was never run).
    #[cfg(test)]
    pub(crate) fn new_for_test_logged_out(base_url: String) -> Self {
        Self::new_for_test_with(base_url, None, std::sync::Arc::new(direct::DirectState::new()))
    }

    /// [`new_for_test`](Self::new_for_test) with a shorter hold after a direct upload was given up, so a test can
    /// tell the per-file flag apart from the remembered failure.
    #[cfg(test)]
    pub(crate) fn new_for_test_with_failure_hold(base_url: String, hold: std::time::Duration) -> Self {
        Self::new_for_test_with(
            base_url,
            Some("test-token"),
            std::sync::Arc::new(direct::DirectState::with_holds(hold, direct::REFUSAL_HOLD)),
        )
    }

    /// The general test constructor: any token (or none) and a direct state the test shares between clients, as the
    /// production constructor does per API base. Keeps the loopback assertion.
    #[cfg(test)]
    pub(crate) fn new_for_test_with(
        base_url: String,
        token: Option<&str>,
        state: std::sync::Arc<direct::DirectState>,
    ) -> Self {
        // Fail closed: a unit test never talks to anything but a server on this machine.
        assert!(
            crate::config::api_url_is_loopback(&base_url),
            "a test must point the client at a local server, not {base_url}"
        );
        Self {
            client: build_client(std::time::Duration::from_secs(300)),
            base_url,
            token: token.map(str::to_string),
            refusal_account_state: tokio::sync::OnceCell::new(),
            direct: state,
            storage_client: std::sync::OnceLock::new(),
            upload_storage_client: std::sync::OnceLock::new(),
        }
    }

    pub fn require_auth(&self) -> Result<&str, ApiError> {
        self.token
            .as_deref()
            .ok_or_else(|| NOT_LOGGED_IN_MESSAGE.to_string().into())
    }

    pub(super) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    #[allow(dead_code)]
    pub async fn opaque_login_start(&self, email: &str, client_message_b64: &str) -> Result<Value, ApiError> {
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
    ) -> Result<Value, ApiError> {
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

    pub async fn logout(&self) -> Result<Value, ApiError> {
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

    /// `GET /api/v1/onboarding` (task 1750; server task 1739, spec 5.2): the
    /// backend-driven onboarding document. Best-effort by design: the caller
    /// falls back to the legacy `/billing/subscription` logic on `Ok(None)`
    /// (a server that predates the endpoint answers 404/405, spec 5.8 rule 6)
    /// and on `Err`.
    ///
    /// `with_session` sends the stored token, so the server answers
    /// `stage: "account"`; without it the answer is `stage: "pre_account"`
    /// (what `bb signup` reads). The extra headers are self-declared context
    /// (spec 5.2): schema major we understand, OS, and `direct` (the CLI is
    /// distributed outside any app store). A short timeout keeps a slow
    /// onboarding call from delaying the command it only decorates.
    pub async fn get_onboarding(&self, with_session: bool) -> Result<Option<Value>, ApiError> {
        let mut req = self
            .client
            .get(self.url("/api/v1/onboarding"))
            .timeout(std::time::Duration::from_secs(5))
            .header("X-Beebeeb-Onboarding-Schema", "1")
            .header("X-Beebeeb-Client-OS", onboarding_os())
            .header("X-Beebeeb-Store-Channel", "direct");
        if with_session {
            req = req.bearer_auth(self.require_auth()?);
        }
        let resp = req.send().await.map_err(format_request_error)?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Ok(None);
        }
        parse_response(resp).await.map(Some)
    }

    pub async fn get_me(&self) -> Result<Value, ApiError> {
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

    /// POST /api/v1/auth/2fa/setup → `{secret, qr_uri, backup_codes}`.
    ///
    /// NOTE the route is `/api/v1/auth/2fa/setup`, not the plan's
    /// `/api/v1/auth/account/2fa/setup` — `repos/server/beebeeb-api/src/routes/totp.rs`
    /// is nested at `/api/v1/auth/2fa` in `router.rs`; the CLI follows the live
    /// server (task 0477 already documented this deviation for `status`).
    ///
    /// No request body: an empty body is a fresh (first-time) setup. The
    /// server's step-up gate for an *already-enabled* account (a `code` field
    /// in the body, or a confirmed-password header) is out of scope for this
    /// command — `commands::twofa::setup()` refuses locally before ever
    /// reaching this call when `bb 2fa status` already reports enabled.
    pub async fn totp_setup(&self) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/auth/2fa/setup"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// POST /api/v1/auth/2fa/enable — activates a pending `2fa/setup` with a
    /// live TOTP code. Body is `{"code": "..."}` (`CodeRequest` in
    /// `repos/server/beebeeb-api/src/routes/totp.rs`). No `X-Confirm-Token`
    /// step-up — the code itself IS the proof of possession the route checks.
    pub async fn totp_enable(&self, code: &str) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/auth/2fa/enable"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "code": code }))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response_typed(resp).await
    }

    /// POST /api/v1/auth/2fa/disable — turns TOTP off. Body is
    /// `{"code": "..."}`, same `CodeRequest` shape as `enable`. The live
    /// handler (`routes/totp.rs::disable`) enforces ONLY the TOTP code —
    /// no `X-Confirm-Token` step-up — so `commands::twofa::disable` sends
    /// nothing beyond the code (eng-0479, deviation from the plan's "DELETE +
    /// step-up confirm" description; see task Notes for the file:line cite).
    pub async fn totp_disable(&self, code: &str) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/auth/2fa/disable"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "code": code }))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response_typed(resp).await
    }

    /// Not called by any current command — reserved API surface.
    #[allow(dead_code)]
    pub async fn get_region(&self) -> Result<Value, ApiError> {
        let resp = self
            .client
            .get(self.url("/api/v1/region"))
            .send()
            .await
            .map_err(format_request_error)?;

        parse_response(resp).await
    }

    /// `GET /api/v1/storage-regions` — the public list of places files can be
    /// stored (task 1919, by country since task 2050): the default "Europe"
    /// row, live countries, and coming-soon ones. No auth. The CLI keeps no
    /// list of its own.
    pub async fn get_storage_regions(&self) -> Result<Value, ApiError> {
        let resp = self
            .client
            .get(self.url("/api/v1/storage-regions"))
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// `PUT /api/v1/me/region` — store the region choice. `Some("europe")` =
    /// default/any data center, `Some("<country id>")` = that country (`de`),
    /// `None` clears the choice (also the default). The id is sent exactly as
    /// the list gave it; a legacy datacenter id is mapped by the server for one
    /// release. A coming-soon or unknown id is a 400
    /// `region_unavailable` (the wire shapes are in
    /// `commands::region::is_region_unavailable`); the stored value then
    /// stays unchanged.
    pub async fn set_my_region(&self, region: Option<&str>) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .put(self.url("/api/v1/me/region"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "preferred_region": region }))
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response_typed(resp).await
    }

    pub async fn get_my_region(&self) -> Result<Value, ApiError> {
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
    pub async fn ping_health(&self) -> Result<u128, ApiError> {
        let start = std::time::Instant::now();
        let resp = self
            .client
            .get(self.url("/api/v1/health"))
            .send()
            .await
            .map_err(format_request_error)?;
        let latency_ms = start.elapsed().as_millis();
        if !resp.status().is_success() {
            return Err(format!("health check failed: {}", resp.status()).into());
        }
        Ok(latency_ms)
    }

    pub async fn get_subscription(&self) -> Result<Value, ApiError> {
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
    pub async fn security_score(&self) -> Result<serde_json::Value, ApiError> {
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
    pub async fn list_sessions_v2(&self) -> Result<serde_json::Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/account/sessions"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response_typed(resp).await
    }

    /// DELETE /api/v1/account/sessions/{id} — revoke one session by id.
    ///
    /// **Deviation (eng-0481):** the live handler
    /// (`account_activity.rs::revoke_session`, ~L325-345) takes only
    /// `AuthUser` — no `ConfirmedAction` extractor — so this sends no
    /// `X-Confirm-Token`. The plan pseudocode for this task assumed step-up
    /// was required; it isn't, confirmed by reading the handler signature.
    pub async fn revoke_session(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .delete(self.url(&format!("/api/v1/account/sessions/{id}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response_typed(resp).await
    }

    /// POST /api/v1/account/sessions/revoke-all-others — revoke every session
    /// except the one making this call.
    ///
    /// **Deviation (eng-0481):** same as `revoke_session` above — the live
    /// handler (`account_activity.rs::revoke_all_other_sessions`, ~L351-373)
    /// takes only `AuthUser`, no step-up. No `X-Confirm-Token` is sent.
    pub async fn revoke_all_other_sessions(&self) -> Result<serde_json::Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/account/sessions/revoke-all-others"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response_typed(resp).await
    }

    /// GET /api/v1/auth/passkeys
    pub async fn list_passkeys(&self) -> Result<serde_json::Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/auth/passkeys"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response_typed(resp).await
    }

    /// DELETE /api/v1/auth/passkeys/{id} — remove one passkey by id.
    ///
    /// **Deviation (eng-0484, task 0484):** the plan's Task 16 pseudocode
    /// assumed step-up (`X-Confirm-Token`, via `acquire_confirm_token`) and a
    /// server-side "last sign-in method" refusal. Reading the live handler
    /// (`repos/server/beebeeb-api/src/routes/passkeys.rs::delete_passkey`,
    /// ~L388-406) end to end: it takes `AuthUser` + `NotImpersonated` +
    /// `Path(id)` only — no `ConfirmedAction` extractor — and its body is
    /// exactly `DELETE FROM passkeys WHERE id = $1 AND user_id = $2`, with no
    /// count-of-remaining-passkeys check anywhere before it. So (a) no
    /// `X-Confirm-Token` is sent, same reasoning as `revoke_session` above
    /// (eng-0481); a token the server never reads is a fabricated password
    /// prompt, not a safety measure. (b) there is no "last sign-in method"
    /// 400 to surface — the live server does not defend against a
    /// passkey-only account deleting its only passkey (a real server-side gap,
    /// out of scope for this CLI-only task; the CLI still confirms
    /// client-side before calling this, see `commands::passkey::remove`).
    pub async fn delete_passkey(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .delete(self.url(&format!("/api/v1/auth/passkeys/{id}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response_typed(resp).await
    }

    /// Step-up re-auth (task 0856). Returns the raw confirmation token; the
    /// caller attaches it as `X-Confirm-Token` on the protected call within 5
    /// minutes.
    ///
    /// The plaintext password never leaves the process for an OPAQUE account.
    /// The sequence mirrors web's `confirmAction` and uses the same
    /// `beebeeb_core::opaque_protocol` client half as the web/mobile login:
    ///
    /// 1. `client_login_start` locally, then `POST /auth/confirm-opaque-start`
    ///    `{ client_message }` -> `{ server_message, server_state, ksf_version }`.
    /// 2. `client_login_finish` locally with the account's `ksf_version`. A
    ///    wrong password fails HERE (the server MAC does not verify), so no
    ///    finish request is sent: reported as the LOCAL code
    ///    `opaque_password_mismatch`, which `commands::confirm::map_confirm_error`
    ///    renders as "incorrect password". It is deliberately not `unauthorized`:
    ///    a server 401 on start/finish means the SESSION is dead (the password
    ///    is only ever checked locally), and must keep its session-expired text.
    /// 3. `POST /auth/confirm-opaque-finish` `{ client_message, server_state }`
    ///    -> `{ confirmation_token, expires_at }`.
    ///
    /// The ONLY plaintext path is the account that has no OPAQUE credential: the
    /// server answers step 1 with `409 { "opaque_unavailable": true }` and we
    /// fall back to the legacy `POST /auth/confirm`. Any other failure, including
    /// any other 409, is surfaced, never retried in plaintext.
    ///
    /// The returned [`ConfirmMethod`] says which of the two paths minted the
    /// token, so a caller can tell an OPAQUE account from a legacy one without
    /// sending the password anywhere (task 1907).
    pub async fn confirm_password(&self, password: &str) -> Result<(String, ConfirmMethod), ApiError> {
        use base64::Engine;
        use beebeeb_core::opaque_protocol as opaque;
        use zeroize::Zeroizing;

        let b64 = base64::engine::general_purpose::STANDARD;
        let token = self.require_auth()?;

        let started = opaque::client_login_start(password.as_bytes()).map_err(|e| e.to_string())?;
        // The client state is derived from the password: wipe it when done.
        let client_state = Zeroizing::new(started.state);

        let resp = self
            .client
            .post(self.url("/api/v1/auth/confirm-opaque-start"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "client_message": b64.encode(&started.message) }))
            .send()
            .await
            .map_err(format_request_error)?;

        let start = if resp.status() == reqwest::StatusCode::CONFLICT {
            // `{ "opaque_unavailable": true }` carries no `error` code, so read
            // the body here rather than through `parse_response_typed`.
            let status = resp.status().as_u16();
            let text = resp.text().await.map_err(|e| ApiError {
                code: None,
                message: format!("failed to read response: {e}"),
                status,
            })?;
            let parsed = serde_json::from_str::<Value>(&text).ok();
            if parsed
                .as_ref()
                .and_then(|v| v.get("opaque_unavailable"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                let token = self.confirm_password_plaintext(password).await?;
                return Ok((token, ConfirmMethod::LegacyPlaintext));
            }
            let field = |k: &str| {
                parsed
                    .as_ref()
                    .and_then(|v| v.get(k))
                    .and_then(Value::as_str)
                    .map(String::from)
            };
            let code = field("error");
            return Err(ApiError {
                message: field("message")
                    .or_else(|| code.clone())
                    .unwrap_or_else(|| format!("{status}: {text}")),
                code,
                status,
            });
        } else {
            parse_response_typed(resp).await?
        };

        let missing = |what: &str| ApiError::from(format!("server confirm-opaque-start response had no {what}"));
        let server_message = b64
            .decode(
                start
                    .get("server_message")
                    .and_then(Value::as_str)
                    .ok_or_else(|| missing("server_message"))?,
            )
            .map_err(|e| format!("invalid server_message: {e}"))?;
        let server_state = start
            .get("server_state")
            .and_then(Value::as_str)
            .ok_or_else(|| missing("server_state"))?;
        // 0 = legacy Identity KSF, 1 = Argon2id. Default to the current suite
        // when the field is absent (same as mobile).
        let ksf_version = start.get("ksf_version").and_then(Value::as_u64).unwrap_or(1) as u32;

        let finished = opaque::client_login_finish(&client_state, password.as_bytes(), &server_message, ksf_version)
            .map_err(|_| ApiError {
                code: Some(OPAQUE_PASSWORD_MISMATCH.to_string()),
                message: "incorrect password".to_string(),
                status: 0,
            })?;
        // Session/export keys are not used by step-up: wipe them.
        let _session_key = Zeroizing::new(finished.session_key);
        let _export_key = Zeroizing::new(finished.export_key);

        let resp = self
            .client
            .post(self.url("/api/v1/auth/confirm-opaque-finish"))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "client_message": b64.encode(&finished.message),
                "server_state": server_state,
            }))
            .send()
            .await
            .map_err(format_request_error)?;
        let token = confirmation_token_from(parse_response_typed(resp).await?)?;
        Ok((token, ConfirmMethod::Opaque))
    }

    /// Legacy step-up: `POST /api/v1/auth/confirm` with the plaintext password.
    /// Reached ONLY from [`Self::confirm_password`], for an account whose
    /// `confirm-opaque-start` answered `409 opaque_unavailable` (no OPAQUE
    /// credential). It is also where the server's session-freshness fallback
    /// (`session_too_old_for_confirmation`) lives.
    async fn confirm_password_plaintext(&self, password: &str) -> Result<String, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/auth/confirm"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "password": password }))
            .send()
            .await
            .map_err(format_request_error)?;
        // `confirm-opaque-start` just accepted this bearer (it answered 409,
        // not 401), so a 401 here is the legacy endpoint rejecting the
        // PASSWORD, not a dead session. Re-label it so `map_confirm_error`
        // does not show the session-expired text for a typo.
        let body = parse_response_typed(resp).await.map_err(|e| {
            if e.status == 401 && e.is_code("unauthorized") {
                ApiError {
                    code: Some(OPAQUE_PASSWORD_MISMATCH.to_string()),
                    message: "incorrect password".to_string(),
                    status: e.status,
                }
            } else {
                e
            }
        })?;
        confirmation_token_from(body)
    }

    /// POST /api/v1/auth/account/export — queue or resume a GDPR export job.
    /// Not called by any current command — reserved API surface.
    #[allow(dead_code)]
    pub async fn request_account_export(&self, confirm_token: &str) -> Result<Value, ApiError> {
        if confirm_token.trim().is_empty() {
            return Err("confirmation token is required to request an account export"
                .to_string()
                .into());
        }

        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/auth/account/export"))
            .bearer_auth(token)
            .header("X-Confirm-Token", confirm_token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// PUT /api/v1/auth/account/email — legacy password-account flow.
    pub async fn account_email_change_legacy(
        &self,
        new_email: &str,
        password: &str,
        confirm_token: &str,
    ) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .put(self.url("/api/v1/auth/account/email"))
            .bearer_auth(token)
            .header("X-Confirm-Token", confirm_token)
            .json(&serde_json::json!({
                "new_email": new_email,
                "password": password,
            }))
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// POST /api/v1/me/email/start — OPAQUE email-change step 1.
    /// Not called by any current command — reserved API surface.
    #[allow(dead_code)]
    pub async fn account_email_change_start_opaque(
        &self,
        new_email: &str,
        opaque_client_message_b64: &str,
        confirm_token: &str,
    ) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/me/email/start"))
            .bearer_auth(token)
            .header("X-Confirm-Token", confirm_token)
            .json(&serde_json::json!({
                "new_email": new_email,
                "opaque_client_message": opaque_client_message_b64,
            }))
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// POST /api/v1/me/email/finish — OPAQUE email-change step 2.
    /// Not called by any current command — reserved API surface.
    #[allow(dead_code)]
    pub async fn account_email_change_finish_opaque(
        &self,
        email_change_token: &str,
        opaque_registration_b64: &str,
        recovery_check_b64: &str,
        x25519_public_key_b64: &str,
    ) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/me/email/finish"))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "email_change_token": email_change_token,
                "opaque_registration": opaque_registration_b64,
                "recovery_check": recovery_check_b64,
                "x25519_public_key": x25519_public_key_b64,
            }))
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// Best-effort fetch of recovery_check + x25519_public_key for OPAQUE
    /// email change. Returns `(recovery_check, x25519_pub)` or an error if the
    /// server has no route or the user has no key material on file.
    /// Not called by any current command — reserved API surface.
    #[allow(dead_code)]
    pub async fn get_my_key_material(&self) -> Result<(Vec<u8>, Vec<u8>), ApiError> {
        use base64::Engine;

        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/me/keys"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        let body = parse_response(resp).await?;
        let b64 = base64::engine::general_purpose::STANDARD;
        let recovery_check = body
            .get("recovery_check")
            .and_then(|v| v.as_str())
            .ok_or("no recovery_check in response")?;
        let x25519_public_key = body
            .get("x25519_public_key")
            .and_then(|v| v.as_str())
            .ok_or("no x25519_public_key in response")?;
        Ok((
            b64.decode(recovery_check)
                .map_err(|e| format!("decode recovery_check: {e}"))?,
            b64.decode(x25519_public_key)
                .map_err(|e| format!("decode x25519_public_key: {e}"))?,
        ))
    }

    /// Alias of `get_subscription` for use from `bb billing show`. The Spec 2
    /// billing tree will move to a dedicated `/api/v1/billing/*` namespace, so
    /// keep the call sites pointed at a billing-named function from day one.
    pub async fn get_billing_subscription(&self) -> Result<Value, ApiError> {
        self.get_subscription().await
    }

    /// GET /api/v1/billing/usage — `{ used_bytes, quota_bytes, percentage }`,
    /// server-authoritative (the server's quota module: base plan quota
    /// from the DB `plans` catalog + referral
    /// `bonus_storage_bytes` + `extra_storage_tb`, capped at `MAX_TOTAL_STORAGE`).
    /// 0485: this used to alias `get_usage()`, which actually calls
    /// `/api/v1/files/usage` — a DIFFERENT route returning `plan_limit_bytes`/
    /// `plan_name` instead of `quota_bytes`, so `bb billing show` never had a
    /// `quota_bytes` field to read and fell back to recomputing the quota
    /// client-side (see `commands::billing::show`). Hits the real billing
    /// route directly so `bb billing show` doesn't reach into the files API.
    pub async fn get_billing_usage(&self) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/billing/usage"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// GET /api/v1/billing/plans — the public plan catalog (no auth). Returns
    /// `{ "plans": [ { "id", "name", "price_eur", "price_yearly_eur",
    /// "storage_label", … }, … ] }`. This is the catalog source of truth used to
    /// render plan prices instead of a hardcoded CLI table.
    pub async fn get_billing_plans(&self) -> Result<Value, ApiError> {
        let resp = self
            .client
            .get(self.url("/api/v1/billing/plans"))
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    pub async fn get_billing_addons(&self) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/billing/addons"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    pub async fn update_billing_addons(&self, body: Value) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .post(self.url("/api/v1/billing/addons"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// GET /api/v1/billing/invoices — the caller's VAT-compliant invoices
    /// (task 0916; the Mollie-backed `invoices` table). Returns
    /// `{ invoices: [...] }` — see
    /// `beebeeb-api/src/routes/billing.rs::invoices`. Emptiness of the
    /// `invoices` array alone decides "no invoices yet" (see
    /// `commands::billing::invoices`, task 0487).
    pub async fn get_billing_invoices(&self) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url("/api/v1/billing/invoices"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }

    /// GET /api/v1/billing/invoices/{id}/pdf — download one invoice as a PDF
    /// (task 0916). Auth-scoped to the caller (the server 404s another user's
    /// invoice id, no cross-user existence leak). Returns the raw PDF bytes —
    /// this is NOT a public link; a bare `curl <pdf_url>` with no bearer token
    /// gets a 401, which is why `bb billing invoices --open <id>` downloads
    /// through this authenticated client instead of opening a browser URL.
    pub async fn download_invoice_pdf(&self, invoice_id: &str) -> Result<Vec<u8>, ApiError> {
        let token = self.require_auth()?;
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/billing/invoices/{invoice_id}/pdf")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(format_request_error)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("invoice pdf download failed ({status}): {body}").into());
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("failed to read response: {e}").into())
    }

    pub async fn get_sessions(&self) -> Result<Value, ApiError> {
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

    /// Mint a short-lived (~1h) bearer token for the SSE sync stream. The
    /// stream cannot use the main session token because SSE auth lives in
    /// the URL query string (browsers can't set headers on EventSource).
    ///
    /// Not currently called — `sync.rs`'s continuous watcher doesn't
    /// establish an SSE connection today (no `EventSource`/`/sessions/live`
    /// reference anywhere in it), so this looks like reserved API surface
    /// for an SSE-based live-sync mode that hasn't landed in the CLI yet.
    /// See also [`Self::stream_url`], [`Self::list_ops_since`],
    /// [`Self::open_sync_stream`] — same unused SSE cluster.
    #[allow(dead_code)]
    pub async fn create_stream_token(&self) -> Result<String, ApiError> {
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
            .ok_or_else(|| "server did not return a stream_token".to_string().into())
    }

    /// Build the absolute URL of the SSE endpoint for a given stream token.
    ///
    /// Part of the same unused SSE live-sync surface as
    /// [`Self::create_stream_token`] — see that method's doc for context.
    /// Only used internally by [`Self::open_sync_stream`], which is itself
    /// unused.
    #[allow(dead_code)]
    pub fn stream_url(&self, stream_token: &str) -> String {
        // Use a query-string token because SSE doesn't allow custom headers.
        // URL-encoding is unnecessary — the server-issued token is base64url
        // (no `+`, `/`, `=`, or other reserved characters).
        format!("{}/api/v1/sync/stream?token={stream_token}", self.base_url)
    }

    /// Fetch sync ops with `seq_id > since` so we can catch up after an SSE
    /// reconnect. The server caps `limit` at 5000.
    ///
    /// Part of the same unused SSE live-sync surface as
    /// [`Self::create_stream_token`] — see that method's doc for context.
    #[allow(dead_code)]
    pub async fn list_ops_since(&self, since: i64) -> Result<Value, ApiError> {
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
    ///
    /// Part of the same unused SSE live-sync surface as
    /// [`Self::create_stream_token`] — see that method's doc for context.
    /// This was `bb watch`'s SSE consumer before that command was removed
    /// (2026-08-13, CI cleanup) in favor of `bb sync`'s polling watcher.
    #[allow(dead_code)]
    pub async fn open_sync_stream(&self, stream_token: &str) -> Result<reqwest::Response, ApiError> {
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
            return Err(format!("sse handshake failed ({status}): {body}").into());
        }
        Ok(resp)
    }

    pub async fn speedtest_upload(&self, data: &[u8]) -> Result<(), ApiError> {
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
            return Err(format!("speedtest upload ({status}): {body}").into());
        }
        Ok(())
    }

    pub async fn speedtest_download(&self, size: usize) -> Result<Vec<u8>, ApiError> {
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
            return Err(format!("speedtest download ({status}): {body}").into());
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("speedtest read: {e}").into())
    }

    /// Register (upsert) a client device with the server.
    ///
    /// Task 1922: the hostname is only ever sent as a field-1 `bbcm1.` envelope
    /// sealed client-side under the device's own record key. The plaintext
    /// `hostname` key is gone; the server stores no device text it could echo.
    pub async fn register_device(
        &self,
        client_key: &Uuid,
        hostname_encrypted: &str,
        platform: &str,
        bb_version: &str,
    ) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let body = serde_json::json!({
            "client_key": client_key,
            "hostname_encrypted": hostname_encrypted,
            "platform": platform,
            "bb_version": bb_version,
        });
        for _ in 0..3 {
            pace_if_needed(&self.base_url).await;
            let resp = self
                .client
                .post(self.url("/api/v1/clients/devices"))
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e.message == super::RATE_LIMITED_MESSAGE => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string().into())
    }

    /// Create a new client session (sync, mount, etc.).
    ///
    /// Task 1922: name and both paths are `bbcm1.` envelopes (fields 2-4) sealed
    /// under the session's own client-generated record key, with the parent
    /// device's client key in the AAD. No plaintext name or path is sent.
    pub async fn create_client_session(
        &self,
        device_id: &str,
        client_key: &Uuid,
        name_encrypted: &str,
        session_type: &str,
        local_path_encrypted: &str,
        remote_path_encrypted: &str,
    ) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let body = serde_json::json!({
            "device_id": device_id,
            "client_key": client_key,
            "name_encrypted": name_encrypted,
            "session_type": session_type,
            "local_path_encrypted": local_path_encrypted,
            "remote_path_encrypted": remote_path_encrypted,
        });
        for _ in 0..3 {
            pace_if_needed(&self.base_url).await;
            let resp = self
                .client
                .post(self.url("/api/v1/clients/sessions"))
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(format_request_error)?;
            match parse_response(resp).await {
                Err(e) if e.message == super::RATE_LIMITED_MESSAGE => continue,
                other => return other,
            }
        }
        Err("rate limited after 3 retries".to_string().into())
    }

    /// Fire-and-forget heartbeat — no retry on rate limit.
    ///
    /// Task 1922: a heartbeat carries numbers and a status word only. There is
    /// no `current_file` and no `detail`; the server discards both anyway.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_heartbeat(
        &self,
        session_id: &str,
        status: &str,
        files_synced: Option<u64>,
        files_total: Option<u64>,
        bytes_synced: Option<u64>,
        bytes_total: Option<u64>,
        speed_bps: Option<u64>,
    ) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let body = serde_json::json!({
            "status": status,
            "files_synced": files_synced,
            "files_total": files_total,
            "bytes_synced": bytes_synced,
            "bytes_total": bytes_total,
            "speed_bps": speed_bps,
        });
        // Heartbeats are fire-and-forget — don't retry
        pace_if_needed(&self.base_url).await;
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

    pub async fn list_client_sessions(&self) -> Result<Value, ApiError> {
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
    pub async fn stop_client_session(&self, session_id: &str) -> Result<Value, ApiError> {
        let token = self.require_auth()?;
        let body = serde_json::json!({ "status": "stopped" });
        let resp = self
            .client
            .patch(self.url(&format!("/api/v1/clients/sessions/{session_id}")))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(format_request_error)?;
        parse_response(resp).await
    }
}
