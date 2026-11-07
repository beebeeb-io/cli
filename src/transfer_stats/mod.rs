//! Transfer statistics (task 2333, spec §4): events in, numbers out.
//!
//! One model per run of `bb push`, `bb pull` or `bb sync`. The drivers report
//! each chunk as it starts, is retried, is confirmed (an upload after the
//! server's answer, a download after it decrypts) and settles; the model
//! derives the progress, the speeds, the ETA and the in-flight gauge.
//!
//! Owns: the numbers every client should show alike (it is the type core's
//! `transfer_stats` lifts, workspace PR #554 phase 2).
//! Must not: do I/O, read a clock, take a lock or draw (spec §4.1; guarded by
//! `transfer_stats_is_pure`). Time comes in as a monotonic millisecond count.
//! Tests: `tests.rs` in this directory (spec §9.1).

use std::collections::HashMap;

mod pool;
mod window;

pub use pool::PoolLabel;
#[cfg(test)]
pub(crate) use pool::absent;
use window::{Entry, Taken, Window};

/// The current rate looks back at least this far (spec §4.5).
pub(crate) const WINDOW_SPAN_MS: u64 = 10_000;
/// At most this many confirmations are kept for the current rate.
pub(crate) const WINDOW_MAX_ENTRIES: usize = 256;
/// The next chunk is late once the newest confirmation is this many chunk-times old.
pub(crate) const LATE_FACTOR: f64 = 1.5;
/// Nothing is stalled or idle before this long without a confirmation.
pub(crate) const STALL_FLOOR_MS: u64 = 30_000;
/// ... or before this many times the newest chunk's own duration, if longer.
pub(crate) const STALL_FACTOR: u64 = 3;
/// The ETA needs a current rate measured over at least this span.
pub(crate) const ETA_MIN_SPAN_MS: u64 = 3_000;
/// The ETA never reads less than this while bytes remain.
pub(crate) const ETA_FLOOR_SECS: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Upload,
    Download,
}

impl Direction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Direction::Upload => "upload",
            Direction::Download => "download",
        }
    }
}

/// How a chunk's bytes travelled: straight to or from object storage, or through the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Direct,
    Api,
}

/// How a confirmed chunk came to be held: this run moved it, or the server already had it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    Transferred,
    AlreadyStored,
}

/// One file attempt of the run. A retried file gets a new slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileSlot(u32);

/// What a failed attempt had counted, to be taken back (spec §4.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub bytes: u64,
    pub chunks: u32,
    pub already_stored: u32,
}

/// The current speed, or why there is none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Speed {
    /// Nothing confirmed yet, or a zero span.
    NoRate,
    /// Nothing in flight for longer than the stall threshold (between files).
    Idle,
    /// Something in flight and no confirmation for longer than the stall threshold.
    Stalled,
    BytesPerSec(f64),
}

/// One pool and the chunks of the run's files that went to it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PoolCount {
    pub(crate) label: PoolLabel,
    pub(crate) chunks: u32,
}

/// A file being transferred right now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FileView {
    pub(crate) name: String,
    pub(crate) bytes_done: u64,
    pub(crate) bytes_total: u64,
    pub(crate) chunk_size: u64,
}

/// Everything a live line shows, at one moment.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) direction: Direction,
    pub(crate) files_done: u32,
    pub(crate) files_total: u32,
    pub(crate) plan_final: bool,
    pub(crate) bytes_done: u64,
    pub(crate) bytes_total: u64,
    pub(crate) bytes_transferred: u64,
    pub(crate) chunks_done: u32,
    pub(crate) chunks_total: u32,
    pub(crate) chunks_already_stored: u32,
    pub(crate) current: Speed,
    pub(crate) avg_bytes_per_sec: Option<f64>,
    pub(crate) eta_secs: Option<f64>,
    pub(crate) in_flight: u32,
    pub(crate) limit: u32,
    pub(crate) peak: u32,
    pub(crate) oldest_in_flight_ms: Option<u64>,
    pub(crate) direct: u32,
    pub(crate) api: u32,
    pub(crate) retries: u32,
    pub(crate) pools: Vec<PoolCount>,
    pub(crate) active: Vec<FileView>,
}

