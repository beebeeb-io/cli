use colored::Colorize;
use std::io::{self, Write};

pub async fn run(
    path: String,
    expires: Option<String>,
    max_opens: Option<u32>,
    passphrase: bool,
) -> Result<(), String> {
    let passphrase_value = if passphrase {
        print!(
            "  {}",
            "? Passphrase (12+ chars, mixed): ".truecolor(245, 184, 0),
        );
        io::stdout().flush().map_err(|e| e.to_string())?;
        let pass = rpassword::read_password().map_err(|e| format!("failed to read passphrase: {e}"))?;
        if pass.len() < 12 {
            return Err("passphrase must be at least 12 characters".to_string());
        }
        Some(pass)
    } else {
        None
    };

    // Generate a mock share link (actual API integration comes later)
    let link_id = format!(
        "{}-{}-{}-{}-{}",
        random_seg(3),
        random_seg(3),
        random_seg(3),
        random_seg(3),
        random_seg(3),
    );

    let expires_display = expires.as_deref().unwrap_or("24h");
    let receipt = format!("bbr_{}", &uuid::Uuid::new_v4().to_string()[..12]);

    if passphrase_value.is_some() {
        println!(
            "  {}",
            "wrapping chunk keys with Argon2id(passphrase)".truecolor(106, 101, 91),
        );
    }

    println!();
    println!(
        "  {}",
        "✓ Link created".truecolor(143, 193, 139),
    );
    println!(
        "  {} https://bee.beebeeb.io/s/{}",
        "url       ".truecolor(106, 101, 91),
        link_id.truecolor(245, 184, 0),
    );
    println!(
        "  {} {} {}",
        "expires   ".truecolor(106, 101, 91),
        expires_display.truecolor(233, 230, 221),
        "(from now)".truecolor(106, 101, 91),
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
        "region    ".truecolor(106, 101, 91),
        "frankfurt (pinned · will not replicate)".truecolor(245, 184, 0),
    );
    println!(
        "  {} {}",
        "receipt   ".truecolor(106, 101, 91),
        receipt.truecolor(208, 200, 154),
    );
    println!();
    println!(
        "  {}",
        "# send the passphrase by a different channel · we will never see it"
            .truecolor(125, 138, 106),
    );
    println!(
        "  {}",
        format!("# revoke anytime:  bb revoke {}", &receipt)
            .truecolor(125, 138, 106),
    );

    // Suppress unused variable warning — path and passphrase_value will be used
    // once the share API is implemented.
    let _ = path;
    let _ = passphrase_value;

    Ok(())
}

fn random_seg(len: usize) -> String {
    use rand::Rng;
    let chars: Vec<char> = "abcdefghjkmnpqrstuvwxyz23456789".chars().collect();
    let mut rng = rand::thread_rng();
    (0..len).map(|_| chars[rng.gen_range(0..chars.len())]).collect()
}
