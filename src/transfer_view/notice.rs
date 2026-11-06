//! One process-wide way to print a line while live transfer lines may be on screen (spec §6.7), for
//! code that holds no sink: the API layer's rate-limit pause, which every request can reach. The sink
//! that draws live lines registers itself here while it draws; a line then goes around its lines
//! (hidden, printed, redrawn), and straight to stderr otherwise.
//!
//! Owns: the one registration slot. Must not: format a line or count anything.
//! Tests: `rate_limit_pause_prints_through_the_live_sink` (`upload::pipeline::live_stats_tests`).

use std::sync::{Mutex, MutexGuard, Weak};

/// What a registered sink does with a line for stderr.
pub(super) trait Notices: Send + Sync {
    fn stderr(&self, line: &str);
}

/// The sink drawing live lines now, if any. Never held while a line is printed.
static LIVE: Mutex<Option<Weak<dyn Notices>>> = Mutex::new(None);

fn slot() -> MutexGuard<'static, Option<Weak<dyn Notices>>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Test-only: the notice slot ([`register`]) is process-global, so a test that asserts what a sink recorded must
/// hold this while it is the registered one, or a sibling test's sink steals or clears the registration.
#[cfg(test)]
pub(crate) static TEST_SINK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Print `line` on stderr, around the live lines when a sink is drawing them.
pub(crate) fn stderr(line: &str) {
    let live = slot().as_ref().and_then(Weak::upgrade);
    match live {
        Some(sink) => sink.stderr(line),
        None => eprintln!("{line}"),
    }
}

/// `sink` draws live lines from now on.
pub(super) fn register(sink: Weak<dyn Notices>) {
    *slot() = Some(sink);
}

/// `sink` stops drawing: the slot is emptied if it is still the one registered.
pub(super) fn unregister(sink: &Weak<dyn Notices>) {
    let mut slot = slot();
    if slot.as_ref().is_some_and(|live| Weak::ptr_eq(live, sink)) {
        *slot = None;
    }
}
