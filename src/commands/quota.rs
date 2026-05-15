use beebeeb_types::quota::{effective_quota, format_storage_si, Plan};
use colored::Colorize;

use crate::api::ApiClient;
use crate::ui;

pub async fn run() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Fetch usage, file count, and subscription in parallel
    let (usage_res, count_res, sub_res) =
        tokio::join!(api.get_usage(), api.get_file_count(), api.get_subscription());

    let usage = usage_res?;
    let count = count_res.unwrap_or_default();
    let sub = sub_res.unwrap_or_default();

    let used_bytes = usage
        .get("used_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let plan_slug = sub
        .get("plan")
        .and_then(|v| v.as_str())
        .unwrap_or("free");
    let plan = Plan::from_slug(plan_slug);
    let extra_tb = sub
        .get("extra_storage_tb")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let bonus_bytes = sub
        .get("bonus_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let quota_bytes = effective_quota(plan, extra_tb, bonus_bytes);

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
    let plan_label = build_plan_label(plan, extra_tb, bonus_bytes);

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
                "bonus_bytes": bonus_bytes,
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
        "  {} {} {:.1}%",
        "         ", // align under labels
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
        format!(
            "{} \u{2014} {} ({})",
            name,
            format_storage_si(total),
            parts.join(" + "),
        )
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
