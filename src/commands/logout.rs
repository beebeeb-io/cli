use colored::Colorize;

use crate::api::ApiClient;
use crate::config::{clear_config, load_config};

pub async fn run() -> Result<(), String> {
    let config = load_config();
    // Signed out = no session AND nothing stored anywhere. A keyring that could not be read this run still has an
    // entry (and a marker in config.json) that logout must remove (LD-0029, F6).
    if config.session_token.is_none() && config.secrets_location.is_none() && !crate::config::has_stored_sign_in() {
        println!("  {}", "Already logged out.".custom_color(crate::colors::INK_DIM),);
        return Ok(());
    }

    if config.session_token.is_some() {
        let api = ApiClient::from_config();
        // Best-effort server-side logout; clear the local sign-in regardless
        let _ = api.logout().await;
    }

    // The keyring entry (or credentials.json), then config.json.
    clear_config()?;

    let email = config.email.as_deref().unwrap_or("unknown");
    println!(
        "  {} {}",
        "Logged out".custom_color(crate::colors::GREEN_OK),
        format!("· session for {email} ended").custom_color(crate::colors::INK_DIM),
    );

    Ok(())
}
