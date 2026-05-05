use base64::Engine as _;
use colored::Colorize;
use std::io::{self, Write};

use crate::api::ApiClient;
use crate::config::load_config;

/// Parse a human duration like "24h", "7d", "1h" into hours.
fn parse_hours(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('d') {
        let days: u64 = rest.parse().map_err(|_| format!("invalid duration: {s}"))?;
        Ok(days * 24)
    } else if let Some(rest) = s.strip_suffix('h') {
        rest.parse().map_err(|_| format!("invalid duration: {s}"))
    } else {
        s.parse::<u64>()
            .map_err(|_| format!("invalid duration: {s} (use e.g. 24h or 7d)"))
    }
}

/// `bb share <file_id>` — create a shareable link for a file.
pub async fn run(
    file_id: String,
    expires: Option<String>,
    max_opens: Option<u32>,
    passphrase: bool,
    double_encrypted: bool,
) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let passphrase_value = if passphrase {
        print!(
            "  {}",
            "? Passphrase (12+ chars, mixed): ".custom_color(crate::colors::AMBER),
        );
        io::stdout().flush().map_err(|e| e.to_string())?;
        let pass =
            rpassword::read_password().map_err(|e| format!("failed to read passphrase: {e}"))?;
        if pass.len() < 12 {
            return Err("passphrase must be at least 12 characters".to_string());
        }
        Some(pass)
    } else {
        None
    };

    let expires_hours = match &expires {
        Some(s) => Some(parse_hours(s)?),
        None => None,
    };

    if passphrase_value.is_some() {
        println!(
            "  {}",
            "wrapping chunk keys with Argon2id(passphrase)".custom_color(crate::colors::INK_DIM),
        );
    }

    // ── Double-encrypted mode ─────────────────────────────────────────────────
    // Client generates K_c, wraps the per-file AES key under it, sends the
    // opaque blob to the server. K_c goes in the URL fragment; server stores
    // only the ciphertext and cannot derive K_c or the file key.
    //
    // Wrapping: treat K_c as a MasterKey → derive a wrap FileKey via HKDF →
    // AES-256-GCM encrypt(wrap_key, file_key.as_bytes()). Stored as a
    // JSON-serialised EncryptedBlob (same format as file chunks).
    let (wrapped_file_key, client_key_b64) = if double_encrypted {
        let config = load_config();
        let mk_b64 = config.master_key.ok_or("No master key. Run `bb login`.")?;
        let mk_bytes = base64::engine::general_purpose::STANDARD
            .decode(&mk_b64)
            .map_err(|e| format!("invalid master key: {e}"))?;
        if mk_bytes.len() != 32 {
            return Err(format!("master key must be 32 bytes, got {}", mk_bytes.len()));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&mk_bytes);
        let master_key = beebeeb_core::kdf::MasterKey::from_bytes(arr);

        // Parse file_id as UUID for key derivation
        let file_uuid: uuid::Uuid = file_id
            .parse()
            .map_err(|_| format!("invalid file id (expected UUID): {file_id}"))?;

        // Derive the real per-file encryption key
        let file_key = beebeeb_core::kdf::derive_file_key(&master_key, file_uuid.as_bytes());

        // Generate client key K_c (random 32 bytes)
        let client_key: [u8; 32] = rand::random();

        // Derive a wrap key from K_c: treat it as a MasterKey + HKDF over file_uuid
        // so the wrap key is file-specific (recipient can only decrypt THIS file).
        let client_mk = beebeeb_core::kdf::MasterKey::from_bytes(client_key);
        let wrap_key = beebeeb_core::kdf::derive_file_key(&client_mk, file_uuid.as_bytes());

        // Encrypt file_key.as_bytes() under wrap_key using AES-256-GCM
        let blob = beebeeb_core::encrypt::encrypt_chunk(&wrap_key, file_key.as_bytes())
            .map_err(|e| format!("encrypt file key: {e}"))?;
        let wfk_json = serde_json::to_string(&blob)
            .map_err(|e| format!("serialize wrapped key: {e}"))?;

        let k_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(client_key);
        (Some(wfk_json), Some(k_b64))
    } else {
        (None, None)
    };

    let result = api
        .create_share(
            &file_id,
            expires_hours,
            max_opens,
            passphrase_value.as_deref(),
            wrapped_file_key,
        )
        .await?;

    let share_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("(unknown)");
    let token = result.get("token").and_then(|v| v.as_str()).unwrap_or("(unknown)");
    let expires_at = result.get("expires_at").and_then(|v| v.as_str()).unwrap_or("never");

    // Build the URL: standard mode uses server-returned URL; double-encrypted
    // builds it locally with K_c in the fragment (server never sees K_c).
    let url = if let Some(ref kc) = client_key_b64 {
        let app_url = std::env::var("APP_URL")
            .unwrap_or_else(|_| "https://app.beebeeb.io".to_string());
        format!("{app_url}/s/{token}#key={kc}")
    } else {
        result
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string()
    };

    println!();
    if double_encrypted {
        println!("  {}", "Link created (double encrypted)".custom_color(crate::colors::GREEN_OK));
    } else {
        println!("  {}", "Link created".custom_color(crate::colors::GREEN_OK));
    }
    println!(
        "  {} {}",
        "url       ".custom_color(crate::colors::INK_DIM),
        url.custom_color(crate::colors::AMBER),
    );
    println!(
        "  {} {}",
        "expires   ".custom_color(crate::colors::INK_DIM),
        expires_at.custom_color(crate::colors::INK),
    );
    if let Some(max) = max_opens {
        println!(
            "  {} {}",
            "max-opens ".custom_color(crate::colors::INK_DIM),
            max.to_string().custom_color(crate::colors::INK),
        );
    }
    println!(
        "  {} {}",
        "share-id  ".custom_color(crate::colors::INK_DIM),
        share_id.custom_color(crate::colors::INK_WARM),
    );
    println!();
    if double_encrypted {
        println!(
            "  {}",
            "Even Beebeeb cannot decrypt this link."
                .custom_color(crate::colors::AMBER),
        );
    }
    if passphrase_value.is_some() {
        println!(
            "  {}",
            "# send the passphrase by a different channel — we will never see it"
                .custom_color(crate::colors::INK_SAGE),
        );
    }
    println!(
        "  {}",
        format!("# revoke anytime:  bb unshare {share_id}").custom_color(crate::colors::INK_SAGE),
    );

    Ok(())
}

