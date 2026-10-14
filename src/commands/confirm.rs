//! Step-up password confirmation — used by every destructive account action.
//!
//! The server's `ConfirmedAction` extractor requires an `X-Confirm-Token`
//! header on protected routes. The token is single-use with a 5-minute TTL.
//! We never cache; each call to `acquire_confirm_token` prompts the user fresh
//! and immediately uses the token.
//!
//! Task 0856: the token is minted over OPAQUE (`POST /auth/confirm-opaque-start`
//! then `-finish`, in `ApiClient::confirm_password`), so the plaintext password never
//! reaches the server. Only an account WITHOUT an OPAQUE credential (the server
//! answers `409 opaque_unavailable`) falls back to the legacy plaintext
//! `POST /auth/confirm`, where the server's session-freshness fallback also
//! lives (reported by `map_confirm_error` below).

use colored::Colorize;
use zeroize::Zeroizing;

use crate::api::{ApiClient, ApiError};
use crate::colors;

pub struct ConfirmedPassword {
    /// Wiped on drop.
    pub password: Zeroizing<String>,
    pub token: String,
}

/// Prompt for the user's password on the same line and exchange it for a
/// single-use confirmation token (5-minute TTL) over OPAQUE — see
/// `ApiClient::confirm_password`.
pub async fn acquire_confirm_token(api: &ApiClient) -> Result<String, String> {
    Ok(acquire_confirmed_password(api).await?.token)
}

/// Same step-up prompt as `acquire_confirm_token`, but retains the password for
/// flows whose legacy endpoint still needs it in the request body.
pub async fn acquire_confirmed_password(api: &ApiClient) -> Result<ConfirmedPassword, String> {
    let prompt = format!("  {} ", "confirm your password:".custom_color(colors::INK_DIM));
    let password =
        Zeroizing::new(rpassword::prompt_password(prompt).map_err(|e| format!("could not read password: {e}"))?);

    let token = api.confirm_password(&password).await.map_err(map_confirm_error)?;
    Ok(ConfirmedPassword { password, token })
}

/// Classify a step-up confirm error (OPAQUE or the legacy fallback).
///
/// `confirm_password` returns `ApiError` (task 1547 finding 1, Codex review
/// on PR #33): the server's real `SessionTooOldForConfirmation` is a
/// genuine two-field response whose `message` — "For security, please log
/// out and log back in before performing this action." — does NOT contain
/// the code, so this must match `code` exactly
/// (`session_too_old_for_confirmation`, `beebeeb-api/src/error.rs`), not a
/// substring of the display text. A wrong password is the LOCAL code
/// `opaque_password_mismatch` (or a legacy-endpoint 401 re-labelled to it); a
/// server `unauthorized` carrying the session-expired text passes through
/// untouched (task 0856 r2, Codex P2). Other `unauthorized` shapes and
/// local/transport errors without a code fall back to message matching.
fn map_confirm_error(e: ApiError) -> String {
    if e.is_code(crate::api::OPAQUE_PASSWORD_MISMATCH) {
        // Local OPAQUE verification failed: the password is wrong.
        "incorrect password".to_string()
    } else if e.is_code("unauthorized") && e.message == crate::api::SESSION_EXPIRED_MESSAGE {
        // The server 401'd start/finish: the password is only ever checked
        // locally, so this is the session (expired or revoked), never a typo.
        e.message
    } else if e.is_code("session_too_old_for_confirmation") || e.message.to_lowercase().contains("session_too_old") {
        "this account has no password (OPAQUE) and your session is older than 15 minutes — \
         run `bb login` again to refresh, then retry the destructive action"
            .to_string()
    } else if e.is_code("unauthorized")
        || e.message.contains("Unauthorized")
        || e.message.contains("401")
        || e.message.contains("incorrect")
    {
        "incorrect password".to_string()
    } else {
        e.message
    }
}

#[cfg(test)]
mod tests {
    use super::map_confirm_error;
    use crate::api::{ApiClient, ApiError};

    // We cannot test the rpassword prompt without a TTY mock; the error
    // mapping is the part worth testing.

    #[test]
    fn confirm_module_compiles() {
        // Smoke test — the file builds and our helper signature is unchanged.
        // Real behaviour is exercised through the integration suite in Task 22.
    }

    #[test]
    fn confirm_error_mapping_handles_common_auth_failures() {
        assert_eq!(
            map_confirm_error(ApiError::test_message("401 Unauthorized")),
            "incorrect password"
        );
        assert_eq!(
            map_confirm_error(ApiError::test_message("incorrect password")),
            "incorrect password"
        );
        // The real shape: `confirm_password` returns `ApiError` with
        // `code: Some("session_too_old_for_confirmation")` (task 1547
        // finding 1). Message-substring matching alone would NOT catch
        // this — see `confirm_password_session_too_old_survives_a_message_
        // that_does_not_mention_the_code` below for that exact regression.
        assert!(
            map_confirm_error(ApiError::test_code("session_too_old_for_confirmation")).contains("run `bb login` again")
        );
        assert_eq!(
            map_confirm_error(ApiError::test_message("network failed")),
            "network failed"
        );
    }

