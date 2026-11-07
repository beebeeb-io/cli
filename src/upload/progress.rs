//! Progress sinks for uploads and downloads: the `ChunkProgress`/`FileProgress`
//! traits, the `InFlight` guard and the no-op sink. What a terminal shows is
//! `transfer_view::LiveProgress` (task 2333), which every transfer command uses.
//!
//! Owns: the sink traits, their byte-only defaults, and the rollback hook that
//! keeps a swept-session retry from double-counting.
//! Must not: know about sessions or HTTP (see `upload::session`), or draw.
//! Tests: `existing_sinks` at the bottom of this file; the rollback race runs
//! against `LiveProgress` (`transfer_view::sink_tests::concurrent_events_commute`).

pub use crate::transfer_stats::{Held, PoolLabel, Route, Tally};

/// Progress sink. One instance per sync/push run; [`begin_file`] is called once
/// per file and returns a per-file handle.
///
/// [`begin_file`]: ChunkProgress::begin_file
pub trait ChunkProgress: Send + Sync {
    /// Start tracking one file. `expected_ciphertext` is the bar length.
    fn begin_file(&self, file_name: &str, expected_ciphertext: u64) -> Box<dyn FileProgress>;
    /// Tear down any shared UI (called once after the upload phase). No-op by
    /// default.
    fn finish_all(&self) {}
    /// Undo ciphertext bytes a FAILED attempt already reported via
    /// [`FileProgress::chunk_confirmed`] on the shared/overall counter (task
    /// 1589, Codex thread #2). A swept-session recovery restarts the same
    /// logical file from chunk 0 under a brand-new [`FileProgress`] handle; if
    /// the failed attempt's bytes are left in place, the retry's own
    /// confirmations on top of them make the run-wide total exceed the
    /// run-wide plan. No-op by default (also correct for [`NoopProgress`]).
    fn rollback(&self, _ciphertext_bytes: u64) {}
    /// Called once every chunk of a file is confirmed, just before its version
    /// is completed. An `Err` stops the upload there instead: its session is
    /// abandoned, so nothing lands, and the error is the caller's (LD-0113: a
    /// backup reads the person's file in place and never completes a copy of a
    /// file that changed while it was read). `Ok` by default.
    fn before_complete(&self, _path: &std::path::Path) -> Result<(), String> {
        Ok(())
    }

    // ── Task 2333 (spec §6.8): every method below has a default, so a sink that
    //    implements only the byte methods above behaves byte for byte as before. ──

    /// [`begin_file`](Self::begin_file), knowing the file's chunk plan (for the
    /// verbose per-chunk lines). Defaults to `begin_file`.
    fn begin_file_chunks(
        &self,
        file_name: &str,
        expected_ciphertext: u64,
        _chunk_count: u32,
        _chunk_size: u64,
    ) -> Box<dyn FileProgress> {
        self.begin_file(file_name, expected_ciphertext)
    }
    /// [`rollback`](Self::rollback) with the chunks too, naming the file whose
    /// failed attempt is taken back. Defaults to `rollback(tally.bytes)`.
    fn rollback_tally(&self, _file_name: &str, tally: Tally) {
        self.rollback(tally.bytes)
    }
    /// The most chunks that may be in flight now (spec §1.3). No-op by default.
    fn set_limit(&self, _max_in_flight: u32) {}
    /// Print a line the run produced (a notice, a per-file line) without tearing
    /// any live lines. Defaults to a plain `println!` / `eprintln!`.
    fn note(&self, line: &str, to: Stream) {
        match to {
            Stream::Stdout => println!("{line}"),
            Stream::Stderr => eprintln!("{line}"),
        }
    }
    /// Run `f` (a prompt) with any live lines hidden. Defaults to calling `f`.
    fn suspend(&self, f: &mut dyn FnMut()) {
        f()
    }
}

/// Where a [`ChunkProgress::note`] goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Per-file progress handle. `Sync` so a `&dyn FileProgress` can be held across
/// `.await` in a `Send` future (the watch loops upload from a spawned task).
pub trait FileProgress: Send + Sync {
    /// One server-confirmed chunk (called only after a 200 from the server, so
    /// progress reflects honest, on-the-wire bytes — never queued bytes).
    fn chunk_confirmed(&self, ciphertext_bytes: u64);
    /// File finished; clears the transient per-file bar and, on success,
    /// advances the overall file counter.
    fn finish(self: Box<Self>, success: bool);

    // ── Task 2333 (spec §6.8): defaults keep every existing sink as it was. ──

