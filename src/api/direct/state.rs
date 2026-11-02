//! The cached verdicts of direct transfer: what the capability answer offered,
//! whether a storage link may be followed, and how long a refusal is held.
//!
//! Owns: `GrantReply`, `DirectOffer`, `Refusal`, `DirectState`,
//! `DiscoveryState`, `Said`, `login_id`, `REFUSAL_HOLD`.
//! Must not: make requests (see `direct::client`).

use super::*;

/// The answer to a grant request.
pub(crate) enum GrantReply {
    Window(dt::DownloadGrantResponse),
    /// A 2xx whose body is not the contract's JSON.
    Malformed,
    /// Anything but a window: the classifier's verdict on the answer.
    Failed(dt::Outcome),
}

/// What the discovery route said about a direction, as far as a resume has to know it.
#[derive(Debug, Clone)]
pub(crate) enum DirectOffer {
    /// The server offers it (the cached or fresh discovery lists the capability).
    Offered(dt::Discovery),
    /// A DEFINITE no: the user forced the API, the discovery lists no such capability, or the server said, in
    /// JSON, that it has no direct transport.
    NotOffered,
    /// Nobody answered: a failed or timed-out lookup, a node without the route. Say nothing about the server; a
    /// new file goes through the API, but a recorded direct session must NOT be thrown away on this.
    Unknown,
    /// The lookup was refused for want of a valid login (see [`Refusal`]). The server was not asked about its
    /// transports, and `--transport api` is not the way out (it would end the recorded session and with it the
    /// chunks it already holds). A new file goes through the API, whose own answer reports the same problem in its
    /// own words.
    Unauthorized(Refusal),
}

/// Why a lookup was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No session at all.
    NoSession,
    /// The capabilities route answered 401 or 403; `code` is the server's error code when the body had one
    /// (`account_suspended` is a 403, an expired session a 401).
    Refused { status: u16, code: Option<String> },
}

impl Refusal {
    /// What a resumed direct upload of `file` says when it cannot be continued for this reason: each case in the words
    /// the rest of the CLI uses for it, and none of them advises the way out of a different problem.
    pub(crate) fn resume_message(&self, file: &str) -> String {
        match self {
            Refusal::NoSession => super::NOT_LOGGED_IN_MESSAGE.to_string(),
            Refusal::Refused { status: 401, .. } => super::SESSION_EXPIRED_MESSAGE.to_string(),
            Refusal::Refused { status, code } => {
                let what = match code {
                    Some(code) => format!("{code}, HTTP {status}"),
                    None => format!("HTTP {status}"),
                };
                format!(
                    "{file}: the interrupted direct upload cannot be continued: the server refused to list its \
                     transfer capabilities ({what}). Its progress is kept"
                )
            }
        }
    }
}

impl DirectOffer {
    /// The discovery when the direction is offered.
    pub(crate) fn discovery(self) -> Option<dt::Discovery> {
        match self {
            DirectOffer::Offered(d) => Some(d),
            DirectOffer::NotOffered | DirectOffer::Unknown | DirectOffer::Unauthorized(_) => None,
        }
    }
}

/// What the CLI remembers about the server's direct transfer: what discovery said, and when a direct upload last
/// had to be given up. One per API base per PROCESS (see [`DirectState::shared`]), not per [`ApiClient`]: the
/// watch/daemon builds a new client for every resync, so a per-client cache never outlived one resync and every
/// uploading resync asked the capabilities route again.
pub(crate) struct DirectState {
    pub(super) discovery: tokio::sync::Mutex<DiscoveryState>,
    /// When a direct upload last had to be given up, on the MONOTONIC clock (a wall-clock step or an NTP correction
    /// neither ends the hold early nor stretches it; on Linux and macOS that clock also stops while the machine
    /// sleeps, so a laptop keeps what remained of the hold, which only delays its next direct try; other platforms
    /// may differ). No new direct upload is
    /// started for [`failure_hold`](Self::failure_hold) after that: when direct is broken server-wide every file would
    /// otherwise pay an init, its tries, an abandon (which tells the user's other devices the file was deleted)
    /// and a second init. `tokio`'s instant so a paused test clock can move it.
    pub(super) upload_failed_at: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// How long that hold lasts: the discovery trust period, shorter only in a test.
    pub(super) failure_hold: Duration,
    /// How long a refused login is answered from memory ([`REFUSAL_HOLD`]), shorter only in a test.
    pub(super) refusal_hold: Duration,
}

impl DirectState {
    pub(crate) fn new() -> Self {
        Self::with_holds(Duration::from_millis(dt::DISCOVERY_TTL_MS.unsigned_abs()), REFUSAL_HOLD)
    }

