//! One pause for every request to an API (spec §6.1): a 429 on any route, or a 503 on a chunk route, closes
//! the gate until its `Retry-After`; the pacer waits on it before every paced request, and the 429 branch waits
//! on it before it hands back its sentinel. So the workers of a push stop together and come back together, a
//! pause costs at most the requests already in flight, and one notice is printed per pause, not per answer.
//!
//! How long a 429 closes the gate is the client pause rule ([`rate_limited`]): a floor that doubles with each
//! pause in a row, or the server's hint when that is longer, plus 0-25 % jitter. A refused chunk of the direct
//! route closes it by the same rule ([`chunk_refused`]). When the gate opens, its waiters are let go one by
//! one over a second ([`wait_open`], [`release_offset_ms`]), so the workers of a push do not all send in the
//! same instant.
//!
//! The gate is kept per API origin (`scheme://host:port`): a process talks to one API, so in a run it is the
//! process's one pause; tests, each with a mock server of its own, never share one.
//!
//! Owns: the gate, its pause count, its 429 streak and the wait rule, the notices, the interrupt that ends a
//! wait. Must not: retry a request (the callers do, with their own attempt bounds).
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
    /// The 429 pauses in a row: the rung of the floor ladder the next one takes. Back to 0 on the first
    /// answer that is not a 429 ([`answered`]).
    streak: u32,
    /// How many callers wait on the running pause: each takes the next place in its release line. Back to 0
    /// when a new pause begins.
    waiting: u32,
    interrupt: Option<Interrupt>,
}

/// The shortest pause a 429 gives; it doubles with each 429 pause in a row.
const RATE_LIMIT_WAIT_FLOOR_MS: u64 = 2_000;
/// Where the doubling floor stops.
const RATE_LIMIT_WAIT_CAP_MS: u64 = 60_000;
/// The longest wait a server may state (core's `MAX_RETRY_AFTER`).
const MAX_RETRY_AFTER_SECONDS: u64 = 120;
/// The jitter added on top of a wait, at most.
const WAIT_JITTER_PERCENT: u64 = 25;
/// The window after a pause's end over which its waiters are let go.
const RELEASE_STAGGER_MS: u64 = 1_000;

/// How long past the pause's end the waiter at `place` (0-based) of `waiters` is held, in milliseconds: its
/// place times an even step (`1000 / waiters`), plus up to a quarter step of jitter (`jitter_permille`,
/// 0..=1000). So 4 waiters go at about 0, 250, 500 and 750 ms, first to last at least 500 ms apart, and one
/// waiter alone is held 0-250 ms. Nobody is held past the window.
fn release_offset_ms(place: u32, waiters: u32, jitter_permille: u32) -> u64 {
    let waiters = u64::from(waiters.max(1));
    let step = RELEASE_STAGGER_MS / waiters;
    let place = u64::from(place).min(waiters - 1);
    place * step + step / 4 * u64::from(jitter_permille.min(1_000)) / 1_000
}

/// How long a 429 pauses the client, in milliseconds. This is `beebeeb_core::retry::rate_limit_wait_ms` with
/// the same numbers, kept here until the CLI's core pin has it; call core's when the pin moves.
///
/// `streak` is 0 for the first 429 of a run of refusals. The base is the larger of the floor ladder (2, 4, 8,
/// 16, 32, 60 s) and the server's hint (at most 120 s); `jitter_permille` (0..=1000) adds 0-25 % of it.
fn rate_limit_wait_ms(streak: u32, retry_after_seconds: Option<u64>, jitter_permille: u32) -> u64 {
    let ladder = RATE_LIMIT_WAIT_FLOOR_MS
        .checked_shl(streak)
        .filter(|ms| ms >> streak == RATE_LIMIT_WAIT_FLOOR_MS)
        .unwrap_or(RATE_LIMIT_WAIT_CAP_MS)
        .min(RATE_LIMIT_WAIT_CAP_MS);
    let hint = retry_after_seconds.unwrap_or(0).min(MAX_RETRY_AFTER_SECONDS) * 1_000;
    let base = ladder.max(hint);
    base + base * WAIT_JITTER_PERCENT * u64::from(jitter_permille.min(1_000)) / 100_000
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
    let extension = with_gate(origin, |gate| close(gate, now, by));
    announce(extension, by, why);
    extension
}

