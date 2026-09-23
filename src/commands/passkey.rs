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
//! **Scope (eng-0482) — `list` only (plan Task 14).** `add` (Task 15) opens
//! a browser for WebAuthn registration — the CLI process itself can never
//! perform a WebAuthn ceremony — and `remove` (Task 16) needs the step-up /
//! id-resolution treatment `bb sessions revoke` got in task 0481. Neither
//! subcommand is wired into the clap tree here; only `PasskeyCmd::List`
//! exists, so `bb passkey add`/`bb passkey remove` do not appear in
//! `bb passkey --help` and are not silent stubs that print a "not yet
//! implemented" error — they simply aren't commands yet.

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
}
