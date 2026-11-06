//! The rate window (spec §4.5): the newest confirmed transfers, each with the
//! time its chunk started and the time it was confirmed.

use std::collections::VecDeque;

use super::{WINDOW_MAX_ENTRIES, WINDOW_SPAN_MS};

#[derive(Debug, Clone, Copy)]
pub(super) struct Entry {
    pub(super) start_ms: u64,
    pub(super) end_ms: u64,
    pub(super) bytes: u64,
}

impl Entry {
    pub(super) fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// What the window holds right now: the bytes of the newest entries that span
/// at least [`WINDOW_SPAN_MS`] (or all of them), and where that span starts and ends.
#[derive(Debug, Clone, Copy)]
pub(super) struct Taken {
    pub(super) bytes: u64,
    pub(super) oldest_start_ms: u64,
    pub(super) newest: Entry,
}

impl Taken {
    /// Newest end minus oldest start; 0 gives no rate.
    pub(super) fn span_ms(&self) -> u64 {
        self.newest.end_ms.saturating_sub(self.oldest_start_ms)
    }
}

#[derive(Debug, Default)]
pub(super) struct Window {
    entries: VecDeque<Entry>,
}

impl Window {
    pub(super) fn push(&mut self, entry: Entry) {
        if self.entries.len() == WINDOW_MAX_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Newest first, until the span reaches [`WINDOW_SPAN_MS`] or the entries run out.
    pub(super) fn take(&self) -> Option<Taken> {
        let newest = *self.entries.back()?;
        let (mut bytes, mut oldest_start_ms) = (0u64, u64::MAX);
        for e in self.entries.iter().rev() {
            bytes += e.bytes;
            oldest_start_ms = oldest_start_ms.min(e.start_ms);
            if newest.end_ms.saturating_sub(oldest_start_ms) >= WINDOW_SPAN_MS {
                break;
            }
        }
        Some(Taken {
            bytes,
            oldest_start_ms,
            newest,
        })
    }
}
