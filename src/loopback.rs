//! Loopback-suppression registry for `bb watch`.
//!
//! When `bb watch` uploads a local file, the server echoes a `file.uploaded`
//! event back through the SSE stream. Without coordination, the remote-event
//! handler would then download the file we just sent — clobbering the
//! original or duplicating it.
//!
//! This module is a process-global, time-bounded set of recently-uploaded
//! file IDs. The push handler calls [`mark_uploaded`] after every successful
//! upload; the SSE handler calls [`was_recently_uploaded`] before downloading.
//! Entries expire after [`SUPPRESS_TTL`] so a real remote update for the same
//! file from another device is still picked up after the window closes.
//!
//! All functions are no-ops when not inside a `bb watch` session — the
//! registry stays empty and lookups always return `false`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long a file ID stays "recently uploaded" after a local push.
/// 60s comfortably covers SSE delivery latency + retries; an actual
/// remote update from another device will arrive after this window.
pub const SUPPRESS_TTL: Duration = Duration::from_secs(60);

static REGISTRY: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, Instant>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record that we just uploaded `file_id` from this process. Future remote
/// events for the same ID arriving within [`SUPPRESS_TTL`] will be ignored
/// by the watch downloader.
pub fn mark_uploaded(file_id: &str) {
    let mut guard = match registry().lock() {
        Ok(g) => g,
        Err(_) => return, // poisoned lock — silently skip rather than panic
    };
    guard.insert(file_id.to_string(), Instant::now());

    // Opportunistic GC: drop expired entries while we hold the lock.
    let now = Instant::now();
    guard.retain(|_, t| now.duration_since(*t) < SUPPRESS_TTL);
}

/// Returns `true` if `file_id` was uploaded by this process within the
/// suppression window. Also lazily evicts expired entries.
pub fn was_recently_uploaded(file_id: &str) -> bool {
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
