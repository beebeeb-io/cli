//! The `bb` binary: the clap entry point, the output-mode setup and the dispatch
//! into `commands`.

mod account_state;
mod api;
mod cli_args;
use cli_args::*;
mod cli_help;
use cli_help::*;
mod colors;
mod commands;
mod config;
mod crypto;
mod custody;
pub mod daemon;
pub mod device;
mod direct_download;
mod direct_upload;
mod download;
mod env_detect;
mod exit;
mod item_id;
mod loopback;
mod owned_keys;
mod path;
mod region;
mod resume;
mod safe_path;
mod shared_folder_index;
mod sync_safety;
mod thumbnail;
mod transport;
mod tui;
mod ui;
mod update;
mod upload;
mod upload_lock;
mod web_url;

#[cfg(test)]
mod readme_flags_tests;
#[cfg(test)]
mod test_server;

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
    after_help = format!("{}\n", help_footer_text().custom_color(crate::colors::INK_SAGE)),
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

    /// File-transfer transport: `auto` (default) or `api` to force the API (debugging).
    /// Also settable with BB_TRANSPORT=api.
    #[arg(long, global = true, value_enum, value_name = "MODE", hide = true)]
    transport: Option<transport::Transport>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Update bb to the latest release
    Update,

    /// Authenticate with your Beebeeb account
    Login {
        /// Skip browser auto-open and print only the URL + code
        /// (use this on SSH or boxes without a window server)
        #[arg(long)]
        headless: bool,
    },

    /// Create an account — opens the web signup (the CLI signs in, it doesn't sign up)
    ///
    /// New accounts start with a trial that needs a payment method, which
    /// only the web app can collect. This prints the signup URL, opens it in
    /// your browser when one is available, and exits. Afterwards run
    /// `bb login`.
    Signup,

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

        /// Overwrite the output file if it already exists. Without this, an
        /// existing file is never replaced: bb asks on an interactive terminal
        /// and refuses (non-zero exit) otherwise — pass -o to save elsewhere
        #[arg(short = 'f', long)]
        force: bool,
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

        /// PERMANENTLY delete (irreversible) instead of trashing — requires
        /// a step-up password confirmation
        #[arg(long = "permanent")]
        permanent: bool,

        /// Skip the (soft-trash) confirmation prompt. Does not bypass the
        /// step-up required by --permanent.
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
        /// File path, short ID (e.g. 3e15382b from `bb ls`), or UUID
        file_id: String,

        /// Link expiry in hours (e.g. 24) or duration (e.g. "7d")
        #[arg(long)]
        expires: Option<String>,

        /// Maximum number of times the link can be opened
        #[arg(long)]
        max_opens: Option<u32>,

        /// Require a passphrase to open the link. The server receives it and
        /// checks it (stored as an Argon2id hash); it is an access gate, not
        /// extra encryption.
        #[arg(long)]
        passphrase: bool,

        /// Removed: every share is end-to-end encrypted and the server refuses
        /// anything else. Kept hidden only so old scripts get a clear error.
        #[arg(long = "no-double-encrypt", hide = true)]
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

        /// Let ONE pass remove more than max(50, 10%) local files that the server
        /// reports trashed. Without it the pass removes nothing and exits 3. The
        /// files go to .bb-trash/ inside the sync folder, never a plain delete.
        /// Not available to the background daemon.
        #[arg(long)]
        allow_mass_delete: bool,

        /// Accept the remote folder the path resolves to now as this directory's
        /// sync root, after it changed (the old folder was trashed and re-created,
        /// or the remote path changed). Until then a changed root pauses every
        /// local removal. Drops the old folder's sync record, so nothing is
        /// removed on its account; local files then sync in as new files.
        #[arg(long, conflicts_with = "daemon")]
        rebind: bool,
    },

    /// Mount vault as a FUSE filesystem (source builds with `--features fuse` only;
    /// requires macFUSE on macOS / libfuse3 on Linux). Hidden from `--help` in builds
    /// without FUSE — which includes every release binary — where `bb webdav` is the
    /// way to open the vault as a drive.
    #[cfg_attr(not(feature = "fuse"), command(hide = true))]
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
    #[cfg_attr(not(feature = "fuse"), command(hide = true))]
    Unmount {
        /// Mountpoint to unmount
        mountpoint: PathBuf,
    },

    /// Serve vault as a local WebDAV server (mounts in Finder, rclone, Cyberduck, WinSCP)
    ///
    /// Read-only by default; pass --read-write to allow uploads, deletes and renames. Listens on
    /// 127.0.0.1 only and prints a random per-run password at startup that every client must send
    /// (HTTP Basic, any user name). Requests with a foreign Host or any Origin header are refused.
    ///
    /// Windows Explorer ("Map network drive") is not supported: it refuses HTTP Basic over plain
    /// HTTP by default. On Windows use the Beebeeb desktop app, rclone, Cyberduck or WinSCP.
    Webdav {
        /// TCP port to listen on (default: 7878)
        #[arg(long, default_value_t = 7878)]
        port: u16,

        /// Allow writes (PUT, DELETE, MKCOL, MOVE). Without this flag the gateway is read-only
        #[arg(long, default_value_t = false)]
        read_write: bool,

        /// Accepted for compatibility: read-only is now the default
        #[arg(long, default_value_t = false, hide = true, conflicts_with = "read_write")]
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

    /// View and manage billing
    Billing {
        #[command(subcommand)]
        action: BillingAction,
    },

    /// End current session
    Logout,

    /// Manage your Beebeeb account — profile, plan, security
    #[command(subcommand)]
    Account(AccountCmd),

    /// Manage two-factor authentication (TOTP)
    #[command(name = "2fa", subcommand)]
    Twofa(TwofaCmd),

    /// Manage active sessions across your devices
    #[command(subcommand)]
    Sessions(SessionsCmd),

    /// Where new uploads are stored: Europe (default, we place your files) or one country
    ///
    /// With no subcommand, lists the options (fetched from the API, so new
    /// countries appear without a new `bb`). A country that is not live yet is
    /// shown as coming soon and cannot be chosen.
    Region {
        #[command(subcommand)]
        action: Option<RegionCmd>,
    },

    /// Manage passkeys (WebAuthn credentials)
    #[command(subcommand)]
    Passkey(PasskeyCmd),

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

    let output_mode = if cli.json {
        ui::OutputMode::Json
    } else if cli.quiet {
        ui::OutputMode::Quiet
    } else {
        ui::OutputMode::Rich
    };
    let color = if cli.no_color {
        ui::ColorChoice::Never
    } else {
        ui::ColorChoice::Auto
    };
    ui::init(output_mode, color);
    transport::init(cli.transport);

    // Explicit updates bypass automatic opt-out/cooldown and report errors.
    if !matches!(cli.command, Commands::Update) {
        update::check_and_update().await;
    }

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
        Commands::Update => update::run_manual().await,
        Commands::Login { headless } => {
            daemon::migrate_sync_services();
            commands::login::run(headless).await
        }
        Commands::Signup => commands::signup::run().await,
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
        } => {
            let strategy = if replace {
                commands::push::ConflictStrategy::Replace
            } else if keep_both {
                commands::push::ConflictStrategy::KeepBoth
            } else {
                commands::push::ConflictStrategy::Prompt
            };
            commands::push::run(path, parent, folder, strategy).await
        }
        Commands::Pull {
            file_id,
            output,
            output_flag,
            zip,
            force,
        } => {
            commands::pull::run(
                file_id,
                output.or(output_flag),
                commands::pull::PullOptions { zip, force },
            )
            .await
        }
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
            permanent,
            yes,
        } => {
            commands::rm::run(
                targets,
                commands::rm::RmOptions {
                    recursive,
                    permanent,
                    yes,
                },
            )
            .await
        }
        Commands::Restore { target } => commands::restore::run(target).await,
        Commands::Trash { cmd } => match cmd {
            TrashCmd::List => commands::trash::list().await,
            TrashCmd::Empty => commands::trash::empty().await,
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
        } => {
            commands::share::run(
                file_id,
                expires,
                max_opens,
                commands::share::ShareOptions {
                    passphrase,
                    no_double_encrypt,
                },
            )
            .await
        }
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
                commands::sync::SyncCommand::Run,
                commands::sync::SyncOptions::watch(),
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
            allow_mass_delete,
            rebind,
        } => {
            // Clamp parallel uploads to a sane window: 0 would stall the pipeline,
            // and unbounded values blow up peak memory (each in-flight file holds
            // ~chunk_size worth of buffers). (1, 8) is the supported range.
            let concurrency = concurrency.clamp(1, 8);
            daemon::migrate_sync_services();
            let command = if let Some(name) = stop_session {
                commands::sync::SyncCommand::StopSession(name)
            } else if stop_all {
                commands::sync::SyncCommand::StopAll
            } else if status {
                commands::sync::SyncCommand::Status
            } else if stop {
                commands::sync::SyncCommand::StopLegacy
            } else if daemon {
                commands::sync::SyncCommand::Daemon
            } else {
                commands::sync::SyncCommand::Run
            };
            let options = commands::sync::SyncOptions {
                dry_run,
                force,
                delete_remote: delete,
                once,
                rehash,
                allow_mass_delete,
                rebind,
                concurrency,
            };
            commands::sync::run(local_dir, remote_path, command, options)
                .await
                .map_err(exit::sync_error)
        }
        Commands::Mount {
            mountpoint,
            foreground,
            cache_ttl,
        } => commands::mount::run(mountpoint, foreground, cache_ttl).await,
        Commands::Unmount { mountpoint } => commands::mount::unmount(mountpoint).await,
        Commands::Webdav {
            port,
            read_write,
            read_only: _,
            cache_ttl,
            no_cache,
            verbose,
        } => {
            commands::webdav::run(
                port,
                cache_ttl,
                commands::webdav::WebdavOptions {
                    read_write,
                    no_cache,
                    verbose,
                },
            )
            .await
        }
        Commands::Speedtest => commands::speedtest::run().await,
        Commands::Repair { dry_run } => commands::repair::run(dry_run).await,
        Commands::Billing { action } => match action {
            BillingAction::Show { json } => commands::billing::show(json).await,
            BillingAction::Usage => commands::billing::usage().await,
            BillingAction::Invoices { open } => commands::billing::invoices(open).await,
            BillingAction::Portal => commands::billing::portal().await,
            BillingAction::Addons { action } => match action {
                Some(AddonsAction::Purchase { addon_id }) => commands::billing::purchase_addon(addon_id).await,
                None => commands::billing::addons().await,
            },
        },
        Commands::Account(cmd) => match cmd {
            AccountCmd::Show => commands::account::show().await,
            AccountCmd::Billing => commands::billing::portal().await,
            AccountCmd::Addons { action } => match action {
                Some(AddonsAction::Purchase { addon_id }) => commands::billing::purchase_addon(addon_id).await,
                None => commands::billing::addons().await,
            },
            AccountCmd::Update { email } => commands::account::update_email(email).await,
        },
        Commands::Twofa(cmd) => match cmd {
            TwofaCmd::Status => commands::twofa::status().await,
            TwofaCmd::Setup => commands::twofa::setup().await,
            TwofaCmd::Enable { code } => commands::twofa::enable(code).await,
            TwofaCmd::Disable { code } => commands::twofa::disable(code).await,
        },
        Commands::Sessions(cmd) => match cmd {
            SessionsCmd::List => commands::sessions::list().await,
            SessionsCmd::Revoke { id } => commands::sessions::revoke(id).await,
            SessionsCmd::RevokeAllOthers { yes } => commands::sessions::revoke_all_others(yes).await,
        },
        Commands::Region { action } => match action {
            None | Some(RegionCmd::List) => commands::region::list().await,
            Some(RegionCmd::Get) => commands::region::get().await,
            Some(RegionCmd::Set { target }) => commands::region::set(target).await,
        },
        Commands::Passkey(cmd) => match cmd {
            PasskeyCmd::List => commands::passkey::list().await,
            PasskeyCmd::Add { print_url } => commands::passkey::add(print_url).await,
            PasskeyCmd::Remove { id, yes } => commands::passkey::remove(id, yes).await,
        },
        Commands::Logout => commands::logout::run().await,
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "bb", &mut std::io::stdout());
            Ok(())
        }
    };

    if let Err(e) = result {
        let line = format!(
            "  {} {}",
            "error:".custom_color(crate::colors::RED_ERR),
            e.custom_color(crate::colors::INK),
        );
        let code = exit::code();
        exit::report_then_disarm(
            &mut std::io::stderr(),
            &mut std::io::stdout(),
            &line,
            code,
            daemon::disarm_launchagent_if_managed,
        );
        std::process::exit(code);
    }
}
