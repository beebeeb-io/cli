//! `bb passkey list` — your registered WebAuthn credentials.
//!
//! Route used:
//!   GET /api/v1/auth/passkeys  (`ApiClient::list_passkeys`)
//!
//! Live response shape (confirmed against
//! `repos/server/beebeeb-api/src/routes/passkeys.rs::list_passkeys`, ~L360-382):
//!
//! ```json
//! { "passkeys": [{
//!     "id": "<uuid>",
//!     "name": "Passkey",
//!     "created_at": "2026-09-01T10:00:00Z"
//! }] }
//! ```
//!
//! **Deviation (eng-0482) — no "last-used" field.** The task text ("renders
//! name, created date, last-used date") and the plan's Task 14 pseudocode
//! both describe a last-used column. The live handler's `SELECT id, name,
//! created_at FROM passkeys` (passkeys.rs ~L361) never reads or returns one,
//! and there is no column to read it from in the first place — the
//! `passkeys` table (`repos/server/beebeeb-api/src/db.rs` ~L446-452:
//! `id, user_id, credential, name, created_at`) has no `last_used_at`/
//! `last_used` column at all, and `login_start`/`login_finish` in the same
//! routes file never write one on a successful passkey authentication. There
//! is nothing to render — the CREATED column is the only timestamp this CLI
//! can show honestly. Not rendered; never invented.
//!
//! **Scope (0483) — `list` + `add` (plan Tasks 14/15).** `add` opens a
//! browser for WebAuthn registration — the CLI process itself can never
//! perform a WebAuthn ceremony, so it only launches the web app's passkey
//! page and confirms the user navigated there. `remove` (Task 16) still
//! needs the step-up / id-resolution treatment `bb sessions revoke` got in
//! task 0481 and is not wired into the clap tree, so `bb passkey remove`
//! does not appear in `bb passkey --help` and is not a silent stub that
//! prints a "not yet implemented" error — it simply isn't a command yet.
//!
//! ## `add` — browser handoff (0483)
//!
//! Route used: the **web app**, not the API — `GET /settings/passkeys`
//! (`repos/web/src/app.tsx:554`, component `PasskeySetup` at
//! `repos/web/src/pages/passkey-setup.tsx:79`). That page runs the actual
//! `navigator.credentials.create()` ceremony against
//! `POST /api/v1/auth/passkey/register-start` + `register-finish`
//! (`repos/web/src/lib/api.ts` `startPasskeyRegistration`/
//! `finishPasskeyRegistration`, server-side `repos/server/beebeeb-api/src/
//! router.rs:541` nests `passkeys::router()` at `/api/v1/auth/passkey`);
//! this command never touches those routes.
//!
//! **No handoff token (plan's Open question 6, still open).** No
//! `POST /api/v1/auth/passkey/handoff`-shaped route exists on the live
//! server (`repos/server/beebeeb-api/src/routes/passkeys.rs` has no such
//! handler), so v1 opens a bare URL — the user re-authenticates in the
//! browser if their web session has expired. Documented in `--help` and the
//! in-terminal output; do not invent a token param that the server can't
//! consume.
//!
//! **URL derivation follows the configured API, never a hard-coded prod
//! host** (`passkey_enrollment_url_from_api`, mirrors the localhost/`api.`→
//! `app.` mapping the plan sketched, generalized to any `api.<host>`, not
//! just `beebeeb.io`) — a `--api`/config pointed at a local or staging
//! server must not silently hand the user a `app.beebeeb.io` link.

use serde_json::Value;

use crate::api::ApiClient;

/// One parsed element of `GET /api/v1/auth/passkeys`'s `passkeys` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyRow {
    pub id: String,
    pub name: String,
    pub created_at: String,
}

impl PasskeyRow {
    /// Parse one element. `id` and `created_at` are required (every live row
    /// has both — `id` is the primary key, `created_at` is `NOT NULL`); a
    /// row missing either is dropped rather than rendered with a fabricated
    /// placeholder. `name` is `NOT NULL` on the live table too, but falls
    /// back to `"Passkey"` (the same default `register_finish` writes when
    /// no name was supplied, passkeys.rs ~L123) defensively rather than
    /// dropping an otherwise-valid row over a missing/blank display label.
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = v.get("id").and_then(Value::as_str)?.to_string();
        let created_at = v.get("created_at").and_then(Value::as_str)?.to_string();
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("Passkey")
            .to_string();
        Some(Self { id, name, created_at })
    }
}

