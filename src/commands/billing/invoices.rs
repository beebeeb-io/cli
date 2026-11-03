//! `bb billing invoices`: list the account's invoices, and `--open <id>` to
//! download one PDF through the authenticated client and open the saved file.
//!
//! Owns: `invoices`, `open_invoice_pdf`, the row rendering and status colours,
//! and the private-temp-file write.
//! Must not: read plan or usage data (see `billing::show`).
//! Tests: `mod tests` at the bottom of this file covers the invoice rows and
//! status colours, the shared price/count formatters and the plan-line
//! decisions.

use super::*;

/// `bb billing invoices [--open <id>]` — list VAT-compliant invoices, or
/// download + open one as a PDF. See the module doc comment above
/// ("`invoices` (task 0487, plan Task 19)") for the field-shape deviations.
pub async fn invoices(open: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let body = api.get_billing_invoices().await?;
    let invoices = body
        .get("invoices")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if let Some(input) = open {
        return open_invoice_pdf(&api, &invoices, &input).await;
    }

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    if ui::is_quiet() {
        // No headers, no summary line, no color — same convention as
        // `passkey list`/`sessions list`'s quiet branch: one bare, stable
        // field per line, safe for xargs/mapfile. The invoice id is what
        // `--open <id>` consumes, so a quiet-mode pipeline
        // (`bb --quiet billing invoices | xargs -n1 bb billing invoices
        // --open`) composes directly (Codex review, PR #27).
        for line in render_quiet_invoices(&invoices) {
            println!("{line}");
        }
        return Ok(());
    }

    if invoices.is_empty() {
        println!("  {}", "no invoices yet".custom_color(crate::colors::INK_DIM));
        return Ok(());
    }

    let headers = ["ID", "NUMBER", "DATE", "AMOUNT", "STATUS", "PERIOD"];
    let mut rows: Vec<Vec<String>> = Vec::new();
    for inv in &invoices {
        rows.push(invoice_row(inv));
    }

    let headers_colored: Vec<String> = headers
        .iter()
        .map(|h| h.custom_color(crate::colors::AMBER).to_string())
        .collect();
    let headers_ref: Vec<&str> = headers_colored.iter().map(|s| s.as_str()).collect();
    print!("{}", ui::table(&headers_ref, &rows));
    println!();
    println!(
        "  {} invoice{} \u{00b7} download a PDF: {}",
        invoices.len(),
        if invoices.len() == 1 { "" } else { "s" },
        "bb billing invoices --open <id>".custom_color(crate::colors::INK_DIM),
    );
    Ok(())
}

/// Resolve `input` to a full invoice id, download its PDF through the
/// authenticated client, save it locally, and (in an interactive session)
/// open it with the OS default PDF viewer. See the module doc comment for why
/// this can't just be a browser URL open.
pub(super) async fn open_invoice_pdf(api: &ApiClient, invoices: &[Value], input: &str) -> Result<(), String> {
    let resolved_id = resolve_invoice_id(invoices, input)?;
    let number = invoices
        .iter()
        .find(|inv| inv.get("id").and_then(|v| v.as_str()) == Some(resolved_id.as_str()))
        .and_then(|inv| inv.get("number").and_then(|v| v.as_str()))
        .unwrap_or(&resolved_id)
        .to_string();

    let pdf_bytes = api.download_invoice_pdf(&resolved_id).await?;
    let path = write_private_temp_pdf(&number, &pdf_bytes)?;

    if !crate::env_detect::is_headless() {
        let _ = open::that(&path);
    }

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": resolved_id,
                "number": number,
                "saved_to": path.to_string_lossy(),
            }))
            .unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!("Invoice saved: {}", path.display());
    }
    Ok(())
}

/// `--quiet` rendering: one bare invoice id per line, no header, no summary,
/// no color. Mirrors `commands::passkey::render_quiet` /
/// `commands::sessions::render_quiet` — the repo's established
/// quiet-list-mode convention (Codex review, PR #27).
pub(super) fn render_quiet_invoices(invoices: &[Value]) -> Vec<String> {
    invoices
        .iter()
        .map(|inv| inv.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string())
        .collect()
}

