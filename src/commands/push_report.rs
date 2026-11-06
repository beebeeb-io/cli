//! What `bb push` plans and prints about its transfer (task 2333, spec §4.2, §6.5): each file's plan, the run's
//! sink, the resolve line, a file's end line, and the summary with its verbose detail. The numbers come from
//! `transfer_stats`, the text from `transfer_view::line`.
//!
//! Owns: the push's plan and end lines. Must not: upload, or decide a conflict (`push`).

use colored::Colorize;

use crate::transfer_stats::{Direction, Summary};
use crate::transfer_view::line::{self, Role};
use crate::transfer_view::{LiveProgress, ViewOptions};
use crate::upload::{CHUNKS_IN_FLIGHT_PER_FILE, ChunkProgress, expected_chunks_for, expected_ciphertext_for};

/// One file's share of the run's totals: its ciphertext and chunks as the upload driver will send them
/// (`bb push` uploads at concurrency 1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct FilePlan {
    bytes: u64,
    chunks: u32,
}

impl FilePlan {
    pub(super) fn of_size(size: u64) -> Self {
        Self {
            bytes: expected_ciphertext_for(size, 1),
            chunks: expected_chunks_for(size, 1),
        }
    }

    /// The plan of the file at `path` as it is now (a file that cannot be read plans as empty: its upload fails).
    pub(super) fn of_path(path: &std::path::Path) -> Self {
        Self::of_size(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
    }

    pub(super) fn add_to(self, progress: &LiveProgress) {
        progress.plan_file(self.bytes, self.chunks);
    }

    pub(super) fn take_from(self, progress: &LiveProgress) {
        progress.unplan_file(self.bytes, self.chunks);
    }

    pub(super) fn bytes(self) -> u64 {
        self.bytes
    }

    /// The plan of the file at `path` now: when its size changed since `self` was planned, the run's totals are
    /// moved from the old plan to the new one.
    pub(super) fn follow(self, path: &std::path::Path, progress: &LiveProgress) -> Self {
        let now = Self::of_path(path);
        if now != self {
            self.take_from(progress);
            now.add_to(progress);
        }
        now
    }
}

/// The sink of one push: every file planned, the plan sealed, one chunk in flight at a time.
pub(super) fn start(plans: impl IntoIterator<Item = FilePlan>, verbose: bool) -> LiveProgress {
    let progress = LiveProgress::new(Direction::Upload, ViewOptions::for_command(verbose));
    for plan in plans {
        plan.add_to(&progress);
    }
    progress.seal_plan();
    progress.set_limit(CHUNKS_IN_FLIGHT_PER_FILE);
    progress
}

/// `  {n} file[s] · {size} · encrypting with AES-256-GCM`, dim (rich mode).
pub(super) fn print_resolve_line(files: usize, size: u64) {
    let files = if files == 1 {
        "1 file".to_string()
    } else {
        format!("{files} files")
    };
    println!(
        "  {}",
        format!(
            "{files} \u{00B7} {} \u{00B7} encrypting with AES-256-GCM",
            crate::ui::human_size(size)
        )
        .custom_color(crate::colors::INK_DIM)
    );
}

/// A file's end line, painted: `  ✓ {name}  {size}  {speed}  {time}` (spec §6.5), at the terminal's width.
pub(super) fn file_line(name: &str, size: u64, transferred: u64, wall: std::time::Duration) -> String {
    line::paint_push_file_line(
        name,
        size,
        transferred,
        wall.as_millis() as u64,
        crate::transfer_view::sink::terminal_columns(),
    )
}

/// The summary (`✓`, or `!` in the error colour when a file failed) and, with `--verbose`, its detail line.
pub(super) fn print_summary(s: &Summary, uploaded: usize, failed: usize, size: u64, verbose: bool) {
    let mark = if failed == 0 {
        line::paint("\u{2713}", Role::Ok)
    } else {
        line::paint("!", Role::Warn)
    };
    let text = line::summary_line("uploaded", uploaded as u32, ("failed", failed as u32), size, s);
    println!("  {mark} {}", line::paint(&text, Role::Dim));
    if verbose {
        println!("{}", line::paint(&line::verbose_detail(s), Role::Dim));
    }
}