/// The end of a run (spec §7; [`Summary::to_json`] is the machine contract).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Summary {
    pub(crate) direction: Direction,
    pub(crate) files_done: u32,
    pub(crate) files_total: u32,
    pub(crate) bytes_done: u64,
    pub(crate) bytes_total: u64,
    pub(crate) bytes_transferred: u64,
    pub(crate) chunks_done: u32,
    pub(crate) chunks_total: u32,
    pub(crate) chunks_already_stored: u32,
    pub(crate) duration_ms: u64,
    /// Mbps (10^6 bit/s), rounded to 0.1; `None` when nothing moved or no time passed.
    pub(crate) avg_mbps: Option<f64>,
    pub(crate) peak: u32,
    pub(crate) max_in_flight: u32,
    pub(crate) direct: u32,
    pub(crate) api: u32,
    pub(crate) retries: u32,
    pub(crate) pools: Vec<PoolCount>,
}

impl Summary {
    /// The `transfer` object of `--json` (spec §7): these keys, these types.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let pools: Vec<serde_json::Value> = self
            .pools
            .iter()
            .map(|p| {
                serde_json::json!({
                    "country": p.label.country,
                    "country_code": p.label.country_code,
                    "chunks": p.chunks,
                })
            })
            .collect();
        serde_json::json!({
            "direction": self.direction.as_str(),
            "files_done": self.files_done,
            "files_total": self.files_total,
            "bytes_done": self.bytes_done,
            "bytes_total": self.bytes_total,
            "bytes_transferred": self.bytes_transferred,
            "chunks_done": self.chunks_done,
            "chunks_total": self.chunks_total,
            "chunks_already_stored": self.chunks_already_stored,
            "duration_ms": self.duration_ms,
            "avg_mbps": self.avg_mbps,
            "peak_chunks_in_flight": self.peak,
            "max_chunks_in_flight": self.max_in_flight,
            "routes": { "direct": self.direct, "api": self.api },
            "retries": self.retries,
            "pools": pools,
        })
    }
}

/// One confirmed chunk, for the verbose per-chunk line (spec §6.4).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChunkRecord {
    pub(crate) file_name: String,
    pub(crate) index: u32,
    pub(crate) chunk_count: u32,
    pub(crate) bytes: u64,
    pub(crate) route: Route,
    pub(crate) held: Held,
    pub(crate) retries: u32,
    pub(crate) duration_ms: u64,
}

/// How a file the command announced with [`TransferStats::next_file`] ended (spec §4.6, I1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileEnd {
    /// Its share was sent.
    Uploaded,
    /// Not sent (a conflict answered "skip", or already uploaded): its share leaves the run.
    NotSent,
    /// Failed for good: its share stays in the totals (the run ends short of them), and what it did not send
    /// leaves the ETA.
    Failed,
}

/// The share of the totals of the file being sent: as planned, then as the driver read it when the file began.
#[derive(Clone, Copy)]
struct Share {
    bytes: u64,
    chunks: u32,
    /// `bytes_done` when the file was announced: what the file sent is measured from here.
    done_at_start: u64,
}

struct FileState {
    name: String,
    bytes_total: u64,
    chunk_count: u32,
    chunk_size: u64,
    bytes_done: u64,
    chunks_done: u32,
    /// `None` while in progress, then whether it finished.
    finished_ok: Option<bool>,
    pool: Option<PoolLabel>,
}

struct OpenChunk {
    file: FileSlot,
    index: u32,
    route: Route,
    start_ms: u64,
}

