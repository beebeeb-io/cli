//! `bb sessions list` — active sessions across all your devices.
//!
//! Route used:
//!   GET /api/v1/account/sessions  (`ApiClient::list_sessions_v2`, already wired
//!   for `bb account show`'s session count — this task adds the first standalone
//!   viewer of the full row-level payload)
//!
//! Live response shape (confirmed against
//! `repos/server/beebeeb-api/src/routes/account_activity.rs::list_sessions`,
//! ~L281-319):
//!
//! ```json
//! { "sessions": [{
//!     "id": "<uuid>",
//!     "device_name": "Chrome 124 on macOS",
//!     "device_kind": "web",
//!     "country_code": "NL",
//!     "last_active_at": "2026-09-23T10:00:00Z",
//!     "created_at": "2026-09-01T10:00:00Z",
//!     "is_current": true
//! }] }
//! ```
//!
//! `device_name` and `device_kind` are already defaulted server-side
//! (`"Unknown device"` / `"web"`, L308-309) so they are never `null` on the
//! wire today — `SessionRow::from_json` still falls back defensively rather
//! than trusting that forever. `country_code` and `last_active_at` ARE
//! genuinely nullable (`Option<...>` columns, no server-side default) and are
//! rendered as `--` / `—` respectively when absent.
//!
//! **Deviation (eng-0480):** the task text names an "IP" column, but the live
//! handler's `SELECT` (account_activity.rs L282-287: `id, token, device_name,
//! device_kind, country_code, last_active_at, created_at, expires_at`) never
//! reads or returns a client IP for a session — there is no IP column on
//! `sessions` in this read path at all (confirmed by reading the handler and
//! its query, not assumed from the plan). Not rendered; never invented.
//!
//! **Scope (eng-0480):** this task was `list` ONLY (plan Task 12). `revoke` /
//! `revoke-all-others` (plan Task 13) are implemented below (task 0481).
//!
//! ## `revoke` / `revoke-all-others` (task 0481)
//!
//! Routes: `DELETE /api/v1/account/sessions/{id}` and
//! `POST /api/v1/account/sessions/revoke-all-others`
//! (`repos/server/beebeeb-api/src/routes/account_activity.rs`, `revoke_session`
//! ~L325-345 / `revoke_all_other_sessions` ~L351-373).
//!
//! **Deviation (eng-0481) — no step-up.** The plan's Task 13 pseudocode
//! assumed a `X-Confirm-Token` step-up header (`commands::confirm`). Reading
//! both live handler signatures: each takes only `AuthUser`, no
//! `ConfirmedAction` extractor. Neither route enforces step-up, so the CLI
//! sends none — a token the server ignores is not "safer", it's a
//! fabricated password prompt for no server-side effect. Proven by
//! `api::session_revoke_request_tests` (a mock echoes the received headers
//! back; asserts `x-confirm-token` is absent).
//!
//! **Deviation (eng-0481) — no interactive picker.** `bb sessions revoke`
//! requires the id (or a unique prefix, resolved via `resolve_session_id`,
//! same 0/1/ambiguous contract as `ApiClient::find_file_by_id_prefix`) as a
//! required positional argument. The repo's one existing interactive picker
//! (`commands::share::pick_share_interactively`) is ~130 lines of hand-rolled
//! raw-mode terminal code, not a `dialoguer`/`inquire` dependency —
//! grepping the repo for those crate names (Cargo.toml + every
//! `src/**/*.rs`) finds nothing. Porting that pattern into a second command
//! is out of scope here.
//!
//! **Decision — revoking the CURRENT session.** The server allows it:
//! `revoke_session`'s `DELETE` matches on `id AND user_id` only — it
//! never compares against the caller's own session token, unlike
//! `revoke_all_other_sessions`'s query, which explicitly excludes
//! `token = auth.token`. Given the server allows it, this CLI still REFUSES
//! and points at `bb logout` instead of warn-and-confirm, because
//! `bb logout` (`commands/logout.rs`) does two things this command's
//! id-based path can't: a best-effort server-side revoke AND
//! `clear_config()` to wipe the local `session_token`. `sessions revoke
//! <own-current-id>` would only do the server half — the local config
//! keeps the now-dead token, so the very next `bb` command fails with a
//! confusing 401 instead of a clean "run `bb login` again" prompt.
//! `bb logout` is strictly the safer, more complete action for this exact
//! case, so refusing and naming it beats warn-and-confirm-then-do-the-
//! worse-thing.

