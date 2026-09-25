//! `bb 2fa` — TOTP management.
//!
//! Routes used today:
//!   GET  /api/v1/auth/me         — `totp_enabled` (TOTP-specific bool); used by `status`
//!   POST /api/v1/auth/2fa/setup   — `{secret, qr_uri, backup_codes}`; used by `setup` (task 0478)
//!   POST /api/v1/auth/2fa/enable  — `{code}` → `{message}`; used by `enable` (task 0479)
//!   POST /api/v1/auth/2fa/disable — `{code}` → `{message}`; used by `disable` (task 0479)
//!
//! **`verify` was REMOVED from the clap tree (eng-0479).** The plan speced it
//! as "re-verify to refresh a last-verified timestamp" — no such endpoint
//! exists. The live `POST /api/v1/auth/2fa/verify` (`routes/totp.rs` ~L253) is
//! the LOGIN-time `{partial_token, code}` → session exchange: it looks up the
//! partial token in `sessions`, checks the TOTP/backup code, and mints a full
//! session + `Set-Cookie`. `bb login` (`login.rs`) is a browser-based device
//! handshake (`beebeeb_core::cli_auth`) that never sees a partial token or
//! calls anything 2FA/TOTP-shaped — grepped for `2fa`/`totp`/`partial`: zero
//! matches. No other command constructs or receives a `partial_token` either.
//! A visible command with no live semantics is a dead end for whoever runs
//! `bb 2fa verify --help` — same reasoning as Guus's 2026-06-24 stub removal
//! (cli commit 84e70e1, `bb account export`/`delete`). If a future CLI flow
//! ever needs the login-time 2FA exchange (e.g. a headless `bb login` variant
//! that surfaces the partial-token step instead of hiding it in the browser
//! handshake), re-add `verify` wired to that flow specifically — not as a
//! bare pass-through of `/2fa/verify`.
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

use crate::api::{ApiClient, ApiError};

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
    /// Parse from the raw `POST /api/v1/auth/2fa/setup` JSON body.
    ///
    /// Unlike `TwofaStatus::from_me_response` (where a missing/wrong-typed
    /// boolean safely defaults to "disabled"), there is no safe default for
    /// a missing secret, QR URI, or backup code: a 2xx response with schema
    /// skew or a server regression must NOT turn into a "successful" setup
    /// with an empty/incomplete credential the user can't actually use to
    /// enable 2FA. Every field is REQUIRED and non-empty, and every backup
    /// code must be a non-empty string — anything else is a hard `Err`.
    pub fn from_setup_response(resp: &Value) -> Result<Self, String> {
        let secret = resp
            .get("secret")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "2fa setup response is missing a secret".to_string())?
            .to_string();

        let qr_uri = resp
            .get("qr_uri")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "2fa setup response is missing a qr_uri".to_string())?
            .to_string();

        let raw_codes = resp
            .get("backup_codes")
            .and_then(Value::as_array)
            .ok_or_else(|| "2fa setup response is missing backup_codes".to_string())?;
        if raw_codes.is_empty() {
            return Err("2fa setup response carried zero backup codes".to_string());
        }
        let mut backup_codes = Vec::with_capacity(raw_codes.len());
        for (i, v) in raw_codes.iter().enumerate() {
            match v.as_str().filter(|s| !s.is_empty()) {
                Some(s) => backup_codes.push(s.to_string()),
                None => return Err(format!("2fa setup response's backup_codes[{i}] is missing or empty")),
            }
        }

        Ok(Self {
            secret,
            qr_uri,
            backup_codes,
        })
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

/// Render the `--quiet` lines for `bb 2fa setup`: bare, uncolored, one value
/// per line — the secret, then each backup code — matching the
/// `quota`/`share` quiet-mode convention (greppable values, no decoration,
/// no hint line). No ASCII QR (a decorative block, not a bare value).
fn render_setup_quiet(setup: &TwofaSetup) -> Vec<String> {
    let mut lines = Vec::with_capacity(1 + setup.backup_codes.len());
    lines.push(setup.secret.clone());
    lines.extend(setup.backup_codes.iter().cloned());
    lines
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
    let setup = TwofaSetup::from_setup_response(&resp)?;

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&setup.to_json()).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    if ui::is_quiet() {
        for line in render_setup_quiet(&setup) {
            println!("{line}");
        }
        return Ok(());
    }

    for line in render_setup(&setup) {
        println!("{line}");
    }
    Ok(())
}