/// Parse the full `{"passkeys": [...]}` body into rows for the human table.
/// A malformed element is skipped rather than failing the whole list — one
/// bad row must not hide every other passkey (mirrors `sessions::parse_sessions`).
pub fn parse_passkeys(body: &Value) -> Vec<PasskeyRow> {
    body.get("passkeys")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(PasskeyRow::from_json).collect())
        .unwrap_or_default()
}

/// Classify a raw `GET /api/v1/auth/passkeys` error string. Like
/// `sessions::classify_list_error`, this route has no request body to be
/// wrong about — only the `AuthUser` extractor can reject it — so a bare
/// `401`/`"unauthorized"` has exactly one cause.
fn classify_list_error(e: String) -> String {
    if e.to_lowercase().contains("unauthorized") || e.contains("401") {
        "your session has expired — run `bb login` again".to_string()
    } else {
        e
    }
}

/// Extract the raw `passkeys` array from the response body, unmodified — the
/// `--json` output is a passthrough of exactly what the server sent (falling
/// back to an empty array if the key is missing), never a reconstruction
/// from `PasskeyRow` (which would silently drop a field a future server
/// response adds).
fn extract_passkeys_json(body: &Value) -> Value {
    body.get("passkeys")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

/// First 8 characters of a UUID, for compact table cells (mirrors
/// `sessions::short_id` / `bb ls`'s short-id convention).
fn short_id(id: &str) -> &str {
    if id.len() >= 8 { &id[..8] } else { id }
}

/// Render the `--quiet` lines: one bare, uncolored passkey id per line — no
/// header, no summary — matching the established quiet-mode convention
/// elsewhere in this CLI (`sessions list`'s `render_quiet`, `ls`'s
/// `print_row`, `search`'s quiet branch, `trash list`'s quiet branch).
fn render_quiet(passkeys: &[PasskeyRow]) -> Vec<String> {
    passkeys.iter().map(|p| p.id.clone()).collect()
}

/// Render the full human-mode table.
fn render_table(passkeys: &[PasskeyRow]) -> String {
    use crate::{colors, ui};
    use colored::Colorize;

    let headers = ["ID", "NAME", "CREATED"];
    let headers_colored: Vec<String> = headers
        .iter()
        .map(|h| h.custom_color(colors::AMBER).to_string())
        .collect();
    let headers_ref: Vec<&str> = headers_colored.iter().map(|s| s.as_str()).collect();

    let rows: Vec<Vec<String>> = passkeys
        .iter()
        .map(|p| {
            vec![
                short_id(&p.id).custom_color(colors::INK).to_string(),
                p.name.clone(),
                ui::relative_time(&p.created_at),
            ]
        })
        .collect();

    ui::table(&headers_ref, &rows)
}

/// `bb passkey list`.
pub async fn list() -> Result<(), String> {
    use crate::{colors, ui};
    use colored::Colorize;

    let api = ApiClient::from_config();
    api.require_auth()?;

    let body = match api.list_passkeys().await {
        Ok(body) => body,
        Err(e) => return Err(classify_list_error(e)),
    };

    if ui::is_json() {
        let raw = extract_passkeys_json(&body);
        println!(
            "{}",
            serde_json::to_string_pretty(&raw).unwrap_or_else(|_| "[]".to_string())
        );
        return Ok(());
    }

    let passkeys = parse_passkeys(&body);

    if ui::is_quiet() {
        // No headers, no summary line, no color — same convention as
        // `sessions list`'s quiet branch. Runs even for an empty list.
        for line in render_quiet(&passkeys) {
            println!("{line}");
        }
        return Ok(());
    }

    if passkeys.is_empty() {
        println!("  {}", "no passkeys registered".custom_color(colors::INK_DIM));
        println!(
            "  {}",
            "manage passkeys in the web app: Settings -> Security".custom_color(colors::INK_DIM)
        );
        return Ok(());
    }

    print!("{}", render_table(&passkeys));
    println!();
    println!(
        "  {} passkey{}",
        passkeys.len(),
        if passkeys.len() == 1 { "" } else { "s" }
    );

    Ok(())
}

// ── `bb passkey add` ────────────────────────────────────────────────────────

/// The web app's dedicated passkey-registration route. Pure path constant —
/// kept separate from the host so `passkey_enrollment_url_from_api` (below)
/// is the only place that decides the host.
const PASSKEY_ENROLLMENT_PATH: &str = "/settings/passkeys";

/// Pull just the host (no scheme, no port, no path) out of an API base URL.
/// Used instead of a whole-string substring search so a custom domain that
/// merely *contains* "localhost" as a label (`api.localhost.example.com`)
/// isn't misidentified as the local dev API (Codex review, PR #23).
fn host_of(api_url: &str) -> &str {
    let without_scheme = api_url
        .strip_prefix("https://")
        .or_else(|| api_url.strip_prefix("http://"))
        .unwrap_or(api_url);
    let host_and_port = without_scheme.split('/').next().unwrap_or(without_scheme);
    // IPv6 literals are bracketed (`[::1]:3001`) — don't split those on ':'.
    if let Some(rest) = host_and_port.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    host_and_port.split(':').next().unwrap_or(host_and_port)
}

/// Derive the web app's passkey-enrollment URL from a configured API base
/// URL, never a hard-coded production host. Pure function of the API URL
/// string so it's unit-testable without touching the real (possibly-live)
/// on-disk config.
///
/// - `localhost`/`127.0.0.1`/`::1` (any port, matched on the parsed host —
///   not a substring search) → the local dev web app, `localhost:5173`
///   (`repos/web` `bun dev` default — see `repos/web/CLAUDE.md`).
/// - `https://api.<host>` / `http://api.<host>` → the same scheme + host
///   with `api.` swapped for `app.` — the convention every other
///   Beebeeb-operated environment (prod `api.beebeeb.io` → `app.beebeeb.io`,
///   and any future staging `api.<env>.beebeeb.io` → `app.<env>.beebeeb.io`)
///   already follows.
/// - Anything else (an API host with no recognizable `api.` prefix) → the
///   API's own scheme+host, so an unrecognized `--api` never silently
///   resolves to Beebeeb's production web app. It may well 404, but a 404
///   is honest; a link to the wrong company's data is not.
fn passkey_enrollment_url_from_api(api_url: &str) -> String {
    let api = api_url.trim_end_matches('/');
    let host = host_of(api);

    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        return format!("http://localhost:5173{PASSKEY_ENROLLMENT_PATH}");
    }
    if let Some(rest) = api.strip_prefix("https://api.") {
        return format!("https://app.{rest}{PASSKEY_ENROLLMENT_PATH}");
    }
    if let Some(rest) = api.strip_prefix("http://api.") {
        return format!("http://app.{rest}{PASSKEY_ENROLLMENT_PATH}");
    }
    format!("{api}{PASSKEY_ENROLLMENT_PATH}")
}

