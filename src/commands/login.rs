use colored::Colorize;
use std::io::{self, Write};

use crate::api::ApiClient;
use crate::config::{load_config, save_config};

pub async fn run() -> Result<(), String> {
    // Prompt for email
    print!("{}", "  email: ".truecolor(106, 101, 91));
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut email = String::new();
    io::stdin()
        .read_line(&mut email)
        .map_err(|e| format!("failed to read email: {e}"))?;
    let email = email.trim().to_string();

    if email.is_empty() {
        return Err("email cannot be empty".to_string());
    }

    // Prompt for password (hidden)
    let password = rpassword::prompt_password(
        format!("{}", "  password: ".truecolor(106, 101, 91)),
    )
    .map_err(|e| format!("failed to read password: {e}"))?;

    if password.is_empty() {
        return Err("password cannot be empty".to_string());
    }

    let api = ApiClient::from_config();
    let result = api.login(&email, &password).await?;

    let token = result
        .get("session_token")
        .and_then(|v| v.as_str())
        .ok_or("server did not return a session token")?;

    // Save to config
    let mut config = load_config();
    config.session_token = Some(token.to_string());
    config.email = Some(email.clone());
    save_config(&config)?;

    // Fetch region for display
    let region = api
        .get_region()
        .await
        .ok()
        .and_then(|v| v.get("region").and_then(|r| r.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown".to_string());

    println!();
    println!(
        "  {} {}",
        "Logged in as".truecolor(143, 193, 139),
        format!("{email} · {region}").truecolor(245, 184, 0),
    );

    Ok(())
}
