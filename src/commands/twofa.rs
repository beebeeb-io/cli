//! `bb 2fa` — TOTP management.
//!
//! Routes used today:
//!   GET  /api/v1/auth/me         — `totp_enabled` (TOTP-specific bool); used by `status`
//!   POST /api/v1/auth/2fa/setup   — not wired here yet (plan Task 10)
//!   POST /api/v1/auth/2fa/enable  — not wired here yet (plan Task 10)
//!   POST /api/v1/auth/2fa/disable — not wired here yet (plan Task 10)
//!   POST /api/v1/auth/2fa/verify  — login-only, exercised inside `bb login`'s
//!                                    handshake (`login.rs`), not this module
//!
//! `status` was originally speced (plan Task 9 / task 0477) against
//! `GET /api/v1/auth/account/2fa/status`, which does not exist server-side —
//! `repos/server/beebeeb-api/src/routes/totp.rs` registers only
//! setup/enable/disable/verify, all POST, nested at `/api/v1/auth/2fa`
//! (`router.rs`). The plan's own Step 2 code sample already abandoned that
//! route in favor of `GET /api/v1/account/security-score`'s
//! `two_factor_enabled` factor — but that factor is `has_totp || has_passkey`
//! (`account_activity.rs`), so it is not actually TOTP-specific and would
//! misreport "enabled" for a passkey-only account on a command named `2fa`.
//!
//! `status` instead reads `totp_enabled` off `GET /api/v1/auth/me`
//! (`routes/auth.rs`'s `me` handler), which is a dedicated
//! `COALESCE(t.enabled, false)` read straight off `totp_secrets` — TOTP-only,
//! no passkey conflation — and already wired via `ApiClient::get_me()`
//! (`api.rs`, already used by `account.rs`/`whoami.rs`). No new API surface.
//!
//! Neither `/auth/me` nor `/account/security-score` nor any other live route
//! exposes backup-codes-remaining or a last-verified timestamp for TOTP (see
//! task 0477 Notes) — `status` renders enabled/disabled only until a
//! dedicated server endpoint exists.

use serde_json::Value;

use crate::api::ApiClient;

/// Parsed `bb 2fa status` view of `GET /api/v1/auth/me`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwofaStatus {
    pub totp_enabled: bool,
}

impl TwofaStatus {
    /// Parse from the raw `/api/v1/auth/me` JSON body. A missing or
    /// non-boolean `totp_enabled` field defaults to `false` — fail closed,
    /// never claim 2FA is on when the server didn't say so.
    pub fn from_me_response(me: &Value) -> Self {
        Self {
            totp_enabled: me.get("totp_enabled").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    pub fn to_json(self) -> Value {
        serde_json::json!({
            "enabled": self.totp_enabled,
            "method": if self.totp_enabled { Some("totp") } else { None },
        })
    }
}

/// Render the human `bb 2fa status` lines.
fn render_status(status: TwofaStatus) -> Vec<String> {
    use crate::colors;
    use colored::Colorize;

    if status.totp_enabled {
        vec![format!("  2fa  {}", "enabled (totp)".custom_color(colors::GREEN_OK))]
    } else {
        vec![
            format!("  2fa  {}", "disabled".custom_color(colors::INK_DIM)),
            format!("  enable with: {}", "bb 2fa setup".custom_color(colors::INK_DIM)),
        ]
    }
}

pub async fn status() -> Result<(), String> {
    use crate::ui;

    let api = ApiClient::from_config();
    api.require_auth()?;

    let me = api.get_me().await?;
    let status = TwofaStatus::from_me_response(&me);

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&status.to_json()).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    for line in render_status(status) {
        println!("{line}");
    }
    Ok(())
}

pub async fn setup() -> Result<(), String> {
    Err("bb 2fa setup — not implemented yet".to_string())
}

pub async fn enable(_code: String) -> Result<(), String> {
    Err("bb 2fa enable — not implemented yet".to_string())
}

pub async fn disable(_code: String) -> Result<(), String> {
    Err("bb 2fa disable — not implemented yet".to_string())
}

pub async fn verify(_partial_token: String, _code: String) -> Result<(), String> {
    Err("bb 2fa verify — not implemented yet".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_me_response_parses_enabled() {
        let me = json!({ "email": "a@b.com", "totp_enabled": true });
        assert_eq!(TwofaStatus::from_me_response(&me), TwofaStatus { totp_enabled: true });
    }

    #[test]
    fn from_me_response_parses_disabled() {
        let me = json!({ "email": "a@b.com", "totp_enabled": false });
        assert_eq!(TwofaStatus::from_me_response(&me), TwofaStatus { totp_enabled: false });
    }

    #[test]
    fn from_me_response_defaults_to_disabled_when_field_missing() {
        // A response shape that lacks the field entirely (e.g. an older
        // server) must fail closed, not panic or default to enabled.
        let me = json!({ "email": "a@b.com" });
        assert_eq!(TwofaStatus::from_me_response(&me), TwofaStatus { totp_enabled: false });
    }

    #[test]
    fn from_me_response_defaults_to_disabled_on_wrong_type() {
        let me = json!({ "email": "a@b.com", "totp_enabled": "yes" });
        assert_eq!(TwofaStatus::from_me_response(&me), TwofaStatus { totp_enabled: false });
    }

    #[test]
    fn to_json_carries_method_only_when_enabled() {
        assert_eq!(
            TwofaStatus { totp_enabled: true }.to_json(),
            json!({ "enabled": true, "method": "totp" })
        );
        assert_eq!(
            TwofaStatus { totp_enabled: false }.to_json(),
            json!({ "enabled": false, "method": null })
        );
    }

    #[test]
    fn render_status_enabled_shows_totp_on_one_line() {
        let lines = render_status(TwofaStatus { totp_enabled: true });
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("enabled (totp)"), "line was: {:?}", lines[0]);
    }

    #[test]
    fn render_status_disabled_shows_hint_line() {
        let lines = render_status(TwofaStatus { totp_enabled: false });
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("disabled"), "line was: {:?}", lines[0]);
        assert!(lines[1].contains("bb 2fa setup"), "line was: {:?}", lines[1]);
    }
}
