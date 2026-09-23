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
//! **Scope (eng-0480):** this task is `list` ONLY (plan Task 12). `revoke` /
//! `revoke-all-others` (plan Task 13 — `DELETE /account/sessions/{id}` and
//! `POST /account/sessions/revoke-all-others`, both already live server-side)
//! are deliberately NOT wired here and no stub variant was added to the clap
//! `SessionsCmd` enum — same reasoning as the eng-0479 `bb 2fa verify` removal:
//! a visible command with no implementation behind it is a dead end for
//! whoever runs `bb sessions --help`.

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
}
