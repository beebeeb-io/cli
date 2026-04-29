use colored::Colorize;
use std::io::{self, Write};

use crate::api::ApiClient;

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
) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let passphrase_value = if passphrase {
        print!(
            "  {}",
            "? Passphrase (12+ chars, mixed): ".truecolor(245, 184, 0),
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
            "wrapping chunk keys with Argon2id(passphrase)".truecolor(106, 101, 91),
        );
    }

    let result = api
        .create_share(
            &file_id,
            expires_hours,
            max_opens,
            passphrase_value.as_deref(),
        )
        .await?;

    let url = result
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let share_id = result
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let expires_at = result
        .get("expires_at")
        .and_then(|v| v.as_str())
        .unwrap_or("never");

    println!();
    println!("  {}", "Link created".truecolor(143, 193, 139));
    println!(
        "  {} {}",
        "url       ".truecolor(106, 101, 91),
        url.truecolor(245, 184, 0),
    );
    println!(
        "  {} {}",
        "expires   ".truecolor(106, 101, 91),
        expires_at.truecolor(233, 230, 221),
    );
    if let Some(max) = max_opens {
        println!(
            "  {} {}",
            "max-opens ".truecolor(106, 101, 91),
            max.to_string().truecolor(233, 230, 221),
        );
    }
    println!(
        "  {} {}",
        "share-id  ".truecolor(106, 101, 91),
        share_id.truecolor(208, 200, 154),
    );
    println!();
    if passphrase_value.is_some() {
        println!(
            "  {}",
            "# send the passphrase by a different channel — we will never see it"
                .truecolor(125, 138, 106),
        );
    }
    println!(
        "  {}",
        format!("# revoke anytime:  bb unshare {share_id}").truecolor(125, 138, 106),
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
            "no active shares".truecolor(106, 101, 91),
        );
        return Ok(());
    };

    if shares.is_empty() {
        println!(
            "  {}",
            "no active shares".truecolor(106, 101, 91),
        );
        return Ok(());
    }

    println!(
        "  {}",
        format!(
            "{:<36}  {:<40}  {:<20}  {}",
            "file", "url", "expires", "opens"
        )
        .truecolor(106, 101, 91),
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
            file_name.truecolor(208, 200, 154),
            url.truecolor(245, 184, 0),
            expires.truecolor(106, 101, 91),
            opens_display.truecolor(233, 230, 221),
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
        "Revoked".truecolor(143, 193, 139),
        format!("· share {share_id} is no longer accessible").truecolor(106, 101, 91),
    );

    Ok(())
}
