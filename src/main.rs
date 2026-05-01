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

    /// Show connection status, session health, storage usage
    Status,

    /// Show current configuration (secrets masked)
    Config,

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
        /// File ID to share
        file_id: String,

        /// Link expiry in hours (e.g. 24) or duration (e.g. "7d")
        #[arg(long)]
        expires: Option<String>,

        /// Maximum number of times the link can be opened
        #[arg(long)]
        max_opens: Option<u32>,

        /// Prompt for a passphrase to protect the link
        #[arg(long)]
        passphrase: bool,
    },

    /// List all active share links
    Shares,

    /// Revoke a share link
    Unshare {
        /// Share ID to revoke
        share_id: String,
    },

    /// Watch a folder and auto-sync changes to your vault
    Watch {
        /// Path to the folder to watch
        path: PathBuf,

        /// Parent folder ID in the vault
        #[arg(long)]
        parent: Option<String>,
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
        Commands::Status => commands::status::run().await,
        Commands::Config => commands::config::run().await,
        Commands::Push { path, parent } => commands::push::run(path, parent).await,
        Commands::Pull { file_id, output } => commands::pull::run(file_id, output).await,
        Commands::Ls { path } => commands::ls::run(path).await,
        Commands::Share {
            file_id,
            expires,
            max_opens,
            passphrase,
        } => commands::share::run(file_id, expires, max_opens, passphrase).await,
        Commands::Shares => commands::share::list().await,
        Commands::Unshare { share_id } => commands::share::revoke(share_id).await,
        Commands::Watch { path, parent } => commands::watch::run(path, parent).await,
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
