use beebeeb_types::quota::{Plan, effective_quota, format_storage_si};
use colored::Colorize;

use crate::api::ApiClient;
use crate::config::load_config;
use crate::ui;

pub async fn run() -> Result<(), String> {
    let config = load_config();
    if config.session_token.is_none() {
        println!(
            "  {}",
            "Not logged in. Run `bb login` to authenticate.".custom_color(crate::colors::RED_ERR),
        );
        return Ok(());
    }

    let api = ApiClient::from_config();

    // Fetch everything in parallel
    let (me_res, sub_res, my_region_res, sessions_res, usage_res, count_res) = tokio::join!(
        api.get_me(),
        api.get_subscription(),
        api.get_my_region(),
        api.get_sessions(),
        api.get_usage(),
        api.get_file_count(),
    );

    let me = me_res.unwrap_or_default();
    let sub = sub_res.unwrap_or_default();
    let my_region = my_region_res.unwrap_or_default();
    let sessions = sessions_res.unwrap_or_default();
    let usage = usage_res.unwrap_or_default();
    let count = count_res.unwrap_or_default();

    // ── Parse fields ─────────────────────────────────────────────────────────

    let email = me.get("email").and_then(|v| v.as_str()).unwrap_or("unknown");

    let plan_slug = sub.get("plan").and_then(|v| v.as_str()).unwrap_or("free");
    let plan = Plan::from_slug(plan_slug);
    let extra_tb = sub.get("extra_storage_tb").and_then(|v| v.as_i64()).unwrap_or(0);
    // 1547 finding 4: quota_bytes is the server's authoritative total (folds
    // in referral bonus_storage_bytes + the DB plan_catalog size) — read it
    // straight off the subscription response, the same way `bb billing show`
    // does post-0485, instead of recomputing via effective_quota() with a
    // `bonus_bytes` field this response never actually carries. Falls back to
    // a client-side compute only if the server ever omits the field.
    let total_bytes = sub
        .get("quota_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| effective_quota(plan, extra_tb, 0));
    let plan_label = build_plan_label(plan, extra_tb, total_bytes);

    let region_label = my_region
        .get("preferred_region")
        .and_then(|v| v.as_str())
        .map(capitalise)
        .unwrap_or_else(|| "Europe".to_string());

    let used_bytes = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let percentage = if total_bytes > 0 {
        used_bytes as f64 / total_bytes as f64
    } else {
        0.0
    };
    let storage_label = format!(
        "{} / {} ({:.1}%)",
        format_storage_si(used_bytes),
        format_storage_si(total_bytes),
        percentage * 100.0,
    );

    let file_count = count
        .get("total_files")
        .or_else(|| count.get("count"))
        .or_else(|| count.get("total"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Auth method
    let token = config.session_token.as_deref().unwrap_or("");
    let auth_label = if token.starts_with("bb_pat_") {
        "personal access token \u{00b7} e2ee"
    } else {
        "session token \u{00b7} e2ee"
    };

    // Session expiry
    let current_session = sessions.get("sessions").and_then(|v| v.as_array()).and_then(|arr| {
        arr.iter()
            .find(|s| s.get("is_current").and_then(|c| c.as_bool()).unwrap_or(false))
    });

    let (session_active, expires_label, expires_str) = match current_session
        .and_then(|s| s.get("expires_at"))
        .and_then(|v| v.as_str())
    {
        Some(ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
            Ok(exp) => {
                let days = exp.signed_duration_since(chrono::Utc::now()).num_days();
                let label = if days <= 0 {
                    "expired".to_string()
                } else if days == 1 {
                    "expires in 1d".to_string()
                } else {
                    format!("expires in {days}d")
                };
                (days > 0, label, ts.to_string())
            }
            Err(_) => (false, "unknown".to_string(), String::new()),
        },
        None => (false, "unknown".to_string(), String::new()),
    };

    // ── JSON mode ────────────────────────────────────────────────────────────

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "email": email,
                "plan": plan.slug(),
                "extra_storage_tb": extra_tb,
                // 1547 finding 4: derived from the server's own quota_bytes
                // (base + extra + real bonus), not a `bonus_bytes` field the
                // subscription response never actually returns (see
                // `implied_bonus_bytes`'s doc comment).
                "bonus_bytes": implied_bonus_bytes(plan, extra_tb, total_bytes),
                "storage_used": used_bytes,
                "storage_total": total_bytes,
                "files": file_count,
                "upload_limit": upload_limit_for_plan(plan),
                "region": region_label,
                "session_expires": expires_str,
            }))
            .unwrap()
        );
        return Ok(());
    }

    // ── Quiet mode ───────────────────────────────────────────────────────────

    if ui::is_quiet() {
        println!("{email}");
        println!("{}", plan_label);
        return Ok(());
    }

    // ── Rich mode ────────────────────────────────────────────────────────────

    let dim = |s: &str| s.custom_color(crate::colors::INK_DIM);
    let val = |s: &str| s.custom_color(crate::colors::INK);

    println!();
    println!("  {} {}", dim("user    "), val(email));
    println!(
        "  {} {}",
        dim("plan    "),
        plan_label.custom_color(crate::colors::AMBER)
    );
    println!("  {} {}", dim("region  "), val(&region_label));

    // Storage line + visual quota bar
    println!("  {} {}", dim("storage "), val(&storage_label));
    println!(
        "            {} {:.1}%", // align under "storage"
        ui::quota_bar(used_bytes.max(0) as u64, total_bytes.max(0) as u64, 40),
        percentage * 100.0,
    );

    // Upload limit
    println!(
        "  {} {}",
        dim("upload  "),
        format!(
            "up to {} \u{00b7} {} parallel",
            upload_limit_for_plan(plan),
            "4 connections"
        )
        .custom_color(crate::colors::INK),
    );

    // File count
    println!("  {} {}", dim("files   "), val(&format_number(file_count)));

    // Session indicator
    let session_display = if session_active {
        format!(
            "{} {}",
            "\u{25cf} active".custom_color(crate::colors::GREEN_OK),
            format!("\u{00b7} {expires_label}").custom_color(crate::colors::INK_DIM),
        )
    } else {
        format!(
            "{} {}",
            "\u{25cf} inactive".custom_color(crate::colors::RED_ERR),
            format!("\u{00b7} {expires_label}").custom_color(crate::colors::INK_DIM),
        )
    };
    println!("  {} {}", dim("session "), session_display);

    // Auth + e2ee badge
    println!("  {} {}", dim("auth    "), val(auth_label));

    println!();
    Ok(())
}

