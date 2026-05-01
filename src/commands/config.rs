use colored::Colorize;

use crate::config::load_config;

pub async fn run() -> Result<(), String> {
    let config = load_config();
    let dim = |s: &str| s.truecolor(106, 101, 91);
    let text = |s: &str| s.truecolor(233, 230, 221);

    println!();
    println!("  {}", "beebeeb config".truecolor(245, 184, 0));
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
                        .truecolor(233, 230, 221)
                } else {
                    "(set)".truecolor(233, 230, 221)
                }
            }
            None => "(none)".truecolor(106, 101, 91),
        },
    );

    println!(
        "  {}  {}",
        dim("master_key   "),
        match &config.master_key {
            Some(_) => "(set, hidden)".truecolor(143, 193, 139),
            None => "(none)".truecolor(106, 101, 91),
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
