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
    let quota_bytes = usage
        .get("quota_bytes")
        .or_else(|| usage.get("plan_limit_bytes"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
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

    let plan_name = sub
        .get("plan")
        .and_then(|v| v.as_str())
        .unwrap_or("free");

    // ── JSON mode ────────────────────────────────────────────────────────────

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "used_bytes": used_bytes,
                "quota_bytes": quota_bytes,
                "percentage": percentage * 100.0,
                "files": file_count,
                "plan": plan_name,
            }))
            .unwrap()
        );
        return Ok(());
    }

    // ── Quiet mode ───────────────────────────────────────────────────────────

    if ui::is_quiet() {
        println!("{}", format_bytes(used_bytes));
        println!("{}", format_bytes(quota_bytes));
        println!("{:.2}%", percentage * 100.0);
        return Ok(());
    }

    // ── Rich mode ────────────────────────────────────────────────────────────

    let dim = |s: &str| s.custom_color(crate::colors::INK_DIM);

    let used_str = format_bytes(used_bytes);
    let quota_str = if quota_bytes <= 0 {
        "unlimited".to_string()
    } else {
        format_bytes(quota_bytes)
    };

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
        dim("used    "),
        used_str.custom_color(crate::colors::INK)
    );
    println!(
        "  {} {}",
        dim("quota   "),
        quota_str.custom_color(crate::colors::INK_WARM)
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
    println!(
        "  {} {}",
        dim("plan    "),
        capitalise(plan_name).custom_color(crate::colors::AMBER)
    );

    // Over-quota warning
    if quota_bytes > 0 && used_bytes >= quota_bytes {
        println!();
        println!(
            "  {} {}",
            "!".custom_color(crate::colors::RED_ERR),
            "Over quota \u{2014} uploads blocked. Upgrade your plan or delete files."
                .custom_color(crate::colors::RED_ERR),
        );
    }

    println!();
    Ok(())
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn format_bytes(bytes: i64) -> String {
    const TB: i64 = 1_099_511_627_776;
    const GB: i64 = 1_073_741_824;
    const MB: i64 = 1_048_576;
    const KB: i64 = 1_024;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} kB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
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
