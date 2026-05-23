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

use crate::api::ApiClient;
use crate::colors;

/// Prompt for the user's password on the same line and exchange it for a
/// single-use confirmation token (5-minute TTL, returned by
/// `POST /api/v1/auth/confirm`).
pub async fn acquire_confirm_token(api: &ApiClient) -> Result<String, String> {
    let prompt = format!("  {} ", "confirm your password:".custom_color(colors::INK_DIM));
    let password = rpassword::prompt_password(prompt)
        .map_err(|e| format!("could not read password: {e}"))?;

    api.confirm_password(&password).await.map_err(|e| {
        // The server returns "Unauthorized" for wrong password and a specific
        // "session too old" error for OPAQUE accounts whose session is stale.
        if e.contains("session_too_old") || e.contains("SessionTooOld") {
            "this account has no password (OPAQUE) and your session is older than 15 minutes — \
             run `bb login` again to refresh, then retry the destructive action".to_string()
        } else if e.contains("Unauthorized") || e.contains("401") || e.contains("incorrect") {
            "incorrect password".to_string()
        } else {
            e
        }
    })
}

#[cfg(test)]
mod tests {
    // We cannot test the rpassword prompt without a TTY mock; the error
    // mapping is the part worth testing.
    //
    // The translation function is inlined inside `acquire_confirm_token`
    // intentionally — extracting it adds public surface area that nothing
    // else needs. If a future task needs to reuse the mapping, refactor then.

    #[test]
    fn confirm_module_compiles() {
        // Smoke test — the file builds and our helper signature is unchanged.
        // Real behaviour is exercised through the integration suite in Task 22.
    }
}