    pub(crate) fn with_holds(failure_hold: Duration, refusal_hold: Duration) -> Self {
        Self {
            discovery: tokio::sync::Mutex::new(DiscoveryState::new()),
            upload_failed_at: std::sync::Mutex::new(None),
            failure_hold,
            refusal_hold,
        }
    }

    /// A direct upload had to be given up at `now`.
    pub(super) fn note_upload_failed(&self, now: tokio::time::Instant) {
        *self.upload_failed_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(now);
    }

    /// Is `now` still inside the hold that began when a direct upload was last given up?
    pub(super) fn upload_failed_within_hold(&self, now: tokio::time::Instant) -> bool {
        self.upload_failed_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|at| now.saturating_duration_since(at) < self.failure_hold)
    }

    /// The process's state for this API base (the same `Arc` for the same base, however many clients are built).
    pub(crate) fn shared(api_base: &str) -> std::sync::Arc<Self> {
        static STATES: std::sync::LazyLock<
            std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<DirectState>>>,
        > = std::sync::LazyLock::new(Default::default);
        let mut states = STATES.lock().unwrap_or_else(|e| e.into_inner());
        std::sync::Arc::clone(
            states
                .entry(api_base.trim_end_matches('/').to_string())
                .or_insert_with(|| std::sync::Arc::new(DirectState::new())),
        )
    }
}

/// The discovery cache. Core's cache does what a long-running process needs (a sync daemon that runs for weeks
/// and meets one bad quarter hour): it forgets a failed discovery AND a JSON 404 "no direct transport" once the
/// trust period has passed (core 2aa1f39). The CLI keeps what core's `Lookup::UseApi` does not carry, in [`Said`]: why
/// the cache gave up (the server SAID no, or asking failed). A resume needs the difference (see [`DirectOffer`]).
///
/// And it keeps ONE rule of its own on top of core's cache, which is a workaround and goes away with its cause: core
/// counts a 401/403 on the lookup as a failed lookup, so two of them would park discovery (downloads too) for the whole
/// trust period, whoever asks next, and a `bb login` could not undo it. Here a refusal is NOT given to core's cache; it
/// is remembered for the LOGIN that was refused, for [`REFUSAL_HOLD`] (follow-up for core: an authorization answer
/// is no evidence about the server, so the cache should not count it).
pub(crate) struct DiscoveryState {
    pub(super) cache: dt::DiscoveryCache,
    pub(super) said: Said,
}

/// What the last answer to a lookup was, beyond what core's cache keeps of it. One value, so that "the server said
/// no" and "this login was refused" cannot both be true.
#[derive(Debug, Clone)]
pub(super) enum Said {
    /// A good answer, a failure to ask, a node without the route: nothing about the server's mind or the login.
    Nothing,
    /// The server's definite "no direct transport" (a JSON 404).
    NoDirect,
    /// A 401 or 403 for the login `login` (see [`login_id`]), answered at `at` on the monotonic clock.
    Refused {
        login: u64,
        refusal: Refusal,
        at: tokio::time::Instant,
    },
}

/// Which login a request carries, comparable inside this process and never written anywhere: a hash of the token.
pub(super) fn login_id(token: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

impl DiscoveryState {
    pub(crate) fn new() -> Self {
        Self {
            cache: dt::DiscoveryCache::new(),
            said: Said::Nothing,
        }
    }

    /// The last answer was the server's definite "no direct transport".
    pub(super) fn said_no(&self) -> bool {
        matches!(self.said, Said::NoDirect)
    }

    /// `login` was refused less than `hold` before `now`: the refusal, to be answered without asking again. Another
    /// login, or an old refusal, is none.
    pub(super) fn refusal_for(&self, login: u64, now: tokio::time::Instant, hold: Duration) -> Option<Refusal> {
        match &self.said {
            Said::Refused {
                login: refused,
                refusal,
                at,
            } if *refused == login && now.saturating_duration_since(*at) < hold => Some(refusal.clone()),
            _ => None,
        }
    }

    /// The lookup was refused for `login` at `at`. Core's cache is not told: it would count a failed lookup.
    pub(super) fn record_refusal(&mut self, login: u64, refusal: Refusal, at: tokio::time::Instant) {
        self.said = Said::Refused { login, refusal, at };
    }

    pub(crate) fn lookup(&self, now_ms: i64) -> dt::Lookup {
        self.cache.lookup(now_ms)
    }

    pub(crate) fn record(&mut self, outcome: &dt::Outcome, discovery: Option<dt::Discovery>, now_ms: i64) {
        self.said = if matches!(outcome, dt::Outcome::NotSupported) {
            Said::NoDirect
        } else {
            Said::Nothing
        };
        self.cache.record(outcome, discovery, now_ms);
    }
}
