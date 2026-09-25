//! Step-up password confirmation — used by every destructive account action.
//!
//! The server's `ConfirmedAction` extractor requires an `X-Confirm-Token`
//! header on protected routes. The token is minted by
//! `POST /api/v1/auth/confirm` with the user's password and is single-use
//! with a 5-minute TTL. We never cache; each call to `acquire_confirm_token`
//! prompts the user fresh and immediately uses the token.
//!
//! For OPAQUE-only accounts (no `password_hash`), the server uses session
//! freshness (last 15 minutes) instead of a password. The CLI still prompts
//! "confirm your password" — the server accepts an empty string and falls
//! through to the freshness check. This is OK from a UX standpoint because
//! OPAQUE accounts are still rare in CLI-using territory; document it in the
//! error path.

use colored::Colorize;

use crate::api::{ApiClient, ApiError};
use crate::colors;

pub struct ConfirmedPassword {
    pub password: String,
    pub token: String,
}

/// Prompt for the user's password on the same line and exchange it for a
/// single-use confirmation token (5-minute TTL, returned by
/// `POST /api/v1/auth/confirm`).
pub async fn acquire_confirm_token(api: &ApiClient) -> Result<String, String> {
    Ok(acquire_confirmed_password(api).await?.token)
}

/// Same step-up prompt as `acquire_confirm_token`, but retains the password for
/// flows whose legacy endpoint still needs it in the request body.
pub async fn acquire_confirmed_password(api: &ApiClient) -> Result<ConfirmedPassword, String> {
    let prompt = format!("  {} ", "confirm your password:".custom_color(colors::INK_DIM));
    let password = rpassword::prompt_password(prompt).map_err(|e| format!("could not read password: {e}"))?;

    let token = api.confirm_password(&password).await.map_err(map_confirm_error)?;
    Ok(ConfirmedPassword { password, token })
}

/// Classify a `POST /api/v1/auth/confirm` error.
///
/// `confirm_password` returns `ApiError` (task 1547 finding 1, Codex review
/// on PR #33): the server's real `SessionTooOldForConfirmation` is a
/// genuine two-field response whose `message` — "For security, please log
/// out and log back in before performing this action." — does NOT contain
/// the code, so this must match `code` exactly
/// (`session_too_old_for_confirmation`, `beebeeb-api/src/error.rs`), not a
/// substring of the display text. `Unauthorized` (wrong password) IS a
/// stable short code too (`{"error": "unauthorized"}`) — matched by
/// `is_code` first, falling back to message-substring matching for
/// local/transport errors that never carry a code.
fn map_confirm_error(e: ApiError) -> String {
    if e.is_code("session_too_old_for_confirmation") || e.message.to_lowercase().contains("session_too_old") {
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
        let app = axum::Router::new().route(
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
}