/// The model of one run (spec §4).
pub(crate) struct TransferStats {
    direction: Direction,
    run_start_ms: u64,
    files_total: u32,
    files_done: u32,
    bytes_total: u64,
    chunks_total: u32,
    /// Planned bytes a failed file will never send (`give_up`): in the totals, out of the ETA.
    given_up_bytes: u64,
    /// The file the command is sending now (`next_file` to `end_file`).
    share: Option<Share>,
    /// `--parallel` (spec §7): the shares of the files sent at once, by their key (`files_at_once`).
    at_once: HashMap<u32, files_at_once::KeyedShare>,
    /// Time spent waiting on the person (a conflict prompt): left out of the average and the run's duration.
    waited_ms: u64,
    plan_final: bool,
    limit: u32,
    max_limit: u32,
    bytes_done: u64,
    chunks_done: u32,
    already_stored: u32,
    bytes_transferred: u64,
    direct: u32,
    api: u32,
    retries: u32,
    peak: u32,
    files: Vec<FileState>,
    open: Vec<OpenChunk>,
    /// Retries per (file, chunk) until the chunk is done: a refetch is a new start of the same chunk.
    chunk_retries: HashMap<(u32, u32), u32>,
    window: Window,
}

impl TransferStats {
    /// A run that starts at `now_ms` (the average speed counts from here).
    pub(crate) fn new(direction: Direction, now_ms: u64) -> Self {
        Self {
            direction,
            run_start_ms: now_ms,
            files_total: 0,
            files_done: 0,
            bytes_total: 0,
            chunks_total: 0,
            given_up_bytes: 0,
            share: None,
            at_once: HashMap::new(),
            waited_ms: 0,
            plan_final: false,
            limit: 0,
            max_limit: 0,
            bytes_done: 0,
            chunks_done: 0,
            already_stored: 0,
            bytes_transferred: 0,
            direct: 0,
            api: 0,
            retries: 0,
            peak: 0,
            files: Vec::new(),
            open: Vec::new(),
            chunk_retries: HashMap::new(),
            window: Window::default(),
        }
    }

    pub(crate) fn plan_file(&mut self, bytes: u64, chunks: u32) {
        self.files_total += 1;
        self.bytes_total += bytes;
        self.chunks_total += chunks;
    }

    /// The next file the command sends was planned as `bytes` and `chunks` (its share of the totals). The driver
    /// reads the file's size again when it begins (after the conflict lookup and any prompt): `begin_file` moves the
    /// share to what it read, so a file that changed meanwhile never takes done past total (spec §4.6, I1).
    pub(crate) fn next_file(&mut self, bytes: u64, chunks: u32) {
        self.share = Some(Share {
            bytes,
            chunks,
            done_at_start: self.bytes_done,
        });
    }

    /// The file announced by [`next_file`](Self::next_file) ended: its share is settled as `end` says.
    pub(crate) fn end_file(&mut self, end: FileEnd) {
        let Some(share) = self.share.take() else { return };
        match end {
            FileEnd::Uploaded => {}
            FileEnd::NotSent => {
                self.files_total = self.files_total.saturating_sub(1);
                self.bytes_total = self.bytes_total.saturating_sub(share.bytes);
                self.chunks_total = self.chunks_total.saturating_sub(share.chunks);
            }
            FileEnd::Failed => {
                let sent = self.bytes_done.saturating_sub(share.done_at_start);
                self.give_up(share.bytes.saturating_sub(sent));
            }
        }
    }

    /// `ms` were spent waiting on the person (a conflict prompt). That is not transfer time: the average and the
    /// summary's duration leave it out (the lead's ruling, 2026-11-06), so `avg × total = bytes` still holds.
    pub(crate) fn waited(&mut self, ms: u64) {
        self.waited_ms = self.waited_ms.saturating_add(ms);
    }

    /// `bytes` of the plan will not be sent: a file failed for good. They stay in the totals (the run ends short
    /// of them), but they are not waiting to be sent, so the ETA leaves them out.
    fn give_up(&mut self, bytes: u64) {
        self.given_up_bytes = self.given_up_bytes.saturating_add(bytes);
    }

    /// A file has begun: the run got as far as moving bytes.
    pub(crate) fn has_begun(&self) -> bool {
        !self.files.is_empty()
    }

    /// The totals are final: the bar, the percent and the ETA may be shown.
    pub(crate) fn seal_plan(&mut self) {
        self.plan_final = true;
    }

    /// The most chunks that may be in flight now; the summary reports the largest set.
    pub(crate) fn set_limit(&mut self, max_in_flight: u32) {
        self.limit = max_in_flight;
        self.max_limit = self.max_limit.max(max_in_flight);
    }

