//! `LiveProgress`: the progress sink every transfer command uses (task 2333,
//! spec §6.1, §6.7, §6.8). It feeds the drivers' events into the model and
//! draws the lines of `line.rs`.
//!
//! **Lock order (spec §6.8).** Two locks matter: the model's `Mutex` and
//! indicatif's state lock, which `MultiProgress::suspend` holds for its whole
//! closure and every draw takes. The model's lock is held only to apply an event
//! or copy a snapshot: nothing is drawn, printed or handed to indicatif while it
//! is held. A `note` inside a `suspend` closure writes directly (the
//! `suspended` flag), so it never re-enters indicatif's lock.
//!
//! Owns: the draw mode, the ticker thread, where each line goes.
//! Must not: compute a number (`transfer_stats`) or format one (`line.rs`).
//! Tests: `sink_tests.rs` in this directory.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use super::line::{self, Role};
use crate::transfer_stats::{
    ChunkRecord, Direction, FileEnd, FileSlot, Held, PoolLabel, Route, Snapshot, Summary, Tally, TransferStats,
};
use crate::upload::{ChunkProgress, FileProgress, Stream};

/// How often the live lines are redrawn (the steady tick the old bars used).
pub(crate) const TICK_MS: u64 = 120;
/// The width used when the terminal's cannot be read.
const FALLBACK_COLUMNS: u16 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    Default,
    Verbose,
}

/// Where the lines go (spec §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Draw {
    /// Live lines on stderr, redrawn by the ticker; per-chunk lines scroll above them.
    Terminal,
    /// No live lines; verbose per-chunk lines as plain stderr lines (`2>log` keeps them).
    Plain,
    /// `--quiet` / `--json`: nothing but the drivers' notices.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewOptions {
    pub(crate) level: Level,
    pub(crate) draw: Draw,
}

impl ViewOptions {
    /// The draw mode from the output mode and both streams (spec §6.1): live lines
    /// only when stdout (the CLI's rule) and stderr (where they are drawn) are terminals.
    pub(crate) fn decide(rich: bool, quiet_or_json: bool, stdout_tty: bool, stderr_tty: bool, verbose: bool) -> Self {
        let draw = if quiet_or_json || !rich {
            Draw::Off
        } else if stdout_tty && stderr_tty {
            Draw::Terminal
        } else {
            Draw::Plain
        };
        let level = if verbose { Level::Verbose } else { Level::Default };
        Self { level, draw }
    }

    /// [`decide`](Self::decide) for this process.
    pub(crate) fn for_command(verbose: bool) -> Self {
        Self::decide(
            crate::ui::is_rich(),
            crate::ui::is_quiet() || crate::ui::is_json(),
            std::io::stdout().is_terminal(),
            std::io::stderr().is_terminal(),
            verbose,
        )
    }
}

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
type Record = Arc<Mutex<Vec<String>>>;

/// The terminal's width now, or [`FALLBACK_COLUMNS`] when it cannot say (a pipe, a container answering 0).
pub(crate) fn terminal_columns() -> u16 {
    crossterm::terminal::size()
        .ok()
        .map(|(c, _)| c)
        .filter(|c| *c > 0)
        .unwrap_or(FALLBACK_COLUMNS)
}

