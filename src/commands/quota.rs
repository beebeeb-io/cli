use beebeeb_types::quota::{Plan, effective_quota, format_storage_si};
use colored::Colorize;

use crate::api::ApiClient;
use crate::ui;

pub async fn run() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Fetch usage, file count, and subscription in parallel
    let (usage_res, count_res, sub_res) = tokio::join!(api.get_usage(), api.get_file_count(), api.get_subscription());

    let usage = usage_res?;
    let count = count_res.unwrap_or_default();
    let sub = sub_res.unwrap_or_default();

    let used_bytes = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);

    let plan_slug = sub.get("plan").and_then(|v| v.as_str()).unwrap_or("free");
    let plan = Plan::from_slug(plan_slug);
    let extra_tb = sub.get("extra_storage_tb").and_then(|v| v.as_i64()).unwrap_or(0);
    // 1547 finding 4: quota_bytes is the server's authoritative total (folds
    // in referral bonus_storage_bytes + the DB plan_catalog size) — read it
    // straight off the subscription response, the same way `bb billing show`
    // does post-0485, instead of recomputing via effective_quota() with a
    // `bonus_bytes` field this response never actually carries. Falls back to
    // a client-side compute only if the server ever omits the field.
    let quota_bytes = sub
        .get("quota_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| effective_quota(plan, extra_tb, 0));

    let percentage = if quota_bytes > 0 {
        used_bytes as f64 / quota_bytes as f64
    } else {
        0.0
    };
    let file_count: i64 = count
        .get("total_files")
        .or_else(|| count.get("count"))
        .or_else(|| count.get("total"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Build the plan label: "Pro — 8.0 TB (5 TB base + 3 TB extra)"
    let plan_label = build_plan_label(plan, extra_tb, quota_bytes);

    // ── JSON mode ────────────────────────────────────────────────────────────

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "used_bytes": used_bytes,
                "quota_bytes": quota_bytes,
                "percentage": percentage * 100.0,
                "files": file_count,
                "plan": plan.slug(),
                "extra_storage_tb": extra_tb,
                // 1547 finding 4: derived from the server's own quota_bytes
                // (base + extra + real bonus), not a `bonus_bytes` field the
                // subscription response never actually returns.
                "bonus_bytes": implied_bonus_bytes(plan, extra_tb, quota_bytes),
            }))
            .unwrap()
        );
        return Ok(());
    }

    // ── Quiet mode ───────────────────────────────────────────────────────────

    if ui::is_quiet() {
        println!("{}", format_storage_si(used_bytes));
        println!("{}", format_storage_si(quota_bytes));
        println!("{:.2}%", percentage * 100.0);
        return Ok(());
    }

    // ── Rich mode ────────────────────────────────────────────────────────────

    let dim = |s: &str| s.custom_color(crate::colors::INK_DIM);

    let used_str = format_storage_si(used_bytes);
    let quota_str = format_storage_si(quota_bytes);

    // Color-code the percentage: green <70%, amber 70-90%, red >90%
    let pct_str = if quota_bytes <= 0 {
        "\u{2014}".custom_color(crate::colors::INK_DIM) // —
    } else {
        let s = format!("{:.2}%", percentage * 100.0);
        if percentage >= 0.90 {
            s.custom_color(crate::colors::RED_ERR)
        } else if percentage >= 0.70 {
            s.custom_color(crate::colors::AMBER)
        } else {
            s.custom_color(crate::colors::GREEN_OK)
        }
    };

    let files_str = if file_count > 0 {
        format_number(file_count)
    } else {
        "\u{2014}".to_string() // —
    };

    println!();
    println!(
        "  {} {}",
        dim("plan    "),
        plan_label.custom_color(crate::colors::AMBER)
    );
    println!(
        "  {} {}",
        dim("used    "),
        format!("{} / {}", used_str, quota_str).custom_color(crate::colors::INK)
    );

    // Visual quota bar
    println!(
        "            {} {:.1}%", // align under labels
        ui::quota_bar(used_bytes.max(0) as u64, quota_bytes.max(0) as u64, 40),
        percentage * 100.0,
    );

    println!("  {} {}", dim("percent "), pct_str);
    println!(
        "  {} {}",
        dim("files   "),
        files_str.custom_color(crate::colors::INK_DIM)
    );

    // Over-quota warning
    if quota_bytes > 0 && used_bytes >= quota_bytes {
        println!();
        let msg = if plan.can_add_storage() {
            "Over quota \u{2014} uploads blocked. Add more storage at app.beebeeb.io/billing or delete files."
        } else {
            "Over quota \u{2014} uploads blocked. Upgrade your plan or delete files."
        };
        println!(
            "  {} {}",
            "!".custom_color(crate::colors::RED_ERR),
            msg.custom_color(crate::colors::RED_ERR),
        );
    }

    println!();
    Ok(())
}

