use colored::Colorize;

use crate::api::ApiClient;
use crate::config::load_config;

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

    // Fetch all three in parallel
    let (me_res, sub_res, region_res, sessions_res) = tokio::join!(
        api.get_me(),
        api.get_subscription(),
        api.get_region(),
        api.get_sessions(),
    );

    let me = me_res.unwrap_or_default();
    let sub = sub_res.unwrap_or_default();
    let region = region_res.unwrap_or_default();
    let sessions = sessions_res.unwrap_or_default();

    // ── Parse fields ─────────────────────────────────────────────────────────

    let email = me
        .get("email").and_then(|v| v.as_str()).unwrap_or("unknown");
    let email_verified = me
        .get("email_verified").and_then(|v| v.as_bool()).unwrap_or(false);

    let plan = sub
        .get("plan").and_then(|v| v.as_str()).unwrap_or("free");
    let quota_bytes = sub
        .get("quota_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let plan_label = format!("{} ({})", capitalise(plan), format_bytes(quota_bytes));

    let city = region
        .get("city").and_then(|v| v.as_str()).unwrap_or("unknown");
    let provider = region
        .get("provider").and_then(|v| v.as_str()).unwrap_or("unknown");
    let region_label = format!("{city} · {provider}");

    // Auth method: PAT vs session (token format tells us PAT; OPAQUE vs
    // legacy requires a server field the /me endpoint doesn't expose yet,
    // so we show the token type instead).
    let token = config.session_token.as_deref().unwrap_or("");
    let auth_label = if token.starts_with("bb_pat_") {
        "Personal Access Token".to_string()
    } else {
        "session token".to_string()
    };

    // Session expiry — find the current session in the sessions list
    let current_session = sessions
        .get("sessions")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.iter().find(|s| s.get("is_current").and_then(|c| c.as_bool()).unwrap_or(false)));

    let expires_label = current_session
        .and_then(|s| s.get("expires_at"))
        .and_then(|v| v.as_str())
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .map(|exp| {
            let days = (exp.signed_duration_since(chrono::Utc::now())).num_days();
            if days <= 0 {
                "expired".to_string()
            } else if days == 1 {
                "expires in 1 day".to_string()
            } else {
                format!("expires in {days} days")
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    // ── Print ─────────────────────────────────────────────────────────────────

    let dim = |s: &str| s.custom_color(crate::colors::INK_DIM);
    let val = |s: &str| s.custom_color(crate::colors::INK);

    println!();
    println!("  {} {}", dim("email   "), {
        let display = if email_verified {
            email.custom_color(crate::colors::INK)
        } else {
            email.custom_color(crate::colors::RED_ERR) // red if unverified
        };
        if email_verified { display } else {
            // append unverified note
            format!("{} {}", email, "(unverified)".custom_color(crate::colors::RED_ERR)).custom_color(crate::colors::INK)
        }
    });
    println!("  {} {}", dim("plan    "), plan_label.custom_color(crate::colors::AMBER));
    println!("  {} {}", dim("region  "), val(&region_label));
    println!("  {} {}", dim("auth    "), val(&auth_label));
    println!("  {} {}", dim("session "), val(&expires_label));
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
        format!("{:.0} TB", bytes as f64 / TB as f64)
    } else {
        format!("{:.0} GB", bytes as f64 / GB as f64)
    }
}
