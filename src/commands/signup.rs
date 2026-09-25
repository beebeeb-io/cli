//! `bb signup` — task 1552. Creates a new Beebeeb account from the terminal:
//! verify the email with a code BEFORE the account is created (server task
//! 1525), then OPAQUE registration with a generated recovery phrase.
//!
//! Mirrors web PR #79 (`repos/web/src/pages/onboarding.tsx` +
//! `src/lib/signup-email-code.ts`) and mobile PR #110
//! (`repos/mobile/src/lib/signup-email-code.ts`), using the SAME
//! `beebeeb-core` crypto primitives those two wrap (WASM / native bindings
//! respectively) — the CLI links `beebeeb-core` directly, so no FFI layer
//! is needed here.
//!
//! **Deviation flagged, not shipped silently** (per the workspace's "Design
//! before code" rule): web's recovery-phrase step makes the user re-type 3
//! random words from the phrase before continuing (`mnemonic-verify.tsx`) —
//! proof they actually copied it down, not just clicked past it. This
//! command asks for a plain typed "yes" confirmation instead. A terminal
//! has no risk of the phrase silently scrolling off-screen behind other
//! UI the way a web modal can be dismissed by accident, and re-typing 3
//! words from a fixed-width terminal render is a materially worse UX than
//! in a styled web input — but it IS a weaker guarantee that the phrase was
//! actually saved somewhere durable. Worth a follow-up if this turns out
//! to matter in practice; flagged here rather than silently narrowing the
//! same safeguard web/mobile both ship.
//!
//! **No prior CLI account-creation command existed** (grounding note, worth
//! recording: task 1525's server-side review cited `repos/cli/src/api.rs:218`
//! as a "legacy `/auth/signup` caller" that would need a ticket — that
//! function (`ApiClient::signup`, plain JSON password, no OPAQUE) is in
//! fact `#[allow(dead_code)]` and has never had a caller; `bb` had no
//! signup/register command in the clap tree at all before this task. Built
//! fresh here using OPAQUE registration exclusively — same as web/mobile's
//! actual (non-deprecated) signup path — rather than wiring up the unused
//! plain-password function.)

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use colored::Colorize;
use std::io::Write;

use crate::api::{ApiClient, ApiError};
use crate::colors;
use crate::config::{load_config, save_config};
use crate::signup_email_code::{
    EMAIL_CODE_LENGTH, MAX_LIVE_CODES_PER_EMAIL, is_legacy_fallback_error, is_ticket_invalid_error,
    resend_copy_for_attempt, sanitize_code,
};

/// Mirrors web onboarding.tsx's `MIN_PASSWORD_LENGTH`.
const MIN_PASSWORD_LENGTH: usize = 12;

