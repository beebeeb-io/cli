use colored::Colorize;

use crate::config::{load_config, save_config};
use crate::env_detect::is_headless;

// ─── WebSocket device-auth login ─────────────────────────────────────────────

async fn browser_login(headless_flag: bool) -> Result<(), String> {
    use aes_gcm::aead::generic_array::GenericArray;
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit};
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use futures_util::{SinkExt, StreamExt};
    use p256::ecdh::EphemeralSecret;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use p256::PublicKey;
    use rand::rngs::OsRng;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let headless = headless_flag || is_headless();

    // 1. Generate ephemeral P-256 key pair
    let secret = EphemeralSecret::random(&mut OsRng);
    let public_key = secret.public_key();
    let pub_key_b64 = B64.encode(public_key.to_encoded_point(false).as_bytes());

    // 2. Connect WebSocket
    let config = load_config();
    let api_url = config.api_url;
    let ws_url = api_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let ws_url = format!("{ws_url}/api/v1/auth/cli");

    println!(
        "  {} Connecting to Beebeeb...",
        "→".custom_color(crate::colors::AMBER)
    );

    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .map_err(|e| format!("WebSocket connection failed: {e}"))?;

    // 3. Send CLI public key so browser can do ECDH on its end
    let init_msg = serde_json::json!({ "ecdh_public_key_b64": pub_key_b64 });
    ws_stream
        .send(Message::Text(init_msg.to_string()))
        .await
        .map_err(|e| format!("Send failed: {e}"))?;

    // 4. Receive device code + verification URI from server
    let msg = ws_stream
        .next()
        .await
        .ok_or("Connection closed before code was received")?
        .map_err(|e| format!("WS error: {e}"))?;
    let text = match msg {
        Message::Text(t) => t,
        _ => return Err("Unexpected message type (expected text)".into()),
    };
    let resp: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid response: {e}"))?;

    if let Some(err) = resp["error"].as_str() {
        return Err(format!("Auth error: {err}"));
    }

    let user_code = resp["user_code"]
        .as_str()
        .ok_or("Missing user_code")?
        .to_string();
    let verification_uri = resp["verification_uri"]
        .as_str()
        .ok_or("Missing verification_uri")?
        .to_string();
    let expires_in: u64 = resp["expires_in"].as_u64().unwrap_or(300);

    // 5. Display code and (optionally) open the browser. Print first so the
    //    user *always* has the escape hatch, even when open::that returns
    //    Ok but no window actually appears (other-spaces macOS, sleeping
    //    display, wrong-profile Chrome, etc.).
    if headless {
        print_headless_block(&user_code, &verification_uri);
    } else {
        print_browser_block(&user_code, &verification_uri);
        // Best-effort — we already showed the URL above.
        let _ = open::that(&verification_uri);
    }

    // 6. Wait up to `expires_in` seconds (default 5 min) for browser to authorize.
    //    Spawn a countdown task that repaints "[ expires in M:SS ]" once per
    //    second on the same line. The task ends when the receive future
    //    completes, regardless of outcome.
    let countdown_handle = spawn_countdown(expires_in);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(expires_in),
        ws_stream.next(),
    )
    .await;

    countdown_handle.abort();
    // Clear the countdown line so it doesn't bleed into the next print.
    eprint!("\r\x1b[2K");

    let result_msg = result
        .map_err(|_| {
            "Timed out waiting for browser authorization (5 min). Run `bb login` again."
                .to_string()
        })?
        .ok_or("Connection closed before authorization completed")?
        .map_err(|e| format!("WS error: {e}"))?;

    let result_text = match result_msg {
        Message::Text(t) => t,
        _ => return Err("Unexpected message type (expected text)".into()),
    };
    let result: serde_json::Value =
        serde_json::from_str(&result_text).map_err(|e| format!("Invalid result: {e}"))?;

    if let Some(err) = result["error"].as_str() {
        return Err(format!("Auth error: {err}"));
    }

    let nonce_b64 = result["nonce_b64"].as_str().ok_or("Missing nonce_b64")?;
    let payload_b64 = result["encrypted_payload_b64"]
        .as_str()
        .ok_or("Missing encrypted_payload_b64")?;
    let browser_pub_b64 = result["browser_ecdh_public_b64"]
        .as_str()
        .ok_or("Missing browser_ecdh_public_b64")?;

    // 7. ECDH key agreement + AES-256-GCM decrypt — unchanged from previous version.
    let browser_pub_bytes = B64
        .decode(browser_pub_b64)
        .map_err(|e| format!("Invalid browser public key encoding: {e}"))?;
    let browser_pub_key = PublicKey::from_sec1_bytes(&browser_pub_bytes)
        .map_err(|e| format!("Invalid browser public key (not on curve): {e}"))?;

    let shared_secret = secret.diffie_hellman(&browser_pub_key);
    let shared_bytes = shared_secret.raw_secret_bytes();

    use hkdf::Hkdf;
    use sha2::Sha256;

    let hk = Hkdf::<Sha256>::new(None, shared_bytes);
    let mut key_bytes = [0u8; 32];
    hk.expand(b"beebeeb-cli-auth-v1", &mut key_bytes)
        .expect("HKDF expand failed — output length is valid");
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&key_bytes));
    let nonce_bytes = B64
        .decode(nonce_b64)
        .map_err(|e| format!("Invalid nonce encoding: {e}"))?;
    let ciphertext = B64
        .decode(payload_b64)
        .map_err(|e| format!("Invalid payload encoding: {e}"))?;
    let plaintext = cipher
        .decrypt(GenericArray::from_slice(&nonce_bytes), ciphertext.as_ref())
        .map_err(|_| "Decryption failed — ECDH key mismatch or corrupted ciphertext")?;

    // 8. Parse credentials and persist — unchanged.
    let creds: serde_json::Value =
        serde_json::from_slice(&plaintext).map_err(|e| format!("Invalid credentials JSON: {e}"))?;

    let session_token = creds["session_token"]
        .as_str()
        .ok_or("Missing session_token in credentials")?;
    let master_key_b64 = creds["master_key_b64"]
        .as_str()
        .ok_or("Missing master_key_b64 in credentials")?;
    let email = creds["email"]
        .as_str()
        .ok_or("Missing email in credentials")?;

    let mut config = load_config();
    config.session_token = Some(session_token.to_string());
    config.master_key = Some(master_key_b64.to_string());
    config.email = Some(email.to_string());
    save_config(&config)?;

    println!();
    println!(
        "  {} Logged in as {}",
        "✓".green(),
        email.custom_color(crate::colors::AMBER)
    );

    Ok(())
}