    /// Regression for Codex's PR #33 review (`src/api.rs:1892`, task 1547).
    /// The server's real `SessionTooOldForConfirmation` body
    /// (`beebeeb-api/src/error.rs`) is a genuine two-field response whose
    /// `message` does NOT repeat the `error` code:
    /// `{"error": "session_too_old_for_confirmation", "message": "For
    /// security, please log out and log back in before performing this
    /// action."}`. `parse_response` used to return only `message`, so this
    /// test's `e.contains("session_too_old")` check silently failed and the
    /// caller saw the generic server text instead of the specific "run `bb
    /// login` again" OPAQUE guidance. Exercises `ApiClient::confirm_password`
    /// end to end (not just `map_confirm_error` in isolation) so it proves
    /// the fix in `parse_response`, not just in this file.
    #[tokio::test]
    async fn confirm_password_session_too_old_survives_a_message_that_does_not_mention_the_code() {
        // The account has no OPAQUE record (409 `opaque_unavailable` on start),
        // so `confirm_password` falls back to the legacy `/auth/confirm` — the
        // only place the freshness fallback (and this 401) lives.
        let app = axum::Router::new()
            .route(
                "/api/v1/auth/confirm-opaque-start",
                axum::routing::post(|| async {
                    (
                        axum::http::StatusCode::CONFLICT,
                        axum::Json(
                            serde_json::json!({ "opaque_unavailable": true, "message": "no OPAQUE credential" }),
                        ),
                    )
                }),
            )
            .route(
                "/api/v1/auth/confirm",
                axum::routing::post(|| async {
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        axum::Json(serde_json::json!({
                            "error": "session_too_old_for_confirmation",
                            "message": "For security, please log out and log back in before performing this action.",
                        })),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let api = ApiClient::new_for_test(format!("http://{addr}"));

        let err = api
            .confirm_password("whatever")
            .await
            .expect_err("mock server returns 401");
        let mapped = map_confirm_error(err);

        assert!(
            mapped.contains("run `bb login` again"),
            "a two-field response whose message doesn't mention the code must still \
             trigger the OPAQUE re-login path, got: {mapped}"
        );
    }

    // ── OPAQUE step-up (task 0856) ───────────────────────────────────────────
    //
    // The mock below is a REAL OPAQUE server (`beebeeb_core::opaque_protocol`'s
    // public server half), not canned bytes: a wrong password fails inside
    // `client_login_finish`, exactly as against the live API. Every request
    // body the client sends is recorded so the tests can assert on ALL of them.

    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use base64::Engine as _;
    use beebeeb_core::opaque_protocol as op;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex, OnceLock};

    const EMAIL: &str = "stepup0856@beebeeb.io";
    /// Distinctive on purpose: `assert_password_absent` greps for it.
    const PASSWORD: &str = "tr0ub4dor-0856-step-up-secret";

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// Real OPAQUE account.
        Opaque,
        /// start -> 409 `opaque_unavailable` (legacy Argon2-only account).
        Legacy,
        /// start -> 409 WITHOUT `opaque_unavailable` (some other conflict).
        OtherConflict,
        /// OPAQUE account whose finish endpoint answers 401.
        FinishRejects,
        /// Expired/revoked session: confirm-opaque-start answers a bare 401.
        ExpiredAtStart,
        /// Legacy account (start -> 409 opaque_unavailable) whose plaintext
        /// `/auth/confirm` answers 401 `unauthorized` (wrong password).
        LegacyWrongPassword,
    }

    struct Mock {
        mode: Mode,
        setup: Vec<u8>,
        file: Vec<u8>,
        /// (path, raw body, X-Confirm-Token-ish: Authorization header)
        log: Mutex<Vec<(String, String, Option<String>)>>,
    }

    fn registered() -> &'static (Vec<u8>, Vec<u8>) {
        static R: OnceLock<(Vec<u8>, Vec<u8>)> = OnceLock::new();
        R.get_or_init(|| {
            let setup = op::create_server_setup();
            let c = op::client_registration_start(PASSWORD.as_bytes()).unwrap();
            let resp = op::server_registration_start(&setup, &c.message, EMAIL.as_bytes()).unwrap();
            let upload = op::client_registration_finish(&c.state, PASSWORD.as_bytes(), &resp).unwrap();
            let file = op::server_registration_finish(&upload).unwrap();
            (setup, file)
        })
    }

    fn b64() -> base64::engine::GeneralPurpose {
        base64::engine::general_purpose::STANDARD
    }

