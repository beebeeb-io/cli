//! `bb 2fa` — TOTP management.
//!
//! Routes used today:
//!   GET  /api/v1/auth/me         — `totp_enabled` (TOTP-specific bool); used by `status`
//!   POST /api/v1/auth/2fa/setup   — `{secret, qr_uri, backup_codes}`; used by `setup` (task 0478)
//!   POST /api/v1/auth/2fa/enable  — not wired here yet (plan Task 11 / task 0479)
//!   POST /api/v1/auth/2fa/disable — not wired here yet (plan Task 11 / task 0479)
//!   POST /api/v1/auth/2fa/verify  — login-only, exercised inside `bb login`'s
//!                                    handshake (`login.rs`), not this module
//!
//! `setup` was speced (plan Task 10) against `POST /api/v1/auth/account/2fa/setup`,
//! which does not exist server-side either — same deviation as `status` below,
//! same live route family (`/api/v1/auth/2fa/*`, `router.rs`). `setup` also
//! checks `bb 2fa status` FIRST and refuses locally if already enabled: the
//! live route's step-up gate for an already-enabled account (`ApiError::ConfirmationRequired`
//! — a `code` in the body or a confirmed-password header) is out of scope for
//! this task, so surfacing that as a confusing mid-flow error would be worse
//! than a clear local refusal.
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

/// Parsed `bb 2fa setup` view of `POST /api/v1/auth/2fa/setup`'s response
/// (`repos/server/beebeeb-api/src/routes/totp.rs`'s `setup` handler):
/// `{"secret": "...", "qr_uri": "otpauth://...", "backup_codes": ["..."]}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwofaSetup {
    pub secret: String,
    pub qr_uri: String,
    pub backup_codes: Vec<String>,
}

impl TwofaSetup {
    /// Parse from the raw `POST /api/v1/auth/2fa/setup` JSON body. Missing
    /// or wrong-typed fields default to empty — fail closed, never fabricate
    /// a secret, QR URI, or backup code the server didn't actually send.
    pub fn from_setup_response(resp: &Value) -> Self {
        Self {
            secret: resp.get("secret").and_then(Value::as_str).unwrap_or("").to_string(),
            qr_uri: resp.get("qr_uri").and_then(Value::as_str).unwrap_or("").to_string(),
            backup_codes: resp
                .get("backup_codes")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        }
    }

    /// Reserialize to the exact `--json` contract: `{secret, qr_uri,
    /// backup_codes}` — independent of whatever extra fields the server
    /// response happens to carry.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "secret": self.secret,
            "qr_uri": self.qr_uri,
            "backup_codes": self.backup_codes,
        })
    }
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

/// Render the `--quiet` line: a single, uncolored, greppable word — no hint
/// line, matching the `quota`/`whoami` quiet-mode convention.
fn render_status_quiet(status: TwofaStatus) -> &'static str {
    if status.totp_enabled { "enabled" } else { "disabled" }
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

    if ui::is_quiet() {
        println!("{}", render_status_quiet(status));
        return Ok(());
    }

    for line in render_status(status) {
        println!("{line}");
    }
    Ok(())
}

/// Render the human `bb 2fa setup` lines: the ASCII QR block, the secret,
/// the backup codes (two columns), and the `bb 2fa enable` hint.
fn render_setup(setup: &TwofaSetup) -> Vec<String> {
    use crate::colors;
    use crate::commands::qr;
    use colored::Colorize;

    let bar = "\u{2501}".repeat(58);
    let mut lines = Vec::new();

    lines.push(format!(
        "  {}",
        "ENABLE TWO-FACTOR AUTHENTICATION".custom_color(colors::AMBER)
    ));
    lines.push(format!("  {}", bar.custom_color(colors::INK_DIM)));
    lines.push(String::new());
    lines.push("  scan this QR code with your authenticator app".to_string());
    lines.push("  (1password, authy, google authenticator, raivo, ...):".to_string());
    lines.push(String::new());

    for line in qr::render_otpauth(&setup.qr_uri).lines() {
        lines.push(format!("    {line}"));
    }

    lines.push(String::new());
    lines.push("  or paste the secret manually:".to_string());
    lines.push(format!("    {}", setup.secret.custom_color(colors::INK)));
    lines.push(String::new());
    lines.push(format!("  {}", "BACKUP CODES".custom_color(colors::AMBER)));
    lines.push(format!("  {}", bar.custom_color(colors::INK_DIM)));
    lines.push(String::new());
    // Honest, not reassuring: these codes are the only recovery path if the
    // authenticator is lost — say so plainly, no "bank-grade" hand-waving.
    lines.push("  store these now — each works once. lose your authenticator AND".to_string());
    lines.push("  these codes, and we cannot get you back into your account.".to_string());
    lines.push(String::new());

    let mut i = 0;
    while i < setup.backup_codes.len() {
        let left = setup.backup_codes.get(i).cloned().unwrap_or_default();
        let right = setup.backup_codes.get(i + 1).cloned().unwrap_or_default();
        if right.is_empty() {
            lines.push(format!("    {}", left.custom_color(colors::INK)));
        } else {
            lines.push(format!(
                "    {}    {}",
                left.custom_color(colors::INK),
                right.custom_color(colors::INK)
            ));
        }
        i += 2;
    }

    lines.push(String::new());
    lines.push(format!("  {}", "NEXT".custom_color(colors::AMBER)));
    lines.push(format!("  {}", bar.custom_color(colors::INK_DIM)));
    lines.push(String::new());
    lines.push("  enter a code from your authenticator to confirm:".to_string());
    lines.push(format!(
        "    {}",
        "bb 2fa enable --code 123456".custom_color(colors::INK_DIM)
    ));

    lines
}