/// A poisoned lock still holds consistent numbers (every update is one call); keep going.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Shared {
    model: Mutex<TransferStats>,
    direction: Direction,
    options: ViewOptions,
    clock: Clock,
    /// A fixed width (tests); `None` reads the terminal at each draw.
    columns: Option<u16>,
    /// The live lines' owner, in the Terminal mode only.
    mp: Option<MultiProgress>,
    bars: Mutex<Vec<ProgressBar>>,
    /// Tests: every line drawn (`live:`) or printed (`out:`, `err:`), in order.
    record: Option<Record>,
    suspended: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn columns(&self) -> u16 {
        self.columns.unwrap_or_else(terminal_columns)
    }

    /// Copy the numbers out; the model's lock is released before anything is drawn.
    fn snapshot(&self) -> Snapshot {
        let now = self.now();
        lock(&self.model).snapshot(now)
    }

    /// The live lines for a snapshot, plain (spec §6.3).
    fn render(&self, s: &Snapshot) -> Vec<String> {
        let columns = self.columns();
        let mut lines = vec![line::run_line(s, columns)];
        if self.options.level == Level::Verbose {
            lines.push(line::stats_line(s, columns));
            lines.push(line::pool_line(s, columns));
        }
        let single = s.files_total <= 1;
        lines.extend(s.active.iter().map(|f| line::file_line(f, single, columns)));
        lines
    }

    fn write(&self, to: Stream, text: &str) {
        if let Some(record) = &self.record {
            let tag = match to {
                Stream::Stdout => "out",
                Stream::Stderr => "err",
            };
            lock(record).push(format!("{tag}:{text}"));
            return;
        }
        match to {
            Stream::Stdout => println!("{text}"),
            Stream::Stderr => eprintln!("{text}"),
        }
    }

    /// Print a line around the live lines: hidden, printed, redrawn. Inside a
    /// `suspend` closure the lines are already hidden, so it writes directly.
    fn emit(&self, to: Stream, text: &str) {
        match &self.mp {
            Some(mp) if !self.suspended.load(Ordering::SeqCst) => mp.suspend(|| self.write(to, text)),
            _ => self.write(to, text),
        }
    }

    fn verbose_lines(&self) -> bool {
        self.options.level == Level::Verbose && self.options.draw != Draw::Off
    }

    fn chunk_line(&self, record: &ChunkRecord, multi_file: bool) {
        if !self.verbose_lines() {
            return;
        }
        let text = line::chunk_line(record, multi_file, self.columns());
        // Coloured only where the live lines are drawn (both streams terminals): `colored` decides by stdout alone,
        // so a `2>log` beside a terminal stdout would fill the log with colour codes.
        let text = if self.options.draw == Draw::Terminal && self.record.is_none() {
            line::paint(&text, Role::Dim)
        } else {
            text
        };
        self.emit(Stream::Stderr, &text);
    }

    /// One redraw of the live lines (the ticker calls it).
    fn draw(&self) {
        let Some(mp) = &self.mp else { return };
        let lines = self.render(&self.snapshot());
        let mut bars = lock(&self.bars);
        while bars.len() < lines.len() {
            let bar = mp.add(ProgressBar::new(0));
            bar.set_style(ProgressStyle::with_template("{msg}").unwrap_or_else(|_| ProgressStyle::default_bar()));
            bars.push(bar);
        }
        while bars.len() > lines.len() {
            if let Some(bar) = bars.pop() {
                bar.finish_and_clear();
                mp.remove(&bar);
            }
        }
        for (i, (bar, text)) in bars.iter().zip(lines).enumerate() {
            let painted = if i == 0 {
                line::paint_bar(&text)
            } else {
                line::paint(&text, Role::Dim)
            };
            bar.set_message(painted);
        }
    }

    fn clear(&self) {
        let mut bars = lock(&self.bars);
        for bar in bars.drain(..) {
            bar.finish_and_clear();
        }
        if let Some(mp) = &self.mp {
            let _ = mp.clear();
            self.mark("draw:cleared");
        }
    }

    /// The lines stay as last drawn (a run ended by an error, spec §6.7): drawn once more, then left.
    fn leave(&self) {
        self.draw();
        let mut bars = lock(&self.bars);
        for bar in bars.drain(..) {
            bar.abandon();
        }
        if self.mp.is_some() {
            self.mark("draw:left");
        }
    }

    /// Tests: what happened to the drawn lines at the end (only a sink that draws through indicatif has any).
    fn mark(&self, what: &str) {
        if let Some(record) = &self.record {
            lock(record).push(what.to_string());
        }
    }
}

impl super::notice::Notices for Shared {
    fn stderr(&self, line: &str) {
        self.emit(Stream::Stderr, line);
    }
}