/// A 6-digit TOTP/authenticator code, or `--code must be 6 digits`.
fn validate_code(code: &str) -> Result<(), String> {
    if !code.chars().all(|c| c.is_ascii_digit()) || code.len() != 6 {
        return Err("--code must be 6 digits".to_string());
    }
    Ok(())
}

/// Outcome of classifying a raw `2fa/enable`/`2fa/disable` error string —
/// pure and testable without a live HTTP call, mirroring
/// `commands::confirm::map_confirm_error` / `commands::account::map_email_change_error`
/// EXCEPT for `AmbiguousUnauthorized`, which the caller must resolve with a
/// live follow-up call (see `diagnose_unauthorized`).
///
/// eng-0479: the raw string classified here is `parse_response`'s (`api.rs`)
/// extracted `error` field — e.g. `"2FA is already enabled"` or bare
/// `"unauthorized"`, NEVER a `"{status}: {body}"`-prefixed string. The
/// server's blanket `IntoResponse for ApiError`
/// (`repos/server/beebeeb-api/src/error.rs:591`) wraps every case in
/// `{"error": message}` JSON, which `parse_response` unwraps straight to
/// that bare `message` before this function ever sees it — confirmed live:
/// `curl … /2fa/enable` with a wrong code returned exactly
/// `{"error":"unauthorized"}` (401), matching
/// `ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized")`
/// verbatim, lowercase, no prefix. Matched case-insensitively as a defensive
/// belt (a non-JSON body — e.g. from a proxy in front of a misconfigured
/// `--api` — would fall through `parse_response`'s `format!("{status}: ..")`
/// path instead, which DOES carry "Unauthorized"/"401").
#[derive(Debug, Clone, PartialEq, Eq)]
enum TotpErrorOutcome {
    /// A known, final user-facing message.
    Message(String),
    /// The raw string was the bare `"unauthorized"`/401 shape. Codex review
    /// (PR #19, `PRRT_kwDOSLX6I86k5ddl`) caught that this SAME body comes
    /// from two different causes on the live server: a wrong TOTP code
    /// (`verify_totp_code`, `routes/totp.rs`) OR a dead/expired session —
    /// this route's own `AuthUser` extractor runs FIRST and rejects an
    /// invalid session with the byte-identical `{"error":"unauthorized"}`
    /// (`repos/server/beebeeb-api/src/auth.rs` ~L105:
    /// `row_opt.ok_or(ApiError::Unauthorized)?`) before the TOTP check ever
    /// runs. Blaming the code unconditionally would mislead a user whose
    /// real problem is a dead session. Caller resolves via
    /// `diagnose_unauthorized`.
    AmbiguousUnauthorized,
}

/// Classify a `POST /api/v1/auth/2fa/enable` error.
///
/// `totp_enable` returns `ApiError` (task 1547 finding 1, Codex review on PR
/// #33), but `beebeeb-api/src/routes/totp.rs`'s `ApiError::BadRequest(msg)`
/// (the "has not been set up"/"already enabled" cases) has no distinct
/// stable code of its own — the wire body is `{"error": <the dynamic
/// message itself>}`, so `code` and `message` are identical text, not a
/// short slug. Those two checks stay message-substring matches for exactly
/// that reason. `Unauthorized` genuinely IS a stable short code
/// (`{"error": "unauthorized"}`) — matched by `is_code` first, falling back
/// to the message text for local/transport errors that never carry a code.
fn classify_enable_error(e: ApiError) -> TotpErrorOutcome {
    if e.message.contains("has not been set up") {
        TotpErrorOutcome::Message("run `bb 2fa setup` first".to_string())
    } else if e.message.contains("already enabled") {
        TotpErrorOutcome::Message("2FA is already enabled".to_string())
    } else if e.is_code("unauthorized")
        || e.message.to_lowercase().contains("unauthorized")
        || e.message.contains("401")
    {
        TotpErrorOutcome::AmbiguousUnauthorized
    } else {
        TotpErrorOutcome::Message(e.message)
    }
}