/// A 429 from `origin`: close its gate by the client pause rule ([`rate_limit_wait_ms`]). `retry_after` is the
/// server's hint in seconds (the larger of the `Retry-After` header and the body's `retry_after`), `remaining`
/// its `X-RateLimit-Remaining` for the notice.
///
/// The gate keeps the streak: each 429 that begins a pause takes the next rung of the floor ladder. A 429 that
/// arrives while a pause runs is an answer to a request sent before it, so it is measured on the running
/// pause's rung and only moves the end by [`extend`]'s rule: it never begins or doubles a pause.
pub(crate) fn rate_limited(origin: &str, retry_after: Option<u64>, remaining: Option<u64>) -> Extension {
    use rand::Rng;
    let jitter_permille = rand::thread_rng().gen_range(0..=1_000u32);
    let now = Instant::now();
    let (extension, by) = with_gate(origin, |gate| {
        let running = gate.until.is_some_and(|until| until > now);
        let rung = if running {
            gate.streak.saturating_sub(1)
        } else {
            gate.streak
        };
        let by = Duration::from_millis(rate_limit_wait_ms(rung, retry_after, jitter_permille));
        let extension = close(gate, now, by);
        if extension == Extension::Opened {
            gate.streak = gate.streak.saturating_add(1);
        }
        (extension, by)
    });
    announce(extension, by, PauseReason::RateLimited { remaining });
    extension
}

/// An answer from `origin` that is not a 429: the streak of 429 pauses is over. An answer that lands while a
/// pause runs belongs to a request sent before the pause, so it says nothing about the limit and is ignored.
pub(crate) fn answered(origin: &str) {
    let now = Instant::now();
    with_gate(origin, |gate| {
        if gate.streak > 0 && gate.until.is_none_or(|until| until <= now) {
            gate.streak = 0;
        }
    });
}

/// How long `origin`'s gate stays closed from now (zero when it is open).
pub(crate) fn closed_for(origin: &str) -> Duration {
    let now = Instant::now();
    with_gate(origin, |gate| {
        gate.until
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(now))
    })
}

fn close(gate: &mut Gate, now: Instant, by: Duration) -> Extension {
    let end = now + by;
    match gate.until {
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
            gate.waiting = 0;
            Extension::Opened
        }
    }
}

fn announce(extension: Extension, by: Duration, why: PauseReason) {
    if extension == Extension::Opened && !crate::ui::is_quiet() {
        crate::transfer_view::notice::stderr(&notice(by, why));
    }
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
///
/// A caller that had to wait takes a place in the pause's release line and is let go at the pause's end plus
/// its place's share of a second ([`release_offset_ms`]), so the waiters of one pause come back spread out,
/// not in one instant. A caller that finds the gate open is not held.
pub(crate) async fn wait_open(origin: &str) -> bool {
    use rand::Rng;
    let jitter_permille = rand::thread_rng().gen_range(0..=1_000u32);
    // This caller's place in a pause's release line: (the pause, by its count; the place).
    let mut place: Option<(u32, u32)> = None;
    loop {
        let now = Instant::now();
        let (wake, interrupt) = with_gate(origin, |gate| {
            let wake = match gate.until {
                // Closed: take a place in this pause's line (a new pause is a new line), sleep to its end,
                // then read the gate again: an extension may have moved the end, and only at the end is the
                // number of waiters known.
                Some(until) if until > now => {
                    if place.map(|(pause, _)| pause) != Some(gate.pauses) {
                        place = Some((gate.pauses, gate.waiting));
                        gate.waiting += 1;
                    }
                    Some(until)
                }
                // The pause this caller waited on is over: its place in the line says when it goes.
                Some(until) => place
                    .filter(|(pause, _)| *pause == gate.pauses)
                    .map(|(_, place)| {
                        until + Duration::from_millis(release_offset_ms(place, gate.waiting, jitter_permille))
                    })
                    .filter(|release| *release > now),
                None => None,
            };
            (wake, gate.interrupt.clone())
        });
        let Some(wake) = wake else {
            return true;
        };
        match interrupt {
            Some(interrupt) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(wake) => {}
                    _ = interrupt.wait() => return false,
                }
            }
            None => tokio::time::sleep_until(wake).await,
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

/// A failed call of a direct chunk upload (grant, storage PUT, commit) that will be tried again, by its HTTP
/// `status` and the wait `after` core's policy ordered for it (spec §6.1). A 429 closes `origin`'s gate by the
/// client pause rule ([`rate_limited`]), with `after` as the server's hint: core read it from the
/// `Retry-After` header or the body's `retry_after`. A 503 closes it for `after`. `None`: any other failure
/// pauses nothing and is the chunk's own to wait out.
pub(crate) fn chunk_refused(origin: &str, status: u16, after: Duration) -> Option<Extension> {
    Some(match reason_for(status)? {
        PauseReason::RateLimited { remaining } => {
            rate_limited(origin, Some(after.as_millis().div_ceil(1_000) as u64), remaining)
        }
        why @ PauseReason::Busy503 => extend(origin, after, why),
    })
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
