//! The pinned per-chunk API route's backoff: how long a refusal says to wait,
//! what a failure means, and when the loop gives up.
//!
//! Owns: `ChunkApiError`, `PinnedRetry`/`PinnedFailure`, `next_wait`,
//! `stated_wait` and `PINNED_RETRY`.
//! Must not: perform requests (see `direct::client`).

use super::*;

/// Why the pinned per-chunk API route did not give a chunk.
pub(crate) enum ChunkApiError {
    /// 409 `version_unavailable`: the pinned version is gone.
    Restart,
    /// 401/403 or a real not-found: do not retry.
    Terminal(String),
    /// Anything else (network, 5xx, wrong length).
    Failed(String),
}

/// How hard the pinned per-chunk route is tried before the chunk, and with it the file, goes to the
/// API stream. Grants and this route share the per-user rate limiter server-side, and a download that
/// has fallen back to it fetches four chunks at a time, so one 429 is likely and must not cost the
/// whole transfer.
#[derive(Clone, Copy)]
pub(super) struct PinnedRetry {
    /// Attempts in all, the first one included.
    pub(super) attempts: u32,
    /// The most the retries may wait in total; a wait that would pass it ends them. It must hold the
    /// longest wait a server can state (`MAX_RETRY_AFTER`, 120 s) plus a margin: the server's real 429
    /// on these routes says `Retry-After: 60`, and a budget of 60 s would give up on the first stated
    /// wait that follows any earlier one, or on a second 429, restarting the whole file.
    pub(super) budget: Duration,
    /// The wait before the second attempt when the server named none; it doubles each time.
    pub(super) base: Duration,
    /// An attempt that hears nothing for this long (no answer, or no body byte) has failed. Without
    /// it a stalled connection would hold an attempt for the request's 600 s ceiling.
    pub(super) idle: Duration,
}

pub(super) const PINNED_RETRY: PinnedRetry = PinnedRetry {
    attempts: 4,
    budget: Duration::from_secs(150),
    base: Duration::from_secs(1),
    idle: Duration::from_secs(60),
};

/// Whether to wait `wait` and try again after attempt number `attempt` (1-based) failed, with `waited`
/// already spent waiting: `None` when the attempts or the time budget are used up.
pub(super) fn next_wait(retry: PinnedRetry, attempt: u32, waited: Duration, wait: Duration) -> Option<Duration> {
    if attempt >= retry.attempts || waited.saturating_add(wait) > retry.budget {
        None
    } else {
        Some(wait)
    }
}

/// One attempt of the pinned route that did not give the chunk.
pub(super) enum PinnedFailure {
    /// Not worth another try (an authorization answer, a gone version, a refusal that will repeat).
    Final(ChunkApiError),
    /// Could be gone in a moment: a 429, a 5xx, a held file, a network blip, a bad read.
    /// `wait` is what the server asked for, if it did.
    Again { wait: Option<Duration>, message: String },
}

/// The wait a server names with `Retry-After: <seconds>`, clamped exactly like a 429's: core's
/// classifier decides the bounds, asked about a 429 that carries this header.
pub(super) fn stated_wait(secs: Option<u64>) -> Option<Duration> {
    let raw = secs?.to_string();
    match dt::classify(
        dt::Call::DownloadGrant,
        &dt::ApiAnswer::from_parts(429, None, b"", Some(&raw)),
    ) {
        dt::Outcome::Backoff(wait) => Some(wait),
        _ => None,
    }
}
