use beebeeb_types::quota::{Plan, effective_quota, format_storage_si};
use colored::Colorize;

use crate::api::ApiClient;
use crate::config::load_config;
use crate::ui;

pub async fn run() -> Result<(), String> {
    let config = load_config();
    if config.session_token.is_none() {
        // Exit non-zero (main prints this on stderr): scripts use `bb whoami`
        // as the "am I signed in?" check.
        return Err("Not logged in. Run `bb login` to sign in.".to_string());
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

    // Identity, plan and usage are what this command exists to report — if
    // any of them failed (e.g. a revoked session → 401), fail with that error
    // instead of printing placeholders ("user unknown, plan Free") that would
    // tell a paying user they are on Free. Region and the session list stay
    // best-effort decorations.
    let me = me_res?;
    let sub = sub_res?;
    let usage = usage_res?;
    let my_region = my_region_res.unwrap_or_default();
    let sessions = sessions_res.unwrap_or_default();
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

/// Build a descriptive plan label like "Pro — 8.0 TB (1.0 TB base + 7.0 TB extra)".
///
/// `total_bytes` MUST be the server-authoritative quota (`quota_bytes` off
/// `GET /billing/subscription`) — **task 1547 finding 4.**
///
/// **Codex review on PR #33 (task 1547 finding 2):** this used to infer a
/// third "bonus" component as `total_bytes - base - extra`, on the theory
/// that any leftover was a referral bonus. That inference is unsound: the
/// subscription response (`beebeeb-api/src/routes/billing.rs::subscription`)
/// does not expose a base/bonus split at all — only `quota_bytes` (the
/// already-folded-in total; see `quota.rs::get_user_quota`, which computes
/// it server-side as `effective_quota(plan, extra_storage_tb,
/// bonus_storage_bytes)` and never returns the pieces separately) and
/// `extra_storage_tb`. `base` here is this CLI's own hardcoded
/// `Plan::base_storage_bytes()` (a `beebeeb-core` constant) — if it drifts
/// from whatever the server actually granted (e.g. a stale `CORE_REV` pin
/// after a pricing constant changes; Pro's base went 5 TB → 1 TB under
/// pricing-v2), the "leftover" is really just that drift, not a bonus, and
/// labeling it "bonus" would be a fabricated, wrong claim.
///
/// Fix: only show the "(base + extra)" breakdown when it EXACTLY accounts
/// for the server's `total_bytes` (this CLI's `base` plus the server's own
/// `extra_storage_tb` sums to exactly what the server granted). Any
/// unexplained remainder — a real referral bonus, or the two sides
/// disagreeing on the plan's base — is never split out or labeled; the
/// total alone is shown, and it is always correct because it comes straight
/// from the server.
fn build_plan_label(plan: Plan, extra_tb: i64, total_bytes: i64) -> String {
    let name = capitalise(plan.slug());
    let base = plan.base_storage_bytes();
    let extra_bytes = if extra_tb > 0 {
        extra_tb * beebeeb_types::quota::ONE_TB
    } else {
        0
    };

    if extra_tb > 0 && base + extra_bytes == total_bytes {
        format!(
            "{} \u{2014} {} ({} base + {} extra)",
            name,
            format_storage_si(total_bytes),
            format_storage_si(base),
            format_storage_si(extra_bytes),
        )
    } else {
        format!("{} \u{2014} {}", name, format_storage_si(total_bytes))
    }
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

    // ── build_plan_label (task 1547 finding 4, Codex review on PR #33) ──────
    //
    // `GET /billing/subscription` exposes only `quota_bytes` (the server-
    // folded total) and `extra_storage_tb` — never a base/bonus split — so a
    // "bonus" inferred client-side from `total_bytes - base - extra` is
    // unsound whenever this CLI's own `Plan::base_storage_bytes()` drifts
    // from what the server actually granted. These tests are RED against
    // the pre-fix `implied_bonus_bytes` behavior (it fabricated a "bonus"
    // line here) and GREEN against the fix: no split is ever shown unless
    // base + extra exactly accounts for the server's total.

    #[test]
    fn build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted() {
        // `total_bytes` is the server's authoritative `quota_bytes` off
        // `GET /billing/subscription` — it ALREADY includes base + extra +
        // any referral bonus. Passing it straight through must not be added
        // on top of the plan's base storage again.
        let total_bytes = 6_000_000_000; // some server-granted total unrelated to Free's 5 GB base
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
    fn build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total() {
        // extra_storage_tb is a real, server-reported field: when base +
        // extra reconciles exactly with the server's total, showing the
        // breakdown is honest.
        let extra_tb = 2;
        let total_bytes = Plan::Pro.base_storage_bytes() + extra_tb * beebeeb_types::quota::ONE_TB;
        let label = build_plan_label(Plan::Pro, extra_tb, total_bytes);
        assert!(label.contains(&format_storage_si(total_bytes)), "label: {label}");
        assert!(label.contains("extra"), "label: {label}");
        assert!(label.contains("base"), "label: {label}");
    }

    /// RED-first regression (Codex, PR #33, task 1547 finding 2): a server
    /// total that does NOT reconcile with `base + extra` — e.g. a real
    /// referral bonus folded server-side, or this CLI's `base` drifting from
    /// what the server actually granted — must never be presented as a
    /// "bonus". Before the fix, `implied_bonus_bytes` happily printed
    /// "… (1.0 TB base + 2.0 TB extra + 500.0 MB bonus)" here. After the
    /// fix, an unreconciled remainder collapses to plan + total only — no
    /// fabricated breakdown, no word "bonus" anywhere in this CLI.
    #[test]
    fn build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder() {
        let extra_tb = 2;
        let unexplained_remainder = 500_000_000i64; // e.g. a referral bonus, or core-version drift
        let total_bytes =
            Plan::Pro.base_storage_bytes() + extra_tb * beebeeb_types::quota::ONE_TB + unexplained_remainder;
        let label = build_plan_label(Plan::Pro, extra_tb, total_bytes);
        assert!(
            !label.contains("bonus"),
            "must never fabricate a bonus label for an unreconciled remainder: {label}"
        );
        assert!(
            label.contains(&format_storage_si(total_bytes)),
            "the server's total must still be shown: {label}"
        );
    }

    #[test]
    fn build_plan_label_with_no_extra_shows_just_plan_and_total() {
        let total_bytes = Plan::Basic.base_storage_bytes();
        let label = build_plan_label(Plan::Basic, 0, total_bytes);
        assert_eq!(label, format!("Basic \u{2014} {}", format_storage_si(total_bytes)));
    }
}