pub async fn run(email: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();

    let email = match email {
        Some(e) => e.trim().to_string(),
        None => prompt_line("  Email address: ")?,
    };
    if email.is_empty() || !email.contains('@') {
        return Err("a valid email address is required".to_string());
    }

    println!();
    println!(
        "  {} Starting signup for {}",
        "→".custom_color(colors::AMBER),
        email.custom_color(colors::AMBER)
    );

    // 1. Email-start is the capability probe (task 1552 mirrors web/mobile's
    //    choice): call it directly rather than a separate health check. 202
    //    → the server has the email-code flow; 404 → a pre-1525 server, fall
    //    back to plain OPAQUE registration with no ticket (harmless — the
    //    field is optional and ignored server-side pre-1525 too).
    let supports_email_code = match api.signup_email_start(&email).await {
        Ok(_) => true,
        Err(e) if is_legacy_fallback_error(&e) => {
            println!(
                "  {} server predates the email-verification signup flow — continuing without it",
                "i".custom_color(colors::INK_DIM)
            );
            false
        }
        Err(e) => return Err(format!("could not start signup: {}", e.message)),
    };

    let mut ticket: Option<String> = if supports_email_code {
        Some(run_code_step(&api, &email).await?)
    } else {
        None
    };

    // 2. Recovery phrase + password + OPAQUE registration. Looped so a
    //    ticket that expires mid-flow (server task 1525: 30-min TTL) can
    //    retry with a fresh code rather than dying — mirrors web
    //    onboarding.tsx's `signup_ticket_invalid` handling, which discards
    //    the stale ticket AND the already-shown phrase and starts that half
    //    over (never registers with a ticket that doesn't match the phrase
    //    the user was just shown).
    loop {
        let (phrase, master_key) = beebeeb_core::recovery::generate_recovery_phrase()
            .map_err(|e| format!("could not generate recovery phrase: {e}"))?;

        show_recovery_phrase(&phrase)?;
        let password = prompt_password_confirmed()?;

        println!();
        println!("  {} Setting up account encryption...", "→".custom_color(colors::AMBER));

        match finish_registration(&api, &email, &password, &master_key, ticket.as_deref()).await {
            Ok(session_token) => {
                let mut config = load_config();
                config.session_token = Some(session_token);
                config.email = Some(email.clone());
                config.master_key = Some(B64.encode(master_key.to_bytes()));
                save_config(&config)?;

                println!();
                println!(
                    "  {} Account created — signed in as {}",
                    "✓".green(),
                    email.custom_color(colors::AMBER)
                );
                return Ok(());
            }
            Err(e) if ticket.is_some() && is_ticket_invalid_error(&e) => {
                println!();
                println!(
                    "  {} Verification expired mid-signup — requesting a fresh code.",
                    "!".custom_color(colors::RED_ERR)
                );
                match api.signup_email_start(&email).await {
                    Ok(_) => {}
                    Err(e2) => return Err(format!("could not request a fresh code: {}", e2.message)),
                }
                ticket = Some(run_code_step(&api, &email).await?);
                // Loop again: a fresh phrase/master key is generated for
                // the fresh ticket — never register the OLD phrase against
                // a NEW ticket, they must be minted together.
            }
            Err(e) => return Err(format!("registration failed: {}", e.message)),
        }
    }
}

/// Prompts for the code, verifies it (with a resend option), and returns
/// the `signup_ticket`. Assumes the caller has JUST triggered a send via
/// `signup_email_start` (either the initial one in `run`, or the
/// ticket-expired retry) — this function only prompts/loops, it never
/// issues the FIRST send itself.
async fn run_code_step(api: &ApiClient, email: &str) -> Result<String, String> {
    println!();
    println!(
        "  {} Check {} for an {}-digit code. It expires in 15 minutes.",
        "✓".green(),
        email.custom_color(colors::AMBER),
        EMAIL_CODE_LENGTH
    );
    println!(
        "  {}",
        "(If an account already exists for this email, you'll get a sign-in link instead — no code.)"
            .custom_color(colors::INK_DIM)
    );

    let mut resend_attempt: u32 = 0;
    loop {
        let raw = prompt_line(&format!(
            "  Enter the {EMAIL_CODE_LENGTH}-digit code (or 'r' to resend): "
        ))?;
        let trimmed = raw.trim();

        if trimmed.eq_ignore_ascii_case("r") {
            resend_attempt += 1;
            match api.signup_email_start(email).await {
                Ok(_) => {
                    let (msg, _likely_sent) = resend_copy_for_attempt(resend_attempt, MAX_LIVE_CODES_PER_EMAIL);
                    println!("  {} {}", "→".custom_color(colors::AMBER), msg);
                }
                Err(e) => {
                    println!(
                        "  {} could not resend: {}",
                        "!".custom_color(colors::RED_ERR),
                        e.message
                    );
                }
            }
            continue;
        }

        let code = sanitize_code(trimmed, EMAIL_CODE_LENGTH);
        if code.len() != EMAIL_CODE_LENGTH {
            println!(
                "  {} that's {} digit{} — expected {}, try again",
                "!".custom_color(colors::RED_ERR),
                code.len(),
                if code.len() == 1 { "" } else { "s" },
                EMAIL_CODE_LENGTH
            );
            continue;
        }

        match api.signup_email_verify(email, &code).await {
            Ok(v) => {
                let ticket = v
                    .get("signup_ticket")
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| "server response missing signup_ticket".to_string())?
                    .to_string();
                println!("  {} email verified", "✓".green());
                return Ok(ticket);
            }
            Err(e) => {
                println!("  {} {}", "!".custom_color(colors::RED_ERR), e.message);
            }
        }
    }
}

