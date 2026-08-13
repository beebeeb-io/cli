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
    let bonus_bytes = sub.get("bonus_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let total_bytes = effective_quota(plan, extra_tb, bonus_bytes);
    let plan_label = build_plan_label(plan, extra_tb, bonus_bytes);

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
                "bonus_bytes": bonus_bytes,
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
fn build_plan_label(plan: Plan, extra_tb: i64, bonus_bytes: i64) -> String {
    let name = capitalise(plan.slug());
    let total = effective_quota(plan, extra_tb, bonus_bytes);
    let base = plan.base_storage_bytes();

    if extra_tb > 0 || bonus_bytes > 0 {
        let mut parts = vec![format!("{} base", format_storage_si(base))];
        if extra_tb > 0 {
            parts.push(format!(
                "{} extra",
                format_storage_si(extra_tb * beebeeb_types::quota::ONE_TB)
            ));
        }
        if bonus_bytes > 0 {
            parts.push(format!("{} bonus", format_storage_si(bonus_bytes)));
        }
        format!("{} \u{2014} {} ({})", name, format_storage_si(total), parts.join(" + "),)
    } else {
        format!("{} \u{2014} {}", name, format_storage_si(total))
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn upload_limit_for_plan(plan: Plan) -> &'static str {
    match plan {
        Plan::Free => "5 GB/hr",
        Plan::Basic => "20 GB/hr",
        Plan::Pro => "50 GB/hr",
        Plan::Business => "100 GB/hr",
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
