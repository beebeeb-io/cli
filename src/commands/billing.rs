//! `bb billing show` — read-only plan + storage + renewal info.
//!
//! Spec: docs/superpowers/specs/2026-05-23-cli-launch-readiness-design.md §4
//! "bb billing show output".

use beebeeb_types::quota::{effective_quota, format_storage_si, Plan};
use chrono::DateTime;
use colored::Colorize;

use crate::api::ApiClient;
use crate::ui;

pub async fn show(json: bool) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Parallel-fetch both endpoints — they're independent.
    let (sub_res, usage_res, count_res) = tokio::join!(
        api.get_billing_subscription(),
        api.get_billing_usage(),
        api.get_file_count(),
    );

    let sub = sub_res?;
    let usage = usage_res?;
    let count = count_res.unwrap_or_default();

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
    let extra_tb = sub.get("extra_storage_tb").and_then(|v| v.as_i64()).unwrap_or(0);
    let bonus_bytes = sub.get("bonus_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let quota_bytes = effective_quota(plan, extra_tb, bonus_bytes);

    let used_bytes = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let file_count = count
        .get("total_files")
        .or_else(|| count.get("count"))
        .or_else(|| count.get("total"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let status = sub.get("status").and_then(|v| v.as_str()).unwrap_or("active");
    let billing_cycle = sub
        .get("billing_cycle")
        .and_then(|v| v.as_str())
        .unwrap_or("monthly");
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
    let price_label = price_for(plan, billing_cycle);
    if let Some(pending) = pending_downgrade_plan {
        let pending_plan = Plan::from_slug(pending);
        let pending_name = capitalise(pending_plan.slug());
        let renew_date = format_date_human(current_period_end);
        println!(
            "    {} → {} on {}    {}",
            plan_name.custom_color(crate::colors::INK),
            pending_name.custom_color(crate::colors::INK),
            renew_date.custom_color(crate::colors::INK_WARM),
            price_for(pending_plan, billing_cycle)
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
        let pending = pending_downgrade_plan
            .map(|p| Plan::from_slug(p))
            .unwrap_or(plan);
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
                format!(
                    "After {downgrade_date}: {new_quota_str} quota — currently over by {over_str}."
                )
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
    println!(
        "    {}",
        region_human(region_slug).custom_color(crate::colors::INK)
    );
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

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn price_for(plan: Plan, billing_cycle: &str) -> Option<String> {
    // Mirror the prices shown in the web billing page. Plan::is_free() doesn't
    // tell us the dollar amount, so we hardcode it here — Spec 2 introduces a
    // server-driven catalog that this function will pull from.
    match (plan.slug(), billing_cycle) {
        ("free", _) => None,
        ("basic", "yearly") => Some("€109.99 / year".into()),
        ("basic", _) => Some("€10.99 / month".into()),
        ("pro", "yearly") => Some("€199.99 / year".into()),
        ("pro", _) => Some("€19.99 / month".into()),
        ("ultra", "yearly") => Some("€399.99 / year".into()),
        ("ultra", _) => Some("€39.99 / month".into()),
        _ => None,
    }
}

fn region_human(slug: &str) -> String {
    match slug {
        "europe" | "eu" => "europe (Falkenstein, Germany)".into(),
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