/// The sink of one run (spec §6.8).
pub(crate) struct LiveProgress {
    shared: Arc<Shared>,
    ticker: Mutex<Option<JoinHandle<()>>>,
    /// `finish_all` ran: the lines are cleared, so dropping the sink leaves nothing behind.
    ended: AtomicBool,
}

impl LiveProgress {
    /// The sink for a command's run, starting now.
    pub(crate) fn new(direction: Direction, options: ViewOptions) -> Self {
        let mp = (options.draw == Draw::Terminal).then(MultiProgress::new);
        let tick = (options.draw == Draw::Terminal).then(|| Duration::from_millis(TICK_MS));
        let p = Self::build(direction, options, Arc::new(super::clock::now_ms), None, mp, None, tick);
        if options.draw == Draw::Terminal {
            p.register_notices();
        }
        p
    }

    /// Stops and joins the ticker, and lets go of the notices. Idempotent.
    fn stop_ticker(&self) {
        super::notice::unregister(&self.notices());
        self.shared.stop.store(true, Ordering::SeqCst);
        let handle = lock(&self.ticker).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// Lines printed by code that holds no sink (`notice::stderr`) go around this sink's lines.
    fn register_notices(&self) {
        super::notice::register(self.notices());
    }

    fn notices(&self) -> std::sync::Weak<dyn super::notice::Notices> {
        Arc::downgrade(&self.shared) as std::sync::Weak<dyn super::notice::Notices>
    }

    fn build(
        direction: Direction,
        options: ViewOptions,
        clock: Clock,
        columns: Option<u16>,
        mp: Option<MultiProgress>,
        record: Option<Record>,
        tick: Option<Duration>,
    ) -> Self {
        let shared = Arc::new(Shared {
            model: Mutex::new(TransferStats::new(direction, clock())),
            direction,
            options,
            clock,
            columns,
            mp,
            bars: Mutex::new(Vec::new()),
            record,
            suspended: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let ticker = tick.filter(|_| shared.mp.is_some()).map(|tick| {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                while !shared.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(tick);
                    if shared.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    shared.draw();
                }
            })
        });
        Self {
            shared,
            ticker: Mutex::new(ticker),
            ended: AtomicBool::new(false),
        }
    }

    pub(crate) fn plan_file(&self, bytes: u64, chunks: u32) {
        lock(&self.shared.model).plan_file(bytes, chunks);
    }

    /// The next file the command sends was planned as `bytes` and `chunks` (`TransferStats::next_file`).
    pub(crate) fn next_file(&self, bytes: u64, chunks: u32) {
        lock(&self.shared.model).next_file(bytes, chunks);
    }

    /// The file announced by [`next_file`](Self::next_file) ended (`TransferStats::end_file`).
    pub(crate) fn end_file(&self, end: FileEnd) {
        lock(&self.shared.model).end_file(end);
    }

    pub(crate) fn seal_plan(&self) {
        lock(&self.shared.model).seal_plan();
    }

    /// The live lines are drawn (both streams are terminals): a command leaves out the per-file lines they show.
    pub(crate) fn draws_live(&self) -> bool {
        self.shared.options.draw == Draw::Terminal
    }

    /// The run's end: stops the ticker, clears the lines, then reads the numbers.
    pub(crate) fn summary(&self) -> Summary {
        self.finish_all();
        let now = self.shared.now();
        lock(&self.shared.model).summary(now)
    }

    /// Tests: a Terminal-level sink with a fixed width and clock, recording every
    /// line it would draw (on [`render_now`](Self::render_now)) or print. No ticker.
    #[cfg(test)]
    pub(crate) fn for_test(
        direction: Direction,
        level: Level,
        columns: u16,
        clock: Arc<std::sync::atomic::AtomicU64>,
    ) -> (Self, Record) {
        let record: Record = Arc::new(Mutex::new(Vec::new()));
        let options = ViewOptions {
            level,
            draw: Draw::Terminal,
        };
        let clock: Clock = Arc::new(move || clock.load(Ordering::SeqCst));
        let p = Self::build(
            direction,
            options,
            clock,
            Some(columns),
            None,
            Some(Arc::clone(&record)),
            None,
        );
        (p, record)
    }

