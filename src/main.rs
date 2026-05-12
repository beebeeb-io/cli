mod api;
mod colors;
mod commands;
mod config;
mod crypto;
mod loopback;
mod update;

use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};
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
            .custom_color(crate::colors::INK_SAGE),
        ""
    ),
)]
struct Cli {
    /// API base URL to use for this command (login persists it for future commands)
    #[arg(long, global = true, value_name = "URL")]
    api: Option<String>,

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

    /// Show storage quota: used / total / file count (color-coded)
    Quota,

    /// Show current configuration (secrets masked)
    Config,

    /// Upload a file or folder to your vault
    #[command(alias = "upload")]
    Push {
        /// Path to the file or folder to upload
        path: PathBuf,

        /// Parent folder ID in the vault
        #[arg(long)]
        parent: Option<String>,

        /// Root-level folder name or ID to upload into
        #[arg(long, conflicts_with = "parent")]
        folder: Option<String>,

        /// When a file with the same name exists: replace it (creates a new version)
        #[arg(long, conflicts_with = "keep_both")]
        replace: bool,

        /// When a file with the same name exists: upload with a numeric suffix
        #[arg(long, conflicts_with = "replace")]
        keep_both: bool,
    },

    /// Download a file from your vault
    #[command(alias = "download")]
    Pull {
        /// File ID to download
        file_id: String,

        /// Output path (defaults to decrypted filename or file ID)
        output: Option<PathBuf>,

        /// Output path (defaults to decrypted filename or file ID)
        #[arg(short = 'o', long = "output", value_name = "PATH", conflicts_with = "output")]
        output_flag: Option<PathBuf>,
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

        /// Double-encrypt: client wraps file key so server cannot decrypt
        #[arg(long)]
        double_encrypted: bool,
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

    /// Bidirectionally sync a local folder with a remote vault path
    Sync {
        /// Local directory to sync
        local_dir: PathBuf,

        /// Remote vault path (e.g. "/Documents"). If omitted, uses path stored in .bb-sync.json.
        remote_path: Option<String>,

        /// Show what would change without making any modifications
        #[arg(long)]
        dry_run: bool,

        /// Overwrite conflicts with the local copy (local wins)
        #[arg(long)]
        force: bool,

        /// Trash remote files that no longer exist locally (use with care)
        #[arg(long)]
        delete: bool,
    },

    /// Mount vault as a FUSE filesystem (read-only Day 1; requires macFUSE on macOS)
    Mount {
        /// Directory to mount the vault at (e.g. ~/Beebeeb)
        mountpoint: PathBuf,

        /// Stay in foreground (default: daemonize after mount succeeds)
        #[arg(long, default_value_t = false)]
        foreground: bool,

        /// Cache TTL for directory listings in seconds (0 = no cache)
        #[arg(long, default_value_t = 30)]
        cache_ttl: u64,
    },

    /// Unmount a previously mounted vault FUSE filesystem
    Unmount {
        /// Mountpoint to unmount
        mountpoint: PathBuf,
    },

    /// Serve vault as a local WebDAV server (mounts in Finder, rclone, Cyberduck)
    Webdav {
        /// TCP port to listen on (default: 7878)
        #[arg(long, default_value_t = 7878)]
        port: u16,

        /// Block all write operations (PUT, DELETE, MKCOL, MOVE)
        #[arg(long, default_value_t = false)]
        read_only: bool,

        /// Directory listing cache TTL in seconds (0 = disabled)
        #[arg(long, default_value_t = 30)]
        cache_ttl: u64,

        /// Disable path cache entirely (useful for debugging)
        #[arg(long, default_value_t = false)]
        no_cache: bool,
    },

    /// Repair files encrypted with old binary-UUID key derivation (makes them readable in the web app)
    Repair {
        /// Show what would be repaired without making changes
        #[arg(long)]
        dry_run: bool,
    },

    /// Rotate your master vault key
    Rotate,

    /// End current session
    Logout,

    /// Print shell completion script to stdout
    ///
    /// Pipe the output into the correct file for your shell:
    ///
    ///   bb completions bash > ~/.local/share/bash-completion/completions/bb
    ///
    ///   bb completions zsh > ~/.zfunc/_bb
    ///
    ///   bb completions fish > ~/.config/fish/completions/bb.fish
    ///
    ///   bb completions powershell > ~/Documents/PowerShell/completions/bb.ps1
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[tokio::main]
async fn main() {
    update::check_and_update().await;

    let cli = Cli::parse();

    if let Some(api_url) = cli.api {
        if let Err(e) = config::set_api_url_override(api_url) {
            eprintln!(
                "  {} {}",
                "error:".custom_color(crate::colors::RED_ERR),
                e.custom_color(crate::colors::INK),
            );
            std::process::exit(1);
        }
    }

    let result = match cli.command {
        Commands::Login => commands::login::run().await,
        Commands::Whoami => commands::whoami::run().await,
        Commands::Status => commands::status::run().await,
        Commands::Quota => commands::quota::run().await,
        Commands::Config => commands::config::run().await,
        Commands::Push { path, parent, folder, replace, keep_both } => {
            commands::push::run(path, parent, folder, replace, keep_both).await
        }
        Commands::Pull { file_id, output, output_flag } => {
            commands::pull::run(file_id, output.or(output_flag)).await
        }
        Commands::Ls { path } => commands::ls::run(path).await,
        Commands::Share {
            file_id,
            expires,
            max_opens,
            passphrase,
            double_encrypted,
        } => commands::share::run(file_id, expires, max_opens, passphrase, double_encrypted).await,
        Commands::Shares => commands::share::list().await,
        Commands::Unshare { share_id } => commands::share::revoke(share_id).await,
        Commands::Watch { path, parent } => commands::watch::run(path, parent).await,
        Commands::Sync {
            local_dir,
            remote_path,
            dry_run,
            force,
            delete,
        } => commands::sync::run(local_dir, remote_path, dry_run, force, delete).await,
        Commands::Mount { mountpoint, foreground, cache_ttl } => {
            commands::mount::run(mountpoint, foreground, cache_ttl).await
        }
        Commands::Unmount { mountpoint } => {
            commands::mount::unmount(mountpoint).await
        }
        Commands::Webdav { port, read_only, cache_ttl, no_cache } => {
            commands::webdav::run(port, read_only, cache_ttl, no_cache).await
        }
        Commands::Repair { dry_run } => commands::repair::run(dry_run).await,
        Commands::Rotate => {
            println!(
                "  {}",
                "▲ Key rotation is not yet implemented.".custom_color(crate::colors::AMBER),
            );
            println!(
                "  {}",
                "  This will rotate your master vault key and re-wrap all file keys."
                    .custom_color(crate::colors::INK_DIM),
            );
            Ok(())
        }
        Commands::Logout => commands::logout::run().await,
        Commands::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "bb",
                &mut std::io::stdout(),
            );
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!(
            "  {} {}",
            "error:".custom_color(crate::colors::RED_ERR),
            e.custom_color(crate::colors::INK),
        );
        std::process::exit(1);
    }
}
