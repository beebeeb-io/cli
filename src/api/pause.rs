//! One pause for every request to an API (spec §6.1): a 429 on any route, or a 503 on a chunk route, closes
//! the gate until its `Retry-After`; the pacer waits on it before every paced request, and the 429 branch waits
//! on it before it hands back its sentinel. So the workers of a push stop together and come back together, a
//! pause costs at most the requests already in flight, and one notice is printed per pause, not per answer.
//!
//! The gate is kept per API origin (`scheme://host:port`): a process talks to one API, so in a run it is the
//! process's one pause; tests, each with a mock server of its own, never share one.
//!
//! Owns: the gate, its pause count, the notices, the interrupt that ends a wait. Must not: retry a request (the
//! callers do, with their own attempt bounds).
//! Tests: `pause/tests.rs`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use crate::interrupt::Interrupt;

/// Why the gate closed: the line printed when a pause begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PauseReason {
    /// A 429; the server's `X-RateLimit-Remaining`, when it sent one.
    RateLimited { remaining: Option<u64> },
    /// A 503 on a chunk route: the storage pool is briefly unavailable.
    Busy503,
}

/// What an [`extend`] did to the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Extension {
    /// The gate was open: a new pause (its notice printed, [`pauses`] counted).
    Opened,
    /// The gate was closed; its end moved later.
    Extended,
    /// The gate was already closed past the new end.
    Within,
}

#[derive(Default)]
struct Gate {
    until: Option<Instant>,
    pauses: u32,
    interrupt: Option<Interrupt>,
}

static GATES: LazyLock<Mutex<HashMap<String, Gate>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Every entry keys by [`origin_of`], so an API base (with or without a trailing slash) and a request URL under
/// it name one gate.
fn with_gate<T>(origin: &str, f: impl FnOnce(&mut Gate) -> T) -> T {
    let mut gates = GATES.lock().unwrap_or_else(|e| e.into_inner());
    f(gates.entry(origin_of(origin)).or_default())
}

/// The gate key of a URL: its origin, so an API base and every request URL under it name one gate.
pub(crate) fn origin_of(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => u.origin().ascii_serialization(),
        Err(_) => url.trim_end_matches('/').to_string(),
    }
}

/// Close `origin`'s gate for `by` from now (or keep it closed until then). Prints the pause's notice when this
/// begins a pause, unless the run is quiet.
pub(crate) fn extend(origin: &str, by: Duration, why: PauseReason) -> Extension {
    let now = Instant::now();
    let end = now + by;
    let extension = with_gate(origin, |gate| match gate.until {
        Some(until) if until > now => {
            if end > until {
                gate.until = Some(end);
                Extension::Extended
            } else {
                Extension::Within
            }
        }
        _ => {
            gate.until = Some(end);
            gate.pauses += 1;
            Extension::Opened
        }
    });
    if extension == Extension::Opened && !crate::ui::is_quiet() {
        crate::transfer_view::notice::stderr(&notice(by, why));
    }
    extension
}

/// The line a pause begins with. The 429 line is today's, byte for byte (its glyph and colour predate the
/// brand rules: spec §6.1); the 503 line has neither.
pub(crate) fn notice(by: Duration, why: PauseReason) -> String {
    match why {
        PauseReason::RateLimited { remaining } => {
            use colored::Colorize;
            format!(
                "  {} rate limited — pausing {}s{}",
                "⏸".custom_color(crate::colors::AMBER),
                by.as_secs(),
                remaining.map_or(String::new(), |r| format!(" ({r} requests remaining)")),
            )
        }
        PauseReason::Busy503 => format!("  server busy (HTTP 503), pausing {}s", by.as_secs()),
    }
}

/// Wait until `origin`'s gate is open. `false` when the interrupt bound to it fired first.
pub(crate) async fn wait_open(origin: &str) -> bool {
    loop {
        let (until, interrupt) = with_gate(origin, |gate| (gate.until, gate.interrupt.clone()));
        let Some(until) = until else {
            return true;
        };
        if until <= Instant::now() {
            return true;
        }
        match interrupt {
            Some(interrupt) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => {}
                    _ = interrupt.wait() => return false,
                }
            }
            // Re-read after the sleep: an extension may have moved the end.
            None => tokio::time::sleep_until(until).await,
        }
    }
}

/// A Ctrl-C ends every wait on `origin`'s gate at once (`bb push` binds its interrupt once, at start).
pub(crate) fn bind_interrupt(origin: &str, interrupt: Interrupt) {
    with_gate(origin, |gate| gate.interrupt = Some(interrupt));
}

/// How many pauses `origin`'s gate had (pauses, not extensions).
pub(crate) fn pauses(origin: &str) -> u32 {
    with_gate(origin, |gate| gate.pauses)
}

/// The reason a chunk-route answer pauses the process (spec §6.1), by its status: a 429 (any route) or a 503
/// (the chunk routes). `None`: the answer is the caller's own to retry or fail.
pub(crate) fn reason_for(status: u16) -> Option<PauseReason> {
    match status {
        429 => Some(PauseReason::RateLimited { remaining: None }),
        503 => Some(PauseReason::Busy503),
        _ => None,
    }
}

#[cfg(test)]
mod tests;

/// The pause through the real requests, against the loopback mock.
#[cfg(test)]
mod requests_tests;
