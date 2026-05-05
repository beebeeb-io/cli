use colored::Colorize;

use crate::config::load_config;

pub async fn run() -> Result<(), String> {
    let config = load_config();
    let dim = |s: &str| s.custom_color(crate::colors::INK_DIM);
    let text = |s: &str| s.custom_color(crate::colors::INK);

    println!();
    println!("  {}", "beebeeb config".custom_color(crate::colors::AMBER));
    println!();

    println!("  {}  {}", dim("api_url      "), text(&config.api_url));

    println!(
        "  {}  {}",
        dim("email        "),
        text(config.email.as_deref().unwrap_or("(none)")),
    );

    println!(
        "  {}  {}",
        dim("session_token"),
        match &config.session_token {
            Some(t) => {
                // Show first 8 and last 4 chars, mask the rest
                if t.len() > 16 {
                    format!("{}...{}", &t[..8], &t[t.len() - 4..])
                        .custom_color(crate::colors::INK)
                } else {
                    "(set)".custom_color(crate::colors::INK)
                }
            }
            None => "(none)".custom_color(crate::colors::INK_DIM),
        },
    );

    println!(
        "  {}  {}",
        dim("master_key   "),
        match &config.master_key {
            Some(_) => "(set, hidden)".custom_color(crate::colors::GREEN_OK),
            None => "(none)".custom_color(crate::colors::INK_DIM),
        },
    );

    // Show config file path
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("beebeeb")
        .join("config.json");
    println!();
    println!(
        "  {}  {}",
        dim("path         "),
        dim(&config_path.display().to_string()),
    );

    println!();
    Ok(())
}
