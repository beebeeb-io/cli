//! `bb logout`: end the session on the server and forget the token locally.

use colored::Colorize;

use crate::api::ApiClient;
use crate::config::{clear_config, load_config};

pub async fn run() -> Result<(), String> {
    let config = load_config();
    // Signed out = no session AND nothing stored anywhere. A keyring that could not be read this run still has an
    // entry (and a marker in config.json) that logout must remove (LD-0029, F6).
    if config.session_token.is_none() && config.secrets_location.is_none() && !crate::config::has_stored_sign_in() {
        println!("  {}", "Already logged out.".custom_color(crate::colors::INK_DIM),);
        return Ok(());
    }

    // Best-effort server-side logout; the local sign-in is cleared regardless. When the session could not be ended
    // (no network, or the keyring could not be read so there was no token to end it with), it stays valid on the
    // server until it expires, and the person is told where to end it (review P3-5).
    let server_ended = match config.session_token {
        Some(_) => ApiClient::from_config().logout().await.is_ok(),
        None => false,
    };
    let session_note = (!server_ended).then(|| {
        format!(
            "bb could not end this session on the server. End it from another device: Settings › Devices and \
             sessions{}.",
            config
                .email
                .as_deref()
                .map(|e| format!(" (signed in as {e})"))
                .unwrap_or_default()
        )
    });

    // The keyring entry (or credentials.json), then config.json.
    if let Err(e) = clear_config() {
        if let Some(note) = &session_note {
            eprintln!("  {note}");
        }
        return Err(e);
    }

    let email = config.email.as_deref().unwrap_or("unknown");
    println!(
        "  {} {}",
        "Logged out".custom_color(crate::colors::GREEN_OK),
        match &session_note {
            None => format!("· session for {email} ended"),
            Some(_) => format!("· {email} signed out on this machine"),
        }
        .custom_color(crate::colors::INK_DIM),
    );
    if let Some(note) = session_note {
        eprintln!("  {}", note.custom_color(crate::colors::RED_ERR));
    }

    Ok(())
}
