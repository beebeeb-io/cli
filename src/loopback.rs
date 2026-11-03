//! Loopback-suppression registry — currently only half-wired.
//!
//! When a continuous `bb sync` uploads a local file, the server echoes a
//! `file.uploaded` event back through the SSE stream. Without coordination,
//! the remote-event handler would then download the file we just sent —
//! clobbering the original or duplicating it.
//!
//! This module is a process-global, time-bounded set of recently-uploaded
//! file IDs, intended so the push handler calls [`mark_uploaded`] after every
//! successful upload and the remote-event handler calls
//! [`was_recently_uploaded`] before downloading. Entries expire after
//! [`SUPPRESS_TTL`] so a real remote update for the same file from another
//! device is still picked up after the window closes.
//!
//! **Current state (found 2026-08-13 during a CI/lint cleanup pass):** only
//! the write side is wired up — `push.rs` calls [`mark_uploaded`] — but
//! nothing calls [`was_recently_uploaded`]. It was originally written for
//! the now-removed `bb watch` command's SSE handler; `bb sync`'s continuous
//! watcher (the current equivalent) never adopted the check. This means
//! echo-suppression as described above is NOT actually happening today —
//! `bb sync --continuous` may re-download a file right after uploading it.
//! Left as `#[allow(dead_code)]` rather than deleted, since wiring this into
//! `sync.rs`'s watcher is a real fix worth doing deliberately, not a
//! drive-by lint change.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long a file ID stays "recently uploaded" after a local push.
/// 60s comfortably covers SSE delivery latency + retries; an actual
/// remote update from another device will arrive after this window.
pub const SUPPRESS_TTL: Duration = Duration::from_secs(60);

/// A file id as the loopback registry keys it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LoopbackId(String);

#[allow(dead_code)] // parse/as_str are the checked API; today only the From impls are called
impl LoopbackId {
    /// Parse a file id, refusing an empty or whitespace-only value.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        (!value.is_empty()).then(|| Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LoopbackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for LoopbackId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for LoopbackId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&String> for LoopbackId {
    fn from(value: &String) -> Self {
        Self(value.clone())
    }
}

static REGISTRY: OnceLock<Mutex<HashMap<LoopbackId, Instant>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<LoopbackId, Instant>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record that we just uploaded `file_id` from this process. Future remote
/// events for the same ID arriving within [`SUPPRESS_TTL`] will be ignored
/// by the watch downloader.
pub fn mark_uploaded(file_id: impl Into<LoopbackId>) {
    let mut guard = match registry().lock() {
        Ok(g) => g,
        Err(_) => return, // poisoned lock — silently skip rather than panic
    };
    guard.insert(file_id.into(), Instant::now());

    // Opportunistic GC: drop expired entries while we hold the lock.
    let now = Instant::now();
    guard.retain(|_, t| now.duration_since(*t) < SUPPRESS_TTL);
}

/// Returns `true` if `file_id` was uploaded by this process within the
/// suppression window. Also lazily evicts expired entries.
///
/// Not currently called anywhere — see the module doc for why.
#[allow(dead_code)]
pub fn was_recently_uploaded(file_id: &LoopbackId) -> bool {
    let mut guard = match registry().lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let now = Instant::now();
    match guard.get(file_id).copied() {
        Some(t) if now.duration_since(t) < SUPPRESS_TTL => true,
        Some(_) => {
            // Stale entry — clean up.
            guard.remove(file_id);
            false
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_refuses_an_empty_id() {
        assert!(LoopbackId::parse("").is_none());
        assert!(LoopbackId::parse("   ").is_none());
    }

    #[test]
    fn parse_keeps_a_real_id() {
        assert_eq!(LoopbackId::parse(" file-1 ").unwrap().as_str(), "file-1");
        assert_eq!(LoopbackId::from("file-2").to_string(), "file-2");
    }
}