use serde_json::Value;

use crate::api::ApiClient;

/// One parsed row of `GET /api/v1/account/sessions`'s `sessions` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub device_name: String,
    pub device_kind: String,
    pub country_code: Option<String>,
    pub last_active_at: Option<String>,
    pub created_at: String,
    pub is_current: bool,
}

impl SessionRow {
    /// Parse one element of the `sessions` array. `id` and `created_at` are
    /// required (every live row has both — `id` is the primary key,
    /// `created_at` is `NOT NULL`); a row missing either is dropped rather
    /// than rendered with fabricated placeholders. `device_name`/`device_kind`
    /// fall back to the same placeholders the server itself uses so a future
    /// schema change that drops the server-side default doesn't panic here.
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = v.get("id").and_then(Value::as_str)?.to_string();
        let created_at = v.get("created_at").and_then(Value::as_str)?.to_string();
        Some(Self {
            id,
            device_name: v
                .get("device_name")
                .and_then(Value::as_str)
                .unwrap_or("Unknown device")
                .to_string(),
            device_kind: v
                .get("device_kind")
                .and_then(Value::as_str)
                .unwrap_or("web")
                .to_string(),
            country_code: v.get("country_code").and_then(Value::as_str).map(str::to_string),
            last_active_at: v.get("last_active_at").and_then(Value::as_str).map(str::to_string),
            created_at,
            is_current: v.get("is_current").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// Parse the full `{"sessions": [...]}` body into rows for the human table.
/// A malformed element (missing `id`/`created_at`) is skipped rather than
/// failing the whole list — one bad row must not hide every other session.
pub fn parse_sessions(body: &Value) -> Vec<SessionRow> {
    body.get("sessions")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(SessionRow::from_json).collect())
        .unwrap_or_default()
}

/// Classify a raw `GET /api/v1/account/sessions` error string. Unlike
/// `commands::twofa`'s `classify_enable_error`, a bare `401`/`"unauthorized"`
/// here has exactly one cause — this route has no request body to be wrong
/// about, only the `AuthUser` extractor can reject it — so no ambiguous case
/// and no follow-up call is needed.
fn classify_list_error(e: String) -> String {
    if e.to_lowercase().contains("unauthorized") || e.contains("401") {
        "your session has expired — run `bb login` again".to_string()
    } else {
        e
    }
}

/// Extract the raw `sessions` array from the response body, unmodified — the
/// `--json` output is a passthrough of exactly what the server sent (falling
/// back to an empty array if the key is missing), never a reconstruction from
/// `SessionRow` (which would silently drop a field a future server response
/// adds).
fn extract_sessions_json(body: &Value) -> Value {
    body.get("sessions")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

/// True when the only session in the list is the caller's own current one —
/// the case that gets a friendlier one-line message instead of a one-row
/// table.
fn should_show_empty_state(sessions: &[SessionRow]) -> bool {
    sessions.len() == 1 && sessions[0].is_current
}

/// Render the `*` current-session marker cell (colored when not JSON/quiet).
fn marker_cell(is_current: bool) -> String {
    use crate::colors;
    use colored::Colorize;
    if is_current {
        "*".custom_color(colors::AMBER).to_string()
    } else {
        String::new()
    }
}

/// Render the `--quiet` lines: one bare, uncolored session id per line — no
/// header, no summary, no per-row decoration of any kind — matching the
/// established quiet-mode convention elsewhere in this CLI (`ls`'s
/// `print_row` prints bare `f.decrypted_name`, even dropping its rich-mode
/// `[trashed]` annotation; `search`'s quiet branch prints bare `m.path`;
/// `trash list`'s quiet branch prints bare `name`). None of those append a
/// state suffix to some rows and not others, so this doesn't either — every
/// line is exactly one whitespace-free token, uniformly script-safe for
/// `xargs`/`mapfile`, not just `awk '{print $1}'`. `is_current` is rich-mode
/// state (it drives `marker_cell`'s `"*"` column in `render_table`); quiet
/// mode's contract is the bare identifying value, nothing else.
fn render_quiet(sessions: &[SessionRow]) -> Vec<String> {
    sessions.iter().map(|s| s.id.clone()).collect()
}

/// Render the full human-mode table for 2+ sessions.
fn render_table(sessions: &[SessionRow]) -> String {
    use crate::{colors, ui};
    use colored::Colorize;

    let headers = ["", "DEVICE", "KIND", "COUNTRY", "LAST ACTIVE"];
    let headers_colored: Vec<String> = headers
        .iter()
        .map(|h| h.custom_color(colors::AMBER).to_string())
        .collect();
    let headers_ref: Vec<&str> = headers_colored.iter().map(|s| s.as_str()).collect();

    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            let last_active = match &s.last_active_at {
                Some(t) => ui::relative_time(t),
                None => "\u{2014}".to_string(), // em dash
            };
            let country = s.country_code.as_deref().unwrap_or("--").to_string();
            vec![
                marker_cell(s.is_current),
                s.device_name.clone(),
                s.device_kind.clone(),
                country,
                last_active,
            ]
        })
        .collect();

    ui::table(&headers_ref, &rows)
}

/// `bb sessions list`.
pub async fn list() -> Result<(), String> {
    use crate::{colors, ui};
    use colored::Colorize;

    let api = ApiClient::from_config();
    api.require_auth()?;

    let body = match api.list_sessions_v2().await {
        Ok(body) => body,
        Err(e) => return Err(classify_list_error(e)),
    };

    if ui::is_json() {
        let raw = extract_sessions_json(&body);
        println!(
            "{}",
            serde_json::to_string_pretty(&raw).unwrap_or_else(|_| "[]".to_string())
        );
        return Ok(());
    }

    let sessions = parse_sessions(&body);

    if ui::is_quiet() {
        // No headers, no summary line, no color — matches `bb ls`/`bb
        // search`'s quiet-mode convention. Runs even for an empty/solo list:
        // quiet mode never prints the human-only friendly sentences below.
        for line in render_quiet(&sessions) {
            println!("{line}");
        }
        return Ok(());
    }

    if sessions.is_empty() {
        // Defensive only — a caller always has at least their own active
        // session server-side; an empty array here means something upstream
        // is broken, not that there truly are zero sessions.
        println!("  {}", "no active sessions".custom_color(colors::INK_DIM));
        return Ok(());
    }

    if should_show_empty_state(&sessions) {
        println!(
            "  {}",
            "only this device is currently signed in".custom_color(colors::INK_DIM)
        );
        return Ok(());
    }

    print!("{}", render_table(&sessions));
    println!();
    println!(
        "  {} active session{}",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    );

    Ok(())
}

/// Resolve `input` against `sessions`' ids: an exact match first, else a
/// unique id-prefix match. Same 0/1/ambiguous contract as
/// `ApiClient::find_file_by_id_prefix` — 0 matches is an error (not `None`)
/// because every caller here is about to act on the result immediately.
fn resolve_session_id(sessions: &[SessionRow], input: &str) -> Result<String, String> {
    if sessions.iter().any(|s| s.id == input) {
        return Ok(input.to_string());
    }
    let matches: Vec<&SessionRow> = sessions.iter().filter(|s| s.id.starts_with(input)).collect();
    match matches.len() {
        0 => Err(format!(
            "no session id starts with '{input}' — run `bb sessions list` to see active sessions"
        )),
        1 => Ok(matches[0].id.clone()),
        n => Err(format!(
            "ambiguous id '{input}' matches {n} sessions — use more characters"
        )),
    }
}

/// The current-session guard for `bb sessions revoke` — see the module doc
/// comment's "Decision — revoking the CURRENT session" for the full
/// reasoning (server allows it; this CLI still refuses in favor of
/// `bb logout`, which also clears local config).
fn guard_not_current(target: &SessionRow) -> Result<(), String> {
    if target.is_current {
        return Err(
            "refusing to revoke the current session — it would delete the server-side session \
             but leave this CLI still holding the now-dead token locally, so the next command \
             would fail with a confusing 401. Run `bb logout` instead; it revokes AND clears \
             local config."
                .to_string(),
        );
    }
    Ok(())
}

/// First 8 characters of a UUID, for compact confirmation lines (mirrors the
/// short-id convention `bb ls` uses elsewhere in this CLI).
fn short_id(id: &str) -> &str {
    if id.len() >= 8 { &id[..8] } else { id }
}

/// Classify a `revoke_session` / `revoke_all_other_sessions` error string.
/// `"not found"` is the live handler's exact (lowercased) 404 body —
/// `ApiError::NotFound` renders as `{"error": "not found"}`
/// (`repos/server/.../error.rs` L533 + L590) — never `"Not Found"` or a
/// `"404: ..."`-prefixed string, since `parse_response` extracts just the
/// `error` field's value with no status-code prefix.
fn classify_revoke_error(e: String) -> String {
    let lower = e.to_lowercase();
    if lower.contains("unauthorized") || e.contains("401") {
        "your session has expired — run `bb login` again".to_string()
    } else if lower.contains("not found") {
        "session not found (already revoked, or never existed)".to_string()
    } else {
        e
    }
}

/// `bb sessions revoke <id-or-prefix>` — see the module doc comment above for
/// the step-up, picker, and current-session deviations/decisions.
pub async fn revoke(id: String) -> Result<(), String> {
    use crate::{colors, ui};
    use colored::Colorize;

    let api = ApiClient::from_config();
    api.require_auth()?;

    let body = match api.list_sessions_v2().await {
        Ok(body) => body,
        Err(e) => return Err(classify_list_error(e)),
    };
    let sessions = parse_sessions(&body);

    let full_id = resolve_session_id(&sessions, &id)?;
    let target = sessions
        .iter()
        .find(|s| s.id == full_id)
        .expect("resolve_session_id only ever returns an id present in `sessions`");

    guard_not_current(target)?;

    let device_name = target.device_name.clone();
    let resp = api.revoke_session(&full_id).await.map_err(classify_revoke_error)?;

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "{}".to_string())
        );
    } else if !ui::is_quiet() {
        println!(
            "  {} revoked session {} ({})",
            "ok".custom_color(colors::GREEN_OK),
            short_id(&full_id).custom_color(colors::INK),
            device_name.custom_color(colors::INK_DIM),
        );
    }
    Ok(())
}