/// Read the configured API URL (respecting `--api`, same as every other
/// command) and derive the enrollment URL from it.
fn passkey_enrollment_url() -> String {
    let config = crate::config::load_config();
    passkey_enrollment_url_from_api(&config.api_url)
}

/// What `add` should do, given its flags and the environment. Pure — no I/O
/// — so every branch is unit-tested directly; `add()` below is the thin,
/// untested wrapper that performs the actual printing/opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddAction {
    /// `--json`: a structured blob, never touch the browser.
    Json,
    /// `--print-url` (or `--quiet`, which is the same "give me the bare
    /// value, no side effects" contract as everywhere else in this CLI):
    /// the bare URL, never touch the browser.
    PrintUrl,
    /// No display reachable (SSH without X forwarding, bare Linux TTY):
    /// show the URL for copying to another device, don't attempt to open.
    Headless,
    /// Normal interactive case: best-effort open the URL in a browser.
    Open,
}

/// Decide the `AddAction` for `add`'s flags + environment. `is_json` wins
/// over everything (a script asked for a machine-readable contract);
/// `print_url`/`is_quiet` both mean "just the value"; otherwise branch on
/// whether a display is reachable.
fn plan_add(print_url: bool, is_json: bool, is_quiet: bool, headless: bool) -> AddAction {
    if is_json {
        AddAction::Json
    } else if print_url || is_quiet {
        AddAction::PrintUrl
    } else if headless {
        AddAction::Headless
    } else {
        AddAction::Open
    }
}

