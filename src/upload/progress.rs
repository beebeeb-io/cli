//! Progress sinks for uploads and downloads: the `ChunkProgress`/`FileProgress`
//! traits, the no-op sink used for `--json`/`--quiet`/non-TTY, and the
//! indicatif bars used on a TTY.
//!
//! Owns: the sink traits and their implementations, bar layout, and the
//! rollback bookkeeping that keeps a swept-session retry from double-counting.
//! Must not: know about sessions or HTTP (see `upload::session`).
//! Tests: `bar_progress_rollback_race` at the bottom of this file.

use super::*;
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

/// Amber-accented multi-bar progress: one persistent overall bar plus a pool of
/// transient per-file bars (at most `concurrency` alive at once, since
/// `bb sync` only runs that many uploads concurrently).
///
/// Brand: the bar fill uses xterm colour 214 (the closest 256-colour match to
/// brand amber `#f5b800`); everything else is dim. No emojis.
pub struct BarProgress {
    pub(super) mp: MultiProgress,
    pub(super) overall: ProgressBar,
    pub(super) files_done: Arc<AtomicU64>,
    pub(super) files_total: u64,
}

impl BarProgress {
    /// Build the multi-bar UI. `files_total` and `total_ciphertext` size the
    /// overall bar (files counter + byte gauge respectively).
    pub fn new(files_total: u64, total_ciphertext: u64) -> Self {
        let mp = MultiProgress::new();
        let overall = mp.add(ProgressBar::new(total_ciphertext.max(1)));
        overall.set_style(
            ProgressStyle::with_template(
                "  {prefix:.dim} {bar:24.214/238} {bytes}/{total_bytes} · {bytes_per_sec} · ETA {eta}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("━━─"),
        );
        overall.set_prefix(format!("0/{files_total} files"));
        // Steady tick so speed/ETA refresh between confirmations.
        overall.enable_steady_tick(Duration::from_millis(120));
        Self {
            mp,
            overall,
            files_done: Arc::new(AtomicU64::new(0)),
            files_total,
        }
    }
}

impl ChunkProgress for BarProgress {
    fn begin_file(&self, file_name: &str, expected_ciphertext: u64) -> Box<dyn FileProgress> {
        let bar = self.mp.add(ProgressBar::new(expected_ciphertext.max(1)));
        bar.set_style(
            ProgressStyle::with_template("    {msg:.dim} {bar:20.214/238} {bytes}/{total_bytes}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("━━─"),
        );
        bar.set_message(display_name(file_name));
        Box::new(BarFileProgress {
            bar,
            overall: self.overall.clone(),
            files_done: Arc::clone(&self.files_done),
            files_total: self.files_total,
        })
    }

    fn finish_all(&self) {
        self.overall.finish_and_clear();
        let _ = self.mp.clear();
    }

    fn rollback(&self, ciphertext_bytes: u64) {
        // MUST NOT read-modify-write the shared run-wide bar (Codex thread,
        // task 1589): `position()` then `set_position(pos - n)` is two
        // separate atomic ops with a window between them, and a concurrent
        // `bb sync` file's `chunk_confirmed` (→ `inc()`) landing in that
        // window gets silently clobbered by the stale write — and two
        // concurrent `rollback` calls racing the same way can lose one of
        // them. `ProgressBar::dec` (indicatif ≥0.11, present in 0.17 — see
        // Cargo.lock) does a single `fetch_sub` on the same atomic `inc()`
        // does `fetch_add` on, so increments and rollbacks commute no matter
        // how they interleave. See `bar_progress_rollback_race` below for
        // the regression test.
        self.overall.dec(ciphertext_bytes);
    }
}

pub(super) struct BarFileProgress {
    pub(super) bar: ProgressBar,
    pub(super) overall: ProgressBar,
    pub(super) files_done: Arc<AtomicU64>,
    pub(super) files_total: u64,
}

impl FileProgress for BarFileProgress {
    fn chunk_confirmed(&self, ciphertext_bytes: u64) {
        self.bar.inc(ciphertext_bytes);
        self.overall.inc(ciphertext_bytes);
    }

    fn finish(self: Box<Self>, success: bool) {
        self.bar.finish_and_clear();
        if success {
            let done = self.files_done.fetch_add(1, Ordering::Relaxed) + 1;
            self.overall.set_prefix(format!("{done}/{} files", self.files_total));
        }
    }
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
/// Task 1589, Codex thread on `BarProgress::rollback`: it
/// used to read `overall.position()` then `set_position(...)` on the
/// run-wide SHARED bar — a non-atomic read-modify-write. A concurrent
/// `bb sync` upload calling `chunk_confirmed` (→ `overall.inc()`) between
/// the read and the write got its increment silently overwritten by the
/// rollback's stale write, and two concurrent rollbacks racing the same way
/// could lose one of them.
#[cfg(test)]
mod bar_progress_rollback_race {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::{BarProgress, ChunkProgress};

    /// Spins up many threads hammering `chunk_confirmed` (→ `inc()`)
    /// alongside threads hammering `rollback` (→ the code under test) on the
    /// SAME shared bar, with no synchronization between them beyond the
    /// bar itself, and asserts the final position is exactly
    /// `confirmed - rolled_back` — i.e. that `inc()` and `rollback()`
    /// commute no matter how they interleave. Repeated over many iterations
    /// (fresh bar each time) so the race window has many chances to land;
    /// a single run is not sufficient evidence either way for a race.
    #[test]
    fn rollback_commutes_with_concurrent_increments() {
        const ITERATIONS: usize = 300;
        const INC_THREADS: u64 = 8;
        const INCS_PER_THREAD: u64 = 200;
        const INC_AMOUNT: u64 = 3;
        const ROLLBACK_THREADS: u64 = 4;
        const ROLLBACKS_PER_THREAD: u64 = 100;
        const ROLLBACK_AMOUNT: u64 = 1;

        for iteration in 0..ITERATIONS {
            let progress = BarProgress::new(1, 1);
            let confirmed = AtomicU64::new(0);
            let rolled_back = AtomicU64::new(0);

            thread::scope(|scope| {
                for _ in 0..INC_THREADS {
                    let progress = &progress;
                    let confirmed = &confirmed;
                    scope.spawn(move || {
                        let file = progress.begin_file("race.bin", INCS_PER_THREAD * INC_AMOUNT);
                        for _ in 0..INCS_PER_THREAD {
                            file.chunk_confirmed(INC_AMOUNT);
                            confirmed.fetch_add(INC_AMOUNT, Ordering::SeqCst);
                        }
                        file.finish(true);
                    });
                }
                for _ in 0..ROLLBACK_THREADS {
                    let progress = &progress;
                    let rolled_back = &rolled_back;
                    scope.spawn(move || {
                        for _ in 0..ROLLBACKS_PER_THREAD {
                            progress.rollback(ROLLBACK_AMOUNT);
                            rolled_back.fetch_add(ROLLBACK_AMOUNT, Ordering::SeqCst);
                        }
                    });
                }
            });

            let expected = confirmed.load(Ordering::SeqCst) - rolled_back.load(Ordering::SeqCst);
            let actual = progress.overall.position();
            assert_eq!(
                actual,
                expected,
                "iteration {iteration}: lost update(s) racing rollback() against concurrent inc() \
                 (confirmed={}, rolled_back={})",
                confirmed.load(Ordering::SeqCst),
                rolled_back.load(Ordering::SeqCst)
            );
        }
    }
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
