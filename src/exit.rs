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
//! A command opts in by building its error with [`with_code`]; `main` reads
//! [`code`] when it exits.

use std::sync::atomic::{AtomicI32, Ordering};

/// Non-interactive run that needed an answer to a prompt.
pub const USAGE: i32 = 2;
/// The run finished but left items failed or unresolved.
pub const INCOMPLETE: i32 = 3;

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