/// Minimal interactive y/N confirmation, mirroring `commands::rm::confirm`
/// (no extra deps; only called on a rich/interactive terminal).
fn confirm_revoke_all() -> Result<bool, String> {
    use std::io::Write;

    use colored::Colorize;

    use crate::colors;

    print!(
        "  {} this signs out every other device — continue? [y/N] ",
        "?".custom_color(colors::AMBER)
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

/// `bb sessions revoke-all-others` — see the module doc comment above for the
/// step-up deviation. Never touches the current session: the live handler's
/// `DELETE ... WHERE user_id = $1 AND token != $2` excludes it server-side,
/// so there is no current-session edge case to guard here (unlike `revoke`).
pub async fn revoke_all_others(yes: bool) -> Result<(), String> {
    use crate::{colors, ui};
    use colored::Colorize;

    let api = ApiClient::from_config();
    api.require_auth()?;

    if !yes && ui::is_rich() && !confirm_revoke_all()? {
        if !ui::is_quiet() {
            println!("  {}", "cancelled".custom_color(colors::INK_DIM));
        }
        return Ok(());
    }

    let resp = api.revoke_all_other_sessions().await.map_err(classify_revoke_error)?;
    let count = resp.get("revoked").and_then(|v| v.as_i64()).unwrap_or(0);

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "{}".to_string())
        );
    } else if !ui::is_quiet() {
        println!(
            "  {} revoked {} session{}",
            "ok".custom_color(colors::GREEN_OK),
            count,
            if count == 1 { "" } else { "s" },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fixture matching the LIVE server's response shape
    /// (`repos/server/beebeeb-api/src/routes/account_activity.rs::list_sessions`).
    fn sessions_fixture() -> Value {
        json!({
            "sessions": [
                {
                    "id": "11111111-1111-1111-1111-111111111111",
                    "device_name": "Chrome 124 on macOS",
                    "device_kind": "web",
                    "country_code": "NL",
                    "last_active_at": "2026-09-23T10:00:00Z",
                    "created_at": "2026-09-01T09:00:00Z",
                    "is_current": true
                },
                {
                    "id": "22222222-2222-2222-2222-222222222222",
                    "device_name": "bb-cli on Linux",
                    "device_kind": "cli",
                    "country_code": null,
                    "last_active_at": null,
                    "created_at": "2026-08-15T09:00:00Z",
                    "is_current": false
                }
            ]
        })
    }

    // ── parse_sessions / SessionRow::from_json ──────────────────────────

    #[test]
    fn parse_sessions_reads_every_field_from_the_live_shape() {
        let rows = parse_sessions(&sessions_fixture());
        assert_eq!(rows.len(), 2, "rows: {rows:?}");

        assert_eq!(rows[0].id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(rows[0].device_name, "Chrome 124 on macOS");
        assert_eq!(rows[0].device_kind, "web");
        assert_eq!(rows[0].country_code.as_deref(), Some("NL"));
        assert_eq!(rows[0].last_active_at.as_deref(), Some("2026-09-23T10:00:00Z"));
        assert_eq!(rows[0].created_at, "2026-09-01T09:00:00Z");
        assert!(rows[0].is_current);

        assert_eq!(rows[1].device_kind, "cli");
        assert_eq!(rows[1].country_code, None, "null country_code must parse to None");
        assert_eq!(rows[1].last_active_at, None, "null last_active_at must parse to None");
        assert!(!rows[1].is_current);
    }

    #[test]
    fn parse_sessions_defaults_missing_device_fields_like_the_server_does() {
        let body = json!({ "sessions": [{
            "id": "33333333-3333-3333-3333-333333333333",
            "created_at": "2026-09-01T09:00:00Z"
        }] });
        let rows = parse_sessions(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].device_name, "Unknown device");
        assert_eq!(rows[0].device_kind, "web");
        assert!(!rows[0].is_current, "missing is_current must fail closed to false");
    }

    #[test]
    fn parse_sessions_skips_a_row_missing_the_required_id() {
        let body = json!({ "sessions": [
            { "created_at": "2026-09-01T09:00:00Z" },
            { "id": "44444444-4444-4444-4444-444444444444", "created_at": "2026-09-01T09:00:00Z" }
        ] });
        let rows = parse_sessions(&body);
        assert_eq!(
            rows.len(),
            1,
            "the malformed row must be dropped, not panic or fabricate an id"
        );
        assert_eq!(rows[0].id, "44444444-4444-4444-4444-444444444444");
    }

    #[test]
    fn parse_sessions_on_missing_sessions_key_returns_empty() {
        assert_eq!(parse_sessions(&json!({})), vec![]);
    }

    // ── render_table (current-session marker) ───────────────────────────

    #[test]
    fn render_table_marks_exactly_the_current_session() {
        let rows = parse_sessions(&sessions_fixture());
        let out = render_table(&rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows: {lines:?}");

        // Row order follows server order (fixture: current row first).
        assert!(
            lines[1].contains('*'),
            "current-session row must carry the * marker: {:?}",
            lines[1]
        );
        assert!(
            !lines[2].contains('*'),
            "non-current row must NOT carry the * marker: {:?}",
            lines[2]
        );
        assert!(lines[1].contains("Chrome 124 on macOS"), "line was: {:?}", lines[1]);
        assert!(lines[2].contains("bb-cli on Linux"), "line was: {:?}", lines[2]);
    }

    #[test]
    fn render_table_shows_dashes_for_null_country_and_last_active() {
        let rows = parse_sessions(&sessions_fixture());
        let out = render_table(&rows);
        // rows[1] has null country_code and null last_active_at.
        assert!(out.contains("--"), "missing country placeholder in:\n{out}");
        assert!(
            out.contains('\u{2014}'),
            "missing last-active em-dash placeholder in:\n{out}"
        );
    }

    #[test]
    fn render_table_uses_relative_time_for_a_present_last_active_at() {
        let rows = parse_sessions(&sessions_fixture());
        let out = render_table(&rows);
        // "2026-09-23T10:00:00Z" is far in the past/future relative to "now"
        // in the test run, but relative_time NEVER echoes back the raw
        // ISO string verbatim for a parseable timestamp — assert that.
        assert!(
            !out.contains("2026-09-23T10:00:00Z"),
            "raw ISO timestamp leaked into the table instead of a relative string:\n{out}"
        );
    }

    // ── --quiet mode ─────────────────────────────────────────────────────

    #[test]
    fn render_quiet_emits_one_bare_id_per_session_with_no_current_marker() {
        let rows = parse_sessions(&sessions_fixture());
        let lines = render_quiet(&rows);
        assert_eq!(lines.len(), 2, "one line per session, no header, no summary: {lines:?}");
        assert_eq!(
            lines[0], "11111111-1111-1111-1111-111111111111",
            "current session's id must be bare — no marker, quiet mode strips all decoration"
        );
        assert_eq!(
            lines[1], "22222222-2222-2222-2222-222222222222",
            "non-current session must be the bare id too"
        );
    }

    #[test]
    fn render_quiet_lines_are_single_whitespace_free_tokens() {
        let rows = parse_sessions(&sessions_fixture());
        for line in render_quiet(&rows) {
            assert!(
                !line.contains(char::is_whitespace),
                "quiet line must be one token, safe for xargs/mapfile, not just awk '{{print $1}}': {line:?}"
            );
        }
    }

    #[test]
    fn render_quiet_contains_no_ansi_escapes() {
        let rows = parse_sessions(&sessions_fixture());
        let joined = render_quiet(&rows).join("\n");
        assert!(
            !joined.contains('\x1b'),
            "quiet output must never carry color codes, current mode or not: {joined:?}"
        );
    }

    #[test]
    fn render_quiet_on_empty_sessions_emits_no_lines() {
        assert_eq!(render_quiet(&[]), Vec::<String>::new());
    }

    // ── JSON passthrough ─────────────────────────────────────────────────

    #[test]
    fn json_mode_extracts_the_raw_sessions_array_unmodified() {
        // Exercises the SAME function `list()` calls for `--json`, including
        // a field `SessionRow` doesn't know about, to prove this is a
        // passthrough and not a reconstruction.
        let body = json!({ "sessions": [
            { "id": "1", "created_at": "2026-01-01T00:00:00Z", "future_field": "kept" }
        ] });
        let raw = extract_sessions_json(&body);
        assert_eq!(raw, body["sessions"]);
        assert_eq!(
            raw[0]["future_field"], "kept",
            "a passthrough must not drop unknown fields"
        );
    }

    #[test]
    fn json_mode_on_missing_sessions_key_falls_back_to_empty_array() {
        assert_eq!(extract_sessions_json(&json!({})), json!([]));
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

    #[test]
    fn empty_state_line_fires_only_when_the_lone_session_is_the_current_one() {
        // Exercises the SAME predicate `list()` branches on. A single
        // non-current session (should never happen live, but must not
        // silently claim "only this device") must NOT take this branch.
        let solo_current = vec![SessionRow {
            id: "1".into(),
            device_name: "d".into(),
            device_kind: "web".into(),
            country_code: None,
            last_active_at: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            is_current: true,
        }];
        assert!(should_show_empty_state(&solo_current));

        let solo_not_current = vec![SessionRow {
            id: "1".into(),
            device_name: "d".into(),
            device_kind: "web".into(),
            country_code: None,
            last_active_at: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            is_current: false,
        }];
        assert!(!should_show_empty_state(&solo_not_current));

        let two_sessions = vec![
            SessionRow {
                id: "1".into(),
                device_name: "d".into(),
                device_kind: "web".into(),
                country_code: None,
                last_active_at: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                is_current: true,
            },
            SessionRow {
                id: "2".into(),
                device_name: "d2".into(),
                device_kind: "cli".into(),
                country_code: None,
                last_active_at: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                is_current: false,
            },
        ];
        assert!(
            !should_show_empty_state(&two_sessions),
            "2+ sessions must always render the table"
        );
    }

    // ── task 0481: resolve_session_id / guard_not_current / classify_revoke_error ──

    fn two_row_fixture() -> Vec<SessionRow> {
        vec![
            SessionRow {
                id: "11111111-1111-1111-1111-111111111111".into(),
                device_name: "Chrome 124 on macOS".into(),
                device_kind: "web".into(),
                country_code: Some("NL".into()),
                last_active_at: None,
                created_at: "2026-09-01T09:00:00Z".into(),
                is_current: true,
            },
            SessionRow {
                id: "22222222-2222-2222-2222-222222222222".into(),
                device_name: "bb-cli on Linux".into(),
                device_kind: "cli".into(),
                country_code: None,
                last_active_at: None,
                created_at: "2026-08-15T09:00:00Z".into(),
                is_current: false,
            },
        ]
    }

    #[test]
    fn resolve_session_id_matches_a_full_id_exactly() {
        let sessions = two_row_fixture();
        assert_eq!(
            resolve_session_id(&sessions, "22222222-2222-2222-2222-222222222222").unwrap(),
            "22222222-2222-2222-2222-222222222222"
        );
    }

    #[test]
    fn resolve_session_id_matches_a_unique_prefix() {
        let sessions = two_row_fixture();
        assert_eq!(
            resolve_session_id(&sessions, "2222").unwrap(),
            "22222222-2222-2222-2222-222222222222"
        );
    }

    #[test]
    fn resolve_session_id_errors_on_zero_matches() {
        let sessions = two_row_fixture();
        let err = resolve_session_id(&sessions, "deadbeef").unwrap_err();
        assert!(err.contains("no session id starts with 'deadbeef'"), "err was: {err:?}");
    }

    #[test]
    fn resolve_session_id_errors_on_ambiguous_prefix() {
        // Both fixture ids start with "1" only via chance of a shared leading
        // digit — construct a genuinely ambiguous pair explicitly.
        let sessions = vec![
            SessionRow {
                id: "aaaa1111-0000-0000-0000-000000000000".into(),
                device_name: "d1".into(),
                device_kind: "cli".into(),
                country_code: None,
                last_active_at: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                is_current: false,
            },
            SessionRow {
                id: "aaaa2222-0000-0000-0000-000000000000".into(),
                device_name: "d2".into(),
                device_kind: "cli".into(),
                country_code: None,
                last_active_at: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                is_current: false,
            },
        ];
        let err = resolve_session_id(&sessions, "aaaa").unwrap_err();
        assert!(
            err.contains("ambiguous id 'aaaa' matches 2 sessions"),
            "err was: {err:?}"
        );
    }

    #[test]
    fn guard_not_current_refuses_the_current_session() {
        let sessions = two_row_fixture();
        let current = sessions.iter().find(|s| s.is_current).unwrap();
        let err = guard_not_current(current).unwrap_err();
        assert!(
            err.contains("refusing to revoke the current session"),
            "err was: {err:?}"
        );
        assert!(err.contains("bb logout"), "err must point at `bb logout`: {err:?}");
    }

    #[test]
    fn guard_not_current_allows_a_non_current_session() {
        let sessions = two_row_fixture();
        let other = sessions.iter().find(|s| !s.is_current).unwrap();
        assert!(guard_not_current(other).is_ok());
    }

    #[test]
    fn short_id_takes_the_first_eight_characters() {
        assert_eq!(short_id("11111111-1111-1111-1111-111111111111"), "11111111");
    }

    #[test]
    fn short_id_returns_the_whole_string_when_shorter_than_eight() {
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn classify_revoke_error_maps_unauthorized_to_a_login_hint() {
        assert_eq!(
            classify_revoke_error("unauthorized".to_string()),
            "your session has expired — run `bb login` again"
        );
        assert_eq!(
            classify_revoke_error("401 Unauthorized: unauthorized".to_string()),
            "your session has expired — run `bb login` again"
        );
    }

    #[test]
    fn classify_revoke_error_maps_the_live_lowercase_not_found_body() {
        // The live 404 body is exactly {"error": "not found"} (error.rs
        // ApiError::NotFound → "not found", L533) — NOT "Not Found" and NOT
        // "404: ..." (parse_response strips the status prefix whenever the
        // body has an `error` field, which it always does here).
        assert_eq!(
            classify_revoke_error("not found".to_string()),
            "session not found (already revoked, or never existed)"
        );
    }

    #[test]
    fn classify_revoke_error_passes_through_unrelated_errors() {
        assert_eq!(classify_revoke_error("network failed".to_string()), "network failed");
    }
}