/// Build one invoice table row from a raw `GET /billing/invoices` entry.
/// Pure — see `commands::billing::tests` for coverage of every field mapping.
pub(super) fn invoice_row(inv: &Value) -> Vec<String> {
    let id = inv.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let number = inv.get("number").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let date = inv
        .get("invoice_date")
        .and_then(|v| v.as_str())
        .unwrap_or("\u{2014}")
        .to_string();
    let amount_cents = inv.get("amount_gross_cents").and_then(|v| v.as_i64()).unwrap_or(0);
    let currency = inv
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("eur")
        .to_uppercase();
    let status = inv.get("status").and_then(|v| v.as_str()).unwrap_or("--").to_string();
    let period_start = inv.get("period_start").and_then(|v| v.as_str()).unwrap_or("");
    let period_end = inv.get("period_end").and_then(|v| v.as_str()).unwrap_or("");
    let period = if period_start.is_empty() || period_end.is_empty() {
        "\u{2014}".to_string()
    } else {
        format!("{period_start} \u{2013} {period_end}")
    };
    let amount_label = format_invoice_amount(amount_cents, &currency);
    let status_color = invoice_status_color(&status);
    vec![
        short_id(id).custom_color(crate::colors::INK_DIM).to_string(),
        number.custom_color(crate::colors::INK).to_string(),
        date,
        amount_label,
        status.custom_color(status_color).to_string(),
        period,
    ]
}

/// Color for an invoice's `status` cell. Live vocabulary is
/// `'draft' | 'issued' | 'void'` (the `invoices` table CHECK constraint,
/// `beebeeb-api/src/db.rs`) — NOT the `'paid'`/`'open'` pair the
/// plan's Task 19 pseudocode assumed (eng-0487 deviation, caught in manual
/// verification: every real seeded row rendered RED_ERR because none of them
/// are `'paid'`). Every row the server inserts gets `status = 'issued'`
/// directly (`invoice_gen.rs`, only ever from an already-PAID Mollie
/// payment — `'issued'` IS the normal/good state here); `'void'` is the
/// correction state (task 0917 credit notes); `'draft'` is schema-permitted
/// but never currently written by any server code path.
pub(super) fn invoice_status_color(status: &str) -> colored::CustomColor {
    match status {
        "issued" => crate::colors::GREEN_OK,
        "draft" => crate::colors::AMBER,
        "void" => crate::colors::RED_ERR,
        _ => crate::colors::AMBER,
    }
}

/// Format an integer gross-cents amount + ISO currency code as a display
/// string, e.g. `(1209, "EUR")` -> `"€12.09"`. EUR gets the symbol prefix
/// (every live invoice today is EUR — `beebeeb-api/src/routes/billing.rs`
/// invoices are issued from EU billing profiles); any other code falls back
/// to `"<CODE> X.XX"` rather than guessing a symbol.
pub(super) fn format_invoice_amount(cents: i64, currency: &str) -> String {
    let amount = format!("{:.2}", cents as f64 / 100.0);
    if currency == "EUR" {
        format!("\u{20ac}{amount}")
    } else {
        format!("{currency} {amount}")
    }
}

/// Resolve a full invoice id or a unique id prefix to a full id. Mirrors
/// `commands::passkey::resolve_passkey_id` / `commands::sessions::resolve_session_id`
/// — the established "full id or unique prefix" convention in this repo.
pub(super) fn resolve_invoice_id(invoices: &[Value], input: &str) -> Result<String, String> {
    fn invoice_id(inv: &Value) -> &str {
        inv.get("id").and_then(|v| v.as_str()).unwrap_or("")
    }
    if invoices.iter().any(|inv| invoice_id(inv) == input) {
        return Ok(input.to_string());
    }
    let matches: Vec<&str> = invoices
        .iter()
        .map(invoice_id)
        .filter(|id| id.starts_with(input))
        .collect();
    match matches.len() {
        0 => Err(format!(
            "no invoice id starts with '{input}' — run `bb billing invoices` to see your invoices"
        )),
        1 => Ok(matches[0].to_string()),
        n => Err(format!(
            "ambiguous id '{input}' matches {n} invoices — use more characters"
        )),
    }
}

/// Shorten a full id to its first 8 characters for table display. Mirrors
/// `commands::passkey::short_id` / `commands::sessions::short_id`.
pub(super) fn short_id(id: &str) -> &str {
    if id.len() >= 8 { &id[..8] } else { id }
}

