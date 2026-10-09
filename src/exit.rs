//! Process exit codes beyond the generic `1`.
//!
//! Every command returns `Result<(), String>` and `main` turns an `Err` into
//! `error: <msg>` + exit 1. A few failures deserve a distinct code so scripts
//! and cron jobs can tell them apart:
//!
//! - [`USAGE`] (2): the command needs input it cannot get non-interactively
//!   (a prompt with stdin not a terminal) — re-run with the named flag.
//! - [`INCOMPLETE`] (3): the command ran, but some items did not make it
//!   (e.g. `bb sync --once` with failed uploads or unresolved conflicts).
//!
//! - [`SESSION_ENDED`] (77, EX_NOPERM): the server no longer accepts the stored
//!   session (`bb sync` only). The sync launchd/systemd units are told NOT to
//!   restart on this code; sign in again with `bb login`.
//!
//! A command opts in by building its error with [`with_code`]; `main` reads
//! [`code`] when it exits.

use std::sync::atomic::{AtomicI32, Ordering};

/// Non-interactive run that needed an answer to a prompt.
pub const USAGE: i32 = 2;
/// The run finished but left items failed or unresolved.
pub const INCOMPLETE: i32 = 3;

/// The stored session was rejected by the server (401). Not retryable.
pub const SESSION_ENDED: i32 = 77;
/// One-line message printed when a sync run finds its session dead.
pub const SESSION_ENDED_MESSAGE: &str = "Your session has ended. Run `bb login` to sign in again.";

/// Map a sync error: a dead-session 401 becomes [`SESSION_ENDED`] + one clear line.
pub fn sync_error(msg: String) -> String {
    if msg == crate::api::SESSION_EXPIRED_MESSAGE {
        crate::daemon::disarm_launchagent_if_managed();
        with_code(SESSION_ENDED, SESSION_ENDED_MESSAGE)
    } else {
        msg
    }
}

static CODE: AtomicI32 = AtomicI32::new(0);

/// Record `code` as the process exit code and return `msg` as the error.
pub fn with_code(code: i32, msg: impl Into<String>) -> String {
    CODE.store(code, Ordering::SeqCst);
    msg.into()
}

/// The exit code for a failed command: the recorded one, else 1.
pub fn code() -> i32 {
    match CODE.load(Ordering::SeqCst) {
        0 => 1,
        c => c,
    }
}

/// True when stdin is an interactive terminal (someone can answer a prompt).
pub fn stdin_is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}
