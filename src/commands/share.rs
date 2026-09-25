use base64::Engine as _;
use colored::Colorize;
use std::io::{self, Write};
use zeroize::Zeroizing;

use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal;

use crate::api::ApiClient;
use crate::ui;

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
///
/// Every share is end-to-end encrypted (the server refuses anything else,
/// task 0538): see "Share wire format" below for exactly what is sent.
pub async fn run(
    file_id: String,
    expires: Option<String>,
    max_opens: Option<u32>,
    passphrase: bool,
    no_double_encrypt: bool,
) -> Result<(), String> {
    if no_double_encrypt {
        return Err(
            "--no-double-encrypt is no longer supported: every share is end-to-end encrypted, \
             and Beebeeb's servers cannot open it"
                .to_string(),
        );
    }

    let api = ApiClient::from_config();
    api.require_auth()?;

    // Validate cheap inputs before prompting for anything.
    let file_uuid: uuid::Uuid = file_id
        .parse()
        .map_err(|_| format!("invalid file id (expected UUID): {file_id}"))?;
    let expires_hours = match &expires {
        Some(s) => Some(parse_hours(s)?),
        None => None,
    };
    let master_key = crate::commands::push::load_master_key()?;

    let passphrase_value = if passphrase {
        print!(
            "  {}",
            "? Passphrase (12+ chars, mixed): ".custom_color(crate::colors::AMBER),
        );
        io::stdout().flush().map_err(|e| e.to_string())?;
        let pass = rpassword::read_password().map_err(|e| format!("failed to read passphrase: {e}"))?;
        if pass.len() < 12 {
            return Err("passphrase must be at least 12 characters".to_string());
        }
        // Rough entropy estimate: ~6.5 bits per character for a decent passphrase
        if !ui::is_quiet() && !ui::is_json() {
            let bits = (pass.len() as f64 * 6.5).round() as u32;
            let (strength, color) = if bits > 60 {
                ("strong", crate::colors::GREEN_OK)
            } else if bits >= 40 {
                ("fair", crate::colors::AMBER)
            } else {
                ("weak", crate::colors::RED_ERR)
            };
            println!(
                "  {} {}",
                "entropy  ".custom_color(crate::colors::INK_DIM),
                format!("{bits} bits \u{00b7} {strength}").custom_color(color),
            );
        }
        Some(pass)
    } else {
        None
    };

    // K_c: random, never sent to the server — it only travels in #key=.
    let client_key: Zeroizing<[u8; 32]> = Zeroizing::new(rand::random());
    let mut material = build_share_material(
        &master_key,
        &file_uuid,
        &client_key,
        beebeeb_core::share_token::generate_share_token(),
    )?;

    let result = match api
        .create_share(
            &file_id,
            expires_hours,
            max_opens,
            passphrase_value.as_deref(),
            &material,
        )
        .await
    {
        // A client-minted token collided with an existing one (≈ never at 160
        // random bits): the server answers 409 — mint a fresh token once.
        Err(e) if e.contains("share token already exists") => {
            material = build_share_material(
                &master_key,
                &file_uuid,
                &client_key,
                beebeeb_core::share_token::generate_share_token(),
            )?;
            api.create_share(
                &file_id,
                expires_hours,
                max_opens,
                passphrase_value.as_deref(),
                &material,
            )
            .await?
        }
        other => other?,
    };

    let share_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("(unknown)");
    let token = result.get("token").and_then(|v| v.as_str()).unwrap_or(&material.token);
    let expires_at = result.get("expires_at").and_then(|v| v.as_str()).unwrap_or("never");

    let bare_url = format!("{}/s/{token}", app_url());
    let full_url = share_link(&app_url(), token, &material.key_fragment);

    // JSON mode: emit machine-readable output
    if ui::is_json() {
        let json_out = serde_json::json!({
            "share_id": share_id,
            "url": full_url,
            "share_link": bare_url,
            "decryption_key": material.key_fragment,
            "expires_at": expires_at,
            "max_opens": max_opens,
            "double_encrypted": true,
            "passphrase_protected": passphrase_value.is_some(),
        });
        println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
        return Ok(());
    }

    if ui::is_quiet() {
        println!("{full_url}");
        return Ok(());
    }

    // Rich mode
    println!();
    println!(
        "  {}",
        "\u{2713} Link created (end-to-end encrypted)".custom_color(crate::colors::GREEN_OK)
    );
    println!(
        "  {} {}",
        "url       ".custom_color(crate::colors::INK_DIM),
        full_url.custom_color(crate::colors::AMBER),
    );
    let expires_display = if expires_at == "never" {
        "never".to_string()
    } else {
        ui::relative_time(expires_at)
    };
    println!(
        "  {} {}",
        "expires   ".custom_color(crate::colors::INK_DIM),
        expires_display.custom_color(crate::colors::INK),
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
        "encryption".custom_color(crate::colors::INK_DIM),
        "end-to-end \u{00b7} server blind".custom_color(crate::colors::GREEN_OK),
    );
    println!(
        "  {} {}",
        "share-id  ".custom_color(crate::colors::INK_DIM),
        share_id.custom_color(crate::colors::INK_WARM),
    );
    println!();
    println!(
        "  {}",
        "The part after #key= is the decryption key. It never reaches Beebeeb:".custom_color(crate::colors::INK_SAGE),
    );
    println!(
        "  {}",
        "we cannot open this share, and cannot recover it if the key is lost.".custom_color(crate::colors::INK_SAGE),
    );
    if passphrase_value.is_some() {
        println!(
            "  {}",
            "# send passphrase by a different channel".custom_color(crate::colors::INK_SAGE),
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
        if ui::is_json() {
            println!("[]");
        } else if !ui::is_quiet() {
            println!("  {}", "no active shares".custom_color(crate::colors::INK_DIM),);
        }
        return Ok(());
    };

    if shares.is_empty() {
        if ui::is_json() {
            println!("[]");
        } else if !ui::is_quiet() {
            println!("  {}", "no active shares".custom_color(crate::colors::INK_DIM),);
        }
        return Ok(());
    }

    if ui::is_json() {
        println!("{}", serde_json::to_string_pretty(shares).unwrap_or_default());
        return Ok(());
    }

    println!(
        "  {}",
        format!("  {:<36}  {:<40}  {:<20}  {}", "file", "url", "expires", "opens").custom_color(crate::colors::INK_DIM),
    );

    let master_key = crate::commands::push::load_master_key()?;

    for share in shares {
        let file_name = {
            let file_id = share.get("file_id").and_then(|v| v.as_str()).unwrap_or("");
            let name_enc = share
                .get("file")
                .and_then(|f| f.get("name_encrypted"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !file_id.is_empty() && !name_enc.is_empty() {
                crate::crypto::decrypt_name(&master_key, file_id, name_enc).unwrap_or_else(|| "(encrypted)".to_string())
            } else {
                "(unknown)".to_string()
            }
        };
        let url = match (
            share.get("owner_wrapped_key").and_then(|v| v.as_str()),
            share.get("owner_wrapped_token").and_then(|v| v.as_str()),
        ) {
            (Some(owk), Some(owt)) => recover_share_link(&master_key, owk, owt, &app_url())
                .unwrap_or_else(|| "(link not recoverable)".to_string()),
            _ => "(link not stored \u{00b7} created by an older client)".to_string(),
        };
        let expires_raw = share.get("expires_at").and_then(|v| v.as_str()).unwrap_or("never");
        let is_revoked = share.get("revoked").and_then(|v| v.as_bool()).unwrap_or(false);
        let opens = share
            .get("open_count")
            .or_else(|| share.get("opens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let max_opens = share.get("max_opens").and_then(|v| v.as_u64());

        // Status indicator: active, expired, or revoked
        let (status_icon, name_color) = if is_revoked {
            ("\u{2717}".custom_color(crate::colors::RED_ERR), crate::colors::INK_DIM)
        } else {
            // Check if expired by trying to parse the timestamp
            let is_expired = expires_raw != "never"
                && chrono::DateTime::parse_from_rfc3339(expires_raw)
                    .map(|dt| dt < chrono::Utc::now())
                    .unwrap_or(false);
            if is_expired {
                ("\u{25CB}".custom_color(crate::colors::INK_DIM), crate::colors::INK_DIM)
            } else {
                (
                    "\u{25CF}".custom_color(crate::colors::GREEN_OK),
                    crate::colors::INK_WARM,
                )
            }
        };

        // Relative expiry display
        let expires_display = if expires_raw == "never" {
            "never".to_string()
        } else {
            ui::relative_time(expires_raw)
        };

        let opens_display = match max_opens {
            Some(max) => format!("{opens}/{max}"),
            None => format!("{opens}"),
        };

        println!(
            "  {} {:<36}  {:<40}  {:<20}  {}",
            status_icon,
            file_name.custom_color(name_color),
            url.as_str().custom_color(crate::colors::AMBER),
            expires_display.custom_color(crate::colors::INK_DIM),
            opens_display.custom_color(crate::colors::INK),
        );
    }

    Ok(())
}

/// `bb unshare [share_id]` — revoke a share link.
///
/// When called without arguments, shows an interactive arrow-key picker of
/// active shares. When called with an ID, revokes directly (for scripting).
pub async fn revoke(share_id: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let resolved_id = match share_id {
        Some(id) => id,
        None => pick_share_interactively(&api).await?,
    };

    api.delete_share(&resolved_id).await?;

    println!(
        "  {} {}",
        "Revoked".custom_color(crate::colors::GREEN_OK),
        format!("· share {resolved_id} is no longer accessible").custom_color(crate::colors::INK_DIM),
    );

    Ok(())
}

/// A share entry parsed from the API response, ready for display.
struct ShareEntry {
    id: String,
    file_name: String,
    expires_display: String,
    open_count: u64,
    is_revoked: bool,
    is_expired: bool,
}

/// Fetch shares from the API and present an interactive terminal picker.
/// Returns the selected share ID, or an error if cancelled / no shares.
async fn pick_share_interactively(api: &ApiClient) -> Result<String, String> {
    let result = api.list_shares().await?;

    let shares_json = result
        .as_array()
        .or_else(|| result.get("shares").and_then(|s| s.as_array()));

    let Some(shares_json) = shares_json else {
        return Err("no active shares".to_string());
    };

    if shares_json.is_empty() {
        return Err("no active shares".to_string());
    }

    let master_key = crate::commands::push::load_master_key()?;

    let entries: Vec<ShareEntry> = shares_json
        .iter()
        .map(|share| {
            let id = share.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let file_id = share.get("file_id").and_then(|v| v.as_str()).unwrap_or("");
            let name_enc = share
                .get("file")
                .and_then(|f| f.get("name_encrypted"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let file_name = if !file_id.is_empty() && !name_enc.is_empty() {
                crate::crypto::decrypt_name(&master_key, file_id, name_enc).unwrap_or_else(|| "(encrypted)".to_string())
            } else {
                "(unknown)".to_string()
            };
            let expires_raw = share.get("expires_at").and_then(|v| v.as_str()).unwrap_or("never");
            let is_revoked = share.get("revoked").and_then(|v| v.as_bool()).unwrap_or(false);
            let is_expired = !is_revoked
                && expires_raw != "never"
                && chrono::DateTime::parse_from_rfc3339(expires_raw)
                    .map(|dt| dt < chrono::Utc::now())
                    .unwrap_or(false);
            let open_count = share
                .get("open_count")
                .or_else(|| share.get("opens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let expires_display = if expires_raw == "never" {
                "never".to_string()
            } else {
                ui::relative_time(expires_raw)
            };

            ShareEntry {
                id,
                file_name,
                expires_display,
                open_count,
                is_revoked,
                is_expired,
            }
        })
        .collect();

    if entries.is_empty() {
        return Err("no shares found".to_string());
    }

    // Enter raw mode for the interactive picker.
    terminal::enable_raw_mode().map_err(|e| format!("failed to enable raw mode: {e}"))?;

    let result = run_picker(&entries);

    // Always restore the terminal, even on error.
    let _ = terminal::disable_raw_mode();

    let selected_idx = result?;
    let selected = &entries[selected_idx];

    // Confirm before revoking.
    print!(
        "  {} ",
        format!("Revoke share for {}? (y/N)", selected.file_name).custom_color(crate::colors::AMBER),
    );
    io::stdout().flush().map_err(|e| e.to_string())?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|e| format!("failed to read input: {e}"))?;

    if !answer.trim().eq_ignore_ascii_case("y") {
        return Err("cancelled".to_string());
    }

    Ok(selected.id.clone())
}

/// Drive the arrow-key picker loop. Returns the index of the selected entry.
fn run_picker(entries: &[ShareEntry]) -> Result<usize, String> {
    use crossterm::cursor;
    use crossterm::terminal::{Clear, ClearType};
    use std::io::Write;

    let mut selected: usize = 0;
    let mut stdout = io::stdout();

    // Hide cursor while picking.
    crossterm::execute!(stdout, cursor::Hide).map_err(|e| e.to_string())?;

    // Draw the initial list.
    draw_picker(&mut stdout, entries, selected)?;

    loop {
        // Block until an event arrives (100ms poll interval to keep responsive).
        if !event::poll(std::time::Duration::from_millis(100)).map_err(|e| e.to_string())? {
            continue;
        }

        let ev = event::read().map_err(|e| e.to_string())?;

        if let Event::Key(key_event) = ev {
            match key_event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if selected + 1 < entries.len() {
                        selected += 1;
                    }
                }
                KeyCode::Enter => {
                    // Restore cursor and clear picker area before returning.
                    crossterm::execute!(stdout, cursor::Show).map_err(|e| e.to_string())?;
                    // Move below the list so subsequent output is clean.
                    write!(stdout, "\r\n").map_err(|e| e.to_string())?;
                    stdout.flush().map_err(|e| e.to_string())?;
                    return Ok(selected);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    crossterm::execute!(stdout, cursor::Show).map_err(|e| e.to_string())?;
                    write!(stdout, "\r\n").map_err(|e| e.to_string())?;
                    stdout.flush().map_err(|e| e.to_string())?;
                    return Err("cancelled".to_string());
                }
                _ => {}
            }
        }

        // Redraw: move cursor up to overwrite previous list, then draw again.
        // Move up by `entries.len() + 1` lines (list + header).
        let lines_up = entries.len() as u16 + 1;
        crossterm::execute!(stdout, cursor::MoveUp(lines_up), Clear(ClearType::FromCursorDown))
            .map_err(|e| e.to_string())?;

        draw_picker(&mut stdout, entries, selected)?;
    }
}

/// Render the share list to the terminal.
fn draw_picker(stdout: &mut io::Stdout, entries: &[ShareEntry], selected: usize) -> Result<(), String> {
    use std::io::Write;

    // Header
    write!(
        stdout,
        "  {}\r\n",
        "Select a share to revoke (arrow keys / j/k, Enter to confirm, q/Esc to cancel)"
            .custom_color(crate::colors::INK_DIM)
    )
    .map_err(|e| e.to_string())?;

    for (i, entry) in entries.iter().enumerate() {
        let pointer = if i == selected { ">" } else { " " };

        // Status indicator
        let status = if entry.is_revoked {
            "\u{2717}".custom_color(crate::colors::RED_ERR) // ✗
        } else if entry.is_expired {
            "\u{25CB}".custom_color(crate::colors::INK_DIM) // ○
        } else {
            "\u{25CF}".custom_color(crate::colors::GREEN_OK) // ●
        };

        let name_color = if i == selected {
            crate::colors::AMBER
        } else if entry.is_revoked || entry.is_expired {
            crate::colors::INK_DIM
        } else {
            crate::colors::INK_WARM
        };

        let pointer_display = if i == selected {
            pointer.custom_color(crate::colors::AMBER)
        } else {
            pointer.custom_color(crate::colors::INK_DIM)
        };

        write!(
            stdout,
            "  {} {} {:<36}  {:<20}  {}\r\n",
            pointer_display,
            status,
            entry.file_name.custom_color(name_color),
            entry.expires_display.custom_color(crate::colors::INK_DIM),
            format!("{} opens", entry.open_count).custom_color(crate::colors::INK_DIM),
        )
        .map_err(|e| e.to_string())?;
    }

    stdout.flush().map_err(|e| e.to_string())?;
    Ok(())
}

// ── Share wire format ───────────────────────────────────────────────────────
//
// Every client mints the SAME single-file share (web `share-dialog.tsx`, the
// server's `POST /api/v1/shares` validator, and the core `share_key_wrap` KAT
// vector, core a04dce5):
//
//   K_c                 = 32 random bytes; lives only in the URL fragment
//   wrapped_file_key    = base64(STANDARD, nonce(12) || AES-256-GCM(K_c, FileKey))
//   owner_wrapped_key   = base64(STANDARD, nonce(12) || AES-256-GCM(MasterKey, K_c))
//   owner_wrapped_token = base64(STANDARD, nonce(12) || AES-256-GCM(MasterKey, utf8(token)))
//   token               = beebeeb_core::share_token::generate_share_token()
//   link                = {APP_URL}/s/{token}#key={base64url-no-pad(K_c)}
//
// The AES-GCM is the raw key with no KDF and no AAD — exactly web's
// wrapKeyForShare/unwrapKeyFromShare — via core's encrypt_chunk_raw /
// decrypt_chunk_raw. The owner blobs let `bb shares` (and the web) rebuild a
// working link later; the server only ever stores opaque ciphertext and a
// hash of the token.

/// Wrap `plaintext` under a raw 32-byte key: nonce(12) || ciphertext+tag.
fn wrap_raw(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    beebeeb_core::encrypt::encrypt_chunk_raw(&beebeeb_core::kdf::FileKey::from_bytes(*key), plaintext)
        .map_err(|e| format!("wrap share key: {e}"))
}

/// Open a nonce(12) || ciphertext+tag blob under a raw 32-byte key.
pub(crate) fn unwrap_share_blob(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>, String> {
    beebeeb_core::encrypt::decrypt_chunk_raw(&beebeeb_core::kdf::FileKey::from_bytes(*key), blob)
        .map_err(|_| "share key blob does not decrypt".to_string())
}

/// `wrapped_file_key` as the server and the web viewer expect it.
pub(crate) fn wrap_file_key_for_share(client_key: &[u8; 32], file_key: &[u8; 32]) -> Result<String, String> {
    Ok(base64::engine::general_purpose::STANDARD.encode(wrap_raw(client_key, file_key)?))
}

/// Everything a single-file share create needs, plus the link to print.
pub(crate) struct ShareMaterial {
    pub token: String,
    pub wrapped_file_key: String,
    pub owner_wrapped_key: String,
    pub owner_wrapped_token: String,
    /// base64url-no-pad K_c — the `#key=` fragment.
    pub key_fragment: String,
}

pub(crate) fn build_share_material(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_uuid: &uuid::Uuid,
    client_key: &[u8; 32],
    token: String,
) -> Result<ShareMaterial, String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mk: Zeroizing<[u8; 32]> = Zeroizing::new(master_key.to_bytes());
    // Same per-file key the web derives in getFileKey(fileId) and the CLI
    // uses for upload: HKDF(master, file-uuid string).
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.to_string().as_bytes());
    Ok(ShareMaterial {
        wrapped_file_key: wrap_file_key_for_share(client_key, file_key.as_bytes())?,
        owner_wrapped_key: b64.encode(wrap_raw(&mk, client_key)?),
        owner_wrapped_token: b64.encode(wrap_raw(&mk, token.as_bytes())?),
        key_fragment: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(client_key),
        token,
    })
}

fn app_url() -> String {
    std::env::var("APP_URL")
        .unwrap_or_else(|_| "https://app.beebeeb.io".to_string())
        .trim_end_matches('/')
        .to_string()
}

pub(crate) fn share_link(app_url: &str, token: &str, key_fragment: &str) -> String {
    format!("{app_url}/s/{token}#key={key_fragment}")
}

/// Rebuild a share's working link from the owner-wrapped blobs returned by
/// `GET /api/v1/shares/mine`. `None` for shares created without them (older
/// clients) or if the blobs do not open under this master key.
pub(crate) fn recover_share_link(
    master_key: &beebeeb_core::kdf::MasterKey,
    owner_wrapped_key: &str,
    owner_wrapped_token: &str,
    app_url: &str,
) -> Option<String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mk: Zeroizing<[u8; 32]> = Zeroizing::new(master_key.to_bytes());
    let kc = Zeroizing::new(unwrap_share_blob(&mk, &b64.decode(owner_wrapped_key).ok()?).ok()?);
    if kc.len() != 32 {
        return None;
    }
    let token = String::from_utf8(unwrap_share_blob(&mk, &b64.decode(owner_wrapped_token).ok()?).ok()?).ok()?;
    let frag = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&kc[..]);
    Some(share_link(app_url, &token, &frag))
}

#[cfg(test)]
mod share_wire_tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // core test-vectors/vectors.json @ 3ee6b90, vector "share_key_wrap" (K3,
    // core a04dce5): pins web's wrapKeyForShare/unwrapKeyFromShare format.
    const KAT_WRAP_KEY: &str = "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5";
    const KAT_KEY_TO_WRAP: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
    const KAT_NONCE: &str = "174e5553a15305c56b28ae60";
    const KAT_CT: &str =
        "296c608d2b6150057284434cacb9ac684db811891e0e5b4cdb39cdec3ce53f99f1b5611f75edd9b75c2614e32bb5c596";

    fn arr32(v: &[u8]) -> [u8; 32] {
        let mut a = [0u8; 32];
        a.copy_from_slice(v);
        a
    }

    /// The server (routes/shares.rs) requires STANDARD base64 of a raw AEAD
    /// blob; the web viewer (unwrapKeyFromShare) splits nonce(12)||ct under the
    /// RAW K_c with no KDF. The CLI's wrapped_file_key must satisfy both.
    #[test]
    fn wrapped_file_key_is_web_wire_format_and_opens_with_raw_client_key() {
        let k_c = arr32(&hex(KAT_WRAP_KEY));
        let fk = arr32(&hex(KAT_KEY_TO_WRAP));
        let wire = wrap_file_key_for_share(&k_c, &fk).expect("wrap");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&wire)
            .expect("wrapped_file_key must be STANDARD base64 (server check)");
        assert_eq!(raw.len(), 60, "nonce(12) || ct(32+16) = 60 bytes (K3 wire format)");
        let opened = beebeeb_core::encrypt::decrypt_chunk_raw(&beebeeb_core::kdf::FileKey::from_bytes(k_c), &raw)
            .expect("must open under the raw K_c exactly as web unwrapKeyFromShare does");
        assert_eq!(opened, fk.to_vec());
    }

    /// The full create body: file key is the upload key HKDF(master, uuid),
    /// token is the canonical 27-char format, and the owner blobs rebuild the
    /// exact link the create printed (what `bb shares` shows).
    #[test]
    fn share_material_roundtrips_to_the_printed_link() {
        let mk = beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32]);
        let file_uuid = uuid::Uuid::parse_str("4c53f27f-c64f-4d65-8872-ea36011ef316").unwrap();
        let k_c = [9u8; 32];
        let token = beebeeb_core::share_token::generate_share_token();
        let m = build_share_material(&mk, &file_uuid, &k_c, token.clone()).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;

        assert_eq!(m.token.len(), 27);
        let fk = beebeeb_core::kdf::derive_file_key(&mk, file_uuid.to_string().as_bytes());
        let opened = unwrap_share_blob(&k_c, &b64.decode(&m.wrapped_file_key).unwrap()).unwrap();
        assert_eq!(
            opened,
            fk.as_bytes().to_vec(),
            "recipient with #key= must get the upload file key"
        );
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&m.key_fragment)
                .unwrap(),
            k_c.to_vec()
        );

        let printed = share_link("http://localhost:5360", &m.token, &m.key_fragment);
        assert!(printed.starts_with(&format!("http://localhost:5360/s/{token}#key=")));
        let rebuilt = recover_share_link(
            &mk,
            &m.owner_wrapped_key,
            &m.owner_wrapped_token,
            "http://localhost:5360",
        )
        .unwrap();
        assert_eq!(rebuilt, printed);

        let other = beebeeb_core::kdf::MasterKey::from_bytes([8u8; 32]);
        assert!(recover_share_link(&other, &m.owner_wrapped_key, &m.owner_wrapped_token, "x").is_none());
    }

    /// Independent AES-256-GCM reference (test-only dep), no AAD.
    fn reference_wrap(k_c: &[u8; 32], plaintext: &[u8], nonce: &[u8]) -> Vec<u8> {
        use aes_gcm::aead::Aead;
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
        let c = Aes256Gcm::new_from_slice(k_c).unwrap();
        let mut out = nonce.to_vec();
        out.extend(c.encrypt(Nonce::from_slice(nonce), plaintext).unwrap());
        out
    }

    /// Byte-for-byte KAT. (1) The reference reproduces the core vector bytes
    /// exactly (pins the reference). (2) The CLI's wrapped_file_key, under the
    /// KAT K_c and KAT file key, equals reference(K_c, key, its own nonce) —
    /// i.e. exactly the vector's construction; with the KAT nonce that is the
    /// vector's bytes. (3) The CLI's unwrap opens the vector blob.
    #[test]
    fn share_wrap_matches_core_kat_vector() {
        let k_c = arr32(&hex(KAT_WRAP_KEY));
        let key = hex(KAT_KEY_TO_WRAP);
        let mut vector_blob = hex(KAT_NONCE);
        vector_blob.extend_from_slice(&hex(KAT_CT));
        let b64 = base64::engine::general_purpose::STANDARD;

        assert_eq!(
            b64.encode(reference_wrap(&k_c, &key, &hex(KAT_NONCE))),
            b64.encode(&vector_blob),
            "reference must reproduce the core share_key_wrap vector"
        );

        let wire = wrap_file_key_for_share(&k_c, &arr32(&key)).expect("wrap");
        let raw = b64.decode(&wire).expect("wrapped_file_key must be STANDARD base64");
        assert!(raw.len() >= 12, "blob too short");
        assert_eq!(
            b64.encode(&raw),
            b64.encode(reference_wrap(&k_c, &key, &raw[..12])),
            "CLI wrapped_file_key must be nonce || AES-256-GCM(raw K_c, file_key) — the core K3 vector construction"
        );

        let opened = unwrap_share_blob(&k_c, &vector_blob).expect("KAT blob must open");
        assert_eq!(opened, key);
    }
}
