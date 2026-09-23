//! `bb billing show` — read-only plan + storage + renewal info.
//!
//! Spec: docs/superpowers/specs/2026-05-23-cli-launch-readiness-design.md §4
//! "bb billing show output".
//!
//! ## `usage` (task 0486, plan Task 18)
//!
//! `GET /api/v1/billing/usage` (`ApiClient::get_billing_usage`, the same
//! billing-namespaced route `show` reads — task 0485 established this is the
//! server-authoritative one, NOT `get_usage()` which hits the different
//! `/api/v1/files/usage` route). Adds a per-region storage breakdown on top of
//! what `show`/`bb quota` already render.
//!
//! **Deviation (eng-0486) — region breakdown is client-side and approximate.**
//! The plan's open question #4 is still open: no
//! `GET /api/v1/billing/usage/by-region` route exists on the live server
//! (confirmed: `grep by.region beebeeb-api/src/routes/billing.rs` — no hit).
//! So, exactly as the plan's Task 18 describes, this aggregates locally over
//! `GET /files` with no `parent_id` (`ApiClient::list_files(None)`) — ONE
//! round-trip, root-level files only, no subfolder recursion — and labels the
//! breakdown "approximate" in both the rich and JSON output. Each file's
//! `storage_location.{region,city}` (confirmed present on `GET /files` rows —
//! `beebeeb-api/src/routes/files.rs`, "Storage location on file responses" in
//! the server's CLAUDE.md) is the aggregation key.
//!
//! **Reuse, not reinvention.** The bar is `ui::quota_bar` (already used by
//! `bb quota`) rather than a new bespoke progress-bar renderer — the plan's
//! pseudocode sketches its own `render_progress_bar`, but this repo already
//! has one with the right green/amber/red thresholds baked in.
//!
//! ## `invoices` (task 0487, plan Task 19)
//!
//! `GET /api/v1/billing/invoices` (`ApiClient::get_billing_invoices`) —
//! `beebeeb-api/src/routes/billing.rs::invoices`.
//!
//! **Deviations (eng-0487) from the plan's Task 19 pseudocode, read against
//! the live handler** (confirmed against `repos/web/src/lib/api.ts`, which
//! reads the same route the same way):
//!
//! - Amount field is `amount_gross_cents` (VAT included), not `amount_paid`.
//! - `invoice_date`/`period_start`/`period_end` are ISO `YYYY-MM-DD` date
//!   strings (`chrono::NaiveDate::to_string()` server-side), NOT Unix
//!   timestamps — there is no epoch-seconds field to feed
//!   `DateTime::from_timestamp`.
//! - The PDF field is `pdf_url` (not `invoice_pdf`), and it is a RELATIVE,
//!   Bearer-auth-scoped path (`/api/v1/billing/invoices/{id}/pdf`) — never a
//!   public link a bare `curl` can fetch (confirmed: the handler takes an
//!   `AuthUser` extractor and 404s a non-owned id). `--open <id>` downloads it
//!   through the authenticated `ApiClient` (`download_invoice_pdf`) and opens
//!   the SAVED FILE locally — a raw browser URL would just 401.
//! - `stripe_configured` is hardcoded `false` on every response post-Mollie-
//!   cutover (see the handler's own doc comment: "kept in the envelope … so
//!   the existing web client shape doesn't break"). It is NEVER a live signal
//!   of "no invoices exist" — the plan's gate (`if !stripe_configured { print
//!   "Stripe is not configured" }`) would print that message on every single
//!   account, including ones with real invoices. **Not implemented.**
//!   Emptiness of the `invoices` array alone decides the "no invoices yet"
//!   message.
//! - `--open <id>` (from the task file's "Why", not in the plan's code
//!   sketch) accepts a full UUID or a unique prefix, reusing the
//!   `resolve_passkey_id`/`resolve_session_id` convention already established
//!   in this repo (`short_id`/`resolve_invoice_id` below).
//! - **Status vocabulary is `'draft' | 'issued' | 'void'`, not Stripe's
//!   `'paid'/'open'`** (`invoices` table CHECK constraint, `beebeeb-api/src/db.rs`;
//!   confirmed live by manual verification — every seeded row came back
//!   `status: "issued"` and, before this fix, rendered red as an unmatched
//!   status). See `invoice_status_color` for the live mapping.