fn show_recovery_phrase(phrase: &str) -> Result<(), String> {
    println!();
    println!(
        "  {}",
        "Your recovery phrase — write it down and store it somewhere safe.".custom_color(colors::INK_WARM)
    );
    println!(
        "  {}",
        "We cannot see it, and we cannot recover it for you. Lose it and your data is gone."
            .custom_color(colors::INK_DIM)
    );
    println!();
    println!("  {}", phrase.custom_color(colors::AMBER).bold());
    println!();

    loop {
        let confirm = prompt_line("  Type 'yes' once you have saved this phrase: ")?;
        if confirm.trim().eq_ignore_ascii_case("yes") {
            return Ok(());
        }
        println!("  {} type exactly 'yes' to continue", "!".custom_color(colors::RED_ERR));
    }
}

fn prompt_password_confirmed() -> Result<String, String> {
    loop {
        let password = rpassword::prompt_password(format!(
            "  {} ",
            "Choose a password (min 12 characters):".custom_color(colors::INK_DIM)
        ))
        .map_err(|e| format!("could not read password: {e}"))?;

        if password.chars().count() < MIN_PASSWORD_LENGTH {
            println!(
                "  {} password must be at least {} characters",
                "!".custom_color(colors::RED_ERR),
                MIN_PASSWORD_LENGTH
            );
            continue;
        }

        let confirm = rpassword::prompt_password(format!("  {} ", "Confirm password:".custom_color(colors::INK_DIM)))
            .map_err(|e| format!("could not read password: {e}"))?;

        if confirm != password {
            println!(
                "  {} passwords do not match — try again",
                "!".custom_color(colors::RED_ERR)
            );
            continue;
        }

        return Ok(password);
    }
}

fn prompt_line(label: &str) -> Result<String, String> {
    print!("{label}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("could not read input: {e}"))?;
    Ok(line.trim().to_string())
}

/// The OPAQUE registration round trip + key derivation. Returns the new
/// account's session token on success. Mirrors web onboarding.tsx's
/// `handlePasswordSubmit` step 1-3 exactly (same core primitives, same
/// wire fields), using `beebeeb_core` directly instead of the WASM/native
/// bindings web/mobile go through.
async fn finish_registration(
    api: &ApiClient,
    email: &str,
    password: &str,
    master_key: &beebeeb_core::kdf::MasterKey,
    signup_ticket: Option<&str>,
) -> Result<String, ApiError> {
    // Round 1: client registration start (blinds the password).
    let reg_start = beebeeb_core::opaque_protocol::client_registration_start(password.as_bytes())
        .map_err(|e| ApiError::from(format!("could not start OPAQUE registration: {e}")))?;
    let client_message_b64 = B64.encode(&reg_start.message);

    let start_resp = api
        .opaque_register_start(email, &client_message_b64, signup_ticket)
        .await?;
    let server_message_b64 = start_resp
        .get("server_message")
        .and_then(|m| m.as_str())
        .ok_or_else(|| ApiError::from("server response missing server_message".to_string()))?;
    let server_message = B64
        .decode(server_message_b64)
        .map_err(|e| ApiError::from(format!("invalid server_message encoding: {e}")))?;

    // Round 2: client registration finish (produces the OPAQUE credential
    // upload the server stores).
    let registration_upload = beebeeb_core::opaque_protocol::client_registration_finish(
        &reg_start.state,
        password.as_bytes(),
        &server_message,
    )
    .map_err(|e| ApiError::from(format!("could not finish OPAQUE registration: {e}")))?;
    let registration_upload_b64 = B64.encode(&registration_upload);

    // X25519 identity keypair + recovery check, both deterministically
    // derived from the recovery-phrase master key (never from the
    // password) — same as web's `deriveX25519Public`/`computeRecoveryCheck`.
    let x25519_private = beebeeb_core::opaque::derive_x25519_private(master_key);
    let x25519_public = beebeeb_core::opaque::derive_x25519_public(&x25519_private);
    let x25519_public_b64 = B64.encode(x25519_public);
    let recovery_check = beebeeb_core::opaque::compute_recovery_check(master_key);
    let recovery_check_b64 = B64.encode(*recovery_check);

    let finish_resp = api
        .opaque_register_finish(
            email,
            &registration_upload_b64,
            Some(&x25519_public_b64),
            Some(&recovery_check_b64),
            signup_ticket,
        )
        .await?;

    finish_resp
        .get("session_token")
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| ApiError::from("server response missing session_token".to_string()))
}
