//! `bb config`: show the stored config, with the session token truncated.

use colored::Colorize;

use crate::config::{SecretsLocation, load_config};

/// Where the key and session are, in words (LD-0029). The unencrypted cases say so.
fn secrets_location(location: Option<&SecretsLocation>) -> (&'static str, String) {
    match location {
        Some(SecretsLocation::Keyring) => ("keyring", format!("OS keyring ({})", crate::credentials::SERVICE)),
        Some(SecretsLocation::File(path)) => ("file", format!("{} (unencrypted)", path.display())),
        Some(SecretsLocation::LegacyConfig(path)) => {
            ("config", format!("{} (unencrypted, not moved yet)", path.display()))
        }
        None => ("none", "(none)".to_string()),
    }
}

pub async fn run() -> Result<(), String> {
    let config = load_config();

    let config_path = crate::config::config_path();
    let (store_key, store_text) = secrets_location(config.secrets_location.as_ref());

    // ── JSON output ──────────────────────────────────────────────────────
    if crate::ui::is_json() {
        let truncated_token = config.session_token.as_deref().map(String::as_str).map(|t| {
            if t.len() > 16 {
                format!("{}...{}", &t[..8], &t[t.len() - 4..])
            } else {
                "(set)".to_string()
            }
        });

        let json = serde_json::json!({
            "api_url": config.api_url,
            "email": config.email,
            "session_token": truncated_token,
            "master_key_set": config.master_key.is_some(),
            "credential_store": store_key,
            "config_path": config_path.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap_or_default());
        return Ok(());
    }

    // ── Rich output ──────────────────────────────────────────────────────
    let dim = |s: &str| s.custom_color(crate::colors::INK_DIM);
    let text = |s: &str| s.custom_color(crate::colors::INK);

    println!();
    println!("  {}", "beebeeb config".custom_color(crate::colors::AMBER));
    println!();

    println!("  {}  {}", dim("api_url      "), text(&config.api_url));

    println!(
        "  {}  {}",
        dim("email        "),
        text(config.email.as_deref().unwrap_or("(none)")),
    );

    println!(
        "  {}  {}",
        dim("session_token"),
        match &config.session_token {
            Some(t) => {
                // Show first 8 and last 4 chars, mask the rest
                if t.len() > 16 {
                    format!("{}...{}", &t[..8], &t[t.len() - 4..]).custom_color(crate::colors::INK)
                } else {
                    "(set)".custom_color(crate::colors::INK)
                }
            }
            None => "(none)".custom_color(crate::colors::INK_DIM),
        },
    );

    println!(
        "  {}  {}",
        dim("master_key   "),
        match &config.master_key {
            Some(_) => "(set, hidden)".custom_color(crate::colors::GREEN_OK),
            None => "(none)".custom_color(crate::colors::INK_DIM),
        },
    );

    println!(
        "  {}  {}",
        dim("stored in    "),
        match config.secrets_location {
            Some(SecretsLocation::Keyring) => store_text.custom_color(crate::colors::GREEN_OK),
            Some(_) => store_text.custom_color(crate::colors::RED_ERR),
            None => store_text.custom_color(crate::colors::INK_DIM),
        },
    );

    println!();
    println!(
        "  {}  {}",
        dim("path         "),
        dim(&config_path.display().to_string()),
    );

    println!();
    Ok(())
}
