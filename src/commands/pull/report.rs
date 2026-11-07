//! What `bb pull` plans and prints about its transfer (task 2333, spec §4.2, §6.5, §7): the run's sink, each file's
//! plan and storage country from its file row, the end lines and the folder pull's summary. The numbers come from
//! `transfer_stats`, the text from `transfer_view`.
//!
//! Owns: pull's plan and end lines. Must not: download (`crate::download`) or decide what to pull (`pull::folder`).

use crate::transfer_stats::{Direction, PoolLabel, Summary};
use crate::transfer_view::LiveProgress;
use crate::transfer_view::line::{self, Role};
use crate::transfer_view::sink::{Draw, ViewOptions};
use crate::transfer_view::units::{bytes, clean_name, duration, mbps};
use crate::upload::{ChunkProgress, Stream};

/// AES-GCM's nonce and tag: what each chunk adds to the plaintext on the wire.
const CHUNK_OVERHEAD: u64 = 28;

/// A file row's share of the run: its ciphertext and its chunks, as the row states them. The driver's own reading
/// of the file (the manifest, the stream's length, a buffered body) replaces the estimate when the file begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FilePlan {
    pub(super) bytes: u64,
    pub(super) chunks: u32,
}

impl FilePlan {
    pub(super) fn of_row(row: &serde_json::Value) -> Self {
        let size = row.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let chunks = row.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        Self::of_size(size, chunks)
    }

    /// A file of `size` plaintext bytes in `chunks` chunks (at least one: an empty file has one).
    pub(super) fn of_size(size: u64, chunks: u32) -> Self {
        let chunks = chunks.max(1);
        Self {
            bytes: size.saturating_add(CHUNK_OVERHEAD * chunks as u64),
            chunks,
        }
    }

    /// This file is the one the run downloads next: added to the plan and announced, so the driver's reading of it
    /// becomes its share (`TransferStats::begin_file`).
    pub(super) fn add_next(self, progress: &LiveProgress) {
        progress.plan_file(self.bytes, self.chunks);
        progress.next_file(self.bytes, self.chunks);
    }
}

/// A folder pull as it goes: one sink for the whole walk (each file is planned when the walk reaches it, so the
/// plan is final only at the end, spec §4.2), and the plaintext it wrote.
pub(super) struct FolderRun {
    pub(super) progress: LiveProgress,
    pub(super) pulled_bytes: u64,
}

/// `1 file`, `{n} files`.
pub(super) fn files(n: usize) -> String {
    line::count(n as u32, "file", "files")
}

/// The storage country a file row names (spec §4.4: nothing else from the row is kept).
pub(super) fn pool_of(row: &serde_json::Value) -> Option<PoolLabel> {
    PoolLabel::from_answer(row)
}

/// The sink of a pull, by the output mode (spec §6.1).
pub(super) fn start(verbose: bool) -> LiveProgress {
    LiveProgress::new(Direction::Download, ViewOptions::for_command(verbose))
}

/// The sink of a buffered download (a file-request upload): its body arrives whole, so a live bar would jump from
/// 0 to 100 % (spec §3.2). No live lines; the end lines, `-v`'s lines and `--json` as for any pull.
pub(super) fn start_buffered(verbose: bool) -> LiveProgress {
    let mut options = ViewOptions::for_command(verbose);
    if options.draw == Draw::Terminal {
        options.draw = Draw::Plain;
    }
    LiveProgress::new(Direction::Download, options)
}

/// `{name}  {size}  {speed}`: a pulled file, its plaintext size, and what it moved over its time.
pub(super) fn file_text(name: &str, size: u64, transferred: u64, ms: u64) -> (String, String, String) {
    (clean_name(name), bytes(size), mbps(transferred, ms))
}

/// `{time} · {how}` (spec §5 durations; `streamed`).
pub(super) fn time_text(ms: u64, how: &str) -> String {
    format!("{} \u{00B7} {how}", duration(ms))
}

/// `{time} download · {time} decrypt`: a buffered file-request download.
pub(super) fn request_time_text(download_ms: u64, decrypt_ms: u64) -> String {
    format!(
        "{} download \u{00B7} {} decrypt",
        duration(download_ms),
        duration(decrypt_ms)
    )
}