    /// Tests: a sink in the given mode, recording what it prints. Clock at 0, 80 columns.
    #[cfg(test)]
    pub(crate) fn for_draw(direction: Direction, options: ViewOptions) -> (Self, Record) {
        let record: Record = Arc::new(Mutex::new(Vec::new()));
        let p = Self::build(
            direction,
            options,
            Arc::new(|| 0),
            Some(80),
            None,
            Some(Arc::clone(&record)),
            None,
        );
        (p, record)
    }

    /// Tests: the real Terminal path (indicatif, the ticker) on a hidden draw target.
    #[cfg(test)]
    pub(crate) fn for_ticker_test(tick: Duration) -> Self {
        let mp = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let options = ViewOptions {
            level: Level::Verbose,
            draw: Draw::Terminal,
        };
        let record: Record = Arc::new(Mutex::new(Vec::new()));
        Self::build(
            Direction::Upload,
            options,
            Arc::new(super::clock::now_ms),
            Some(80),
            Some(mp),
            Some(record),
            Some(tick),
        )
    }

    /// Tests: the Terminal path on a hidden draw target, without a ticker, recording what happens to its lines.
    #[cfg(test)]
    pub(crate) fn for_terminal_test() -> (Self, Record) {
        let mp = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let options = ViewOptions {
            level: Level::Default,
            draw: Draw::Terminal,
        };
        let record: Record = Arc::new(Mutex::new(Vec::new()));
        let p = Self::build(
            Direction::Upload,
            options,
            Arc::new(|| 0),
            Some(80),
            Some(mp),
            Some(Arc::clone(&record)),
            None,
        );
        (p, record)
    }

    /// Tests: the live lines now, recorded as `live:` entries.
    #[cfg(test)]
    pub(crate) fn render_now(&self) -> Vec<String> {
        let lines = self.shared.render(&self.shared.snapshot());
        if let Some(record) = &self.shared.record {
            lock(record).extend(lines.iter().map(|l| format!("live:{l}")));
        }
        lines
    }

    /// Tests: take `notice::stderr`'s lines, as the Terminal sink of a run does.
    #[cfg(test)]
    pub(crate) fn register_notices_for_test(&self) {
        self.register_notices();
    }

    /// Tests: the planned bytes failed files will never send, which the ETA leaves out.
    #[cfg(test)]
    pub(crate) fn given_up_for_test(&self) -> u64 {
        lock(&self.shared.model).given_up_bytes()
    }

    #[cfg(test)]
    pub(crate) fn ticker_running(&self) -> bool {
        lock(&self.ticker).as_ref().is_some_and(|t| !t.is_finished())
    }
}

/// Dropped without a summary, the run ended in an error: the ticker stops and the last lines stay on screen above
/// the error message (spec §6.7), when a file had begun. A run stopped before that (a lock held, a refused
/// conflict) leaves no transfer line behind. After a summary the lines are already cleared.
impl Drop for LiveProgress {
    fn drop(&mut self) {
        if !self.ended.load(Ordering::SeqCst) {
            self.stop_ticker();
            let begun = lock(&self.shared.model).has_begun();
            if begun {
                self.shared.leave();
            } else {
                self.shared.clear();
            }
        }
    }
}

impl ChunkProgress for LiveProgress {
    fn begin_file(&self, file_name: &str, expected_ciphertext: u64) -> Box<dyn FileProgress> {
        self.begin_file_chunks(file_name, expected_ciphertext, 0, 0)
    }

    fn begin_file_chunks(
        &self,
        file_name: &str,
        expected_ciphertext: u64,
        chunk_count: u32,
        chunk_size: u64,
    ) -> Box<dyn FileProgress> {
        let slot = lock(&self.shared.model).begin_file(file_name, expected_ciphertext, chunk_count, chunk_size);
        Box::new(LiveFile {
            slot,
            shared: Arc::clone(&self.shared),
            next_index: AtomicU32::new(0),
        })
    }

