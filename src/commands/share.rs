use base64::Engine as _;
use colored::Colorize;
use std::io::{self, Write};
use zeroize::Zeroizing;

use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal;

use crate::api::ApiClient;
use crate::config::load_config;
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
        let mk_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(&mk_b64)
                .map_err(|e| format!("invalid master key: {e}"))?,
        );
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
        let file_key = beebeeb_core::kdf::derive_file_key(&master_key, file_uuid.to_string().as_bytes());

        // Generate client key K_c (random 32 bytes)
        let client_key: [u8; 32] = rand::random();

        // Derive a wrap key from K_c: treat it as a MasterKey + HKDF over file_uuid
        // so the wrap key is file-specific (recipient can only decrypt THIS file).
        let client_mk = beebeeb_core::kdf::MasterKey::from_bytes(client_key);
        let wrap_key = beebeeb_core::kdf::derive_file_key(&client_mk, file_uuid.to_string().as_bytes());

        // Encrypt file_key.as_bytes() under wrap_key using AES-256-GCM
        let blob = beebeeb_core::encrypt::encrypt_chunk(&wrap_key, file_key.as_bytes())
            .map_err(|e| format!("encrypt file key: {e}"))?;
        let wfk_json = serde_json::to_string(&blob).map_err(|e| format!("serialize wrapped key: {e}"))?;

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

    // Extract file info from result if available
    let file_name = result
        .get("file")
        .and_then(|f| f.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let file_size = result
        .get("file")
        .and_then(|f| f.get("size_bytes"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Build the URL and key parts. In double-encrypted mode the URL fragment
    // (#key=…) is the decryption material — we print it on its own line so
    // the user is forced to think about the link and key as two separate
    // things, sent through different channels.
    let (bare_url, full_url) = if let Some(ref kc) = client_key_b64 {
        let app_url = std::env::var("APP_URL").unwrap_or_else(|_| "https://app.beebeeb.io".to_string());
        let bare = format!("{app_url}/s/{token}");
        let full = format!("{bare}#key={kc}");
        (bare, full)
    } else {
        let url = result
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string();
        // Standard mode: the server URL already contains the key fragment.
        // bare_url is the same — the key is not separable in this mode.
        (url.clone(), url)
    };

    // JSON mode: emit machine-readable output
    if ui::is_json() {
        let mut json_out = serde_json::json!({
            "share_id": share_id,
            "url": full_url,
            "expires_at": expires_at,
            "max_opens": max_opens,
            "double_encrypted": double_encrypted,
            "passphrase_protected": passphrase_value.is_some(),
        });
        if double_encrypted {
            if let Some(obj) = json_out.as_object_mut() {
                obj.insert("share_link".into(), serde_json::Value::String(bare_url.clone()));
                if let Some(ref kc) = client_key_b64 {
                    obj.insert("decryption_key".into(), serde_json::Value::String(kc.clone()));
                }
            }
        }
        println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
        return Ok(());
    }

    if ui::is_quiet() {
        println!("{full_url}");
        return Ok(());
    }

    // Rich mode
    println!();
    if double_encrypted {
        println!(
            "  {}",
            "\u{2713} Link created (end-to-end encrypted)".custom_color(crate::colors::GREEN_OK)
        );
    } else {
        println!("  {}", "\u{2713} Link created".custom_color(crate::colors::GREEN_OK));
    }

    if double_encrypted {
        // Print the URL and the decryption key on separate lines so the user
        // is reminded to send them through different channels.
        println!(
            "  {} {}",
            "share link    ".custom_color(crate::colors::INK_DIM),
            bare_url.custom_color(crate::colors::AMBER),
        );
        if let Some(ref kc) = client_key_b64 {
            println!(
                "  {} {}",
                "decryption key".custom_color(crate::colors::INK_DIM),
                kc.custom_color(crate::colors::AMBER),
            );
            println!(
                "  {}",
                "               (send this through a SEPARATE channel)".custom_color(crate::colors::INK_SAGE),
            );
        }
    } else {
        println!(
            "  {} {}",
            "url       ".custom_color(crate::colors::INK_DIM),
            full_url.custom_color(crate::colors::AMBER),
        );
    }
    if !file_name.is_empty() {
        let size_str = if file_size > 0 {
            format!(" \u{00b7} {}", ui::human_size(file_size))
        } else {
            String::new()
        };
        println!(
            "  {} {}",
            "file      ".custom_color(crate::colors::INK_DIM),
            format!("{file_name}{size_str}").custom_color(crate::colors::INK),
        );
    }
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
    if double_encrypted {
        println!(
            "  {} {}",
            "encryption".custom_color(crate::colors::INK_DIM),
            "end-to-end \u{00b7} server blind".custom_color(crate::colors::GREEN_OK),
        );
    } else {
        println!(
            "  {} {}",
            "encryption".custom_color(crate::colors::INK_DIM),
            "server-assisted recovery \u{00b7} less secure".custom_color(crate::colors::AMBER),
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
            "Beebeeb cannot decrypt this share. If the recipient loses the key,".custom_color(crate::colors::AMBER),
        );
        println!(
            "  {}",
            "the share is permanently inaccessible.".custom_color(crate::colors::AMBER),
        );
    } else {
        println!(
            "  {}",
            "Heads up: Beebeeb's servers can technically decrypt this share.".custom_color(crate::colors::AMBER),
        );
    }
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
        let url = share.get("url").and_then(|v| v.as_str()).unwrap_or("-");
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
            url.custom_color(crate::colors::AMBER),
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

        match ev {
            Event::Key(key_event) => match key_event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if selected > 0 {
                        selected -= 1;
                    }
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
            },
            _ => {}
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