/// A pulled file's two end lines, painted, at `indent`: `✓ {name}  {size}  {speed}`, then the time line.
pub(super) fn print_file(
    progress: &dyn ChunkProgress,
    indent: &str,
    (name, size, transferred, ms): (&str, u64, u64, u64),
    time_line: &str,
) {
    let (name, size, speed) = file_text(name, size, transferred, ms);
    progress.note(
        &format!(
            "{indent}{} {}  {}  {}",
            line::paint("\u{2713}", Role::Ok),
            line::paint(&name, Role::Ink),
            line::paint(&size, Role::Dim),
            line::paint(&speed, Role::Ok)
        ),
        Stream::Stdout,
    );
    progress.note(
        &format!("{indent}  {}", line::paint(time_line, Role::Dim)),
        Stream::Stdout,
    );
}

/// With `--verbose`, the run's detail line under its end lines (spec §6.5).
pub(super) fn print_detail(s: &Summary, verbose: bool) {
    if verbose {
        println!("{}", line::paint(&line::verbose_detail(s), Role::Dim));
    }
}

/// The folder pull's summary (new, spec §6.5): `✓`, or `!` in the error colour when something was skipped.
pub(super) fn summary_text(files_ok: u32, skipped: u32, size: u64, s: &Summary) -> String {
    line::summary_line("pulled", files_ok, ("skipped", skipped), size, s)
}

pub(super) fn print_summary(s: &Summary, skipped: u32, size: u64, verbose: bool) {
    let mark = if skipped == 0 {
        line::paint("\u{2713}", Role::Ok)
    } else {
        line::paint("!", Role::Warn)
    };
    println!(
        "  {mark} {}",
        line::paint(&summary_text(s.files_done, skipped, size, s), Role::Dim)
    );
    print_detail(s, verbose);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn summary(bytes_transferred: u64, duration_ms: u64) -> Summary {
        let (live, _) = LiveProgress::for_draw(
            Direction::Download,
            ViewOptions {
                level: crate::transfer_view::sink::Level::Default,
                draw: Draw::Off,
            },
        );
        let mut s = live.summary();
        s.bytes_transferred = bytes_transferred;
        s.duration_ms = duration_ms;
        s
    }

    /// Spec §9.2 `pull_end_lines`: the §5 duration, never a raw `ms`; the folder summary in Mbps.
    #[test]
    fn pull_end_lines() {
        assert_eq!(time_text(4_240, "streamed"), "4.2s · streamed");
        assert_eq!(time_text(440_673, "streamed"), "7m 21s · streamed");
        assert_eq!(request_time_text(1_200, 300), "1.2s download · 0.3s decrypt");
        assert_eq!(
            file_text("movie.mkv", 12_300_000, 12_500_000, 1_000),
            ("movie.mkv".to_string(), "12.3 MB".to_string(), "100.0 Mbps".to_string())
        );
        let s = summary(12_500_000, 92_100);
        assert_eq!(
            summary_text(2, 0, 12_300_000, &s),
            "2 files pulled · 12.3 MB · avg 1.1 Mbps · 1m 32s total"
        );
        assert_eq!(
            summary_text(1, 1, 4_000, &s),
            "1 file pulled · 1 skipped · 4 KB · avg 1.1 Mbps · 1m 32s total"
        );
    }

    /// Review Focus 4: a server-provided name cannot carry a control character to the terminal.
    #[test]
    fn a_pulled_name_never_carries_control_characters() {
        let (name, _, _) = file_text("evil\u{1b}[2J\nname.txt", 1, 1, 1);
        assert_eq!(name, "evil?[2J?name.txt");
    }

    /// A file row's plan: its ciphertext (28 bytes a chunk) and chunks; a row without them plans one empty chunk.
    #[test]
    fn a_rows_plan_and_country() {
        let row = json!({
            "size_bytes": 1000, "chunk_count": 3,
            "storage_location": { "country": "Finland", "country_code": "FI", "city": "Testville" },
        });
        assert_eq!(FilePlan::of_row(&row), FilePlan { bytes: 1084, chunks: 3 });
        assert_eq!(FilePlan::of_row(&json!({})), FilePlan { bytes: 28, chunks: 1 });
        let pool = pool_of(&row).expect("the row names a country");
        assert_eq!(pool.country.as_deref(), Some("Finland"));
        assert!(!format!("{pool:?}").contains("Testville"));
    }
}