/// Classify a `POST /api/v1/auth/2fa/disable` error. Same pure/testable
/// shape as `classify_enable_error` — see its doc comment for the confirmed
/// live wire format and the ambiguous-unauthorized rationale.
fn classify_disable_error(e: ApiError) -> TotpErrorOutcome {
    if e.message.contains("not currently enabled") || e.message.contains("not set up") {
        TotpErrorOutcome::Message("2FA is not currently enabled".to_string())
    } else if e.is_code("unauthorized")
        || e.message.to_lowercase().contains("unauthorized")
        || e.message.contains("401")
    {
        TotpErrorOutcome::AmbiguousUnauthorized
    } else {
        TotpErrorOutcome::Message(e.message)
    }
}

/// Resolve `TotpErrorOutcome::AmbiguousUnauthorized` with a live follow-up
/// call. `GET /api/v1/auth/me` runs through the exact same `AuthUser`
/// extractor as `/2fa/enable`/`/2fa/disable` — if IT also fails, the session
/// itself is the problem (dead/expired/revoked), not the 6-digit code; if it
/// succeeds, the session is alive and the code really was wrong. One extra
/// request, but only on the already-slow error path — never on success.
async fn diagnose_unauthorized(api: &ApiClient) -> String {
    match api.get_me().await {
        Ok(_) => "incorrect code — try again".to_string(),
        Err(_) => "your session has expired — run `bb login` again".to_string(),
    }
}

/// Resolve a `TotpErrorOutcome` to the final user-facing message, calling
/// `diagnose_unauthorized` only for the ambiguous case.
async fn resolve_totp_error(api: &ApiClient, outcome: TotpErrorOutcome) -> String {
    match outcome {
        TotpErrorOutcome::Message(msg) => msg,
        TotpErrorOutcome::AmbiguousUnauthorized => diagnose_unauthorized(api).await,
    }
}

/// The `bb 2fa enable` success line (non-json, non-quiet mode).
fn render_enable_success() -> String {
    use crate::colors;
    use colored::Colorize;
    format!(
        "  {} 2fa is now active. you'll be asked for a code on next login.",
        "ok".custom_color(colors::GREEN_OK)
    )
}

/// The `bb 2fa disable` success line (non-json, non-quiet mode).
fn render_disable_success() -> String {
    use crate::colors;
    use colored::Colorize;
    format!("  {} 2fa disabled.", "ok".custom_color(colors::GREEN_OK))
}

/// `bb 2fa enable --code <6 digits>` — activates a pending `bb 2fa setup`
/// by POSTing the live TOTP code to `POST /api/v1/auth/2fa/enable`
/// (`ApiClient::totp_enable`). No step-up token: the code itself is the
/// route's proof of possession (`routes/totp.rs::enable`).
pub async fn enable(code: String) -> Result<(), String> {
    use crate::ui;

    let api = ApiClient::from_config();
    api.require_auth()?;
    validate_code(&code)?;

    let resp = match api.totp_enable(&code).await {
        Ok(resp) => resp,
        Err(e) => return Err(resolve_totp_error(&api, classify_enable_error(e)).await),
    };

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "{}".to_string())
        );
    } else if !ui::is_quiet() {
        println!("{}", render_enable_success());
    }
    Ok(())
}

