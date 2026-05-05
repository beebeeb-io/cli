use colored::Colorize;

use crate::api::ApiClient;

pub async fn run() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Fetch usage + file count in parallel
    let (usage_res, count_res) = tokio::join!(api.get_usage(), api.get_file_count());

    let usage = usage_res?;
    let count = count_res.unwrap_or_default();

    let used_bytes = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let quota_bytes = usage.get("quota_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let percentage = usage.get("percentage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let file_count: i64 = count
        .get("count").and_then(|v| v.as_i64())
        .or_else(|| count.get("total").and_then(|v| v.as_i64()))
        .unwrap_or(0);

    let dim = |s: &str| s.truecolor(106, 101, 91);

    // Color-code the percentage: green <70%, amber 70-90%, red >90%
    let pct_str = if quota_bytes <= 0 {
        "—".truecolor(106, 101, 91)
    } else {
        let s = format!("{:.2}%", percentage * 100.0);
        if percentage >= 0.90 {
            s.truecolor(224, 122, 106)  // red
        } else if percentage >= 0.70 {
            s.truecolor(245, 184, 0)    // amber
        } else {
            s.truecolor(143, 193, 139)  // green
        }
    };

    let used_str = format_bytes(used_bytes);
    let quota_str = if quota_bytes <= 0 {
        "unlimited".to_string()
    } else {
        format_bytes(quota_bytes)
    };

    let files_str = if file_count > 0 {
        format_number(file_count)
    } else {
        "—".to_string()
    };

    println!();
    println!("  {} {}", dim("used    "), used_str.truecolor(233, 230, 221));
    println!("  {} {}", dim("quota   "), quota_str.truecolor(208, 200, 154));
    println!("  {} {}", dim("percent "), pct_str);
    println!("  {} {}", dim("files   "), files_str.truecolor(106, 101, 91));

    // Over-quota warning
    if quota_bytes > 0 && used_bytes >= quota_bytes {
        println!();
        println!(
            "  {} {}",
            "⚠".truecolor(224, 122, 106),
            "Over quota — uploads blocked. Upgrade your plan or delete files."
                .truecolor(224, 122, 106),
        );
    }

    println!();
    Ok(())
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

/// Format a number with thousands separators, e.g. 1234 → "1,234".
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