/// Strip characters that are awkward in a filename (kept simple: invoice
/// numbers are server-generated, short, ASCII — this is a defensive
/// fallback, not a general sanitizer).
pub(super) fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Write `bytes` to a fresh, private, collision-safe file under the OS temp
/// dir and return its path.
///
/// **Security (Codex review, PR #27 P1).** The OS temp dir is commonly
/// world-writable and shared across accounts (`/tmp` on most Unix hosts). The
/// original version wrote to a PREDICTABLE `<invoice number>.pdf` path with a
/// plain `std::fs::write`, which (a) follows an existing symlink at that path
/// instead of refusing, letting another local account pre-plant a symlink to
/// truncate a victim-writable file the CLI process can write to, and (b)
/// creates the file at the process umask — typically world-readable `0644` —
/// letting another local account read the persisted VAT invoice PDF.
///
/// Fixed by generating an UNPREDICTABLE filename (a random UUID suffix, so
/// there is no fixed path to pre-plant a symlink at) and opening it with
/// `create_new(true)` (`O_CREAT | O_EXCL` — refuses if anything, including a
/// symlink, already exists at the path; never follows) and, on Unix, mode
/// `0o600` (owner read/write only, applied by the kernel through the umask —
/// `0o600 & !umask` stays `0o600` under any typical `022`/`077` umask since
/// those only ever clear group/other bits that are already zero).
pub(super) fn write_private_temp_pdf(stem: &str, bytes: &[u8]) -> Result<std::path::PathBuf, String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let unique = uuid::Uuid::new_v4().simple().to_string();
    let filename = format!("{}-{}.pdf", sanitize_filename(stem), &unique[..8]);
    let path = std::env::temp_dir().join(filename);

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let mut file = opts
        .open(&path)
        .map_err(|e| format!("failed to create invoice pdf file at {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("failed to write invoice pdf: {e}"))?;
    Ok(path)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── aggregate_by_region (eng-0486, by country since task 2050) ─────────

    fn at(region: &str, country: &str) -> (String, String) {
        (region.to_string(), country.to_string())
    }

    #[test]
    fn aggregate_by_region_sums_bytes_per_country_and_skips_folders() {
        let files = vec![
            json!({
                "is_folder": false, "size_bytes": 100,
                "storage_location": { "region": "europe", "country": "Germany", "country_code": "DE" },
            }),
            json!({
                "is_folder": false, "size_bytes": 250,
                "storage_location": { "region": "europe", "country": "Germany", "country_code": "DE" },
            }),
            json!({
                "is_folder": true, "size_bytes": 999_999,
                "storage_location": { "region": "europe", "country": "Germany", "country_code": "DE" },
            }),
        ];
        let (by_region, file_count) = aggregate_by_region(&files);
        assert_eq!(file_count, 2, "the folder row must not be counted as a file");
        assert_eq!(
            by_region.get(&at("europe", "Germany")),
            Some(&350),
            "folder bytes (999_999) must NOT be summed into the region total: {by_region:?}"
        );
        assert_eq!(by_region.len(), 1);
    }

    #[test]
    fn aggregate_by_region_splits_countries_but_merges_two_datacenters_of_one_country() {
        // Two German files whose rows still carry different (deprecated) cities
        // are ONE group; a Finnish file is another.
        let files = vec![
            json!({
                "is_folder": false, "size_bytes": 10,
                "storage_location": { "region": "europe", "country": "Germany", "country_code": "DE", "city": "Falkenstein" },
            }),
            json!({
                "is_folder": false, "size_bytes": 5,
                "storage_location": { "region": "europe", "country": "Germany", "country_code": "DE", "city": "Nuremberg" },
            }),
            json!({
                "is_folder": false, "size_bytes": 20,
                "storage_location": { "region": "europe", "country": "Finland", "country_code": "FI", "city": "Helsinki" },
            }),
        ];
        let (by_region, file_count) = aggregate_by_region(&files);
        assert_eq!(file_count, 3);
        assert_eq!(
            by_region.len(),
            2,
            "one group per country, never per city: {by_region:?}"
        );
        assert_eq!(by_region.get(&at("europe", "Germany")), Some(&15));
        assert_eq!(by_region.get(&at("europe", "Finland")), Some(&20));
    }

    #[test]
    fn aggregate_by_region_falls_back_to_europe_never_a_city() {
        // No storage_location at all (defensive), and a location with only the
        // deprecated city (an older server): both read "Europe", not the city.
        let files = vec![
            json!({ "is_folder": false, "size_bytes": 5 }),
            json!({
                "is_folder": false, "size_bytes": 7,
                "storage_location": { "region": "europe", "city": "Falkenstein" },
            }),
        ];
        let (by_region, file_count) = aggregate_by_region(&files);
        assert_eq!(file_count, 2);
        assert_eq!(
            by_region.get(&at("europe", "Europe")),
            Some(&12),
            "missing country must fall back to Europe, not be dropped or name a city: {by_region:?}"
        );
        assert_eq!(by_region.len(), 1);
    }

    #[test]
    fn aggregate_by_region_empty_input_is_empty_output() {
        let (by_region, file_count) = aggregate_by_region(&[]);
        assert_eq!(file_count, 0);
        assert!(by_region.is_empty());
    }

    #[test]
    fn usage_json_rows_keep_region_and_city_keys_but_carry_the_country() {
        let mut m = BTreeMap::new();
        m.insert(at("europe", "Germany"), 350i64);
        let rows = region_rows_json(&m);
        assert_eq!(
            rows,
            json!([{ "region": "europe", "country": "Germany", "city": "Germany", "used_bytes": 350 }]),
            "`city` is a deprecated alias that carries the country name"
        );
    }

    // ── invoices (eng-0487) ──────────────────────────────────────────────────

    fn sample_invoice() -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "number": "BB-2026-0001",
            "invoice_date": "2026-01-15",
            "period_start": "2026-01-01",
            "period_end": "2026-01-31",
            "currency": "eur",
            "amount_net_cents": 999,
            "vat_rate_bps": 2100,
            "vat_amount_cents": 210,
            "amount_gross_cents": 1209,
            "vat_treatment": "domestic",
            "description": "Pro plan",
            "status": "issued",
            "pdf_url": "/api/v1/billing/invoices/11111111-1111-1111-1111-111111111111/pdf",
        })
    }

    #[test]
    fn invoice_row_reads_the_live_field_names_not_the_plan_pseudocode_names() {
        // Regression guard for eng-0487: the plan's Task 19 pseudocode reads
        // `amount_paid` and epoch-second period_start/period_end — the live
        // server has neither. If invoice_row() regresses back to those field
        // names, every cell falls back to its default and this test catches it.
        let row = invoice_row(&sample_invoice());
        assert_eq!(row.len(), 6, "ID, NUMBER, DATE, AMOUNT, STATUS, PERIOD: {row:?}");
        assert!(row[0].contains("11111111"), "ID column (short id): {row:?}");
        assert!(row[1].contains("BB-2026-0001"), "NUMBER column: {row:?}");
        assert!(row[2].contains("2026-01-15"), "DATE column (invoice_date): {row:?}");
        assert!(
            row[3].contains("12.09"),
            "AMOUNT column must read amount_gross_cents (1209 -> 12.09), not amount_paid: {row:?}"
        );
        assert!(row[4].contains("issued"), "STATUS column: {row:?}");
        assert!(
            row[5].contains("2026-01-01") && row[5].contains("2026-01-31"),
            "PERIOD column must read the ISO date strings, not parse them as epoch seconds: {row:?}"
        );
    }

    #[test]
    fn invoice_row_period_dash_when_either_bound_is_missing() {
        let mut inv = sample_invoice();
        inv.as_object_mut().unwrap().remove("period_end");
        let row = invoice_row(&inv);
        assert_eq!(
            row[5], "\u{2014}",
            "missing period_end must render as an em dash, not a half-range: {row:?}"
        );
    }

    #[test]
    fn invoice_status_color_maps_the_live_vocabulary() {
        // The invoices table CHECK constraint is 'draft' | 'issued' | 'void'.
        // 'issued' is the normal/good state (every real row is inserted with
        // it) — it must be green, not fall through to the red catch-all the
        // way the plan's 'paid'/'open' match arms would leave it.
        assert_eq!(invoice_status_color("issued"), crate::colors::GREEN_OK);
        assert_eq!(invoice_status_color("draft"), crate::colors::AMBER);
        assert_eq!(invoice_status_color("void"), crate::colors::RED_ERR);
        assert_eq!(invoice_status_color("something-unexpected"), crate::colors::AMBER);
        // The `'paid'`/`'open'` statuses are NOT in the live vocabulary and
        // must not be specially colored — they fall through to the same
        // amber-for-unknown default as any other unrecognized string.
        assert_eq!(invoice_status_color("paid"), crate::colors::AMBER);
        assert_eq!(invoice_status_color("open"), crate::colors::AMBER);
    }

    #[test]
    fn format_invoice_amount_eur_gets_the_symbol_prefix() {
        assert_eq!(format_invoice_amount(1209, "EUR"), "\u{20ac}12.09");
        assert_eq!(format_invoice_amount(0, "EUR"), "\u{20ac}0.00");
    }

    #[test]
    fn format_invoice_amount_non_eur_falls_back_to_code_prefix() {
        assert_eq!(format_invoice_amount(500, "USD"), "USD 5.00");
    }

    #[test]
    fn resolve_invoice_id_matches_a_full_id_exactly() {
        let invoices = vec![sample_invoice()];
        assert_eq!(
            resolve_invoice_id(&invoices, "11111111-1111-1111-1111-111111111111").unwrap(),
            "11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn resolve_invoice_id_matches_a_unique_prefix() {
        let invoices = vec![sample_invoice()];
        assert_eq!(
            resolve_invoice_id(&invoices, "1111").unwrap(),
            "11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn resolve_invoice_id_rejects_an_ambiguous_prefix() {
        let invoices = vec![
            sample_invoice(),
            json!({ "id": "11112222-2222-2222-2222-222222222222", "number": "BB-2026-0002" }),
        ];
        let err = resolve_invoice_id(&invoices, "1111").unwrap_err();
        assert!(err.contains("ambiguous"), "error was: {err}");
    }

    #[test]
    fn resolve_invoice_id_rejects_an_unknown_prefix() {
        let invoices = vec![sample_invoice()];
        let err = resolve_invoice_id(&invoices, "zzzz").unwrap_err();
        assert!(err.contains("no invoice id starts with"), "error was: {err}");
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
    fn sanitize_filename_replaces_non_ascii_alnum_characters() {
        assert_eq!(sanitize_filename("BB-2026_0001"), "BB-2026_0001");
        assert_eq!(sanitize_filename("BB/2026 0001"), "BB_2026_0001");
    }

    // ── render_quiet_invoices / write_private_temp_pdf (Codex review, PR #27) ──

    #[test]
    fn render_quiet_invoices_emits_one_bare_id_per_line() {
        let invoices = vec![
            sample_invoice(),
            json!({ "id": "22222222-2222-2222-2222-222222222222", "number": "BB-2026-0002" }),
        ];
        let lines = render_quiet_invoices(&invoices);
        assert_eq!(lines.len(), 2, "one line per invoice, no header, no summary: {lines:?}");
        assert_eq!(lines[0], "11111111-1111-1111-1111-111111111111");
        assert_eq!(lines[1], "22222222-2222-2222-2222-222222222222");
    }

    #[test]
    fn render_quiet_invoices_lines_are_single_whitespace_free_tokens() {
        let invoices = vec![sample_invoice()];
        for line in render_quiet_invoices(&invoices) {
            assert!(
                !line.contains(char::is_whitespace),
                "quiet line must be one token, safe for xargs/mapfile: {line:?}"
            );
        }
    }

    #[test]
    fn render_quiet_invoices_on_empty_list_emits_no_lines() {
        assert_eq!(render_quiet_invoices(&[]), Vec::<String>::new());
    }

    #[test]
    fn write_private_temp_pdf_writes_the_exact_bytes_and_returns_a_pdf_path() {
        let path = write_private_temp_pdf("BB-2026-0001", b"%PDF-1.4 fake").unwrap();
        assert!(
            path.extension().and_then(|e| e.to_str()) == Some("pdf"),
            "path was: {path:?}"
        );
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, b"%PDF-1.4 fake");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_private_temp_pdf_never_collides_across_repeated_calls_for_the_same_invoice() {
        // The predictable-path bug this fixes (P1, PR #27) meant calling
        // `--open` twice for the SAME invoice number wrote to the identical
        // path each time. Two calls with the same stem must now produce two
        // DIFFERENT paths (the random suffix), not a collision/overwrite.
        let path1 = write_private_temp_pdf("BB-2026-0001", b"one").unwrap();
        let path2 = write_private_temp_pdf("BB-2026-0001", b"two").unwrap();
        assert_ne!(path1, path2, "repeated calls for the same invoice must not collide");
        assert_eq!(std::fs::read(&path1).unwrap(), b"one");
        assert_eq!(std::fs::read(&path2).unwrap(), b"two");
        let _ = std::fs::remove_file(&path1);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    #[cfg(unix)]
    fn write_private_temp_pdf_is_owner_only_mode_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let path = write_private_temp_pdf("BB-2026-0001", b"x").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "invoice pdf must be owner-only (0600), was {:o} — world/group-readable on a shared temp dir leaks the VAT invoice",
            mode & 0o777
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[cfg(unix)]
    fn write_private_temp_pdf_refuses_to_follow_an_existing_symlink_at_the_target_path() {
        // Simulate the attack this fixes: pre-plant a symlink at the exact
        // path `write_private_temp_pdf` would use, pointing at a file the
        // "victim" (this test) does NOT want truncated. create_new(true)
        // must refuse (O_EXCL semantics) rather than follow the symlink.
        use std::os::unix::fs::symlink;

        let victim = std::env::temp_dir().join(format!("bb-0487-victim-{}.txt", uuid::Uuid::new_v4().simple()));
        std::fs::write(&victim, b"do not touch").unwrap();

        // Reproduce the exact filename write_private_temp_pdf would compute
        // for a fixed, attacker-guessable stem + a KNOWN random suffix by
        // pre-computing it is impossible (that's the whole point of the
        // fix) — so instead this test proves the underlying OS-level
        // mechanism directly: create_new(true) against a path that is
        // ALREADY a symlink must error, never follow it.
        let trap_path = std::env::temp_dir().join(format!("bb-0487-trap-{}.pdf", uuid::Uuid::new_v4().simple()));
        symlink(&victim, &trap_path).unwrap();

        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        let result = opts.open(&trap_path);
        assert!(
            result.is_err(),
            "create_new(true) must refuse an existing symlink, not follow it and overwrite the victim file"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"do not touch",
            "the victim file must be untouched"
        );

        let _ = std::fs::remove_file(&trap_path);
        let _ = std::fs::remove_file(&victim);
    }

    #[test]
    fn region_human_never_names_a_place() {
        // The live server's no-subscription-row branch
        // (beebeeb-api/src/routes/billing.rs::subscription) returns the
        // literal string "falkenstein" for every fresh free-tier account.
        assert_eq!(region_human("falkenstein"), "Europe");
        assert_eq!(region_human("some-other-billing-label"), "Europe");
        assert_eq!(region_human("europe"), "Europe");
        assert_eq!(region_human("eu"), "Europe");
    }

    // ── plan_line_decision (task 1547 finding 1) ────────────────────────────

    #[test]
    fn plan_line_decision_trialing_shows_no_price_and_a_trial_end_date_not_a_renews_line() {
        let decision = plan_line_decision(
            "trialing",
            Some("€10.99 / month"), // must be IGNORED — a trial has no card on file
            Some("2026-10-09T00:00:00Z"),
            Some("2026-10-09T00:00:00Z"), // == trial_ends_at per trial.rs:249 — must not print as "Renews"
            false,
        );
        assert_eq!(
            decision.price, None,
            "trialing must never show a price — no card is on file and nothing will be charged"
        );
        let (label, value) = decision.sub_line.expect("trialing must show a sub-line");
        assert_eq!(label, "Trial:    ");
        assert!(
            value.contains("October 9, 2026") && value.contains("no card on file"),
            "trial line must name the trial end date and state no card is on file: {value:?}"
        );
    }

    #[test]
    fn plan_line_decision_trialing_with_missing_trial_ends_at_still_says_no_card_on_file() {
        let decision = plan_line_decision("trialing", Some("€10.99 / month"), None, None, false);
        assert_eq!(decision.price, None);
        let (_, value) = decision
            .sub_line
            .expect("trialing must show a sub-line even with no trial_ends_at");
        assert!(value.contains("no card on file"), "value was: {value:?}");
    }

    #[test]
    fn plan_line_decision_free_shows_neither_price_nor_sub_line() {
        let decision = plan_line_decision("active", None, None, Some("2026-10-09T00:00:00Z"), true);
        assert_eq!(decision.price, None);
        assert_eq!(decision.sub_line, None, "Free must never show a Renews line either");
    }

    #[test]
    fn plan_line_decision_active_paid_shows_price_and_renews_line() {
        let decision = plan_line_decision(
            "active",
            Some("€10.99 / month"),
            None,
            Some("2026-10-09T00:00:00Z"),
            false,
        );
        assert_eq!(decision.price.as_deref(), Some("€10.99 / month"));
        let (label, value) = decision
            .sub_line
            .expect("active paid plan with a period end must show Renews");
        assert_eq!(label, "Renews:   ");
        assert!(value.contains("October 9, 2026"), "value was: {value:?}");
    }

    #[test]
    fn plan_line_decision_active_paid_with_no_period_end_shows_no_sub_line() {
        let decision = plan_line_decision("active", Some("€10.99 / month"), None, None, false);
        assert_eq!(decision.price.as_deref(), Some("€10.99 / month"));
        assert_eq!(decision.sub_line, None);
    }

    #[test]
    fn format_price_renders_cents() {
        assert_eq!(format_price(399, "monthly"), "€3.99 / month");
        assert_eq!(format_price(1099, "monthly"), "€10.99 / month");
        assert_eq!(format_price(5495, "monthly"), "€54.95 / month");
        assert_eq!(format_price(10990, "yearly"), "€109.90 / year");
        // Single-digit cents must zero-pad.
        assert_eq!(format_price(1005, "monthly"), "€10.05 / month");
    }

    #[test]
    fn price_from_subscription_prefers_billed_total() {
        // mollie_amount_cents (add-ons included) wins over base + addon.
        let sub = json!({
            "mollie_amount_cents": 5495,
            "base_plan_cents": 1099,
            "addon_cents": 4396,
        });
        assert_eq!(
            price_from_subscription(&sub, "monthly").as_deref(),
            Some("€54.95 / month")
        );
    }

    #[test]
    fn price_from_subscription_falls_back_to_base_plus_addon() {
        let sub = json!({
            "mollie_amount_cents": null,
            "base_plan_cents": 1099,
            "addon_cents": 1099,
        });
        assert_eq!(
            price_from_subscription(&sub, "monthly").as_deref(),
            Some("€21.98 / month")
        );
    }

    #[test]
    fn price_from_subscription_free_is_none() {
        let sub = json!({ "mollie_amount_cents": null, "base_plan_cents": 0, "addon_cents": 0 });
        assert_eq!(price_from_subscription(&sub, "monthly"), None);
    }

    #[test]
    fn price_from_catalog_matches_slug() {
        // Shape mirrors GET /api/v1/billing/plans (pricing-v2 fallback).
        let plans = json!({
            "plans": [
                { "id": "free", "price_eur": 0, "price_yearly_eur": 0 },
                { "id": "basic", "price_eur": 3.99, "price_yearly_eur": 39.90 },
                { "id": "pro", "price_eur": 10.99, "price_yearly_eur": 109.90 },
                { "id": "business", "price_eur": 54.95, "price_yearly_eur": 549.50 },
            ]
        });
        assert_eq!(
            price_from_catalog(&plans, "basic", "monthly").as_deref(),
            Some("€3.99 / month")
        );
        assert_eq!(
            price_from_catalog(&plans, "pro", "monthly").as_deref(),
            Some("€10.99 / month")
        );
        assert_eq!(
            price_from_catalog(&plans, "business", "yearly").as_deref(),
            Some("€549.50 / year")
        );
        // free → None (zero price); unknown slug → None.
        assert_eq!(price_from_catalog(&plans, "free", "monthly"), None);
        assert_eq!(price_from_catalog(&plans, "ultra", "monthly"), None);
    }
}