// ─── Output blocks ───────────────────────────────────────────────────────────

fn print_browser_block(user_code: &str, verification_uri: &str) {
    println!();
    println!(
        "  {} Opening Beebeeb in your browser...",
        "→".custom_color(crate::colors::AMBER)
    );
    println!();
    println!(
        "  {} {}",
        "Authorization code:".custom_color(crate::colors::INK_SAGE),
        user_code.bold().custom_color(crate::colors::AMBER)
    );
    println!(
        "  {} {}",
        "URL:               ".custom_color(crate::colors::INK_SAGE),
        verification_uri.custom_color(crate::colors::INK_SAGE)
    );
    println!();
    println!(
        "  {}",
        "If your browser did not open, paste the URL above into any"
            .custom_color(crate::colors::INK_DIM)
    );
    println!(
        "  {}",
        "signed-in browser. The code shown there must match the one above."
            .custom_color(crate::colors::INK_DIM)
    );
    println!();
}

fn print_headless_block(user_code: &str, verification_uri: &str) {
    println!();
    println!(
        "  {}",
        "No browser detected on this machine.".custom_color(crate::colors::INK)
    );
    println!(
        "  {}",
        "Open this URL in a browser on any device you trust:"
            .custom_color(crate::colors::INK)
    );
    println!();
    println!(
        "      {}",
        verification_uri.custom_color(crate::colors::AMBER)
    );
    println!();
    println!(
        "  When prompted, confirm the code shown:  {}",
        user_code.bold().custom_color(crate::colors::AMBER)
    );
    println!();
    println!(
        "  {}",
        "(If you are not signed in there, you will be asked to sign in"
            .custom_color(crate::colors::INK_DIM)
    );
    println!(
        "  {}",
        " and complete two-factor authentication first.)"
            .custom_color(crate::colors::INK_DIM)
    );
    println!();
}

// ─── Countdown updater ───────────────────────────────────────────────────────

/// Spawn a task that repaints "Waiting... [ expires in M:SS ]" once per second
/// on the same terminal line. The caller is responsible for aborting the handle
/// when the wait is over.
fn spawn_countdown(total_secs: u64) -> tokio::task::JoinHandle<()> {
    use std::io::Write;
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        loop {
            let elapsed = start.elapsed().as_secs();
            if elapsed >= total_secs {
                break;
            }
            let remaining = total_secs - elapsed;
            let m = remaining / 60;
            let s = remaining % 60;
            // Carriage return + clear line + paint.
            eprint!(
                "\r\x1b[2K  {} Waiting for confirmation...  [ expires in {}:{:02} ]",
                "→".custom_color(crate::colors::AMBER),
                m,
                s
            );
            let _ = std::io::stderr().flush();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
}

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn run(headless: bool) -> Result<(), String> {
    // If already logged in with a valid session, say so and return early.
    let config = load_config();
    if let Some(email) = &config.email {
        if config.session_token.is_some() {
            let api = crate::api::ApiClient::from_config();
            match api.get_me().await {
                Ok(_) => {
                    println!(
                        "  {} Already logged in as {}",
                        "✓".green(),
                        email.custom_color(crate::colors::AMBER)
                    );
                    println!(
                        "  {}",
                        "Run `bb logout` first to switch accounts."
                            .custom_color(crate::colors::INK_DIM)
                    );
                    return Ok(());
                }
                Err(_) => {
                    println!(
                        "  {}",
                        "Session expired. Re-authenticating..."
                            .custom_color(crate::colors::INK_DIM)
                    );
                }
            }
        }
    }

    browser_login(headless).await
}