/// `bb 2fa disable --code <6 digits>` — turns TOTP off by POSTing the live
/// code to `POST /api/v1/auth/2fa/disable` (`ApiClient::totp_disable`).
///
/// **Deviation (eng-0479):** the plan describes this as "DELETE, requires
/// step-up confirm" (like `commands::confirm::acquire_confirm_token` /
/// `X-Confirm-Token`). The LIVE handler
/// (`repos/server/beebeeb-api/src/routes/totp.rs::disable`, ~L155-244) takes
/// `auth: AuthUser` + `Json<CodeRequest>` only — no
/// `crate::confirmation::consume_confirmation_from_headers` call anywhere in
/// that function, unlike `setup`'s step-up gate a few lines above it. The
/// TOTP code IS the step-up proof here; sending anything beyond the code
/// would not match what the server checks. Followed the server, not the plan.
pub async fn disable(code: String) -> Result<(), String> {
    use crate::ui;

    let api = ApiClient::from_config();
    api.require_auth()?;
    validate_code(&code)?;

    let resp = match api.totp_disable(&code).await {
        Ok(resp) => resp,
        Err(e) => return Err(resolve_totp_error(&api, classify_disable_error(e)).await),
    };

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "{}".to_string())
        );
    } else if !ui::is_quiet() {
        println!("{}", render_disable_success());
    }
    Ok(())
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
        let parsed = TwofaSetup::from_setup_response(&setup_fixture()).expect("valid fixture must parse");
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
        let parsed = TwofaSetup::from_setup_response(&setup_fixture()).expect("valid fixture must parse");
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
        let parsed = TwofaSetup::from_setup_response(&setup_fixture()).expect("valid fixture must parse");
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

    // ── eng-0478 code-review fixes (Codex PR #18) ───────────────────────

    #[test]
    fn from_setup_response_rejects_a_missing_secret() {
        // A 2xx with schema skew (secret dropped/renamed) must be a hard
        // error, never a "successful" setup with an empty secret the user
        // can't actually use (Codex PRRT_kwDOSLX6I86k4hWV).
        let mut resp = setup_fixture();
        resp.as_object_mut().unwrap().remove("secret");
        let err = TwofaSetup::from_setup_response(&resp).expect_err("missing secret must be rejected");
        assert!(err.contains("secret"), "error should mention the missing field: {err}");
    }

    #[test]
    fn from_setup_response_rejects_an_empty_qr_uri() {
        let mut resp = setup_fixture();
        resp["qr_uri"] = json!("");
        let err = TwofaSetup::from_setup_response(&resp).expect_err("empty qr_uri must be rejected");
        assert!(err.contains("qr_uri"), "error should mention the missing field: {err}");
    }

    #[test]
    fn from_setup_response_rejects_missing_backup_codes() {
        let mut resp = setup_fixture();
        resp.as_object_mut().unwrap().remove("backup_codes");
        let err = TwofaSetup::from_setup_response(&resp).expect_err("missing backup_codes must be rejected");
        assert!(err.contains("backup"), "error should mention backup codes: {err}");
    }

    #[test]
    fn from_setup_response_rejects_an_empty_backup_codes_list() {
        let mut resp = setup_fixture();
        resp["backup_codes"] = json!([]);
        let err = TwofaSetup::from_setup_response(&resp).expect_err("empty backup_codes must be rejected");
        assert!(err.contains("backup"), "error should mention backup codes: {err}");
    }

    #[test]
    fn from_setup_response_rejects_a_non_string_backup_code() {
        // A partially-malformed list (e.g. one code came back as `null` due
        // to server-side truncation) must not silently drop that entry and
        // hand the user an incomplete recovery-code list — reject the whole
        // response instead.
        let mut resp = setup_fixture();
        resp["backup_codes"] = json!(["81720394", null, "56213409"]);
        let err = TwofaSetup::from_setup_response(&resp).expect_err("a non-string backup code must be rejected");
        assert!(err.contains("backup"), "error should mention backup codes: {err}");
    }

    #[test]
    fn render_setup_quiet_is_bare_secret_and_codes_no_decoration() {
        let parsed = TwofaSetup::from_setup_response(&setup_fixture()).expect("valid fixture must parse");
        let lines = render_setup_quiet(&parsed);

        assert_eq!(lines[0], parsed.secret, "first line should be the bare secret");
        assert_eq!(
            lines.len(),
            1 + parsed.backup_codes.len(),
            "should be exactly secret + one line per backup code, no decoration: {lines:?}"
        );
        for code in &parsed.backup_codes {
            assert!(
                lines.contains(code),
                "missing backup code {code} in quiet output: {lines:?}"
            );
        }
        let joined = lines.join("\n");
        assert!(
            !joined.chars().any(|c| c == '█' || c == '▄' || c == '▀' || c == '━'),
            "quiet output must not contain decorative/QR characters: {lines:?}"
        );
    }

    // ── eng-0479: `bb 2fa enable` / `bb 2fa disable` ────────────────────

    #[test]
    fn validate_code_rejects_wrong_length() {
        assert!(validate_code("12345").is_err(), "5 digits must be rejected");
        assert!(validate_code("1234567").is_err(), "7 digits must be rejected");
    }

    #[test]
    fn validate_code_rejects_non_digits() {
        assert!(validate_code("12a456").is_err(), "letters must be rejected");
    }

    #[test]
    fn validate_code_accepts_six_digits() {
        assert!(validate_code("000000").is_ok());
        assert!(validate_code("654321").is_ok());
    }

    /// Live server: `routes/totp.rs::enable` → `ok_or(ApiError::BadRequest(
    /// "2FA has not been set up yet".into()))` when no `totp_secrets` row
    /// exists. `error.rs`'s blanket `IntoResponse for ApiError` wraps EVERY
    /// case in `{"error": message}` (confirmed at `error.rs:591`), and
    /// `parse_response` (api.rs) unwraps that straight to the bare message —
    /// no `"{status}: {body}"` prefix. Confirmed live (eng-0479 manual run,
    /// isolated HOME): a real 400 from this branch reaches the CLI as
    /// exactly `"2FA has not been set up yet"`.
    #[test]
    fn classify_enable_error_reports_setup_required() {
        let outcome = classify_enable_error(ApiError::test_message("2FA has not been set up yet"));
        assert_eq!(
            outcome,
            TotpErrorOutcome::Message("run `bb 2fa setup` first".to_string()),
            "got: {outcome:?}"
        );
    }

    /// Live server: `routes/totp.rs::enable` → `BadRequest("2FA is already
    /// enabled")` when `totp.enabled` is already true. Confirmed live
    /// (eng-0479 manual run): `bb 2fa enable --code 000000` on an
    /// already-enabled account printed exactly `error: 2FA is already
    /// enabled`, exit 1.
    #[test]
    fn classify_enable_error_reports_already_enabled() {
        let outcome = classify_enable_error(ApiError::test_message("2FA is already enabled"));
        assert_eq!(
            outcome,
            TotpErrorOutcome::Message("2FA is already enabled".to_string()),
            "got: {outcome:?}"
        );
    }

    /// Live server: `verify_totp_code` → `ApiError::Unauthorized`, which
    /// `error.rs`'s match arm renders as `(401, "unauthorized")` — wrapped to
    /// `{"error":"unauthorized"}` and unwrapped by `parse_response` to the
    /// bare, lowercase `"unauthorized"`. Confirmed live via raw curl
    /// (eng-0479): `POST /2fa/enable` with a wrong code on a pending-setup
    /// account returned exactly `{"error":"unauthorized"}`, status 401 — NOT
    /// a `"401 Unauthorized: ..."`-prefixed string (an earlier draft of this
    /// classification assumed that prefix and silently failed to match;
    /// caught by the manual exercise, not by a unit test — see task Notes).
    /// This is the AMBIGUOUS case (Codex PR #19 finding
    /// `PRRT_kwDOSLX6I86k5ddl`) — the classifier does NOT resolve it to a
    /// final message; see `diagnose_unauthorized_*` below for that.
    #[test]
    fn classify_enable_error_reports_ambiguous_on_bare_unauthorized() {
        // The real shape: `totp_enable` returns `ApiError` with
        // `code: Some("unauthorized")` (task 1547 finding 1).
        let outcome = classify_enable_error(ApiError::test_code("unauthorized"));
        assert_eq!(outcome, TotpErrorOutcome::AmbiguousUnauthorized, "got: {outcome:?}");
    }

    /// Defensive belt: a non-JSON-body error (e.g. a proxy 401 in front of a
    /// misconfigured `--api`) falls through `parse_response`'s
    /// `format!("{status}: {body}")` path instead, which DOES carry the
    /// capitalized/status-coded form, and carries no code at all — must
    /// still classify as ambiguous via the message-substring fallback.
    #[test]
    fn classify_enable_error_reports_ambiguous_on_status_prefixed_form() {
        let outcome = classify_enable_error(ApiError::test_message("401 Unauthorized: unauthorized"));
        assert_eq!(outcome, TotpErrorOutcome::AmbiguousUnauthorized, "got: {outcome:?}");
    }

    #[test]
    fn classify_enable_error_passes_through_unrecognized_errors() {
        let outcome = classify_enable_error(ApiError::test_message("network failed"));
        assert_eq!(outcome, TotpErrorOutcome::Message("network failed".to_string()));
    }

    /// Live server: `routes/totp.rs::disable` → `ok_or(BadRequest("2FA is
    /// not set up"))` when no row exists at all. See
    /// `classify_enable_error_reports_setup_required` for the confirmed
    /// bare-message wire format.
    #[test]
    fn classify_disable_error_reports_not_enabled_when_never_set_up() {
        let outcome = classify_disable_error(ApiError::test_message("2FA is not set up"));
        assert_eq!(
            outcome,
            TotpErrorOutcome::Message("2FA is not currently enabled".to_string()),
            "got: {outcome:?}"
        );
    }

    /// Live server: `routes/totp.rs::disable` → `BadRequest("2FA is not
    /// currently enabled")` when a row exists but `enabled` is false.
    /// Confirmed live (eng-0479 manual run): `bb 2fa disable --code 000000`
    /// on a disabled account printed exactly `error: 2FA is not currently
    /// enabled`, exit 1.
    #[test]
    fn classify_disable_error_reports_not_enabled_when_row_disabled() {
        let outcome = classify_disable_error(ApiError::test_message("2FA is not currently enabled"));
        assert_eq!(
            outcome,
            TotpErrorOutcome::Message("2FA is not currently enabled".to_string()),
            "got: {outcome:?}"
        );
    }

    #[test]
    fn classify_disable_error_reports_ambiguous_on_bare_unauthorized() {
        let outcome = classify_disable_error(ApiError::test_code("unauthorized"));
        assert_eq!(outcome, TotpErrorOutcome::AmbiguousUnauthorized, "got: {outcome:?}");
    }

    #[test]
    fn classify_disable_error_passes_through_unrecognized_errors() {
        let outcome = classify_disable_error(ApiError::test_message("network failed"));
        assert_eq!(outcome, TotpErrorOutcome::Message("network failed".to_string()));
    }

    #[test]
    fn render_enable_success_confirms_2fa_is_active() {
        let line = render_enable_success();
        assert!(line.contains("2fa is now active"), "line was: {line:?}");
    }

    #[test]
    fn render_disable_success_confirms_2fa_disabled() {
        let line = render_disable_success();
        assert!(line.contains("2fa disabled"), "line was: {line:?}");
    }

    // ── eng-0479 code-review fix (Codex PR #19, `PRRT_kwDOSLX6I86k5ddl`):
    // `diagnose_unauthorized` disambiguates a dead session from a wrong code
    // via a live `GET /api/v1/auth/me` follow-up call, through the SAME
    // in-process axum mock pattern `api.rs`'s test modules use. ───────────

    async fn spawn_me_mock(status: axum::http::StatusCode, body: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/api/v1/auth/me",
            axum::routing::get(move || {
                let body = body.clone();
                async move { (status, axum::Json(body)) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn diagnose_unauthorized_blames_the_code_when_the_session_is_alive() {
        let url = spawn_me_mock(
            axum::http::StatusCode::OK,
            json!({ "email": "a@b.com", "totp_enabled": false }),
        )
        .await;
        let api = ApiClient::new_for_test(url);
        let msg = diagnose_unauthorized(&api).await;
        assert_eq!(
            msg, "incorrect code — try again",
            "a live session (GET /auth/me succeeded) must blame the code, not the session"
        );
    }

    #[tokio::test]
    async fn diagnose_unauthorized_blames_the_session_when_it_is_dead() {
        let url = spawn_me_mock(axum::http::StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" })).await;
        let api = ApiClient::new_for_test(url);
        let msg = diagnose_unauthorized(&api).await;
        assert_eq!(
            msg, "your session has expired — run `bb login` again",
            "a dead session (GET /auth/me ALSO 401s) must not be blamed on the code"
        );
    }
}