    /// A file (or another attempt at it) begins, as the driver read it: `bytes` of ciphertext in `chunk_count`
    /// chunks (0: not known). The announced file's share of the totals becomes these numbers.
    pub(crate) fn begin_file(&mut self, name: &str, bytes: u64, chunk_count: u32, chunk_size: u64) -> FileSlot {
        if let Some(share) = self.share.as_mut() {
            self.bytes_total = self.bytes_total.saturating_sub(share.bytes).saturating_add(bytes);
            share.bytes = bytes;
            if chunk_count > 0 {
                self.chunks_total = self
                    .chunks_total
                    .saturating_sub(share.chunks)
                    .saturating_add(chunk_count);
                share.chunks = chunk_count;
            }
        }
        self.files.push(FileState {
            name: name.to_string(),
            bytes_total: bytes,
            chunk_count,
            chunk_size,
            bytes_done: 0,
            chunks_done: 0,
            finished_ok: None,
            pool: None,
        });
        FileSlot((self.files.len() - 1) as u32)
    }

    pub(crate) fn chunk_started(&mut self, f: FileSlot, index: u32, route: Route, now_ms: u64) {
        self.open.push(OpenChunk {
            file: f,
            index,
            route,
            start_ms: now_ms,
        });
        self.peak = self.peak.max(self.open.len() as u32);
    }

    pub(crate) fn chunk_retried(&mut self, f: FileSlot, index: u32) {
        self.retries += 1;
        *self.chunk_retries.entry((f.0, index)).or_default() += 1;
    }

    /// The chunk left the wire: confirmed, failed, interrupted or dropped. A settle
    /// with nothing open for it changes nothing (the gauge never goes negative).
    pub(crate) fn chunk_settled(&mut self, f: FileSlot, index: u32, _now_ms: u64) {
        if let Some(at) = self.open.iter().position(|c| c.file == f && c.index == index) {
            self.open.remove(at);
        }
    }

    /// A chunk is held: confirmed by the server (upload) or decrypted (download).
    pub(crate) fn chunk_done(&mut self, f: FileSlot, index: u32, bytes: u64, held: Held, now_ms: u64) -> ChunkRecord {
        let (route, start_ms) = self.open_chunk(f, index, now_ms);
        self.bytes_done += bytes;
        self.chunks_done += 1;
        if held == Held::AlreadyStored {
            self.already_stored += 1;
        } else {
            self.transferred(route, 1, bytes, start_ms, now_ms);
        }
        self.file_progress(f, bytes, 1);
        self.check_invariants();
        ChunkRecord {
            file_name: self.files[f.0 as usize].name.clone(),
            index,
            chunk_count: self.files[f.0 as usize].chunk_count,
            bytes,
            route,
            held,
            retries: self.chunk_retries.remove(&(f.0, index)).unwrap_or(0),
            duration_ms: now_ms.saturating_sub(start_ms),
        }
    }

    /// A buffered body: all `chunks` of the file arrived and decrypted at once.
    ///
    /// Its length is known only now (a legacy file's JSON chunks are longer than the file row says): a body longer
    /// than the file's share of the plan grows it, as `begin_file` follows what a driver read (I1).
    pub(crate) fn body_done(&mut self, f: FileSlot, chunks: u32, bytes: u64, route: Route, now_ms: u64) -> ChunkRecord {
        let file = &mut self.files[f.0 as usize];
        let longer = file.bytes_done.saturating_add(bytes).saturating_sub(file.bytes_total);
        if longer > 0 {
            file.bytes_total += longer;
            self.bytes_total = self.bytes_total.saturating_add(longer);
            if let Some(share) = self.share.as_mut() {
                share.bytes = share.bytes.saturating_add(longer);
            }
        }
        let start_ms = self
            .open
            .iter()
            .filter(|c| c.file == f)
            .map(|c| c.start_ms)
            .min()
            .unwrap_or(now_ms);
        self.bytes_done += bytes;
        self.chunks_done += chunks;
        self.transferred(route, chunks, bytes, start_ms, now_ms);
        self.file_progress(f, bytes, chunks);
        self.check_invariants();
        ChunkRecord {
            file_name: self.files[f.0 as usize].name.clone(),
            index: 0,
            chunk_count: self.files[f.0 as usize].chunk_count,
            bytes,
            route,
            held: Held::Transferred,
            retries: 0,
            duration_ms: now_ms.saturating_sub(start_ms),
        }
    }

