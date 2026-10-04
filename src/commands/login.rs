use colored::Colorize;
use zeroize::Zeroizing;

use crate::config::{load_config, save_config};
use crate::env_detect::is_headless;

// ─── WebSocket device-auth login ─────────────────────────────────────────────

async fn browser_login(headless_flag: bool) -> Result<(), String> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    use beebeeb_core::cli_auth::CliEphemeralKey;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let headless = headless_flag || is_headless();

    // 1. Generate ephemeral P-256 key pair (handshake crypto lives in beebeeb-core).
    let cli_key = CliEphemeralKey::generate();
    let pub_key_b64 = B64.encode(cli_key.public_key_bytes());

    // 2. Connect WebSocket
    let config = load_config();
    let api_url = config.api_url;
    let ws_url = api_url.replace("https://", "wss://").replace("http://", "ws://");
    let ws_url = format!("{ws_url}/api/v1/auth/cli");

    println!("  {} Connecting to Beebeeb...", "→".custom_color(crate::colors::AMBER));

    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .map_err(|e| format!("WebSocket connection failed: {e}"))?;

    // 3. Send CLI public key so browser can do ECDH on its end
    let init_msg = init_frame(&pub_key_b64, device_hostname());
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
    let resp: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Invalid response: {e}"))?;

    if let Some(err) = resp["error"].as_str() {
        return Err(format!("Auth error: {err}"));
    }

    let user_code = resp["user_code"].as_str().ok_or("Missing user_code")?.to_string();
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

    let result = tokio::time::timeout(std::time::Duration::from_secs(expires_in), ws_stream.next()).await;

    countdown_handle.abort();
    // Clear the countdown line so it doesn't bleed into the next print.
    eprint!("\r\x1b[2K");

    let result_msg = result
        .map_err(|_| "Timed out waiting for browser authorization (5 min). Run `bb login` again.".to_string())?
        .ok_or("Connection closed before authorization completed")?
        .map_err(|e| format!("WS error: {e}"))?;

    let result_text = match result_msg {
        Message::Text(t) => t,
        _ => return Err("Unexpected message type (expected text)".into()),
    };
    let result: serde_json::Value = serde_json::from_str(&result_text).map_err(|e| format!("Invalid result: {e}"))?;

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

    // 7. ECDH key agreement + AES-256-GCM decrypt — delegated to beebeeb-core
    //    (P-256 ECDH + HKDF-SHA256 "beebeeb-cli-auth-v1", with the raw-shared-secret
    //    fallback for the legacy v0.4 web app). No crypto is hand-rolled here.
    let browser_pub_bytes = B64
        .decode(browser_pub_b64)
        .map_err(|e| format!("Invalid browser public key encoding: {e}"))?;
    let nonce_bytes = B64
        .decode(nonce_b64)
        .map_err(|e| format!("Invalid nonce encoding: {e}"))?;
    let ciphertext = B64
        .decode(payload_b64)
        .map_err(|e| format!("Invalid payload encoding: {e}"))?;

    // Wrap the decrypted plaintext in Zeroizing so the key material (master_key_b64
    // + session_token) is wiped from memory on drop. The serde_json parse borrows
    // the slice, so no copy is made at this step.
    let plaintext = Zeroizing::new(
        cli_key
            .decrypt_browser_payload(&browser_pub_bytes, &nonce_bytes, &ciphertext)
            .map_err(|_| "Decryption failed — ECDH key mismatch or corrupted ciphertext")?,
    );

    // 8. Parse credentials and persist — unchanged.
    let creds: serde_json::Value =
        serde_json::from_slice(&plaintext).map_err(|e| format!("Invalid credentials JSON: {e}"))?;

    let session_token = creds["session_token"]
        .as_str()
        .ok_or("Missing session_token in credentials")?;
    let master_key_b64 = creds["master_key_b64"]
        .as_str()
        .ok_or("Missing master_key_b64 in credentials")?;
    let email = creds["email"].as_str().ok_or("Missing email in credentials")?;

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

// ─── Init frame ──────────────────────────────────────────────────────────────

/// This machine's name, if it has one.
fn device_hostname() -> Option<String> {
    hostname::get().ok().and_then(|h| h.into_string().ok())
}

/// The first WebSocket frame: our public key (so the browser can do its half of
/// the ECDH) plus what this program can honestly say about itself, which the
/// approval page shows the person labelled "reported by the device" - a fake
/// client can say anything, so the page treats it as a hint, never as proof.
/// The server ignores anything it does not recognise.
fn init_frame(pub_key_b64: &str, hostname: Option<String>) -> serde_json::Value {
    let mut frame = serde_json::json!({
        "ecdh_public_key_b64": pub_key_b64,
        "client": "cli",
        "client_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
    });
    if let Some(name) = hostname.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()) {
        frame["device_name"] = serde_json::Value::String(name);
    }
    frame
}

// ─── Output blocks ───────────────────────────────────────────────────────────

fn browser_block(user_code: &str, verification_uri: &str) -> String {
    let mut out = String::new();
    out.push('\n');
    out.push_str(&format!(
        "  {} Opening Beebeeb in your browser...\n\n",
        "→".custom_color(crate::colors::AMBER)
    ));
    out.push_str(&format!(
        "  {} {}\n",
        "Authorization code:".custom_color(crate::colors::INK_SAGE),
        user_code.bold().custom_color(crate::colors::AMBER)
    ));
    out.push_str(&format!(
        "  {} {}\n\n",
        "URL:               ".custom_color(crate::colors::INK_SAGE),
        verification_uri.custom_color(crate::colors::INK_SAGE)
    ));
    out.push_str(&format!(
        "  {}\n",
        "Type the code above on the page that opens. It is not in the link on".custom_color(crate::colors::INK)
    ));
    out.push_str(&format!(
        "  {}\n\n",
        "purpose: a link someone else sends you has no code to approve.".custom_color(crate::colors::INK_DIM)
    ));
    out.push_str(&format!(
        "  {}\n",
        "If your browser did not open, paste the URL above into any".custom_color(crate::colors::INK_DIM)
    ));
    out.push_str(&format!(
        "  {}\n",
        "signed-in browser. Only approve it if you started this sign-in".custom_color(crate::colors::INK_DIM)
    ));
    out.push_str(&format!(
        "  {}\n\n",
        "yourself, just now, on this machine.".custom_color(crate::colors::INK_DIM)
    ));
    out
}

