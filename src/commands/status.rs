use colored::Colorize;

use crate::api::ApiClient;
use crate::config::load_config;

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
    let dim = |s: &str| s.truecolor(106, 101, 91);
    let text = |s: &str| s.truecolor(233, 230, 221);

    // User / server from config
    let email = config.email.as_deref().unwrap_or("not logged in");
    let server = &config.api_url;

    println!();
    println!("  {}", "beebeeb status".truecolor(245, 184, 0));
    println!();
    println!("  {}  {}", dim("user   "), text(email));
    println!("  {}  {}", dim("server "), text(server));

    // Session validity — requires auth
    if config.session_token.is_none() {
        println!("  {}  {}", dim("session"), "no session".truecolor(224, 122, 106));
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
                    "  {}  {}",
                    dim("session"),
                    format!("valid ({exp})").truecolor(143, 193, 139),
                ),
                None => println!(
                    "  {}  {}",
                    dim("session"),
                    "valid".truecolor(143, 193, 139),
                ),
            }
        }
        Err(_) => {
            println!(
                "  {}  {}",
                dim("session"),
                "expired or invalid".truecolor(224, 122, 106),
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

            println!(
                "  {}  {}",
                dim("storage"),
                text(&format!(
                    "{} / {} ({:.1}%)",
                    format_bytes(used),
                    format_bytes(limit),
                    pct,
                )),
            );
        }
        Err(_) => {
            println!("  {}  {}", dim("storage"), dim("unavailable"));
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
            println!("  {}  {}", dim("files  "), text(&count.to_string()));
        }
        Err(_) => {
            println!("  {}  {}", dim("files  "), dim("unavailable"));
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