/// `bb passkey add`. The CLI process can never run a WebAuthn ceremony
/// itself (no browser, no platform authenticator API) — this command's
/// entire job is getting the user to the right web page with the right
/// context, then getting out of the way.
pub async fn add(print_url: bool) -> Result<(), String> {
    use crate::{colors, env_detect, ui};
    use colored::Colorize;

    let api = ApiClient::from_config();
    api.require_auth()?;

    let url = passkey_enrollment_url();
    let action = plan_add(print_url, ui::is_json(), ui::is_quiet(), env_detect::is_headless());

    match action {
        AddAction::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "url": url,
                    "note": "open in a WebAuthn-capable browser to register a passkey",
                })
            );
        }
        AddAction::PrintUrl => {
            println!("{url}");
        }
        AddAction::Headless => {
            println!("  passkeys need a browser with WebAuthn support.");
            println!();
            println!("  no display detected — open this URL in a browser (this device or another):");
            println!();
            println!("  {}", url.custom_color(colors::INK));
            println!();
            println!(
                "  (next time, use {} to skip this hint)",
                "bb passkey add --print-url".custom_color(colors::INK_DIM)
            );
            println!();
            println!("  after registration completes in the browser, run:");
            println!("    {}", "bb passkey list".custom_color(colors::INK_DIM));
        }
        AddAction::Open => {
            println!("  passkeys need a browser with WebAuthn support.");
            println!();
            println!("  opening {} ...", url.custom_color(colors::INK));
            // Best-effort — the URL is already printed above, so a failed
            // launch (no default browser configured, etc.) still leaves the
            // user with something to click or paste.
            let _ = open::that(&url);
            println!();
            println!("  after registration completes in the browser, run:");
            println!("    {}", "bb passkey list".custom_color(colors::INK_DIM));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fixture matching the LIVE server's response shape
    /// (`repos/server/beebeeb-api/src/routes/passkeys.rs::list_passkeys`).
    fn passkeys_fixture() -> Value {
        json!({
            "passkeys": [
                {
                    "id": "11111111-1111-1111-1111-111111111111",
                    "name": "MacBook Touch ID",
                    "created_at": "2026-09-01T09:00:00Z"
                },
                {
                    "id": "22222222-2222-2222-2222-222222222222",
                    "name": "YubiKey",
                    "created_at": "2026-08-15T09:00:00Z"
                }
            ]
        })
    }

    // ── parse_passkeys / PasskeyRow::from_json ──────────────────────────

    #[test]
    fn parse_passkeys_reads_every_field_from_the_live_shape() {
        let rows = parse_passkeys(&passkeys_fixture());
        assert_eq!(rows.len(), 2, "rows: {rows:?}");

        assert_eq!(rows[0].id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(rows[0].name, "MacBook Touch ID");
        assert_eq!(rows[0].created_at, "2026-09-01T09:00:00Z");

        assert_eq!(rows[1].id, "22222222-2222-2222-2222-222222222222");
        assert_eq!(rows[1].name, "YubiKey");
    }

    #[test]
    fn parse_passkeys_defaults_a_missing_name_to_passkey() {
        let body = json!({ "passkeys": [{
            "id": "33333333-3333-3333-3333-333333333333",
            "created_at": "2026-09-01T09:00:00Z"
        }] });
        let rows = parse_passkeys(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Passkey");
    }

    #[test]
    fn parse_passkeys_defaults_an_empty_name_to_passkey() {
        let body = json!({ "passkeys": [{
            "id": "33333333-3333-3333-3333-333333333333",
            "name": "",
            "created_at": "2026-09-01T09:00:00Z"
        }] });
        let rows = parse_passkeys(&body);
        assert_eq!(rows[0].name, "Passkey", "a blank name must not render as an empty cell");
    }

    #[test]
    fn parse_passkeys_skips_a_row_missing_the_required_id() {
        let body = json!({ "passkeys": [
            { "name": "no id", "created_at": "2026-09-01T09:00:00Z" },
            { "id": "44444444-4444-4444-4444-444444444444", "name": "ok", "created_at": "2026-09-01T09:00:00Z" }
        ] });
        let rows = parse_passkeys(&body);
        assert_eq!(
            rows.len(),
            1,
            "the malformed row must be dropped, not panic or fabricate an id"
        );
        assert_eq!(rows[0].id, "44444444-4444-4444-4444-444444444444");
    }

    #[test]
    fn parse_passkeys_skips_a_row_missing_the_required_created_at() {
        let body = json!({ "passkeys": [
            { "id": "55555555-5555-5555-5555-555555555555", "name": "no created_at" }
        ] });
        assert_eq!(parse_passkeys(&body).len(), 0);
    }

    #[test]
    fn parse_passkeys_on_missing_passkeys_key_returns_empty() {
        assert_eq!(parse_passkeys(&json!({})), vec![]);
    }

    // ── render_table ──────────────────────────────────────────────────────

    #[test]
    fn render_table_shows_short_id_name_and_relative_created() {
        let rows = parse_passkeys(&passkeys_fixture());
        let out = render_table(&rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows: {lines:?}");
        assert!(lines[1].contains("11111111"), "line was: {:?}", lines[1]);
        assert!(lines[1].contains("MacBook Touch ID"), "line was: {:?}", lines[1]);
        assert!(lines[2].contains("YubiKey"), "line was: {:?}", lines[2]);
    }

    #[test]
    fn render_table_never_leaks_a_raw_iso_timestamp() {
        let rows = parse_passkeys(&passkeys_fixture());
        let out = render_table(&rows);
        assert!(
            !out.contains("2026-09-01T09:00:00Z"),
            "raw ISO timestamp leaked into the table instead of a relative string:\n{out}"
        );
    }

    #[test]
    fn render_table_never_mentions_last_used() {
        // Guards the eng-0482 deviation: the live response has no
        // last-used field, so the table must never claim to show one.
        let rows = parse_passkeys(&passkeys_fixture());
        let out = render_table(&rows).to_lowercase();
        assert!(
            !out.contains("last"),
            "table must not fabricate a last-used column: {out}"
        );
    }

    // ── --quiet mode ─────────────────────────────────────────────────────

    #[test]
    fn render_quiet_emits_one_bare_id_per_passkey() {
        let rows = parse_passkeys(&passkeys_fixture());
        let lines = render_quiet(&rows);
        assert_eq!(lines.len(), 2, "one line per passkey, no header, no summary: {lines:?}");
        assert_eq!(lines[0], "11111111-1111-1111-1111-111111111111");
        assert_eq!(lines[1], "22222222-2222-2222-2222-222222222222");
    }

    #[test]
    fn render_quiet_lines_are_single_whitespace_free_tokens() {
        let rows = parse_passkeys(&passkeys_fixture());
        for line in render_quiet(&rows) {
            assert!(
                !line.contains(char::is_whitespace),
                "quiet line must be one token, safe for xargs/mapfile: {line:?}"
            );
        }
    }

    #[test]
    fn render_quiet_contains_no_ansi_escapes() {
        let rows = parse_passkeys(&passkeys_fixture());
        let joined = render_quiet(&rows).join("\n");
        assert!(
            !joined.contains('\x1b'),
            "quiet output must never carry color codes: {joined:?}"
        );
    }

    #[test]
    fn render_quiet_on_empty_passkeys_emits_no_lines() {
        assert_eq!(render_quiet(&[]), Vec::<String>::new());
    }

    // ── JSON passthrough ─────────────────────────────────────────────────

    #[test]
    fn json_mode_extracts_the_raw_passkeys_array_unmodified() {
        // Exercises the SAME function `list()` calls for `--json`, including
        // a field `PasskeyRow` doesn't know about, to prove this is a
        // passthrough and not a reconstruction.
        let body = json!({ "passkeys": [
            { "id": "1", "name": "n", "created_at": "2026-01-01T00:00:00Z", "future_field": "kept" }
        ] });
        let raw = extract_passkeys_json(&body);
        assert_eq!(raw, body["passkeys"]);
        assert_eq!(
            raw[0]["future_field"], "kept",
            "a passthrough must not drop unknown fields"
        );
    }

    #[test]
    fn json_mode_on_missing_passkeys_key_falls_back_to_empty_array() {
        assert_eq!(extract_passkeys_json(&json!({})), json!([]));
    }

    // ── logged-out / 401 mapping ─────────────────────────────────────────

    #[test]
    fn classify_list_error_maps_bare_unauthorized_to_a_login_hint() {
        assert_eq!(
            classify_list_error("unauthorized".to_string()),
            "your session has expired — run `bb login` again"
        );
    }

    #[test]
    fn classify_list_error_maps_status_prefixed_unauthorized_too() {
        assert_eq!(
            classify_list_error("401 Unauthorized: unauthorized".to_string()),
            "your session has expired — run `bb login` again"
        );
    }

    #[test]
    fn classify_list_error_passes_through_unrelated_errors() {
        assert_eq!(classify_list_error("network failed".to_string()), "network failed");
    }

    // ── short_id ─────────────────────────────────────────────────────────

    #[test]
    fn short_id_takes_the_first_eight_characters() {
        assert_eq!(short_id("11111111-1111-1111-1111-111111111111"), "11111111");
    }

    #[test]
    fn short_id_returns_the_whole_string_when_shorter_than_eight() {
        assert_eq!(short_id("abc"), "abc");
    }

    // ── passkey_enrollment_url_from_api ─────────────────────────────────────

    #[test]
    fn enrollment_url_for_prod_api_points_at_the_prod_web_app() {
        let url = passkey_enrollment_url_from_api("https://api.beebeeb.io");
        assert_eq!(url, "https://app.beebeeb.io/settings/passkeys");
    }

    #[test]
    fn enrollment_url_for_local_api_points_at_the_local_dev_web_app() {
        let url = passkey_enrollment_url_from_api("http://localhost:3001");
        assert_eq!(url, "http://localhost:5173/settings/passkeys");
    }

    #[test]
    fn local_api_enrollment_url_is_never_the_prod_host() {
        // The concrete regression this task exists to prevent: a CLI pointed
        // at a local/dev API must never hand the user a link to production.
        let prod = passkey_enrollment_url_from_api("https://api.beebeeb.io");
        let local = passkey_enrollment_url_from_api("http://localhost:3001");
        assert_ne!(prod, local);
        assert!(
            !local.contains("beebeeb.io"),
            "local --api must not resolve to any beebeeb.io host: {local}"
        );
    }

    #[test]
    fn enrollment_url_strips_trailing_slash_before_appending_the_path() {
        let url = passkey_enrollment_url_from_api("https://api.beebeeb.io/");
        assert_eq!(url, "https://app.beebeeb.io/settings/passkeys");
    }

    #[test]
    fn enrollment_url_handles_http_scheme_api_host_too() {
        let url = passkey_enrollment_url_from_api("http://api.beebeeb.io");
        assert_eq!(url, "http://app.beebeeb.io/settings/passkeys");
    }

    #[test]
    fn enrollment_url_generalizes_beyond_the_literal_beebeeb_io_host() {
        // A non-prod environment that still follows the api./app. convention
        // (e.g. a staging deploy) must resolve correctly too — this must not
        // be special-cased to the string "beebeeb.io".
        let url = passkey_enrollment_url_from_api("https://api.staging.example.net");
        assert_eq!(url, "https://app.staging.example.net/settings/passkeys");
    }

    #[test]
    fn enrollment_url_for_unrecognized_host_stays_on_that_hosts_origin() {
        // No "api." prefix to swap for "app." — must NOT silently fall back
        // to app.beebeeb.io (that would point a custom/unknown API at
        // production's web app). Staying on the same origin is honest even
        // if it 404s.
        let url = passkey_enrollment_url_from_api("https://weird-custom-host.example.com");
        assert_eq!(url, "https://weird-custom-host.example.com/settings/passkeys");
        assert!(!url.contains("beebeeb.io"));
    }

    #[test]
    fn enrollment_url_matches_127_0_0_1_as_local_too() {
        let url = passkey_enrollment_url_from_api("http://127.0.0.1:3001");
        assert_eq!(url, "http://localhost:5173/settings/passkeys");
    }

    #[test]
    fn enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local() {
        // Codex review, PR #23: a whole-string `.contains("localhost")` check
        // would misfire on a legitimate custom/staging domain that merely has
        // "localhost" as a label, silently redirecting a real remote --api to
        // this machine's dev web app. The host must be parsed, not grepped.
        let url = passkey_enrollment_url_from_api("https://api.localhost.example.com");
        assert_eq!(url, "https://app.localhost.example.com/settings/passkeys");
        assert!(!url.starts_with("http://localhost:5173"));
    }

    #[test]
    fn enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local() {
        let url = passkey_enrollment_url_from_api("https://api.127.0.0.1.example.com");
        assert_eq!(url, "https://app.127.0.0.1.example.com/settings/passkeys");
        assert!(!url.starts_with("http://localhost:5173"));
    }

    #[test]
    fn enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped() {
        let url = passkey_enrollment_url_from_api("http://[::1]:3001");
        assert_eq!(url, "http://localhost:5173/settings/passkeys");
    }

    // ── host_of ───────────────────────────────────────────────────────────

    #[test]
    fn host_of_strips_scheme_port_and_path() {
        assert_eq!(host_of("https://api.beebeeb.io:8443/foo"), "api.beebeeb.io");
        assert_eq!(host_of("http://localhost:3001"), "localhost");
        assert_eq!(host_of("api.beebeeb.io"), "api.beebeeb.io");
    }

    // ── plan_add ──────────────────────────────────────────────────────────

    #[test]
    fn plan_add_json_wins_over_every_other_flag() {
        assert_eq!(plan_add(true, true, true, true), AddAction::Json);
        assert_eq!(plan_add(false, true, false, false), AddAction::Json);
    }

    #[test]
    fn plan_add_print_url_flag_never_opens_a_browser() {
        // The exact regression this flag exists for: --print-url must route
        // to PrintUrl, never Open — Open is the only variant that calls
        // `open::that`.
        assert_eq!(plan_add(true, false, false, false), AddAction::PrintUrl);
        assert_eq!(plan_add(true, false, false, true), AddAction::PrintUrl);
    }

    #[test]
    fn plan_add_quiet_mode_behaves_like_print_url() {
        // --quiet is "bare value, no side effects" everywhere else in this
        // CLI (`sessions list`, `ls`, `quota`, ...) — add must match that
        // convention rather than opening a browser mid-script.
        assert_eq!(plan_add(false, false, true, false), AddAction::PrintUrl);
    }

    #[test]
    fn plan_add_headless_without_print_url_shows_the_url_but_does_not_open() {
        assert_eq!(plan_add(false, false, false, true), AddAction::Headless);
    }

    #[test]
    fn plan_add_default_interactive_case_opens_the_browser() {
        assert_eq!(plan_add(false, false, false, false), AddAction::Open);
    }

    // ── add() JSON body shape ────────────────────────────────────────────

    #[test]
    fn add_json_body_carries_the_url_and_a_note_never_a_token() {
        // Guards the "no handoff token" deviation documented at the top of
        // this file: v1 has nothing to embed, so the JSON body must not
        // fabricate a `token` field a future server route might expect.
        let url = passkey_enrollment_url_from_api("https://api.beebeeb.io");
        let body = serde_json::json!({
            "url": url,
            "note": "open in a WebAuthn-capable browser to register a passkey",
        });
        assert_eq!(body["url"], "https://app.beebeeb.io/settings/passkeys");
        assert!(body.get("token").is_none());
    }
}