/// Build a descriptive plan label like "Pro — 8.0 TB (5.0 TB base + 3.0 TB extra)".
///
/// `total_bytes` MUST be the server-authoritative quota (`quota_bytes` off
/// `GET /billing/subscription`) — **task 1547 finding 4.** See
/// `commands::whoami::build_plan_label`'s doc comment (identical bug,
/// duplicated helper): this used to take a `bonus_bytes` param sourced from
/// `sub.get("bonus_bytes")`, a key the subscription response never actually
/// returns (always read back 0), and ADD it to `effective_quota(plan,
/// extra_tb, bonus_bytes)` client-side, silently under-reporting quota for
/// any referral-bonus or catalog-overridden account.
fn build_plan_label(plan: Plan, extra_tb: i64, total_bytes: i64) -> String {
    let name = capitalise(plan.slug());
    let base = plan.base_storage_bytes();
    let extra_bytes = if extra_tb > 0 {
        extra_tb * beebeeb_types::quota::ONE_TB
    } else {
        0
    };
    let bonus_bytes = implied_bonus_bytes(plan, extra_tb, total_bytes);

    if extra_tb > 0 || bonus_bytes > 0 {
        let mut parts = vec![format!("{} base", format_storage_si(base))];
        if extra_tb > 0 {
            parts.push(format!("{} extra", format_storage_si(extra_bytes)));
        }
        if bonus_bytes > 0 {
            parts.push(format!("{} bonus", format_storage_si(bonus_bytes)));
        }
        format!(
            "{} \u{2014} {} ({})",
            name,
            format_storage_si(total_bytes),
            parts.join(" + "),
        )
    } else {
        format!("{} \u{2014} {}", name, format_storage_si(total_bytes))
    }
}

/// The referral/other bonus implied by the gap between the server's
/// authoritative `total_bytes` and this plan's `base + extra`. Never negative
/// (a mismatched/stale total must not print as a negative bonus).
fn implied_bonus_bytes(plan: Plan, extra_tb: i64, total_bytes: i64) -> i64 {
    let base = plan.base_storage_bytes();
    let extra_bytes = if extra_tb > 0 {
        extra_tb * beebeeb_types::quota::ONE_TB
    } else {
        0
    };
    (total_bytes - base - extra_bytes).max(0)
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Format a number with thousands separators, e.g. 1234 -> "1,234".
fn format_number(n: i64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_plan_label (task 1547 finding 4) ──────────────────────────────
    //
    // Same regression as `commands::whoami::tests` — `bb quota` duplicates
    // this helper and had the identical bug: the third param meant
    // `bonus_bytes` sourced from `sub.get("bonus_bytes")`, a key
    // `GET /billing/subscription` never actually returns (always read back
    // 0), and it was ADDED on top of `effective_quota(plan, extra_tb,
    // bonus_bytes)` client-side. `total_bytes` here must be the server's
    // authoritative `quota_bytes`, used directly.

    #[test]
    fn build_plan_label_shows_the_servers_total_bytes_directly_not_base_plus_bonus_double_counted() {
        let total_bytes = 6_000_000_000; // Free plan (5 GB base) + a 1 GB referral bonus
        let label = build_plan_label(Plan::Free, 0, total_bytes);
        assert!(
            label.contains(&format_storage_si(total_bytes)),
            "label must show the server's total (6.0 GB) directly, not base+total double-counted: {label}"
        );
        assert!(
            !label.contains(&format_storage_si(11_000_000_000)),
            "must not double-count base + total_bytes: {label}"
        );
    }

    #[test]
    fn build_plan_label_breaks_down_extra_and_the_implied_bonus_from_the_servers_total() {
        let extra_tb = 2;
        let bonus = 500_000_000i64;
        let total_bytes = Plan::Pro.base_storage_bytes() + extra_tb * beebeeb_types::quota::ONE_TB + bonus;
        let label = build_plan_label(Plan::Pro, extra_tb, total_bytes);
        assert!(label.contains(&format_storage_si(total_bytes)), "label: {label}");
        assert!(label.contains("extra"), "label: {label}");
        assert!(label.contains("bonus"), "label: {label}");
        assert!(
            label.contains(&format_storage_si(bonus)),
            "bonus amount must be the implied remainder: {label}"
        );
    }

    #[test]
    fn build_plan_label_with_no_extra_or_bonus_shows_just_plan_and_total() {
        let total_bytes = Plan::Basic.base_storage_bytes();
        let label = build_plan_label(Plan::Basic, 0, total_bytes);
        assert_eq!(label, format!("Basic \u{2014} {}", format_storage_si(total_bytes)));
    }
}
