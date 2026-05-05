use base64::Engine;
use colored::Colorize;
use std::io::{self, Write};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde::Deserialize;
use tokio::sync::{oneshot, Mutex};

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

// ─── Browser login (--browser flag) ──────────────────────────────────────────

/// JSON body POSTed by the web app to `http://localhost:<port>/callback`.
#[derive(Deserialize, Clone)]
struct CallbackPayload {
    nonce: String,
    session_token: String,
    master_key_b64: String,
    email: String,
}

/// Shared state for the local callback server.
struct BrowserState {
    expected_nonce: String,
    /// One-shot sender for the callback payload.  Consumed on first valid use.
    payload_tx: Mutex<Option<oneshot::Sender<CallbackPayload>>>,
}

async fn handle_callback(
    State(state): State<Arc<BrowserState>>,
    req: axum::extract::Request,
) -> Response {
    let method = req.method().clone();

    // CORS pre-flight — browsers send this before the actual POST
    let mut cors_headers = HeaderMap::new();
    cors_headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    cors_headers.insert(
        "Access-Control-Allow-Headers",
        "Content-Type".parse().unwrap(),
    );
    cors_headers.insert(
        "Access-Control-Allow-Methods",
        "POST, OPTIONS".parse().unwrap(),
    );

    if method == Method::OPTIONS {
        return (StatusCode::OK, cors_headers, "").into_response();
    }

    // Parse JSON body
    let body = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, cors_headers, format!("body error: {e}"))
                .into_response();
        }
    };
    let payload: CallbackPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, cors_headers, format!("invalid JSON: {e}"))
                .into_response();
        }
    };

    // Nonce check — prevents replay and cross-origin token injection
    if payload.nonce != state.expected_nonce {
        return (StatusCode::BAD_REQUEST, cors_headers, "nonce mismatch").into_response();
    }

    // Send payload to the waiting main task (single use — take the sender)
    if let Some(tx) = state.payload_tx.lock().await.take() {
        let _ = tx.send(payload);
    }

    (StatusCode::OK, cors_headers, "authorized").into_response()
}

/// Open a URL in the system default browser (cross-platform).
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg(url).spawn();

    #[cfg(target_os = "linux")]
    let cmd = std::process::Command::new("xdg-open").arg(url).spawn();

    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn();

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let cmd: Result<_, _> = Err("unsupported platform");

    if let Err(e) = cmd {
        eprintln!("  Could not open browser automatically: {e}");
        eprintln!("  Please open this URL manually:\n  {url}");
    }
}

/// Browser-based login: spawn a local HTTP callback server, open the web auth
/// page, wait for the token, then persist credentials.
async fn browser_login() -> Result<(), String> {
    let nonce = uuid::Uuid::new_v4().to_string();

    // Channel 1: callback payload from the HTTP handler → main task
    let (payload_tx, payload_rx) = oneshot::channel::<CallbackPayload>();
    // Channel 2: shutdown signal from main task → axum server
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let shared = Arc::new(BrowserState {
        expected_nonce: nonce.clone(),
        payload_tx: Mutex::new(Some(payload_tx)),
    });

    // Bind to an OS-assigned port on loopback only
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("failed to bind callback server: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .port();

    // Spawn the callback server
    let app = Router::new()
        .route("/callback", any(handle_callback))
        .with_state(shared);

    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    // Build the auth URL and open the browser
    let auth_url = format!(
        "https://app.beebeeb.io/cli-auth?nonce={nonce}&port={port}"
    );
    open_browser(&auth_url);

    println!();
    println!(
        "  {} {}",
        "◆".custom_color(crate::colors::AMBER_DARK),
        "Waiting for authorization in your browser...".custom_color(crate::colors::INK_WARM),
    );
    println!(
        "  {}",
        format!("If the browser didn't open: {auth_url}").custom_color(crate::colors::INK_DIM),
    );

    // Wait for the callback — 120s timeout
    let payload =
        tokio::time::timeout(std::time::Duration::from_secs(120), payload_rx)
            .await
            .map_err(|_| "authorization timed out — please run `bb login --browser` again")?
            .map_err(|_| "callback channel closed unexpectedly")?;

    // Stop the local HTTP server
    let _ = shutdown_tx.send(());

    // Persist credentials (same format as normal login)
    let mut config = load_config();
    config.session_token = Some(payload.session_token);
    config.email = Some(payload.email.clone());
    config.master_key = Some(payload.master_key_b64);
    save_config(&config)?;

    // Fetch region for the success banner
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
        "Logged in as".custom_color(crate::colors::GREEN_OK),
        format!("{} · {region}", payload.email).custom_color(crate::colors::AMBER),
    );

    Ok(())
}

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn run(browser: bool) -> Result<(), String> {
    if browser {
        return browser_login().await;
    }

    // Prompt for email
    print!("{}", "  email: ".custom_color(crate::colors::INK_DIM));
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
        "  password: ".custom_color(crate::colors::INK_DIM)
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
        "Logged in as".custom_color(crate::colors::GREEN_OK),
        format!("{email} · {region}").custom_color(crate::colors::AMBER),
    );

    Ok(())
}