use std::collections::BTreeMap;

use beebeeb_types::quota::{Plan, effective_quota, format_storage_si};
use chrono::DateTime;
use colored::Colorize;
use serde_json::Value;

use crate::api::ApiClient;
use crate::ui;

const ADDON_FIELDS: [&str; 10] = [
    "plan",
    "base_storage_tb",
    "extra_storage_tb",
    "total_storage_tb",
    "max_storage_tb",
    "storage_addon_price_cents",
    "extra_users",
    "base_users",
    "max_users",
    "user_addon_price_cents",
];

pub async fn show(json: bool) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Parallel-fetch the endpoints — they're independent. The plan catalog
    // (`/billing/plans`) is best-effort: it's only needed to price a *pending
    // downgrade* plan, so a failure there must not break `bb billing`.
    let (sub_res, usage_res, count_res, plans_res) = tokio::join!(
        api.get_billing_subscription(),
        api.get_billing_usage(),
        api.get_file_count(),
        api.get_billing_plans(),
    );

    let sub = sub_res?;
    let usage = usage_res?;
    let count = count_res.unwrap_or_default();
    let plans = plans_res.unwrap_or_default();

    // ── JSON mode ────────────────────────────────────────────────────────────

    if json || ui::is_json() {
        let merged = serde_json::json!({
            "subscription": sub,
            "usage": usage,
            "file_count": count,
        });
        println!("{}", serde_json::to_string_pretty(&merged).unwrap());
        return Ok(());
    }

    // ── Parse fields ─────────────────────────────────────────────────────────

    let plan_slug = sub.get("plan").and_then(|v| v.as_str()).unwrap_or("free");
    let plan = Plan::from_slug(plan_slug);
    // 0485: quota_bytes comes from the server's `/billing/usage` response, NOT
    // a client-side `effective_quota(plan, extra_tb, bonus_bytes)` recompute.
    // The server's `get_user_quota()` folds in the DB `plans` catalog's
    // storage_bytes (which can diverge from this crate's hardcoded per-Plan
    // constants) AND a referral `bonus_storage_bytes` that the subscription
    // response never exposed under a `bonus_bytes` key in the first place (it
    // read back as 0 always) — so the old computation silently under-reported
    // quota for any bonus-storage or catalog-overridden account. See
    // `beebeeb-api/src/quota.rs::get_user_quota` (server, read-only reference).
    let quota_bytes = usage.get("quota_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let used_bytes = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let file_count = count
        .get("total_files")
        .or_else(|| count.get("count"))
        .or_else(|| count.get("total"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let status = sub.get("status").and_then(|v| v.as_str()).unwrap_or("active");
    let billing_cycle = sub.get("billing_cycle").and_then(|v| v.as_str()).unwrap_or("monthly");
    let current_period_end = sub.get("current_period_end").and_then(|v| v.as_str());
    let pending_downgrade_plan = sub
        .get("pending_downgrade_plan")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let storage_grace_deadline = sub
        .get("storage_grace_deadline")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let region_slug = sub.get("region").and_then(|v| v.as_str()).unwrap_or("europe");

    // ── Render ───────────────────────────────────────────────────────────────

    let dim = |s: &str| s.custom_color(crate::colors::INK_DIM);
    let label = |s: &str| s.custom_color(crate::colors::INK_SAGE);

    println!();
    println!("  {}", "PLAN".custom_color(crate::colors::AMBER));

    let plan_name = capitalise(plan.slug());
    // Current plan price: the server's authoritative billed amount (task 0947
    // fields on the subscription response), NOT a hardcoded table. Includes the
    // user's storage/seat add-ons.
    let price_label = price_from_subscription(&sub, billing_cycle);
    if let Some(pending) = pending_downgrade_plan {
        let pending_plan = Plan::from_slug(pending);
        let pending_name = capitalise(pending_plan.slug());
        let renew_date = format_date_human(current_period_end);
        println!(
            "    {} → {} on {}    {}",
            plan_name.custom_color(crate::colors::INK),
            pending_name.custom_color(crate::colors::INK),
            renew_date.custom_color(crate::colors::INK_WARM),
            // Pending-downgrade plan price comes from the public catalog
            // (`/billing/plans`), since the subscription only carries the
            // *current* plan's billed amount.
            price_from_catalog(&plans, pending, billing_cycle)
                .map(|p| format!("{p} after"))
                .unwrap_or_default()
                .custom_color(crate::colors::INK_DIM),
        );
        if let Some(date) = current_period_end {
            println!(
                "    {}    {}",
                label("Renews:   "),
                format_date_human(Some(date)).custom_color(crate::colors::INK_WARM)
            );
        }
    } else {
        if let Some(price) = price_label.as_deref() {
            println!(
                "    {:<35} {}",
                plan_name.custom_color(crate::colors::INK),
                price.custom_color(crate::colors::INK)
            );
        } else {
            // Free plan — no price line.
            println!("    {}", plan_name.custom_color(crate::colors::INK));
        }
        if let Some(date) = current_period_end {
            if !matches!(plan, Plan::Free) {
                println!(
                    "    {}    {}",
                    label("Renews:   "),
                    format_date_human(Some(date)).custom_color(crate::colors::INK_WARM)
                );
            }
        }
    }
    println!();

    // STORAGE
    println!("  {}", "STORAGE".custom_color(crate::colors::AMBER));
    let pct = if quota_bytes > 0 {
        used_bytes as f64 / quota_bytes as f64
    } else {
        0.0
    };
    let pct_str = format!("{}%", (pct * 100.0).round() as i64);
    println!(
        "    {}     {}    {}",
        label("Used:  "),
        format!(
            "{} of {}",
            format_storage_si(used_bytes),
            format_storage_si(quota_bytes)
        )
        .custom_color(crate::colors::INK),
        colour_pct(pct, &pct_str),
    );
    println!(
        "    {}     {}",
        label("Files: "),
        format_number(file_count).custom_color(crate::colors::INK_DIM)
    );

    if let Some(deadline) = storage_grace_deadline {
        let pending = pending_downgrade_plan.map(Plan::from_slug).unwrap_or(plan);
        let new_quota = effective_quota(pending, 0, 0);
        let over_by = (used_bytes - new_quota).max(0);
        if over_by > 0 {
            let pending_name = capitalise(pending.slug());
            let new_quota_str = format_storage_si(new_quota);
            let over_str = format_storage_si(over_by);
            let downgrade_date = current_period_end
                .map(|d| format_date_human(Some(d)))
                .unwrap_or_else(|| "the downgrade date".into());
            println!(
                "    {}",
                format!("After {downgrade_date}: {new_quota_str} quota — currently over by {over_str}.")
                    .custom_color(crate::colors::INK_WARM)
            );
            let deadline_str = format_date_human(Some(deadline));
            println!(
                "    {} {} {}",
                "Auto-deletion of oldest files starts".custom_color(crate::colors::RED_ERR),
                deadline_str.bold().custom_color(crate::colors::RED_ERR),
                ".".custom_color(crate::colors::RED_ERR)
            );
            let _ = pending_name; // currently unused in output, kept for future variants
        }
    }
    println!();

    // REGION
    println!("  {}", "REGION".custom_color(crate::colors::AMBER));
    println!("    {}", region_human(region_slug).custom_color(crate::colors::INK));
    println!();

    // STATUS (only render for paid plans — free plans are always "active" in
    // a way that doesn't surface anything useful).
    if !matches!(plan, Plan::Free) {
        println!("  {}", "STATUS".custom_color(crate::colors::AMBER));
        let s = capitalise(status);
        let coloured = match status {
            "active" | "trialing" => s.custom_color(crate::colors::GREEN_OK),
            "past_due" | "unpaid" | "incomplete" => s.custom_color(crate::colors::RED_ERR),
            _ => s.custom_color(crate::colors::INK_WARM),
        };
        println!("    {}", coloured);
        println!();
    }

    // Footer CTA — different copy for free vs paid.
    let footer = if matches!(plan, Plan::Free) {
        "Upgrade at https://app.beebeeb.io/billing"
    } else {
        "Manage your plan at https://app.beebeeb.io/billing"
    };
    println!("  {}", footer.custom_color(crate::colors::INK_DIM));
    println!();

    // Silence warnings on the unused dim binding (used in tests path only).
    let _ = dim;

    Ok(())
}

/// `bb billing usage` — storage usage with a per-region breakdown. See the
/// module doc comment above ("`usage` (task 0486, plan Task 18)") for the
/// approximate-breakdown deviation.
pub async fn usage() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let (usage_res, files_res) = tokio::join!(api.get_billing_usage(), api.list_files(None));
    let usage = usage_res?;

    let used = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let quota = usage.get("quota_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let pct = if quota > 0 { used as f64 / quota as f64 } else { 0.0 };

    // Local per-region aggregation over root-level files only — see the
    // module doc comment (eng-0486) for why this is approximate.
    let region_breakdown_failed = files_res.is_err();
    let (by_region, file_count) = files_res
        .ok()
        .and_then(|body| body.get("files").and_then(|v| v.as_array()).cloned())
        .map(|files| aggregate_by_region(&files))
        .unwrap_or_default();

    if ui::is_json() {
        let regions: Vec<_> = by_region
            .iter()
            .map(|((r, c), b)| serde_json::json!({ "region": r, "city": c, "used_bytes": b }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "used_bytes": used,
                "quota_bytes": quota,
                "percentage": pct,
                "file_count": file_count,
                "by_region": regions,
                "by_region_note": "approximate — top-level files only, no /billing/usage/by-region route yet",
                "by_region_unavailable": region_breakdown_failed,
            }))
            .unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    if ui::is_quiet() {
        println!("{}", format_storage_si(used));
        println!("{}", format_storage_si(quota));
        println!("{:.2}%", pct * 100.0);
        return Ok(());
    }

    println!();
    println!("  {}", "STORAGE".custom_color(crate::colors::AMBER));
    println!(
        "  {} / {}  ({:.0}%)",
        format_storage_si(used).custom_color(crate::colors::INK),
        format_storage_si(quota).custom_color(crate::colors::INK),
        pct * 100.0,
    );
    println!(
        "            {}",
        ui::quota_bar(used.max(0) as u64, quota.max(0) as u64, 40)
    );
    println!();
    println!(
        "  {} {}",
        "files  ".custom_color(crate::colors::INK_SAGE),
        file_count.to_string().custom_color(crate::colors::INK)
    );
    if region_breakdown_failed {
        println!(
            "  {} {}",
            "region ".custom_color(crate::colors::INK_SAGE),
            "unavailable — could not list files for the breakdown".custom_color(crate::colors::RED_ERR)
        );
    } else {
        println!(
            "  {} {}",
            "region ".custom_color(crate::colors::INK_SAGE),
            "approximate — top-level files only".custom_color(crate::colors::INK_DIM)
        );
        for ((region, city), bytes) in &by_region {
            println!(
                "           {} \u{00b7} {}    {}",
                region.custom_color(crate::colors::INK_DIM),
                city.custom_color(crate::colors::INK_DIM),
                format_storage_si(*bytes).custom_color(crate::colors::INK),
            );
        }
    }
    println!();
    Ok(())
}

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
async fn open_invoice_pdf(api: &ApiClient, invoices: &[Value], input: &str) -> Result<(), String> {
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
fn render_quiet_invoices(invoices: &[Value]) -> Vec<String> {
    invoices
        .iter()
        .map(|inv| inv.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string())
        .collect()
}

/// Build one invoice table row from a raw `GET /billing/invoices` entry.
/// Pure — see `commands::billing::tests` for coverage of every field mapping.
fn invoice_row(inv: &Value) -> Vec<String> {
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
/// `beebeeb-api/src/db.rs`) — NOT the Stripe-style `'paid'`/`'open'` the
/// plan's Task 19 pseudocode assumed (eng-0487 deviation, caught in manual
/// verification: every real seeded row rendered RED_ERR because none of them
/// are `'paid'`). Every row the server inserts gets `status = 'issued'`
/// directly (`invoice_gen.rs`, only ever from an already-PAID Mollie
/// payment — `'issued'` IS the normal/good state here); `'void'` is the
/// correction state (task 0917 credit notes); `'draft'` is schema-permitted
/// but never currently written by any server code path.
fn invoice_status_color(status: &str) -> colored::CustomColor {
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
fn format_invoice_amount(cents: i64, currency: &str) -> String {
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
fn resolve_invoice_id(invoices: &[Value], input: &str) -> Result<String, String> {
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
fn short_id(id: &str) -> &str {
    if id.len() >= 8 { &id[..8] } else { id }
}

/// Strip characters that are awkward in a filename (kept simple: invoice
/// numbers are server-generated, short, ASCII — this is a defensive
/// fallback, not a general sanitizer).
fn sanitize_filename(s: &str) -> String {
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
fn write_private_temp_pdf(stem: &str, bytes: &[u8]) -> Result<std::path::PathBuf, String> {
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

pub async fn portal() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let response = api.create_billing_portal_session().await?;
    let url = response
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "server did not return a url".to_string())?;

    if !crate::env_detect::is_headless() {
        let _ = open::that(url);
    }

    println!("Billing portal: {url}");
    Ok(())
}

pub async fn addons() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let addons = api.get_billing_addons().await?;
    print_addons(&addons);
    Ok(())
}

pub async fn purchase_addon(addon_id: String) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let addons = api.get_billing_addons().await?;
    let field = match addon_id.as_str() {
        "extra_storage_tb" => "extra_storage_tb",
        "extra_users" => "extra_users",
        _ => {
            return Err(
                "unknown ADDON_ID; use one of the server add-on fields: extra_storage_tb, extra_users".to_string(),
            );
        }
    };

    let current = addons
        .get(field)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("server did not return {field}"))?;
    let next = current
        .checked_add(1)
        .ok_or_else(|| format!("{field} is too large to increment"))?;

    let mut body = serde_json::Map::new();
    body.insert(field.to_string(), serde_json::json!(next));
    let response = api.update_billing_addons(Value::Object(body)).await?;
    let confirmed = response.get(field).and_then(|v| v.as_i64()).unwrap_or(next);

    println!("Add-on purchase confirmed: {field}={confirmed}");
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn print_addons(addons: &Value) {
    let field_width = ADDON_FIELDS.iter().map(|field| field.len()).max().unwrap_or(5);

    println!("{:<field_width$}  value", "field", field_width = field_width);
    for field in ADDON_FIELDS {
        let value = addons.get(field).map(format_json_value).unwrap_or_else(|| "-".into());
        println!("{:<field_width$}  {value}", field, field_width = field_width);
    }
}

fn format_json_value(value: &Value) -> String {
    match value {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Price for the user's *current* plan, taken from the server's authoritative
/// billing fields on the subscription response (task 0947) — never a hardcoded
/// table. Prefers `mollie_amount_cents` (the amount actually billed, add-ons
/// included); falls back to `base_plan_cents + addon_cents`. Returns `None` for
/// free / zero-cost subscriptions so the caller omits the price line.
fn price_from_subscription(sub: &Value, billing_cycle: &str) -> Option<String> {
    let cents = sub.get("mollie_amount_cents").and_then(|v| v.as_i64()).or_else(|| {
        let base = sub.get("base_plan_cents").and_then(|v| v.as_i64());
        let addon = sub.get("addon_cents").and_then(|v| v.as_i64()).unwrap_or(0);
        base.map(|b| b + addon)
    })?;
    if cents <= 0 {
        return None;
    }
    Some(format_price(cents, billing_cycle))
}

/// Price for an arbitrary plan slug, read from the public plan catalog
/// (`GET /api/v1/billing/plans`). Used to price a *pending downgrade* plan,
/// whose amount isn't on the current subscription. Returns `None` when the slug
/// isn't in the catalog or the price is zero/free.
fn price_from_catalog(plans: &Value, slug: &str, billing_cycle: &str) -> Option<String> {
    let list = plans.get("plans").and_then(|v| v.as_array())?;
    let plan = list
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(slug))?;
    let key = if billing_cycle == "yearly" {
        "price_yearly_eur"
    } else {
        "price_eur"
    };
    let eur = plan.get(key).and_then(|v| v.as_f64())?;
    let cents = (eur * 100.0).round() as i64;
    if cents <= 0 {
        return None;
    }
    Some(format_price(cents, billing_cycle))
}

/// Format an integer cents amount as `€X.XX / month` (or `/ year`).
fn format_price(cents: i64, billing_cycle: &str) -> String {
    let period = if billing_cycle == "yearly" { "year" } else { "month" };
    format!("€{}.{:02} / {}", cents / 100, cents % 100, period)
}

/// Aggregate a `GET /files` `"files"` array into `(region, city) -> used_bytes`
/// plus a non-folder file count. Pure function so the region-breakdown logic
/// (`bb billing usage`, eng-0486) is unit-testable without a mock server.
/// Folders are skipped (they don't hold their own bytes); a missing/absent
/// `storage_location` falls back to `("europe", "Falkenstein")` — the live
/// server's documented default for the region field.
fn aggregate_by_region(files: &[Value]) -> (BTreeMap<(String, String), i64>, i64) {
    let mut by_region: BTreeMap<(String, String), i64> = BTreeMap::new();
    let mut file_count: i64 = 0;
    for f in files {
        let is_folder = f.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        if is_folder {
            continue;
        }
        file_count += 1;
        let size = f.get("size_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
        let loc = f.get("storage_location");
        let region = loc
            .and_then(|v| v.get("region"))
            .and_then(|v| v.as_str())
            .unwrap_or("europe")
            .to_string();
        let city = loc
            .and_then(|v| v.get("city"))
            .and_then(|v| v.as_str())
            .unwrap_or("Falkenstein")
            .to_string();
        *by_region.entry((region, city)).or_insert(0) += size;
    }
    (by_region, file_count)
}

fn region_human(slug: &str) -> String {
    match slug {
        // "falkenstein" is the live server's literal fallback value
        // (`beebeeb-api/src/routes/billing.rs::subscription`'s no-subscription-
        // row branch, returned for every fresh free-tier account — the exact
        // account this command's manual verification rung uses) — not just
        // "europe"/"eu" from an actual subscriptions row.
        "europe" | "eu" | "falkenstein" => "europe (Falkenstein, Germany)".into(),
        other => other.to_string(),
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn format_number(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_date_human(iso: Option<&str>) -> String {
    let Some(s) = iso else {
        return "—".into();
    };
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.format("%B %-d, %Y").to_string())
        .unwrap_or_else(|_| s.to_string())
}

fn colour_pct(pct: f64, s: &str) -> colored::ColoredString {
    if pct >= 0.90 {
        s.custom_color(crate::colors::RED_ERR)
    } else if pct >= 0.70 {
        s.custom_color(crate::colors::AMBER)
    } else {
        s.custom_color(crate::colors::GREEN_OK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── aggregate_by_region (eng-0486) ──────────────────────────────────────

    #[test]
    fn aggregate_by_region_sums_bytes_per_region_and_skips_folders() {
        let files = vec![
            json!({
                "is_folder": false, "size_bytes": 100,
                "storage_location": { "region": "europe", "city": "Falkenstein", "provider": "Hetzner" },
            }),
            json!({
                "is_folder": false, "size_bytes": 250,
                "storage_location": { "region": "europe", "city": "Falkenstein", "provider": "Hetzner" },
            }),
            json!({
                "is_folder": true, "size_bytes": 999_999,
                "storage_location": { "region": "europe", "city": "Falkenstein", "provider": "Hetzner" },
            }),
        ];
        let (by_region, file_count) = aggregate_by_region(&files);
        assert_eq!(file_count, 2, "the folder row must not be counted as a file");
        assert_eq!(
            by_region.get(&("europe".to_string(), "Falkenstein".to_string())),
            Some(&350),
            "folder bytes (999_999) must NOT be summed into the region total: {by_region:?}"
        );
        assert_eq!(by_region.len(), 1);
    }

    #[test]
    fn aggregate_by_region_splits_multiple_regions() {
        let files = vec![
            json!({
                "is_folder": false, "size_bytes": 10,
                "storage_location": { "region": "europe", "city": "Falkenstein", "provider": "Hetzner" },
            }),
            json!({
                "is_folder": false, "size_bytes": 20,
                "storage_location": { "region": "europe", "city": "Helsinki", "provider": "Local" },
            }),
        ];
        let (by_region, file_count) = aggregate_by_region(&files);
        assert_eq!(file_count, 2);
        assert_eq!(
            by_region.len(),
            2,
            "two distinct (region, city) pairs must stay separate: {by_region:?}"
        );
        assert_eq!(
            by_region.get(&("europe".to_string(), "Falkenstein".to_string())),
            Some(&10)
        );
        assert_eq!(
            by_region.get(&("europe".to_string(), "Helsinki".to_string())),
            Some(&20)
        );
    }

    #[test]
    fn aggregate_by_region_defaults_missing_storage_location_to_europe_falkenstein() {
        // A file row with no storage_location at all (defensive — every live
        // row carries one, but the field must never be assumed present).
        let files = vec![json!({ "is_folder": false, "size_bytes": 5 })];
        let (by_region, file_count) = aggregate_by_region(&files);
        assert_eq!(file_count, 1);
        assert_eq!(
            by_region.get(&("europe".to_string(), "Falkenstein".to_string())),
            Some(&5),
            "missing storage_location must fall back to europe/Falkenstein, not be dropped: {by_region:?}"
        );
    }

    #[test]
    fn aggregate_by_region_empty_input_is_empty_output() {
        let (by_region, file_count) = aggregate_by_region(&[]);
        assert_eq!(file_count, 0);
        assert!(by_region.is_empty());
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
    fn invoice_status_color_maps_the_live_vocabulary_not_the_plans_stripe_style_one() {
        // The invoices table CHECK constraint is 'draft' | 'issued' | 'void'.
        // 'issued' is the normal/good state (every real row is inserted with
        // it) — it must be green, not fall through to the red catch-all the
        // way the plan's 'paid'/'open' match arms would leave it.
        assert_eq!(invoice_status_color("issued"), crate::colors::GREEN_OK);
        assert_eq!(invoice_status_color("draft"), crate::colors::AMBER);
        assert_eq!(invoice_status_color("void"), crate::colors::RED_ERR);
        assert_eq!(invoice_status_color("something-unexpected"), crate::colors::AMBER);
        // The plan's Stripe-style statuses are NOT in the live vocabulary and
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
    fn region_human_maps_the_live_no_subscription_fallback() {
        // The live server's no-subscription-row branch
        // (beebeeb-api/src/routes/billing.rs::subscription) returns the
        // literal string "falkenstein" — exactly what a fresh free-tier
        // account (this command's manual verification rung) sees.
        assert_eq!(region_human("falkenstein"), "europe (Falkenstein, Germany)");
        assert_eq!(region_human("europe"), "europe (Falkenstein, Germany)");
        assert_eq!(region_human("eu"), "europe (Falkenstein, Germany)");
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