/// `bb shares` — list all active share links.
pub async fn list() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let result = api.list_shares().await?;

    let shares = result
        .as_array()
        .or_else(|| result.get("shares").and_then(|s| s.as_array()));

    let Some(shares) = shares else {
        println!(
            "  {}",
            "no active shares".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    };

    if shares.is_empty() {
        println!(
            "  {}",
            "no active shares".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    }

    println!(
        "  {}",
        format!(
            "{:<36}  {:<40}  {:<20}  {}",
            "file", "url", "expires", "opens"
        )
        .custom_color(crate::colors::INK_DIM),
    );

    for share in shares {
        let file_name = share
            .get("file_name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let url = share
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let expires = share
            .get("expires_at")
            .and_then(|v| v.as_str())
            .unwrap_or("never");
        let opens = share
            .get("opens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let max_opens = share
            .get("max_opens")
            .and_then(|v| v.as_u64());

        let opens_display = match max_opens {
            Some(max) => format!("{opens}/{max}"),
            None => format!("{opens}"),
        };

        println!(
            "  {:<36}  {:<40}  {:<20}  {}",
            file_name.custom_color(crate::colors::INK_WARM),
            url.custom_color(crate::colors::AMBER),
            expires.custom_color(crate::colors::INK_DIM),
            opens_display.custom_color(crate::colors::INK),
        );
    }

    Ok(())
}

/// `bb unshare <share_id>` — revoke a share link.
pub async fn revoke(share_id: String) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    api.delete_share(&share_id).await?;

    println!(
        "  {} {}",
        "Revoked".custom_color(crate::colors::GREEN_OK),
        format!("· share {share_id} is no longer accessible").custom_color(crate::colors::INK_DIM),
    );

    Ok(())
}
