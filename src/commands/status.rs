use colored::Colorize;

use crate::api::ApiClient;
use crate::config::load_config;
use crate::ui;

/// Format bytes into a human-readable string (e.g. "142 MB", "1.2 GB").
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * KB;
    const GB: u64 = 1_024 * MB;
    const TB: u64 = 1_024 * GB;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

pub async fn run() -> Result<(), String> {
    let config = load_config();

    // User / server from config
    let email = config.email.as_deref().unwrap_or("not logged in");
    let server = &config.api_url;

    // JSON mode: collect all data, emit at end
    if ui::is_json() {
        let mut json_out = serde_json::json!({
            "email": email,
            "server": server,
        });

        if config.session_token.is_some() {
            let api = ApiClient::from_config();
            json_out["session"] = match api.get_me().await {
                Ok(_) => serde_json::json!("valid"),
                Err(_) => serde_json::json!("expired"),
            };
            if let Ok(usage) = api.get_usage().await {
                json_out["used_bytes"] = usage.get("used_bytes").cloned().unwrap_or(serde_json::json!(0));
                json_out["plan_limit_bytes"] = usage.get("plan_limit_bytes").cloned().unwrap_or(serde_json::json!(0));
            }
        } else {
            json_out["session"] = serde_json::json!("none");
        }
        println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
        return Ok(());
    }

    if ui::is_quiet() {
        if config.session_token.is_some() {
            let api = ApiClient::from_config();
            match api.get_me().await {
                Ok(_) => println!("ok {email}"),
                Err(_) => println!("expired {email}"),
            }
        } else {
            println!("no session");
        }
        return Ok(());
    }

    // Rich mode
    println!();
    println!("  {}", "beebeeb status".custom_color(crate::colors::AMBER));
    println!();
    println!(
        "  {}  {}",
        "user   ".custom_color(crate::colors::INK_DIM),
        email.custom_color(crate::colors::INK),
    );
    println!(
        "  {}  {}",
        "server ".custom_color(crate::colors::INK_DIM),
        server.custom_color(crate::colors::INK),
    );

    // Session validity -- requires auth
    if config.session_token.is_none() {
        println!(
            "  {}  {}",
            "session".custom_color(crate::colors::INK_DIM),
            "no session".custom_color(crate::colors::RED_ERR),
        );
        println!();
        return Ok(());
    }

    let api = ApiClient::from_config();

    // Check session by calling /auth/me
    match api.get_me().await {
        Ok(_) => {
            // Try to get session expiry from /auth/sessions
            let expiry = get_session_expiry(&api).await;
            match expiry {
                Some(exp) => println!(
                    "  {}  {} {}",
                    "session".custom_color(crate::colors::INK_DIM),
                    "\u{25CF}".custom_color(crate::colors::GREEN_OK),
                    format!("valid ({exp})").custom_color(crate::colors::GREEN_OK),
                ),
                None => println!(
                    "  {}  {} {}",
                    "session".custom_color(crate::colors::INK_DIM),
                    "\u{25CF}".custom_color(crate::colors::GREEN_OK),
                    "valid".custom_color(crate::colors::GREEN_OK),
                ),
            }
        }
        Err(_) => {
            println!(
                "  {}  {} {}",
                "session".custom_color(crate::colors::INK_DIM),
                "\u{25CB}".custom_color(crate::colors::RED_ERR),
                "expired or invalid".custom_color(crate::colors::RED_ERR),
            );
            println!();
            return Ok(());
        }
    }

    // Storage usage from /files/usage
    match api.get_usage().await {
        Ok(usage) => {
            let used = usage
                .get("used_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let limit = usage
                .get("plan_limit_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let pct = if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            };

            let pct_color = if pct < 50.0 {
                crate::colors::GREEN_OK
            } else if pct < 80.0 {
                crate::colors::AMBER
            } else {
                crate::colors::RED_ERR
            };

            println!(
                "  {}  {} {} {}",
                "storage".custom_color(crate::colors::INK_DIM),
                format_bytes(used).custom_color(crate::colors::INK),
                format!("/ {}", format_bytes(limit)).custom_color(crate::colors::INK_DIM),
                format!("({:.1}%)", pct).custom_color(pct_color),
            );
        }
        Err(_) => {
            println!(
                "  {}  {}",
                "storage".custom_color(crate::colors::INK_DIM),
                "unavailable".custom_color(crate::colors::INK_DIM),
            );
        }
    }

    // File count from /files (root listing)
    match api.list_files(None).await {
        Ok(files) => {
            let count = files
                .get("files")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!(
                "  {}  {}",
                "files  ".custom_color(crate::colors::INK_DIM),
                count.to_string().custom_color(crate::colors::INK),
            );
        }
        Err(_) => {
            println!(
                "  {}  {}",
                "files  ".custom_color(crate::colors::INK_DIM),
                "unavailable".custom_color(crate::colors::INK_DIM),
            );
        }
    }

    println!();
    Ok(())
}

/// Try to determine session expiry from the sessions endpoint.
/// Returns a human-readable string like "expires in 29d".
async fn get_session_expiry(api: &ApiClient) -> Option<String> {
    let resp = api.get_sessions().await.ok()?;
    let sessions = resp.get("sessions")?.as_array()?;

    // Find the current session
    let current = sessions
        .iter()
        .find(|s| s.get("is_current").and_then(|v| v.as_bool()).unwrap_or(false))?;

    let expires_at = current.get("expires_at")?.as_str()?;

    // Parse the expiry timestamp
    let expires = chrono::DateTime::parse_from_rfc3339(expires_at).ok()?;
    let now = chrono::Utc::now();
    let remaining = expires.signed_duration_since(now);

    if remaining.num_seconds() <= 0 {
        return Some("expired".to_string());
    }

    let days = remaining.num_days();
    if days > 0 {
        Some(format!("expires in {days}d"))
    } else {
        let hours = remaining.num_hours();
        if hours > 0 {
            Some(format!("expires in {hours}h"))
        } else {
            let mins = remaining.num_minutes();
            Some(format!("expires in {mins}m"))
        }
    }
}