fn headless_block(user_code: &str, verification_uri: &str) -> String {
    let mut out = String::new();
    out.push('\n');
    out.push_str(&format!(
        "  {}\n",
        "No browser detected on this machine.".custom_color(crate::colors::INK)
    ));
    out.push_str(&format!(
        "  {}\n\n",
        "Open this URL in a browser on any device you trust:".custom_color(crate::colors::INK)
    ));
    out.push_str(&format!(
        "      {}\n\n",
        verification_uri.custom_color(crate::colors::AMBER)
    ));
    out.push_str(&format!(
        "  {} {}\n\n",
        "Authorization code:".custom_color(crate::colors::INK_SAGE),
        user_code.bold().custom_color(crate::colors::AMBER)
    ));
    out.push_str(&format!(
        "  {}\n",
        "Type that code on the page when it asks. It is not in the link on".custom_color(crate::colors::INK)
    ));
    out.push_str(&format!(
        "  {}\n",
        "purpose: a link someone else sends you has no code to approve.".custom_color(crate::colors::INK_DIM)
    ));
    out.push_str(&format!(
        "  {}\n\n",
        "Only approve it if you started this sign-in yourself, just now.".custom_color(crate::colors::INK_DIM)
    ));
    out.push_str(&format!(
        "  {}\n",
        "(If you are not signed in there, you will be asked to sign in".custom_color(crate::colors::INK_DIM)
    ));
    out.push_str(&format!(
        "  {}\n\n",
        " and complete two-factor authentication first.)".custom_color(crate::colors::INK_DIM)
    ));
    out
}

fn print_browser_block(user_code: &str, verification_uri: &str) {
    print!("{}", browser_block(user_code, verification_uri));
}

fn print_headless_block(user_code: &str, verification_uri: &str) {
    print!("{}", headless_block(user_code, verification_uri));
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
                        "Run `bb logout` first to switch accounts.".custom_color(crate::colors::INK_DIM)
                    );
                    return Ok(());
                }
                Err(_) => {
                    println!(
                        "  {}",
                        "Session expired. Re-authenticating...".custom_color(crate::colors::INK_DIM)
                    );
                }
            }
        }
    }

    browser_login(headless).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain text of a rendered block (colour off), so assertions read the words.
    fn plain(block: String) -> String {
        colored::control::set_override(false);
        block
    }

    const CODE: &str = "ABCD-EFGH";
    const URI: &str = "https://app.beebeeb.io/cli-auth";

    // Task 1734: the approval page no longer shows a code to "match" - the
    // person TYPES the code from this terminal. The instructions must say so,
    // and must never tell anyone to compare or "confirm" a code they were shown.
    #[test]
    fn browser_block_tells_the_user_to_type_the_code_into_the_page() {
        let out = plain(browser_block(CODE, URI));
        assert!(out.contains(CODE), "the code must be shown: {out}");
        assert!(out.contains(URI), "the page must be named: {out}");
        let lower = out.to_lowercase();
        assert!(lower.contains("type"), "must tell the user to TYPE the code: {out}");
        assert!(
            lower.contains("started this"),
            "must warn to approve only what they started: {out}"
        );
        assert!(
            !lower.contains("must match"),
            "the page shows no code to match any more: {out}"
        );
        assert!(!out.contains("code="), "the link must carry no code: {out}");
    }

    #[test]
    fn headless_block_tells_the_user_to_type_the_code_into_the_page() {
        let out = plain(headless_block(CODE, URI));
        assert!(out.contains(CODE), "the code must be shown: {out}");
        assert!(out.contains(URI), "the page must be named: {out}");
        let lower = out.to_lowercase();
        assert!(lower.contains("type"), "must tell the user to TYPE the code: {out}");
        assert!(
            lower.contains("started this"),
            "must warn to approve only what they started: {out}"
        );
        assert!(
            !lower.contains("confirm the code"),
            "the page shows no code to confirm any more: {out}"
        );
        assert!(!out.contains("code="), "the link must carry no code: {out}");
    }

    #[test]
    fn both_blocks_label_the_code_so_scripts_and_people_can_find_it() {
        for out in [plain(browser_block(CODE, URI)), plain(headless_block(CODE, URI))] {
            assert!(out.contains(&format!("Authorization code: {CODE}")), "{out}");
        }
    }

    #[test]
    fn init_frame_carries_the_key_and_what_this_device_can_say_about_itself() {
        let frame = init_frame("cHVia2V5", Some("Guus-MBP".to_string()));
        assert_eq!(frame["ecdh_public_key_b64"], "cHVia2V5");
        assert_eq!(frame["client"], "cli");
        assert_eq!(frame["client_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(frame["device_name"], "Guus-MBP");
        assert_eq!(frame["os"], std::env::consts::OS);
    }

    #[test]
    fn init_frame_omits_a_device_name_when_the_machine_has_none() {
        let frame = init_frame("cHVia2V5", None);
        assert!(frame.get("device_name").is_none(), "{frame}");
        let blank = init_frame("cHVia2V5", Some("   ".to_string()));
        assert!(blank.get("device_name").is_none(), "{blank}");
    }
}
