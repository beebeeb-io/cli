mod api;
mod colors;
mod commands;
mod config;
mod crypto;
pub mod daemon;
pub mod device;
mod download;
mod env_detect;
mod loopback;
mod path;
mod resume;
mod thumbnail;
mod tui;
mod ui;
mod update;
mod upload;

// Peak-heap tracking allocator, active ONLY in test builds (`#[cfg(test)]`), so
// the shipped `bb` binary keeps the default system allocator. Used by the
// upload RSS/peak-memory regression test (task 0666) to convert the modeled
// per-file memory multiplier into a measured, guarded fact.
#[cfg(test)]
pub(crate) mod test_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    pub struct Tracking;

    unsafe impl GlobalAlloc for Tracking {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
                PEAK.fetch_max(live, Ordering::Relaxed);
            }
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    /// Current live (allocated-not-freed) bytes across the process.
    pub fn live() -> usize {
        LIVE.load(Ordering::Relaxed)
    }
    /// High-water mark of `live()` since the last [`reset_peak`].
    pub fn peak() -> usize {
        PEAK.load(Ordering::Relaxed)
    }
    /// Reset the peak high-water mark to the current live total.
    pub fn reset_peak() {
        PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

#[cfg(test)]
#[global_allocator]
static GLOBAL_ALLOC: test_alloc::Tracking = test_alloc::Tracking;

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

    /// Output structured JSON
    #[arg(long, global = true)]
    json: bool,

    /// Minimal output, no progress or colors
    #[arg(long, global = true)]
    quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate with your Beebeeb account
    Login {
        /// Skip browser auto-open and print only the URL + code
        /// (use this on SSH or boxes without a window server)
        #[arg(long)]
        headless: bool,
    },

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

    /// Download a file by UUID, short ID, or path
    #[command(alias = "download")]
    Pull {
        /// File UUID, short ID prefix (e.g. 3e15382b), or vault path
        file_id: String,

        /// Output path (defaults to decrypted filename or file ID)
        output: Option<PathBuf>,

        /// Output path (defaults to decrypted filename or file ID)
        #[arg(short = 'o', long = "output", value_name = "PATH", conflicts_with = "output")]
        output_flag: Option<PathBuf>,

        /// Download an entire folder as a zip archive
        #[arg(long)]
        zip: bool,
    },

    /// List files (decrypts names locally)
    Ls {
        /// Folder path or ID to list
        path: Option<String>,

        /// Long format — adds a CREATED column
        #[arg(short = 'l', long = "long")]
        long: bool,

        /// Include trashed entries (flagged)
        #[arg(short = 'a', long = "all")]
        all: bool,

        /// Recurse into subfolders
        #[arg(short = 'R', long = "recursive")]
        recursive: bool,

        /// Maximum recursion depth (with --recursive)
        #[arg(long = "depth", default_value_t = 3)]
        depth: usize,

        /// Sort field: name | size | modified | created
        #[arg(long = "sort", default_value = "name")]
        sort: String,

        /// Reverse the sort order
        #[arg(short = 'r', long = "reverse")]
        reverse: bool,
    },

    /// Create a folder in your vault (mirrors `mkdir`)
    Mkdir {
        /// Vault path of the new folder (e.g. `/Photos/2026`)
        path: String,

        /// Create intermediate folders as needed (mirrors `mkdir -p`)
        #[arg(short = 'p', long = "parents")]
        parents: bool,
    },

    /// Move or rename a file or folder (mirrors `mv`)
    Mv {
        /// One or more sources, plus a destination as the final positional
        #[arg(num_args = 2.., required = true)]
        paths: Vec<String>,
    },

    /// Move files/folders to the trash (reversible with `bb restore`)
    Rm {
        /// One or more vault paths or UUIDs to trash
        #[arg(required = true, num_args = 1..)]
        targets: Vec<String>,

        /// Allow trashing folders and their contents (mirrors `rm -r`)
        #[arg(short = 'r', long = "recursive")]
        recursive: bool,

        /// Skip the confirmation prompt
        #[arg(short = 'f', long = "yes", visible_alias = "force")]
        yes: bool,
    },

    /// Restore a trashed file/folder
    Restore {
        /// Trashed entry name or UUID (see `bb trash list`)
        target: String,
    },

    /// Browse the trash
    Trash {
        #[command(subcommand)]
        cmd: TrashCmd,
    },

    /// Search file and folder names across your vault (decrypted locally)
    Search {
        /// Substring (default) or regex (with --regex) to match against names
        query: String,

        /// Treat the query as a case-insensitive regular expression
        #[arg(long)]
        regex: bool,

        /// Maximum number of matches to return
        #[arg(long, default_value_t = 50)]
        limit: usize,

        /// Restrict the search to a subtree (vault path of a folder)
        #[arg(long)]
        folder: Option<String>,
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

        /// Opt out of double encryption. Default is end-to-end encrypted —
        /// the server stores an opaque blob and cannot decrypt the share.
        /// Passing this flag lets Beebeeb hold a server-wrapped copy of the
        /// key (less secure, allows server-assisted recovery).
        #[arg(long = "no-double-encrypt")]
        no_double_encrypt: bool,
    },

    /// Create and manage file requests — links that let anyone upload into your vault
    #[command(subcommand)]
    Request(RequestCmd),

    /// List all active share links
    Shares,

    /// Revoke a share link
    Unshare {
        /// Share ID to revoke (omit for interactive picker)
        share_id: Option<String>,
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
        /// Local directory to sync (omit to show all active sessions)
        local_dir: Option<PathBuf>,

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

        /// One-shot sync then exit (default is continuous watch after sync)
        #[arg(long)]
        once: bool,

        /// Run as a background daemon with auto-start on login
        #[arg(long)]
        daemon: bool,

        /// Uninstall the sync LaunchAgent (legacy — use --stop-session <name>)
        #[arg(long)]
        stop: bool,

        /// Show all active sync sessions across devices
        #[arg(long)]
        status: bool,

        /// Stop a sync session by name
        #[arg(long, value_name = "NAME")]
        stop_session: Option<String>,

        /// Stop all sync sessions on this device
        #[arg(long)]
        stop_all: bool,

        /// Number of parallel uploads (default: 4)
        #[arg(long, default_value_t = 4)]
        concurrency: usize,

        /// Re-hash every local file instead of trusting unchanged (size+mtime)
        /// entries from the last sync — slower, but catches same-size edits
        #[arg(long)]
        rehash: bool,
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

        /// Log every request (default: compact activity counters)
        #[arg(long, default_value_t = false)]
        verbose: bool,
    },

    /// Benchmark network latency, upload/download throughput, and crypto speed
    Speedtest,

    /// Repair files encrypted with old binary-UUID key derivation (makes them readable in the web app)
    Repair {
        /// Show what would be repaired without making changes
        #[arg(long)]
        dry_run: bool,
    },

    /// View billing info (read-only — manage plans on the web)
    Billing {
        #[command(subcommand)]
        action: BillingAction,
    },

    /// End current session
    Logout,

    /// Manage your Beebeeb account — profile, plan, security, export, delete
    #[command(subcommand)]
    Account(AccountCmd),

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

#[derive(Subcommand)]
enum TrashCmd {
    /// List trashed files and folders
    List,
}

#[derive(Subcommand)]
enum RequestCmd {
    /// Create a file request and print its shareable link
    Create {
        /// Target folder (UUID or top-level folder name) the uploads land in
        #[arg(long)]
        folder: Option<String>,

        /// Maximum number of files the request will accept
        #[arg(long)]
        max_files: Option<u32>,

        /// Maximum total bytes (e.g. 100MB, 2GB, 500MiB)
        #[arg(long)]
        max_bytes: Option<String>,

        /// Expiry (e.g. 7d, 24h, 30m, 2w; a bare number means days)
        #[arg(long)]
        expires: Option<String>,

        /// Title shown to whoever opens the link (defaults to "File request")
        #[arg(long)]
        title: Option<String>,

        /// Optional description shown to the uploader
        #[arg(long)]
        desc: Option<String>,
    },

    /// List your file requests with received counts and links
    List,

    /// Close (default) or hard-delete a file request
    Rm {
        /// File request ID
        id: String,

        /// Hard-delete instead of closing (destructive)
        #[arg(long)]
        delete: bool,
    },

    /// Upload file(s) to a request link — no account needed
    Send {
        /// The file-request link (…/r/<token>#<public-key>)
        url: String,

        /// One or more files to upload
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Show profile, plan, security, sessions, passkeys
    Show,
    /// Change email address (verification sent to the new inbox)
    Update {
        /// New email address
        #[arg(long)]
        email: String,
    },
    /// GDPR data export — initiate, poll, or download
    #[command(subcommand)]
    Export(AccountExportCmd),
    /// Permanently delete account (irreversible)
    Delete {
        /// Your current email address (must match exactly)
        #[arg(long)]
        confirm: String,
    },
}

#[derive(Subcommand)]
enum AccountExportCmd {
    /// Queue a new export (rate-limited to 1 per 24h)
    Start,
    /// Check the status of an export job
    Status { job_id: String },
    /// Download a completed export
    Download {
        job_id: String,
        /// Destination path (default: ./beebeeb-export-<date>.zip)
        #[arg(short = 'o', long = "output")]
        output: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum BillingAction {
    /// Show plan, storage usage, renewal date, and pending changes
    Show {
        /// Output the raw API merge as JSON (use with jq)
        #[arg(long)]
        json: bool,
    },
}

fn print_custom_help() {
    use crate::{colors, ui};

    // Ensure color system is initialised (ui::init hasn't been called yet).
    ui::init(false, false, false);

    let version = env!("CARGO_PKG_VERSION");
    let w = 58;

    println!("{}", ui::box_header("BEEBEEB", w));
    println!(
        "{}",
        ui::box_line(
            &format!(
                "{}",
                "end-to-end encrypted vault, from the terminal".custom_color(colors::INK_DIM)
            ),
            w,
        )
    );
    println!(
        "{}",
        ui::box_line(
            &format!(
                "v{} · {}",
                version.custom_color(colors::INK_DIM),
                "e2ee".custom_color(colors::GREEN_OK)
            ),
            w,
        )
    );
    println!("{}", ui::box_footer(w));
    println!();

    println!("  {}", "COMMANDS".custom_color(colors::AMBER));
    let cmds: &[(&str, &str, &str)] = &[
        ("ls", "[path]", "list files (decrypts names locally)"),
        ("mkdir", "<path>", "create a folder · mirrors mkdir -p"),
        ("mv", "<src> <dst>", "move or rename · mirrors mv"),
        ("rm", "<path|id>...", "trash files/folders · -r · reversible"),
        ("restore", "<name|id>", "restore from trash"),
        ("trash", "list", "browse the trash"),
        ("search", "<query>", "find files by name · --regex"),
        ("push", "<path>", "upload · encrypts on the fly"),
        ("pull", "<id|path>", "download and decrypt"),
        ("share", "<id>", "create encrypted link (expiry, passphrase)"),
        ("sync", "<dir> [remote]", "sync + watch · continuous by default"),
        ("webdav", "", "mount vault in Finder / Explorer"),
        ("whoami", "", "user · plan · region · quota · session"),
        ("speedtest", "", "benchmark network + crypto speed"),
        ("repair", "", "fix cross-client encryption"),
    ];
    for (name, args, desc) in cmds {
        println!(
            "  {:<10}{:<18}{}",
            name.custom_color(colors::GREEN_OK),
            args.custom_color(colors::PATH),
            desc.custom_color(colors::INK_DIM)
        );
    }
    println!(
        "  {}",
        "+ login, logout, mount, shares, unshare, config, quota, status, completions".custom_color(colors::INK_DIM)
    );
    println!();

    println!("  {}", "FLAGS".custom_color(colors::AMBER));
    let flags: &[(&str, &str)] = &[
        ("--json", "structured JSON output"),
        ("--quiet", "minimal · no progress"),
        ("--api <url>", "override API endpoint"),
    ];
    for (flag, desc) in flags {
        println!(
            "  {:<14}{}",
            flag.custom_color(colors::CYAN),
            desc.custom_color(colors::INK_DIM)
        );
    }
    println!();
    println!(
        "  {}",
        "# docs · beebeeb.io/cli · fingerprints · beebeeb.io/fingerprints".custom_color(colors::INK_SAGE)
    );
}

#[tokio::main]
async fn main() {
    update::check_and_update().await;

    // Custom help — intercept before clap's default
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() == 1 || args.iter().any(|a| a == "--help" || a == "-h") {
            // Only intercept top-level help, not subcommand help like "bb push --help"
            let has_subcommand = args.iter().skip(1).any(|a| !a.starts_with('-'));
            if !has_subcommand && !args.iter().any(|a| a == "--json") {
                print_custom_help();
                std::process::exit(0);
            }
        }
    }

    let cli = Cli::parse();

    ui::init(cli.json, cli.quiet, cli.no_color);

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
        Commands::Login { headless } => commands::login::run(headless).await,
        Commands::Whoami => commands::whoami::run().await,
        Commands::Status => commands::status::run().await,
        Commands::Quota => commands::quota::run().await,
        Commands::Config => commands::config::run().await,
        Commands::Push {
            path,
            parent,
            folder,
            replace,
            keep_both,
        } => commands::push::run(path, parent, folder, replace, keep_both).await,
        Commands::Pull {
            file_id,
            output,
            output_flag,
            zip,
        } => commands::pull::run(file_id, output.or(output_flag), zip).await,
        Commands::Ls {
            path,
            long,
            all,
            recursive,
            depth,
            sort,
            reverse,
        } => match commands::ls::SortField::parse(&sort) {
            Ok(sort) => {
                let opts = commands::ls::LsOpts {
                    long,
                    all,
                    recursive,
                    depth,
                    sort,
                    reverse,
                };
                commands::ls::run(path, opts).await
            }
            Err(e) => Err(e),
        },
        Commands::Mkdir { path, parents } => commands::mkdir::run(path, parents).await,
        Commands::Mv { mut paths } => {
            // clap enforces num_args = 2.., so there is always a trailing dst.
            let dst = paths.pop().unwrap_or_default();
            commands::mv::run(paths, dst).await
        }
        Commands::Rm {
            targets,
            recursive,
            yes,
        } => commands::rm::run(targets, recursive, yes).await,
        Commands::Restore { target } => commands::restore::run(target).await,
        Commands::Trash { cmd } => match cmd {
            TrashCmd::List => commands::trash::list().await,
        },
        Commands::Search {
            query,
            regex,
            limit,
            folder,
        } => commands::search::run(query, regex, limit, folder).await,
        Commands::Share {
            file_id,
            expires,
            max_opens,
            passphrase,
            no_double_encrypt,
        } => commands::share::run(file_id, expires, max_opens, passphrase, !no_double_encrypt).await,
        Commands::Request(cmd) => match cmd {
            RequestCmd::Create {
                folder,
                max_files,
                max_bytes,
                expires,
                title,
                desc,
            } => commands::request::create(folder, max_files, max_bytes, expires, title, desc).await,
            RequestCmd::List => commands::request::list().await,
            RequestCmd::Rm { id, delete } => commands::request::rm(id, delete).await,
            RequestCmd::Send { url, files } => commands::request::send(url, files).await,
        },
        Commands::Shares => commands::share::list().await,
        Commands::Unshare { share_id } => commands::share::revoke(share_id).await,
        Commands::Watch { path, parent } => {
            eprintln!(
                "  {} bb watch is now bb sync. Redirecting...",
                "note".custom_color(crate::colors::INK_DIM)
            );
            commands::sync::run(
                Some(path),
                parent,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                None,
                false,
                4,
                false,
            )
            .await
        }
        Commands::Sync {
            local_dir,
            remote_path,
            dry_run,
            force,
            delete,
            once,
            daemon,
            stop,
            status,
            stop_session,
            stop_all,
            concurrency,
            rehash,
        } => {
            // Clamp parallel uploads to a sane window: 0 would stall the pipeline,
            // and unbounded values blow up peak memory (each in-flight file holds
            // ~chunk_size worth of buffers). (1, 8) is the supported range.
            let concurrency = concurrency.clamp(1, 8);
            commands::sync::run(
                local_dir,
                remote_path,
                dry_run,
                force,
                delete,
                once,
                daemon,
                stop,
                status,
                stop_session,
                stop_all,
                concurrency,
                rehash,
            )
            .await
        }
        Commands::Mount {
            mountpoint,
            foreground,
            cache_ttl,
        } => commands::mount::run(mountpoint, foreground, cache_ttl).await,
        Commands::Unmount { mountpoint } => commands::mount::unmount(mountpoint).await,
        Commands::Webdav {
            port,
            read_only,
            cache_ttl,
            no_cache,
            verbose,
        } => commands::webdav::run(port, read_only, cache_ttl, no_cache, verbose).await,
        Commands::Speedtest => commands::speedtest::run().await,
        Commands::Repair { dry_run } => commands::repair::run(dry_run).await,
        Commands::Billing { action } => match action {
            BillingAction::Show { json } => commands::billing::show(json).await,
        },
        Commands::Account(cmd) => match cmd {
            AccountCmd::Show => commands::account::show().await,
            AccountCmd::Update { email } => commands::account::update_email(email).await,
            AccountCmd::Export(sub) => match sub {
                AccountExportCmd::Start => commands::account::export_start().await,
                AccountExportCmd::Status { job_id } => commands::account::export_status(job_id).await,
                AccountExportCmd::Download { job_id, output } => {
                    commands::account::export_download(job_id, output).await
                }
            },
            AccountCmd::Delete { confirm } => commands::account::delete(confirm).await,
        },
        Commands::Logout => commands::logout::run().await,
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "bb", &mut std::io::stdout());
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
