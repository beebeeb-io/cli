use base64::Engine;
use colored::Colorize;
use std::io::{self, Write};

use crate::api::ApiClient;
use crate::config::{load_config, save_config};

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

struct LoginResult {
    token: String,
    master_key_b64: String,
}

/// Try OPAQUE login. Returns Ok(LoginResult) on success, Err on failure.
/// The OPAQUE export_key is deterministic for the same password and is used
/// as the master encryption key (first 32 bytes of the 64-byte export_key).
async fn opaque_login(api: &ApiClient, email: &str, password: &[u8]) -> Result<LoginResult, String> {
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

    // Derive master key from the OPAQUE export_key (first 32 bytes).
    // The export_key is deterministic for the same password+registration,
    // making it ideal as the root of our key hierarchy.
    let export_key = &finish.export_key;
    if export_key.len() < 32 {
        return Err("OPAQUE export key too short".to_string());
    }
    let mut mk_bytes = [0u8; 32];
    mk_bytes.copy_from_slice(&export_key[..32]);
    let master_key_b64 = b64().encode(mk_bytes);

    Ok(LoginResult {
        token: token.to_string(),
        master_key_b64,
    })
}

/// Legacy login for pre-OPAQUE accounts. Derives master key from password
/// using Argon2id with the salt returned by the server.
async fn legacy_login(api: &ApiClient, email: &str, password: &str) -> Result<LoginResult, String> {
    let result = api.login(email, password).await?;

    let token = result
        .get("session_token")
        .and_then(|v| v.as_str())
        .ok_or("server did not return a session token")?
        .to_string();

    let salt_hex = result
        .get("salt")
        .and_then(|v| v.as_str())
        .ok_or("server did not return salt")?;

    // Decode hex salt
    let salt_bytes: Vec<u8> = (0..salt_hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&salt_hex[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|e| format!("invalid salt hex: {e}"))?;

    // Derive master key via Argon2id (same as beebeeb-core KDF)
    let mk = beebeeb_core::kdf::derive_master_key(password, &salt_bytes)
        .map_err(|e| format!("key derivation failed: {e}"))?;
    let master_key_b64 = b64().encode(mk.to_bytes());

    Ok(LoginResult {
        token,
        master_key_b64,
    })
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
    let login_result = match opaque_login(&api, &email, password_bytes).await {
        Ok(r) => r,
        Err(_opaque_err) => {
            legacy_login(&api, &email, &password).await?
        }
    };

    // Save to config (session token + master key)
    let mut config = load_config();
    config.session_token = Some(login_result.token);
    config.email = Some(email.clone());
    config.master_key = Some(login_result.master_key_b64);
    save_config(&config)?;

    // Fetch region for display (use a new client with the saved token)
    let api = ApiClient::from_config();
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