/// Build a descriptive plan label like "Pro — 8.0 TB (5.0 TB base + 3.0 TB extra)".
///
/// `total_bytes` MUST be the server-authoritative quota (`quota_bytes` off
/// `GET /billing/subscription`) — **task 1547 finding 4.** This used to take
/// a `bonus_bytes` param sourced from `sub.get("bonus_bytes")`, a key the
/// subscription response never actually returns (it always read back 0), and
/// ADD it to `effective_quota(plan, extra_tb, bonus_bytes)` client-side. That
/// silently under-reported quota for any account with a referral bonus or a
/// DB-catalog-overridden plan size, diverging from `bb billing show` (fixed
/// for this exact bug under task 0485). Any leftover between `total_bytes`
/// and `base + extra` is shown as "bonus" — implied from the server's own
/// total, not a field that doesn't exist on the wire.
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

/// Default per-plan hourly upload volume caps, as displayed by `bb whoami`.
///
/// Source of truth: `repos/server/beebeeb-api/src/upload_throttle.rs`
/// (`UploadThrottle::new`, the `gb(slug, default_gb)` table) — free 50,
/// starter/basic 200, pro 500, business 1000 GB/hr. These are the server's
/// *compile-time defaults*, not a guarantee: each is overridable per-plan
/// without a redeploy via `BB_UPLOAD_VOLUME_<PLAN>_GB`, so the figure shown
/// here can drift from what a given deployment actually enforces. The
/// call site already prefixes this with "up to" (see `run` above).
fn upload_limit_for_plan(plan: Plan) -> &'static str {
    match plan {
        Plan::Free => "50 GB/hr",
        // Starter groups with Basic, mirroring the pre-`Plan`-enum mapping
        // (`"starter" | "basic" => "200 GB/hr"`, commit 2083636) that was
        // dropped when core didn't yet have `Plan::Starter` to match on.
        Plan::Starter => "200 GB/hr",
        Plan::Basic => "200 GB/hr",
        Plan::Pro => "500 GB/hr",
        // 1000 GB/hr in the server table; shown as "1 TB/hr" to match this
        // CLI's TB-at-1000-GB display convention (see
        // `beebeeb_types::quota::format_storage_si`, which switches from GB
        // to TB at the same threshold) rather than a bare "1000 GB/hr".
        Plan::Business => "1 TB/hr",
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

    #[test]
    fn upload_limit_for_plan_covers_every_plan() {
        // No `_ =>` catch-all above: adding a new `Plan` variant must fail
        // this match (and `cargo build`) again, not silently fall through.
        // Values match the server's enforced defaults in
        // `upload_throttle.rs::UploadThrottle::new` (free/starter/basic/pro/
        // business = 50/200/200/500/1000 GB/hr).
        assert_eq!(upload_limit_for_plan(Plan::Free), "50 GB/hr");
        assert_eq!(upload_limit_for_plan(Plan::Starter), "200 GB/hr");
        assert_eq!(upload_limit_for_plan(Plan::Basic), "200 GB/hr");
        assert_eq!(upload_limit_for_plan(Plan::Pro), "500 GB/hr");
        assert_eq!(upload_limit_for_plan(Plan::Business), "1 TB/hr");
    }

    #[test]
    fn upload_limit_for_starter_matches_basic() {
        // Starter (100 GB base, task 1386) shares Basic's throttle tier —
        // same intent as the pre-enum-refactor string match.
        assert_eq!(upload_limit_for_plan(Plan::Starter), upload_limit_for_plan(Plan::Basic));
    }

    // ── build_plan_label (task 1547 finding 4) ──────────────────────────────

    #[test]
    fn build_plan_label_shows_the_servers_total_bytes_directly_not_base_plus_bonus_double_counted() {
        // `total_bytes` is the server's authoritative `quota_bytes` off
        // `GET /billing/subscription` — it ALREADY includes base + extra +
        // any referral bonus. Passing it straight through must not be added
        // on top of the plan's base storage again (the old signature's third
        // arg meant `bonus_bytes`, which got ADDED to base — the exact
        // regression this guards: 5 GB base + 6 GB "bonus" used to render as
        // 11 GB instead of the real 6 GB total).
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
        // extra_storage_tb is a real, separately-known field; any remainder
        // between total_bytes and (base + extra) is shown as "bonus" —
        // derived from the server's own total rather than a `bonus_bytes`
        // key the subscription response never actually returns.
        let extra_tb = 2;
        let bonus = 500_000_000i64; // 500 MB referral bonus baked into total_bytes
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