    /// Take back what a failed attempt counted. Progress drops; what crossed the
    /// wire (`bytes_transferred`, the routes, the rate window) stays (spec §4.3).
    /// Saturating: a report larger than what was counted leaves 0, never a panic.
    pub(crate) fn rollback(&mut self, tally: Tally) {
        self.bytes_done = self.bytes_done.saturating_sub(tally.bytes);
        self.chunks_done = self.chunks_done.saturating_sub(tally.chunks);
        self.already_stored = self.already_stored.saturating_sub(tally.already_stored);
    }

    pub(crate) fn pool(&mut self, f: FileSlot, label: PoolLabel) {
        self.files[f.0 as usize].pool = Some(label);
    }

    pub(crate) fn file_done(&mut self, f: FileSlot, ok: bool) {
        if ok {
            self.files_done += 1;
        }
        self.files[f.0 as usize].finished_ok = Some(ok);
    }

    /// Files planned for the run (cheap; the sink asks it per chunk).
    pub(crate) fn files_total(&self) -> u32 {
        self.files_total
    }

    pub(crate) fn snapshot(&self, now_ms: u64) -> Snapshot {
        let (current, taken, late) = self.current_speed(now_ms);
        Snapshot {
            direction: self.direction,
            files_done: self.files_done,
            files_total: self.files_total,
            plan_final: self.plan_final,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            bytes_transferred: self.bytes_transferred,
            chunks_done: self.chunks_done,
            chunks_total: self.chunks_total,
            chunks_already_stored: self.already_stored,
            current,
            avg_bytes_per_sec: self.average(now_ms),
            eta_secs: self.eta(current, taken, late, now_ms),
            in_flight: self.open.len() as u32,
            limit: self.limit,
            peak: self.peak,
            oldest_in_flight_ms: self.open.iter().map(|c| now_ms.saturating_sub(c.start_ms)).max(),
            direct: self.direct,
            api: self.api,
            retries: self.retries,
            pools: self.pool_counts(),
            active: self
                .files
                .iter()
                .filter(|f| f.finished_ok.is_none())
                .map(|f| FileView {
                    name: f.name.clone(),
                    bytes_done: f.bytes_done,
                    bytes_total: f.bytes_total,
                    chunk_size: f.chunk_size,
                })
                .collect(),
        }
    }

    pub(crate) fn summary(&self, now_ms: u64) -> Summary {
        let duration_ms = self.working_ms(now_ms);
        Summary {
            direction: self.direction,
            files_done: self.files_done,
            files_total: self.files_total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            bytes_transferred: self.bytes_transferred,
            chunks_done: self.chunks_done,
            chunks_total: self.chunks_total,
            chunks_already_stored: self.already_stored,
            duration_ms,
            avg_mbps: self
                .average(now_ms)
                .map(|bps| (bps * 8.0 / 1_000_000.0 * 10.0).round() / 10.0),
            peak: self.peak,
            max_in_flight: self.max_limit,
            direct: self.direct,
            api: self.api,
            retries: self.retries,
            pools: self.pool_counts(),
        }
    }

    // ── internals ──────────────────────────────────────────────────────────

    /// The open chunk's route and start; a chunk confirmed without a start began now.
    fn open_chunk(&self, f: FileSlot, index: u32, now_ms: u64) -> (Route, u64) {
        self.open
            .iter()
            .find(|c| c.file == f && c.index == index)
            .map(|c| (c.route, c.start_ms))
            .unwrap_or((Route::Api, now_ms))
    }

    fn transferred(&mut self, route: Route, chunks: u32, bytes: u64, start_ms: u64, end_ms: u64) {
        self.bytes_transferred += bytes;
        match route {
            Route::Direct => self.direct += chunks,
            Route::Api => self.api += chunks,
        }
        self.window.push(Entry {
            start_ms,
            end_ms,
            bytes,
        });
    }

