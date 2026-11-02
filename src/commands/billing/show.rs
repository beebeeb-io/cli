//! `bb billing show`: the plan, storage and renewal view, plus the formatting
//! helpers every billing view shares (prices, counts, dates, region labels).
//!
//! Owns: `show`, `ADDON_FIELDS`, the price/region/number/date formatters and
//! `PlanLineDecision`.
//! Must not: fetch invoices or add-ons (see `billing::invoices`,
//! `billing::addons`).

use super::*;

pub(super) const ADDON_FIELDS: [&str; 10] = [
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
    let trial_ends_at = sub.get("trial_ends_at").and_then(|v| v.as_str());
    // Task 1037: `needs_plan` / `lapsed` — read-only, quota 0.
    let account_notice =
        crate::account_state::AccountState::from_subscription(&sub).notice(&crate::web_url::web_app_base());

    // ── Render ───────────────────────────────────────────────────────────────

    let dim = |s: &str| s.custom_color(crate::colors::INK_DIM);
    let label = |s: &str| s.custom_color(crate::colors::INK_SAGE);

    println!();
    if let Some(notice) = &account_notice {
        println!(
            "  {} {}",
            "!".custom_color(crate::colors::RED_ERR),
            notice.custom_color(crate::colors::RED_ERR),
        );
        println!();
    }
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
        // Task 1547 finding 1: a trialing subscription (Pattern B — no Mollie
        // call, no card on file) must never show a price + "Renews:" pair —
        // that implies an automatic charge that is not coming. See
        // `plan_line_decision`'s doc comment for the full bug.
        let decision = plan_line_decision(
            status,
            price_label.as_deref(),
            trial_ends_at,
            current_period_end,
            matches!(plan, Plan::Free),
        );
        if let Some(price) = decision.price.as_deref() {
            println!(
                "    {:<35} {}",
                plan_name.custom_color(crate::colors::INK),
                price.custom_color(crate::colors::INK)
            );
        } else {
            println!("    {}", plan_name.custom_color(crate::colors::INK));
        }
        if let Some((label_text, value)) = decision.sub_line {
            println!(
                "    {}    {}",
                label(label_text),
                value.custom_color(crate::colors::INK_WARM)
            );
        }
    }
    println!();

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

pub(super) fn format_json_value(value: &Value) -> String {
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
pub(super) fn price_from_subscription(sub: &Value, billing_cycle: &str) -> Option<String> {
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

/// Aggregate a `GET /files` `"files"` array into `(region, country) -> used_bytes`
/// plus a non-folder file count. Pure function so the region-breakdown logic
/// (`bb billing usage`, eng-0486) is unit-testable without a mock server.
/// Folders are skipped (they don't hold their own bytes). Task 2050: the group
/// is the COUNTRY (`storage_location.country`), so two datacenters in one
/// country are one row; a missing `storage_location`, or one without a country
/// (an older server that only sent the deprecated `city`), falls back to
/// `("europe", "Europe")`. A city is never read.
pub(super) fn aggregate_by_region(files: &[Value]) -> (BTreeMap<(String, String), i64>, i64) {
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
        let text = |key: &str| {
            loc.and_then(|v| v.get(key))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let region = text("region").unwrap_or_else(|| "europe".to_string());
        let country = text("country").unwrap_or_else(|| "Europe".to_string());
        *by_region.entry((region, country)).or_insert(0) += size;
    }
    (by_region, file_count)
}

/// The `by_region` rows of `bb --json billing usage`. `city` is a DEPRECATED
/// alias that carries the country name (the server does the same on its strict
/// refusal), so a script that reads `city` keeps working and never gets a city.
pub(super) fn region_rows_json(by_region: &BTreeMap<(String, String), i64>) -> Value {
    Value::Array(
        by_region
            .iter()
            .map(|((region, country), bytes)| {
                serde_json::json!({
                    "region": region,
                    "country": country,
                    "city": country,
                    "used_bytes": bytes,
                })
            })
            .collect(),
    )
}

/// The subscription's `region` is a BILLING label (the server's flat default is
/// the name of a city), not where the files are, and storage is chosen by
/// country now (task 2050; `bb region`). It never names a place: whatever the
/// server sends reads "Europe".
pub(super) fn region_human(_billing_region: &str) -> String {
    "Europe".into()
}

pub(super) fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

pub(super) fn format_number(n: i64) -> String {
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

/// What to print on the PLAN line's price slot and the line below it, for the
/// non-pending-downgrade branch of `show()`.
///
/// **Task 1547 finding 1.** A trialing subscription (Pattern B: no Mollie
/// call, no card on file — see `beebeeb-api/src/trial.rs`) used to fall
/// through to the same `price_from_subscription()` + `current_period_end`
/// "Renews:" pair as an actively-billed account. Since `mollie_amount_cents`
/// is null during a trial, `price_from_subscription()` fell back to the
/// catalog's regular price, and `current_period_end` is set equal to
/// `trial_ends_at` in the same UPDATE (`trial.rs:249`) — so the output read
/// as "plan €X.XX / month … Renews: <date>" with no cue that no card is on
/// file and nothing will actually be charged. This decides a distinct TRIAL
/// line instead, sourced from the subscription's own `trial_ends_at` field.
pub(super) struct PlanLineDecision {
    /// Price to print on the PLAN line itself. `None` means "no price" (Free,
    /// or trialing — a trial has no billed amount to show).
    pub(super) price: Option<String>,
    /// Optional `(label, value)` line printed below the PLAN line.
    pub(super) sub_line: Option<(&'static str, String)>,
}

pub(super) fn plan_line_decision(
    status: &str,
    price_label: Option<&str>,
    trial_ends_at: Option<&str>,
    current_period_end: Option<&str>,
    is_free: bool,
) -> PlanLineDecision {
    if status == "trialing" {
        let ends = format_date_human(trial_ends_at);
        return PlanLineDecision {
            price: None,
            sub_line: Some(("Trial:    ", format!("ends {ends} \u{00b7} no card on file"))),
        };
    }
    if is_free {
        return PlanLineDecision {
            price: None,
            sub_line: None,
        };
    }
    PlanLineDecision {
        price: price_label.map(str::to_string),
        sub_line: current_period_end.map(|d| ("Renews:   ", format_date_human(Some(d)))),
    }
}

pub(super) fn format_date_human(iso: Option<&str>) -> String {
    let Some(s) = iso else {
        return "—".into();
    };
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.format("%B %-d, %Y").to_string())
        .unwrap_or_else(|_| s.to_string())
}

pub(super) fn colour_pct(pct: f64, s: &str) -> colored::ColoredString {
    if pct >= 0.90 {
        s.custom_color(crate::colors::RED_ERR)
    } else if pct >= 0.70 {
        s.custom_color(crate::colors::AMBER)
    } else {
        s.custom_color(crate::colors::GREEN_OK)
    }
}