    fn record(m: &Mock, path: &str, headers: &HeaderMap, body: &str) {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        m.log.lock().unwrap().push((path.to_string(), body.to_string(), auth));
    }

    async fn h_start(State(m): State<Arc<Mock>>, headers: HeaderMap, body: String) -> Response {
        record(&m, "/api/v1/auth/confirm-opaque-start", &headers, &body);
        match m.mode {
            Mode::ExpiredAtStart => {
                (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "unauthorized" }))).into_response()
            }
            Mode::Legacy | Mode::LegacyWrongPassword => (
                StatusCode::CONFLICT,
                axum::Json(json!({ "opaque_unavailable": true, "message": "account has no OPAQUE credential; use /auth/confirm" })),
            )
                .into_response(),
            Mode::OtherConflict => (
                StatusCode::CONFLICT,
                axum::Json(json!({ "error": "conflict", "message": "something else is in the way" })),
            )
                .into_response(),
            Mode::Opaque | Mode::FinishRejects => {
                let req: Value = serde_json::from_str(&body).unwrap();
                let msg = b64().decode(req["client_message"].as_str().unwrap()).unwrap();
                let r = op::server_login_start(&m.setup, &m.file, &msg, EMAIL.as_bytes()).unwrap();
                axum::Json(json!({
                    "server_message": b64().encode(&r.message),
                    "server_state": b64().encode(&r.state),
                    "ksf_version": 1,
                }))
                .into_response()
            }
        }
    }

    async fn h_finish(State(m): State<Arc<Mock>>, headers: HeaderMap, body: String) -> Response {
        record(&m, "/api/v1/auth/confirm-opaque-finish", &headers, &body);
        if m.mode == Mode::FinishRejects {
            return (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "unauthorized" }))).into_response();
        }
        let req: Value = serde_json::from_str(&body).unwrap();
        let msg = b64().decode(req["client_message"].as_str().unwrap()).unwrap();
        let state = b64().decode(req["server_state"].as_str().unwrap()).unwrap();
        match op::server_login_finish(&state, &msg) {
            Ok(_) => axum::Json(json!({ "confirmation_token": "opaque-token", "expires_at": "2099-01-01T00:00:00Z" }))
                .into_response(),
            Err(_) => (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "unauthorized" }))).into_response(),
        }
    }

    async fn h_legacy(State(m): State<Arc<Mock>>, headers: HeaderMap, body: String) -> Response {
        record(&m, "/api/v1/auth/confirm", &headers, &body);
        if m.mode == Mode::LegacyWrongPassword {
            return (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "unauthorized" }))).into_response();
        }
        axum::Json(json!({ "confirmation_token": "legacy-token", "expires_at": "2099-01-01T00:00:00Z" }))
            .into_response()
    }

    async fn spawn(mode: Mode) -> (ApiClient, Arc<Mock>) {
        let (setup, file) = registered().clone();
        let m = Arc::new(Mock {
            mode,
            setup,
            file,
            log: Mutex::new(Vec::new()),
        });
        let app = axum::Router::new()
            .route("/api/v1/auth/confirm-opaque-start", axum::routing::post(h_start))
            .route("/api/v1/auth/confirm-opaque-finish", axum::routing::post(h_finish))
            .route("/api/v1/auth/confirm", axum::routing::post(h_legacy))
            .with_state(m.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (ApiClient::new_for_test(format!("http://{addr}")), m)
    }

    fn paths(m: &Mock) -> Vec<String> {
        m.log.lock().unwrap().iter().map(|(p, _, _)| p.clone()).collect()
    }

    /// Assert on EVERY captured request (path + body), not just the OPAQUE ones.
    fn assert_password_absent(m: &Mock, password: &str) {
        let log = m.log.lock().unwrap();
        assert!(
            !log.is_empty(),
            "no request was captured — the assertion would be vacuous"
        );
        for (path, body, _) in log.iter() {
            assert!(
                !body.contains(password),
                "{path} body carries the plaintext password: {body}"
            );
            assert!(!path.contains(password), "{path} carries the plaintext password");
        }
    }

    #[tokio::test]
    async fn step_up_uses_opaque_and_never_sends_the_password() {
        let (api, m) = spawn(Mode::Opaque).await;

        let token = api.confirm_password(PASSWORD).await.expect("opaque step-up");

        assert_eq!(token, "opaque-token");
        assert_eq!(
            paths(&m),
            [
                "/api/v1/auth/confirm-opaque-start",
                "/api/v1/auth/confirm-opaque-finish"
            ],
            "exactly start then finish — the legacy /auth/confirm must not be touched"
        );
        assert_password_absent(&m, PASSWORD);
        // Both calls carry the session bearer (they are authenticated endpoints).
        for (path, _, auth) in m.log.lock().unwrap().iter() {
            assert_eq!(auth.as_deref(), Some("Bearer test-token"), "{path}");
        }
        // The finish message round-trips the server state verbatim.
        let log = m.log.lock().unwrap();
        let fin: Value = serde_json::from_str(&log[1].1).unwrap();
        assert!(fin["client_message"].is_string() && fin["server_state"].is_string());
    }

    #[tokio::test]
    async fn step_up_falls_back_to_plaintext_only_on_409_opaque_unavailable() {
        let (api, m) = spawn(Mode::Legacy).await;

        let token = api.confirm_password(PASSWORD).await.expect("legacy fallback");

        assert_eq!(token, "legacy-token");
        assert_eq!(
            paths(&m),
            ["/api/v1/auth/confirm-opaque-start", "/api/v1/auth/confirm"],
            "start (409) then the legacy confirm"
        );
        let log = m.log.lock().unwrap();
        assert!(
            !log[0].1.contains(PASSWORD),
            "the OPAQUE start must not carry the password even when it is about to 409"
        );
        let legacy: Value = serde_json::from_str(&log[1].1).unwrap();
        assert_eq!(legacy["password"], PASSWORD, "the documented legacy exception");
    }

    #[tokio::test]
    async fn other_409_is_surfaced_not_treated_as_the_legacy_fallback() {
        let (api, m) = spawn(Mode::OtherConflict).await;

        let err = api
            .confirm_password(PASSWORD)
            .await
            .expect_err("a plain 409 is an error");

        assert_eq!(err.status, 409);
        assert!(err.message.contains("something else is in the way"), "{}", err.message);
        assert_eq!(paths(&m), ["/api/v1/auth/confirm-opaque-start"]);
        assert_password_absent(&m, PASSWORD);
    }

    #[tokio::test]
    async fn wrong_password_is_rejected_client_side_as_incorrect_password() {
        let (api, m) = spawn(Mode::Opaque).await;

        let err = api
            .confirm_password("not the password")
            .await
            .expect_err("wrong password must fail");

        assert_eq!(map_confirm_error(err), "incorrect password");
        assert_eq!(
            paths(&m),
            ["/api/v1/auth/confirm-opaque-start"],
            "OPAQUE finish fails locally on a wrong password: no finish POST, and NO plaintext fallback"
        );
        assert_password_absent(&m, "not the password");
    }

    #[tokio::test]
    async fn finish_401_maps_to_session_expired_not_incorrect_password() {
        // The client already verified the server MAC locally, so the password
        // is right by the time finish is sent: a 401 here is the session.
        let (api, m) = spawn(Mode::FinishRejects).await;

        let err = api.confirm_password(PASSWORD).await.expect_err("finish answers 401");

        assert_eq!(map_confirm_error(err), crate::api::SESSION_EXPIRED_MESSAGE);
        assert_eq!(
            paths(&m),
            [
                "/api/v1/auth/confirm-opaque-start",
                "/api/v1/auth/confirm-opaque-finish"
            ]
        );
        assert_password_absent(&m, PASSWORD);
    }

    #[tokio::test]
    async fn expired_session_at_start_maps_to_session_expired_not_incorrect_password() {
        let (api, m) = spawn(Mode::ExpiredAtStart).await;

        let err = api.confirm_password(PASSWORD).await.expect_err("start answers 401");

        assert_eq!(map_confirm_error(err), crate::api::SESSION_EXPIRED_MESSAGE);
        assert_eq!(paths(&m), ["/api/v1/auth/confirm-opaque-start"]);
        assert_password_absent(&m, PASSWORD);
    }

    #[tokio::test]
    async fn legacy_401_after_a_live_start_is_a_wrong_password() {
        // `confirm-opaque-start` just accepted the bearer (409, not 401), so
        // the session is alive and the legacy 401 can only be the password.
        let (api, m) = spawn(Mode::LegacyWrongPassword).await;

        let err = api.confirm_password("not the password").await.expect_err("legacy 401");

        assert_eq!(map_confirm_error(err), "incorrect password");
        assert_eq!(paths(&m), ["/api/v1/auth/confirm-opaque-start", "/api/v1/auth/confirm"]);
    }

    #[test]
    fn server_session_expired_error_is_not_rewritten_but_local_mismatch_is_incorrect_password() {
        let expired = ApiError {
            code: Some("unauthorized".to_string()),
            message: crate::api::SESSION_EXPIRED_MESSAGE.to_string(),
            status: 401,
        };
        assert_eq!(map_confirm_error(expired), crate::api::SESSION_EXPIRED_MESSAGE);
        let mismatch = ApiError {
            code: Some("opaque_password_mismatch".to_string()),
            message: "incorrect password".to_string(),
            status: 0,
        };
        assert_eq!(map_confirm_error(mismatch), "incorrect password");
    }
}