pub async fn setup() -> Result<(), String> {
    use crate::ui;

    let api = ApiClient::from_config();
    api.require_auth()?;

    // Check status FIRST, before calling the live setup route. An
    // already-enabled account hitting POST /api/v1/auth/2fa/setup triggers
    // the server's step-up gate (`ConfirmationRequired` — a code or a
    // confirmed-password header) that this task does not implement (that's
    // the `bb 2fa disable`/step-up work, out of scope for Task 10). Refusing
    // locally with a clear message is more honest than surfacing a
    // confusing mid-flow 403 from a route this command can't satisfy.
    let me = api.get_me().await?;
    if TwofaStatus::from_me_response(&me).totp_enabled {
        return Err(
            "2FA is already enabled on this account. Run `bb 2fa disable --code <code>` first if you need to reset it."
                .to_string(),
        );
    }

    let resp = api.totp_setup().await?;
    let setup = TwofaSetup::from_setup_response(&resp);

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&setup.to_json()).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    for line in render_setup(&setup) {
        println!("{line}");
    }
    Ok(())
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

    #[test]
    fn render_status_quiet_is_a_single_bare_word() {
        assert_eq!(render_status_quiet(TwofaStatus { totp_enabled: true }), "enabled");
        assert_eq!(render_status_quiet(TwofaStatus { totp_enabled: false }), "disabled");
    }

    // ── eng-0478: `bb 2fa setup` ────────────────────────────────────────

    /// A fixture matching the live server's response shape
    /// (`repos/server/beebeeb-api/src/routes/totp.rs`'s `setup` handler).
    fn setup_fixture() -> Value {
        json!({
            "secret": "KJSXG43LEBQXG4RAINVHK4TPNZ2HG2LDOJSWG===",
            "qr_uri": "otpauth://totp/Beebeeb:guus@devidee.nl?secret=KJSXG43LEBQXG4RAINVHK4TPNZ2HG2LDOJSWG&issuer=Beebeeb&algorithm=SHA1&digits=6&period=30",
            "backup_codes": ["81720394", "24910837", "56213409", "90284715", "33019826", "77465021", "12938405", "60582147"],
        })
    }

    #[test]
    fn from_setup_response_parses_the_live_server_shape() {
        let parsed = TwofaSetup::from_setup_response(&setup_fixture());
        assert_eq!(
            parsed,
            TwofaSetup {
                secret: "KJSXG43LEBQXG4RAINVHK4TPNZ2HG2LDOJSWG===".to_string(),
                qr_uri: "otpauth://totp/Beebeeb:guus@devidee.nl?secret=KJSXG43LEBQXG4RAINVHK4TPNZ2HG2LDOJSWG&issuer=Beebeeb&algorithm=SHA1&digits=6&period=30".to_string(),
                backup_codes: vec![
                    "81720394".to_string(),
                    "24910837".to_string(),
                    "56213409".to_string(),
                    "90284715".to_string(),
                    "33019826".to_string(),
                    "77465021".to_string(),
                    "12938405".to_string(),
                    "60582147".to_string(),
                ],
            }
        );
    }

    #[test]
    fn to_json_round_trips_exactly_secret_qr_uri_backup_codes() {
        let parsed = TwofaSetup::from_setup_response(&setup_fixture());
        assert_eq!(
            parsed.to_json(),
            json!({
                "secret": "KJSXG43LEBQXG4RAINVHK4TPNZ2HG2LDOJSWG===",
                "qr_uri": "otpauth://totp/Beebeeb:guus@devidee.nl?secret=KJSXG43LEBQXG4RAINVHK4TPNZ2HG2LDOJSWG&issuer=Beebeeb&algorithm=SHA1&digits=6&period=30",
                "backup_codes": ["81720394", "24910837", "56213409", "90284715", "33019826", "77465021", "12938405", "60582147"],
            })
        );
    }

    #[test]
    fn render_setup_contains_secret_qr_block_all_codes_and_enable_hint() {
        let parsed = TwofaSetup::from_setup_response(&setup_fixture());
        let lines = render_setup(&parsed);
        let joined = lines.join("\n");

        assert!(joined.contains(&parsed.secret), "missing secret:\n{joined}");
        // The ASCII QR renders unicode block characters — none of which
        // appear anywhere else in the output, so their presence is a
        // reliable proxy for "a QR block was rendered".
        assert!(
            joined
                .chars()
                .any(|c| c == '█' || c == '▄' || c == '▀' || c == '▐' || c == '▌'),
            "missing QR block (no unicode block characters found):\n{joined}"
        );
        for code in &parsed.backup_codes {
            assert!(joined.contains(code), "missing backup code {code}:\n{joined}");
        }
        assert_eq!(
            parsed.backup_codes.len(),
            8,
            "fixture should carry all 8 backup codes the server generates"
        );
        assert!(
            joined.contains("bb 2fa enable"),
            "missing the `bb 2fa enable` next-step hint:\n{joined}"
        );
    }
}
