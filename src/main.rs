#[allow(dead_code)]
mod api;
mod commands;
mod config;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use colored::Colorize;

/// bb — Beebeeb CLI · end-to-end encrypted vault from the terminal
#[derive(Parser)]
#[command(
    name = "bb",
    version,
    about = "end-to-end encrypted vault, from the terminal",
    long_about = None,
    after_help = format!(
        "{}\n{}",
        "# docs · beebeeb.io/cli · key fingerprints · beebeeb.io/fingerprints"
            .truecolor(125, 138, 106),
        ""
    ),
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate with your Beebeeb account
    Login,

    /// Show current session, device, region, quota
    Whoami,

    /// Upload a file or folder to your vault
    Push {
        /// Path to the file or folder to upload
        path: PathBuf,

        /// Parent folder ID in the vault
        #[arg(long)]
        parent: Option<String>,
    },

    /// Download a file from your vault
    Pull {
        /// File ID to download
        file_id: String,

        /// Output path (defaults to file ID as filename)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// List files (decrypts names locally)
    Ls {
        /// Folder path or ID to list
        path: Option<String>,
    },

    /// Create an encrypted share link
    Share {
        /// File path or ID to share
        path: String,

        /// Link expiry duration (e.g. "24h", "7d")
        #[arg(long, default_value = "24h")]
        expires: Option<String>,

        /// Maximum number of times the link can be opened
        #[arg(long)]
        max_opens: Option<u32>,

        /// Prompt for a passphrase to protect the link
        #[arg(long)]
        passphrase: bool,
    },

    /// Rotate your master vault key
    Rotate,

    /// End current session
    Logout,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Login => commands::login::run().await,
        Commands::Whoami => commands::whoami::run().await,
        Commands::Push { path, parent } => commands::push::run(path, parent).await,
        Commands::Pull { file_id, output } => commands::pull::run(file_id, output).await,
        Commands::Ls { path } => commands::ls::run(path).await,
        Commands::Share {
            path,
            expires,
            max_opens,
            passphrase,
        } => commands::share::run(path, expires, max_opens, passphrase).await,
        Commands::Rotate => {
            println!(
                "  {}",
                "▲ Key rotation is not yet implemented.".truecolor(245, 184, 0),
            );
            println!(
                "  {}",
                "  This will rotate your master vault key and re-wrap all file keys."
                    .truecolor(106, 101, 91),
            );
            Ok(())
        }
        Commands::Logout => commands::logout::run().await,
    };

    if let Err(e) = result {
        eprintln!(
            "  {} {}",
            "error:".truecolor(224, 122, 106),
            e.truecolor(233, 230, 221),
        );
        std::process::exit(1);
    }
}
