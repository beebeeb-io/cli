use base64::Engine;
use colored::Colorize;
use std::io::{self, Write};

use crate::api::ApiClient;
use crate::config::{load_config, save_config};

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Try OPAQUE login. Returns Ok(session_token) on success, Err on failure.
async fn opaque_login(api: &ApiClient, email: &str, password: &[u8]) -> Result<String, String> {
    // Step 1: Client starts OPAQUE login
    let start =
        beebeeb_core::opaque_protocol::client_login_start(password).map_err(|e| e.to_string())?;

    let client_message_b64 = b64().encode(&start.message);

    // Step 2: Send to server, get credential response + server state
    let server_resp = api
        .opaque_login_start(email, &client_message_b64)
        .await?;

    let server_message_b64 = server_resp
        .get("server_message")
        .and_then(|v| v.as_str())
        .ok_or("server did not return server_message")?;

    let server_state_b64 = server_resp
        .get("server_state")
        .and_then(|v| v.as_str())
        .ok_or("server did not return server_state")?;

    let server_message = b64()
        .decode(server_message_b64)
        .map_err(|e| format!("invalid base64 in server_message: {e}"))?;

    // Step 3: Client finishes OPAQUE login
    let finish = beebeeb_core::opaque_protocol::client_login_finish(
        &start.state,
        password,
        &server_message,
    )
    .map_err(|e| e.to_string())?;

    let client_finish_b64 = b64().encode(&finish.message);

    // Step 4: Send finalization to server, get session token
    let login_resp = api
        .opaque_login_finish(email, &client_finish_b64, server_state_b64)
        .await?;

    let token = login_resp
        .get("session_token")
        .and_then(|v| v.as_str())
        .ok_or("server did not return a session token")?;

    Ok(token.to_string())
}

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
    let password = rpassword::prompt_password(format!(
        "{}",
        "  password: ".truecolor(106, 101, 91)
    ))
    .map_err(|e| format!("failed to read password: {e}"))?;

    if password.is_empty() {
        return Err("password cannot be empty".to_string());
    }

    let api = ApiClient::from_config();
    let password_bytes = password.as_bytes();

    // Try OPAQUE first, fall back to legacy for accounts not yet migrated
    let token = match opaque_login(&api, &email, password_bytes).await {
        Ok(t) => t,
        Err(_opaque_err) => {
            // Legacy plaintext-password login for pre-OPAQUE accounts
            let result = api.login(&email, &password).await?;
            result
                .get("session_token")
                .and_then(|v| v.as_str())
                .ok_or("server did not return a session token")?
                .to_string()
        }
    };

    // Save to config
    let mut config = load_config();
    config.session_token = Some(token);
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
