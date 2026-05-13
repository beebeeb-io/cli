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

    let email = me
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let plan = sub
        .get("plan")
        .and_then(|v| v.as_str())
        .unwrap_or("free");
    let quota_bytes = sub
        .get("quota_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let plan_label = format!("{} ({})", capitalise(plan), format_bytes(quota_bytes));

    let region_label = my_region
        .get("preferred_region")
        .and_then(|v| v.as_str())
        .map(|s| capitalise(s))
        .unwrap_or_else(|| "Europe".to_string());

    let used_bytes = usage
        .get("used_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_bytes = usage
        .get("quota_bytes")
        .or_else(|| usage.get("plan_limit_bytes"))
        .and_then(|v| v.as_i64())
        .unwrap_or(quota_bytes); // fall back to subscription quota
    let percentage = if total_bytes > 0 {
        used_bytes as f64 / total_bytes as f64
    } else {
        0.0
    };
    let storage_label = format!(
        "{} / {} ({:.1}%)",
        ui::human_size(used_bytes.max(0) as u64),
        format_bytes(total_bytes),
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
    let current_session = sessions
        .get("sessions")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|s| {
                s.get("is_current")
                    .and_then(|c| c.as_bool())
                    .unwrap_or(false)
            })
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
                "plan": plan,
                "storage_used": used_bytes,
                "storage_total": total_bytes,
                "files": file_count,
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
        "  {} {} {:.1}%",
        "         ", // align under "storage"
        ui::quota_bar(used_bytes.max(0) as u64, total_bytes.max(0) as u64, 40),
        percentage * 100.0,
    );

    // File count
    println!(
        "  {} {}",
        dim("files   "),
        val(&format_number(file_count))
    );

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
    if bytes <= 0 {
        return "unlimited".to_string();
    }
    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else {
        format!("{:.0} GB", bytes as f64 / GB as f64)
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