    fn file_progress(&mut self, f: FileSlot, bytes: u64, chunks: u32) {
        let file = &mut self.files[f.0 as usize];
        file.bytes_done += bytes;
        file.chunks_done += chunks;
    }

    /// I1 (spec §4.6): a bug in a driver, not a state to hide. Release builds show the real numbers.
    fn check_invariants(&self) {
        debug_assert!(
            !self.plan_final || (self.chunks_done <= self.chunks_total && self.bytes_done <= self.bytes_total),
            "transfer stats: done exceeds the plan ({} of {} chunks, {} of {} bytes)",
            self.chunks_done,
            self.chunks_total,
            self.bytes_done,
            self.bytes_total
        );
    }

    /// The run's time so far, less the time spent waiting on the person.
    fn working_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.run_start_ms).saturating_sub(self.waited_ms)
    }

    /// Bytes per second over the run's working time; none before anything moved or time passed.
    fn average(&self, now_ms: u64) -> Option<f64> {
        let elapsed = self.working_ms(now_ms);
        (elapsed > 0 && self.bytes_transferred > 0).then(|| self.bytes_transferred as f64 * 1000.0 / elapsed as f64)
    }

    /// The current speed (spec §4.5), what the window took, and whether the next chunk is late.
    fn current_speed(&self, now_ms: u64) -> (Speed, Option<Taken>, bool) {
        let Some(taken) = self.window.take() else {
            return (Speed::NoRate, None, false);
        };
        let since = now_ms.saturating_sub(taken.newest.end_ms);
        let newest_ms = taken.newest.duration_ms();
        let in_flight = !self.open.is_empty();
        if since > STALL_FLOOR_MS.max(STALL_FACTOR * newest_ms) {
            let speed = if in_flight { Speed::Stalled } else { Speed::Idle };
            return (speed, Some(taken), false);
        }
        let late = in_flight && since as f64 > LATE_FACTOR * newest_ms as f64;
        let span = if late {
            now_ms.saturating_sub(taken.oldest_start_ms)
        } else {
            taken.span_ms()
        };
        if span == 0 {
            return (Speed::NoRate, Some(taken), false);
        }
        (
            Speed::BytesPerSec(taken.bytes as f64 * 1000.0 / span as f64),
            Some(taken),
            late,
        )
    }

    /// The ETA (spec §4.5): counts down while on time, rises while late, else hidden.
    fn eta(&self, current: Speed, taken: Option<Taken>, late: bool, now_ms: u64) -> Option<f64> {
        let (Speed::BytesPerSec(rate), Some(taken)) = (current, taken) else {
            return None;
        };
        let settled = self.bytes_done.saturating_add(self.given_up_bytes);
        if !self.plan_final || taken.span_ms() < ETA_MIN_SPAN_MS || settled >= self.bytes_total {
            return None;
        }
        let remaining = self
            .bytes_total
            .saturating_sub(self.bytes_done)
            .saturating_sub(self.given_up_bytes) as f64;
        let secs = if late {
            remaining / rate
        } else {
            remaining / rate - now_ms.saturating_sub(taken.newest.end_ms) as f64 / 1000.0
        };
        Some(secs.max(ETA_FLOOR_SECS))
    }

    /// Chunks per pool, in the order the pools were first seen. A failed attempt's
    /// chunks are left out: its rollback took them back, or the file failed.
    fn pool_counts(&self) -> Vec<PoolCount> {
        let mut out: Vec<PoolCount> = Vec::new();
        for file in self.files.iter().filter(|f| f.finished_ok != Some(false)) {
            let Some(label) = &file.pool else { continue };
            match out.iter_mut().find(|p| &p.label == label) {
                Some(p) => p.chunks += file.chunks_done,
                None => out.push(PoolCount {
                    label: label.clone(),
                    chunks: file.chunks_done,
                }),
            }
        }
        out
    }
}

mod files_at_once;

#[cfg(test)]
mod tests;
