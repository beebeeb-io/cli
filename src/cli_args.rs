//! The nested clap subcommand enums (`bb trash …`, `bb request …`,
//! `bb account …`, `bb 2fa …`, `bb sessions …`, `bb region …`, `bb passkey …`,
//! `bb billing …`), split out of the crate root so `main.rs` keeps only the
//! top-level `Cli`/`Commands` surface and the dispatch.
//!
//! Owns: the argument shapes of every nested command group.
//! Must not: dispatch anything (that is `commands::`), or define the top-level
//! commands (they stay in `main.rs`).

use super::*;

#[derive(Subcommand)]
pub(super) enum BackupCmd {
    /// Back up a folder of this machine (read in place, never changed)
    Add {
        /// The folder to back up
        folder: PathBuf,
    },

    /// Stop backing up a folder (what is backed up stays in Backups)
    Remove {
        /// The folder to stop backing up
        folder: PathBuf,
    },

    /// List the folders this machine backs up
    List,

    /// Back up every folder once: one pass, for a systemd timer or cron
    Run,

    /// Show each folder's state and the counts of its last run
    Status,
}

#[derive(Subcommand)]
pub(super) enum TrashCmd {
    /// List trashed files and folders
    List,

    /// Permanently delete everything in the trash (irreversible; requires
    /// a step-up password confirmation)
    Empty,
}

#[derive(Subcommand)]
pub(super) enum RequestCmd {
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
pub(super) enum AccountCmd {
    /// Show profile, plan, security, sessions, passkeys
    Show,
    /// Open the Beebeeb web billing page
    Billing,
    /// View or purchase billing add-ons
    Addons {
        #[command(subcommand)]
        action: Option<AddonsAction>,
    },
    /// Change email address (verification sent to the new inbox)
    Update {
        /// New email address
        #[arg(long)]
        email: String,
    },
}

#[derive(Subcommand)]
pub(super) enum TwofaCmd {
    /// Show 2FA status (enabled/disabled)
    Status,
    /// Begin 2FA setup — returns secret, QR, and backup codes
    Setup,
    /// Confirm a TOTP code to enable 2FA after setup
    Enable {
        /// 6-digit TOTP code from your authenticator
        #[arg(long)]
        code: String,
    },
    /// Disable 2FA — requires a valid TOTP code
    Disable {
        #[arg(long)]
        code: String,
    },
    // `Verify` (exchange a login-time partial token + code) was REMOVED
    // (eng-0479): the live POST /api/v1/auth/2fa/verify is the LOGIN-time
    // exchange, but `bb login`'s browser handshake never produces or needs a
    // partial token — see the module doc in `commands/twofa.rs` for the full
    // grep evidence. A visible command with no live semantics is a dead end.
}

#[derive(Subcommand)]
pub(super) enum SessionsCmd {
    /// List all active sessions (current one marked with `*`)
    List,
    /// Revoke one session by id (or a unique id prefix from `bb sessions
    /// list`). Refuses on the current session — run `bb logout` instead
    /// (see commands/sessions.rs module doc for why).
    Revoke {
        /// Full session id or a unique prefix
        id: String,
    },
    /// Revoke every session except the one you're running this from
    RevokeAllOthers {
        /// Skip the confirmation prompt
        #[arg(short = 'f', long = "yes", visible_alias = "force")]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub(super) enum RegionCmd {
    /// List the storage locations by country, with your current choice marked (default)
    List,
    /// Show your current choice
    Get,
    /// Choose where new uploads are stored, by country
    Set {
        /// `europe` (default: any of our European data centers, we place your
        /// files), or a country: its id, code or name from `bb region list`
        /// (for example `de` or `Germany`; case does not matter)
        target: String,
    },
}

#[derive(Subcommand)]
pub(super) enum PasskeyCmd {
    /// List your registered passkeys
    List,
    /// Register a new passkey (opens the web app — WebAuthn only runs in a browser)
    Add {
        /// Print the enrollment URL instead of opening a browser (for SSH / headless use)
        #[arg(long)]
        print_url: bool,
    },
    /// Remove a passkey by id (or a unique id prefix from `bb passkey list`)
    Remove {
        /// Full passkey id or a unique prefix
        id: String,
        /// Skip the confirmation prompt
        #[arg(short = 'f', long = "yes", visible_alias = "force")]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub(super) enum BillingAction {
    /// Show plan, storage usage, renewal date, and pending changes
    Show {
        /// Output the raw API merge as JSON (use with jq)
        #[arg(long)]
        json: bool,
    },
    /// Storage usage with a per-region breakdown (approximate — top-level files only)
    Usage,
    /// List VAT-compliant invoices (number, date, amount, status, period)
    Invoices {
        /// Download and open one invoice's PDF (full invoice id or a unique prefix)
        #[arg(long, value_name = "ID")]
        open: Option<String>,
    },
    /// Open the Beebeeb web billing page
    Portal,
    /// View or purchase billing add-ons
    Addons {
        #[command(subcommand)]
        action: Option<AddonsAction>,
    },
}

#[derive(Subcommand)]
pub(super) enum AddonsAction {
    /// Purchase one unit of an add-on
    Purchase {
        /// Server add-on field to increment: extra_storage_tb or extra_users
        addon_id: String,
    },
}