    /// Stops and joins the ticker, then clears the lines. Idempotent.
    fn finish_all(&self) {
        self.stop_ticker();
        self.shared.clear();
        self.ended.store(true, Ordering::SeqCst);
    }

    fn rollback(&self, ciphertext_bytes: u64) {
        lock(&self.shared.model).rollback(Tally {
            bytes: ciphertext_bytes,
            ..Tally::default()
        });
    }

    fn rollback_tally(&self, file_name: &str, tally: Tally) {
        lock(&self.shared.model).rollback(tally);
        if self.shared.verbose_lines() && tally.chunks > 0 {
            let text = line::rollback_line(file_name, tally, self.shared.direction);
            self.shared.emit(Stream::Stderr, &text);
        }
    }

    fn rollback_failed(&self, tally: Tally) {
        lock(&self.shared.model).rollback(tally);
    }

    fn set_limit(&self, max_in_flight: u32) {
        lock(&self.shared.model).set_limit(max_in_flight);
    }

    fn note(&self, text: &str, to: Stream) {
        self.shared.emit(to, text);
    }

    /// `f` is a prompt: the lines are hidden while it runs, and the time it waits on the person is left out of the
    /// average and the run's duration (`TransferStats::waited`).
    fn suspend(&self, f: &mut dyn FnMut()) {
        let start = self.shared.now();
        match &self.shared.mp {
            Some(mp) => {
                self.shared.suspended.store(true, Ordering::SeqCst);
                mp.suspend(f);
                self.shared.suspended.store(false, Ordering::SeqCst);
            }
            None => f(),
        }
        let waited = self.shared.now().saturating_sub(start);
        lock(&self.shared.model).waited(waited);
    }
}

/// One file attempt's handle.
struct LiveFile {
    slot: FileSlot,
    shared: Arc<Shared>,
    /// For a driver that still reports bytes only (`chunk_confirmed`).
    next_index: AtomicU32,
}

impl LiveFile {
    fn done(&self, f: impl FnOnce(&mut TransferStats, u64) -> ChunkRecord) {
        let now = self.shared.now();
        let (record, multi_file) = {
            let mut model = lock(&self.shared.model);
            let record = f(&mut model, now);
            (record, model.files_total() > 1)
        };
        self.shared.chunk_line(&record, multi_file);
    }
}

impl FileProgress for LiveFile {
    fn chunk_confirmed(&self, ciphertext_bytes: u64) {
        let index = self.next_index.fetch_add(1, Ordering::SeqCst);
        self.chunk_done(index, ciphertext_bytes, Held::Transferred);
    }

    fn finish(self: Box<Self>, success: bool) {
        lock(&self.shared.model).file_done(self.slot, success);
    }

    fn chunk_started(&self, index: u32, route: Route) {
        let now = self.shared.now();
        lock(&self.shared.model).chunk_started(self.slot, index, route, now);
    }

    fn chunk_retried(&self, index: u32) {
        lock(&self.shared.model).chunk_retried(self.slot, index);
    }

    fn chunk_done(&self, index: u32, bytes: u64, held: Held) {
        let slot = self.slot;
        self.done(|m, now| m.chunk_done(slot, index, bytes, held, now));
    }

    fn body_done(&self, chunks: u32, bytes: u64, route: Route) {
        let slot = self.slot;
        self.done(|m, now| m.body_done(slot, chunks, bytes, route, now));
    }

    fn chunk_settled(&self, index: u32) {
        let now = self.shared.now();
        lock(&self.shared.model).chunk_settled(self.slot, index, now);
    }

    fn pool(&self, label: PoolLabel) {
        lock(&self.shared.model).pool(self.slot, label);
    }
}

mod files_at_once;
pub(crate) use files_at_once::{FileSink, RunLimit};
