//! What `bb push` plans and prints about its transfer (task 2333, spec §4.2, §6.5): each file's plan, the run's
//! sink, the resolve line, a file's end line, and the summary with its verbose detail. The numbers come from
//! `transfer_stats`, the text from `transfer_view::line`.
//!
//! Owns: the push's plan and end lines. Must not: upload, or decide a conflict (`push`).

use colored::Colorize;

use crate::transfer_stats::{Direction, Summary};
use crate::transfer_view::line::{self, Role};
use crate::transfer_view::{LiveProgress, ViewOptions};
use crate::upload::limits::ChunkLimits;
use crate::upload::{ChunkProgress, expected_at_chunk_size, planned_chunk_size};

/// One file's share of the run's totals: its ciphertext and chunks as the upload driver will send them: a fresh
/// upload planned at `--parallel`, a resumed one at the chunk size its record names (`planned_chunk_size`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct FilePlan {
    bytes: u64,
    chunks: u32,
}

impl FilePlan {
    /// The plan of the file at `path` as it is now (a file that cannot be read plans as empty: its upload fails),
    /// at the chunk size its resume record names, else at `parallel`. A record from before `--parallel` plans as
    /// `bb push` did then (1), so the totals match what the driver sends (spec §4.3).
    pub(super) fn of_path(path: &std::path::Path, parallel: u32) -> Self {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let pending = crate::resume::resumable_upload_now(path);
        Self::at_chunk_size(size, planned_chunk_size(size, parallel, 1, pending.as_ref()))
    }

    fn at_chunk_size(size: u64, chunk_size: u64) -> Self {
        let (bytes, chunks) = expected_at_chunk_size(size, chunk_size);
        Self { bytes, chunks }
    }

    fn add_to(self, progress: &LiveProgress) {
        progress.plan_file(self.bytes, self.chunks);
    }

    /// This file is the one sent next: the run's totals follow the size the driver reads when it begins.
    pub(super) fn announce(self, progress: &LiveProgress) {
        progress.next_file(self.bytes, self.chunks);
    }

    /// [`announce`](Self::announce) for a file sent beside others (`--parallel`, spec §7), under its `key`.
    pub(super) fn announce_as(self, progress: &LiveProgress, key: u32) {
        progress.next_file_as(key, self.bytes, self.chunks);
    }
}

/// A folder push's line for a file that failed (`  ! {path}: {error}`, on stderr around the live lines), as it
/// fails; none in `--json`, whose `failed` list carries it.
pub(super) fn note_failure(progress: &dyn ChunkProgress, rel: &str, error: &str) {
    if crate::ui::is_json() {
        return;
    }
    progress.note(
        &format!(
            "  {} {}: {}",
            "!".custom_color(crate::colors::RED_ERR),
            rel.custom_color(crate::colors::INK),
            error,
        ),
        crate::upload::Stream::Stderr,
    );
}

/// The sink of one push: every file planned, the plan sealed, the limit what `--parallel` asked (the driver
/// lowers it per file when the budget does, spec §4.4).
pub(super) fn start(plans: impl IntoIterator<Item = FilePlan>, verbose: bool, limits: &ChunkLimits) -> LiveProgress {
    let progress = LiveProgress::new(Direction::Upload, ViewOptions::for_command(verbose));
    for plan in plans {
        plan.add_to(&progress);
    }
    progress.seal_plan();
    progress.set_limit(limits.asked());
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