    /// Chunk `index` is about to cross the wire by `route` (use [`InFlight`]).
    fn chunk_started(&self, _index: u32, _route: Route) {}
    /// Chunk `index` is being tried again.
    fn chunk_retried(&self, _index: u32) {}
    /// Chunk `index` is held: confirmed by the server (upload) or decrypted
    /// (download), `held` saying whether this run moved it. Defaults to
    /// `chunk_confirmed(bytes)`.
    fn chunk_done(&self, _index: u32, bytes: u64, _held: Held) {
        self.chunk_confirmed(bytes)
    }
    /// A buffered body: all `chunks` arrived and decrypted at once. Defaults to
    /// `chunk_confirmed(bytes)`.
    fn body_done(&self, _chunks: u32, bytes: u64, _route: Route) {
        self.chunk_confirmed(bytes)
    }
    /// Chunk `index` left the wire: done, failed, interrupted or dropped.
    fn chunk_settled(&self, _index: u32) {}
    /// The storage pool this file's bytes go to (spec §4.4).
    fn pool(&self, _label: PoolLabel) {}
}

/// A chunk in flight: [`FileProgress::chunk_started`] when made,
/// [`FileProgress::chunk_settled`] when dropped, whichever way the transfer ends.
pub struct InFlight<'a> {
    prog: &'a dyn FileProgress,
    index: u32,
}

impl<'a> InFlight<'a> {
    pub fn start(prog: &'a dyn FileProgress, index: u32, route: Route) -> Self {
        prog.chunk_started(index, route);
        Self { prog, index }
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.prog.chunk_settled(self.index);
    }
}

/// Progress sink that does nothing — the caller keeps its plain `println!`
/// status lines instead.
pub struct NoopProgress;

impl ChunkProgress for NoopProgress {
    fn begin_file(&self, _: &str, _: u64) -> Box<dyn FileProgress> {
        Box::new(NoopFileProgress)
    }
}

pub(super) struct NoopFileProgress;

impl FileProgress for NoopFileProgress {
    fn chunk_confirmed(&self, _: u64) {}
    fn finish(self: Box<Self>, _: bool) {}
}

/// Truncate a long filename for a progress label, keeping the (informative)
/// tail.
pub(super) fn display_name(name: &str) -> String {
    const MAX: usize = 40;
    if name.chars().count() <= MAX {
        return name.to_string();
    }
    let tail: String = name
        .chars()
        .rev()
        .take(MAX - 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}
/// Task 2333: the new sink methods default to the old byte methods, so a sink
/// that implements only `chunk_confirmed` and `rollback` sees exactly the bytes
/// it saw before (spec §6.8).
#[cfg(test)]
mod existing_sinks {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{ChunkProgress, FileProgress, Held, InFlight, Route, Tally};

    #[derive(Default)]
    struct Bytes {
        confirmed: Arc<AtomicU64>,
        rolled_back: Arc<AtomicU64>,
    }
    struct OneFile(Arc<AtomicU64>);
    impl FileProgress for OneFile {
        fn chunk_confirmed(&self, n: u64) {
            self.0.fetch_add(n, Ordering::SeqCst);
        }
        fn finish(self: Box<Self>, _: bool) {}
    }
    impl ChunkProgress for Bytes {
        fn begin_file(&self, _: &str, _: u64) -> Box<dyn FileProgress> {
            Box::new(OneFile(Arc::clone(&self.confirmed)))
        }
        fn rollback(&self, n: u64) {
            self.rolled_back.fetch_add(n, Ordering::SeqCst);
        }
    }

    #[test]
    fn existing_sinks_keep_their_bytes() {
        let sink = Bytes::default();
        let f = sink.begin_file_chunks("a", 30, 3, 10);
        {
            let _flight = InFlight::start(f.as_ref(), 0, Route::Api);
            f.chunk_retried(0);
            f.chunk_done(0, 10, Held::Transferred);
        }
        f.chunk_done(1, 7, Held::AlreadyStored);
        f.body_done(2, 13, Route::Direct);
        sink.rollback_tally(
            "a",
            Tally {
                bytes: 5,
                chunks: 1,
                already_stored: 0,
            },
        );
        sink.set_limit(4);
        let mut called = false;
        sink.suspend(&mut || called = true);
        assert!(called);
        assert_eq!(sink.confirmed.load(Ordering::SeqCst), 30);
        assert_eq!(sink.rolled_back.load(Ordering::SeqCst), 5);
    }
}

/// Task 2333: a sink that records every driver event as a line (the driver tests).
#[cfg(test)]
pub(crate) mod recording;
