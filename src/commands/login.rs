use colored::Colorize;

use crate::config::{load_config, save_config};

// ─── WebSocket device-auth login ─────────────────────────────────────────────

async fn browser_login() -> Result<(), String> {
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

    // 1. Generate ephemeral P-256 key pair
    let secret = EphemeralSecret::random(&mut OsRng);
    let public_key = secret.public_key();
    // Serialize as 65-byte uncompressed point (0x04 || x || y) — "raw" format
    // for Web Crypto API importKey("raw", ..., {name: "ECDH", namedCurve: "P-256"})
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
        .send(Message::Text(init_msg.to_string().into()))
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

    // 5. Display code and open browser
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
        "URL:".custom_color(crate::colors::INK_SAGE),
        verification_uri.custom_color(crate::colors::INK_SAGE)
    );
    println!();
    println!(
        "  {}",
        "Waiting for browser authorization..."
            .custom_color(crate::colors::INK_SAGE)
    );

    let _ = open::that(&verification_uri);

    // 6. Wait up to 5 minutes for the browser to authorize
    let result_msg = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        ws_stream.next(),
    )
    .await
    .map_err(|_| "Timed out waiting for browser authorization (5 min). Run `bb login` again.")?
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

    // 7. ECDH key agreement + AES-256-GCM decrypt
    //    Browser sends its public key as a 65-byte uncompressed P-256 point (raw format).
    let browser_pub_bytes = B64
        .decode(browser_pub_b64)
        .map_err(|e| format!("Invalid browser public key encoding: {e}"))?;
    let browser_pub_key = PublicKey::from_sec1_bytes(&browser_pub_bytes)
        .map_err(|e| format!("Invalid browser public key (not on curve): {e}"))?;

    let shared_secret = secret.diffie_hellman(&browser_pub_key);
    // Use first 32 bytes of the shared secret directly as AES-256 key.
    // Both sides must agree on this derivation (no HKDF in v1 — keep it simple
    // and auditable; can upgrade to HKDF in a future protocol version).
    let shared_bytes = shared_secret.raw_secret_bytes();

    let cipher = Aes256Gcm::new(GenericArray::from_slice(&shared_bytes[..32]));
    let nonce_bytes = B64
        .decode(nonce_b64)
        .map_err(|e| format!("Invalid nonce encoding: {e}"))?;
    let ciphertext = B64
        .decode(payload_b64)
        .map_err(|e| format!("Invalid payload encoding: {e}"))?;
    let plaintext = cipher
        .decrypt(GenericArray::from_slice(&nonce_bytes), ciphertext.as_ref())
        .map_err(|_| "Decryption failed — ECDH key mismatch or corrupted ciphertext")?;

    // 8. Parse credentials and persist
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

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn run() -> Result<(), String> {
    // If already logged in with a valid session, say so and return early.
    let config = load_config();
    if let Some(email) = &config.email {
        if config.session_token.is_some() {
            // Quick /me check to confirm the token is still valid.
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
                    // Token expired or revoked — fall through to re-auth.
                    println!(
                        "  {}",
                        "Session expired. Re-authenticating..."
                            .custom_color(crate::colors::INK_DIM)
                    );
                }
            }
        }
    }

    browser_login().await
}
