use colored::Colorize;

use crate::api::ApiClient;
use crate::config::{clear_config, load_config};

pub async fn run() -> Result<(), String> {
    let config = load_config();
    if config.session_token.is_none() {
        println!(
            "  {}",
            "Already logged out.".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    }

    let api = ApiClient::from_config();

    // Best-effort server-side logout; clear config regardless
    let _ = api.logout().await;

    clear_config()?;

    let email = config.email.as_deref().unwrap_or("unknown");
    println!(
        "  {} {}",
        "Logged out".custom_color(crate::colors::GREEN_OK),
        format!("· session for {email} ended").custom_color(crate::colors::INK_DIM),
    );

    Ok(())
}
