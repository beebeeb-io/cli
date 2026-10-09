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
/// Same, when sync ran as a background service (it was stopped, not just failed).
pub const SESSION_ENDED_SERVICE_MESSAGE: &str =
    "Your session has ended. Run `bb login`, then `bb sync --daemon` to restart background sync.";

/// The session-ended line for the current process environment.
pub fn session_ended_message(xpc_service_name: Option<&str>, service_marker: Option<&str>) -> &'static str {
    if crate::daemon::running_as_service(xpc_service_name, service_marker) {
        SESSION_ENDED_SERVICE_MESSAGE
    } else {
        SESSION_ENDED_MESSAGE
    }
}

/// Map a sync error: a dead-session 401 becomes [`SESSION_ENDED`] + one clear line.
pub fn sync_error(msg: String) -> String {
    if msg == crate::api::SESSION_EXPIRED_MESSAGE {
        // The launchd disarm happens later, in `finish`, after this line is out.
        with_code(
            SESSION_ENDED,
            session_ended_message(
                std::env::var("XPC_SERVICE_NAME").ok().as_deref(),
                std::env::var("BEEBEEB_SYNC_SERVICE").ok().as_deref(),
            ),
        )
    } else {
        msg
    }
}

/// Print the error line, flush both streams, and only then run `disarm`
/// (the launchd self-removal, which SIGTERMs us). Order is the contract.
pub fn report_then_disarm(
    err: &mut impl std::io::Write,
    out: &mut impl std::io::Write,
    line: &str,
    code: i32,
    disarm: impl FnOnce(),
) {
    let _ = writeln!(err, "{line}");
    let _ = err.flush();
    let _ = out.flush();
    if code == SESSION_ENDED {
        disarm();
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

#[cfg(test)]
mod message_tests {
    use super::*;

    #[test]
    fn message_variants() {
        assert_eq!(session_ended_message(None, None), SESSION_ENDED_MESSAGE);
        assert!(!SESSION_ENDED_MESSAGE.contains("--daemon"));
        let m = session_ended_message(Some("io.beebeeb.sync.a"), None);
        assert!(m.contains("`bb login`") && m.contains("`bb sync --daemon`"), "{m}");
        assert_eq!(session_ended_message(None, Some("1")), SESSION_ENDED_SERVICE_MESSAGE);
    }

    #[test]
    fn message_is_written_and_flushed_before_disarm() {
        use std::cell::RefCell;
        use std::io::Write;
        thread_local!(static EVENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });
        struct W(&'static str);
        impl Write for W {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                EVENTS.with(|e| {
                    e.borrow_mut()
                        .push(format!("{}:write:{}", self.0, String::from_utf8_lossy(b).trim()))
                });
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                EVENTS.with(|e| e.borrow_mut().push(format!("{}:flush", self.0)));
                Ok(())
            }
        }
        report_then_disarm(&mut W("err"), &mut W("out"), "MSG", SESSION_ENDED, || {
            EVENTS.with(|e| e.borrow_mut().push("disarm".into()))
        });
        let ev = EVENTS.with(|e| e.borrow().clone());
        let d = ev.iter().position(|x| x == "disarm").expect("disarm ran");
        let w = ev.iter().position(|x| x == "err:write:MSG").expect("message written");
        let fe = ev.iter().position(|x| x == "err:flush").expect("stderr flushed");
        let fo = ev.iter().position(|x| x == "out:flush").expect("stdout flushed");
        assert!(w < fe && fe < d && fo < d, "{ev:?}");
    }

    #[test]
    fn other_exit_codes_do_not_disarm() {
        let mut called = false;
        report_then_disarm(&mut Vec::new(), &mut Vec::new(), "x", 1, || called = true);
        assert!(!called);
    }
}
