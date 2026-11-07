//! One pass's summary: the json, quiet and rich output and the one-shot exit code; and the pass's transfer sinks
//! (task 2333: live lines on a terminal, one run model per direction).

use super::*;
use crate::transfer_stats::{Direction as Way, Summary};
use crate::transfer_view::LiveProgress;
use crate::transfer_view::line::{self as view_line, Role};
use crate::transfer_view::sink::{Draw, ViewOptions};
use crate::transfer_view::units::{duration, mbps};

/// One sync pass's tally plus the flags that decide how it is reported.
pub(super) struct PassReport<'a> {
    pub uploaded: u32,
    pub downloaded: u32,
    pub conflicts: u32,
    pub failed: u32,
    pub skipped: u32,
    pub deletes: u32,
    pub held_back: u32,
    pub read_only_kept: u32,
    pub total_bytes: u64,
    pub elapsed: std::time::Duration,
    pub index_report: &'a crate::shared_folder_index::IndexRepairReport,
    pub dry_run: bool,
    pub will_enter_watch: bool,
    pub delete_refused: bool,
    /// What each direction moved (task 2333): `--json`'s `transfer_upload` / `transfer_download`, `-v`'s lines.
    pub upload: Summary,
    pub download: Summary,
    pub verbose: bool,
}

/// The sink of one direction of a pass. `moves`: this pass transfers something that way; otherwise (nothing
/// pending, or `--dry-run`, which moves nothing) it draws nothing, as the old bars did not.
pub(super) fn transfer_sink(way: Way, moves: bool, verbose: bool) -> LiveProgress {
    let mut options = ViewOptions::for_command(verbose);
    if !moves {
        options.draw = Draw::Off;
    }
    LiveProgress::new(way, options)
}

/// A remote file's share of the download plan, from its row: its ciphertext (28 bytes a chunk) and its chunks.
pub(super) fn download_plan(remote: &RemoteFile) -> (u64, u32) {
    let chunks = remote.chunk_count.max(1);
    let size = remote
        .resolved
        .as_ref()
        .and_then(|r| r.canonical.get("size_bytes"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    (size + 28 * chunks as u64, chunks)
}

/// The storage country a remote file's row names (spec §4.4).
pub(super) fn download_pool(remote: &RemoteFile) -> Option<crate::transfer_stats::PoolLabel> {
    remote
        .resolved
        .as_ref()
        .and_then(|r| crate::transfer_stats::PoolLabel::from_answer(&r.canonical))
}

impl PassReport<'_> {
    /// A pass that left uploads failed or conflicts unresolved is not "synced".
    fn incomplete(&self) -> bool {
        self.failed > 0 || self.conflicts > 0 || self.held_back > 0 || self.read_only_kept > 0 || self.delete_refused
    }
}

/// The pass summary: the json / quiet / rich output and the one-shot exit code.
pub(super) fn report_pass(report: PassReport<'_>) -> Result<(), String> {
    let incomplete = report.incomplete();
    if ui::is_json() {
        print_json(&report);
    } else if ui::is_quiet() {
        print_quiet(&report);
    } else {
        print_rich(&report, incomplete);
    }

    // A one-shot run (`--once`, json/quiet, …) exits 3 so scripts and cron jobs
    // notice (flow-6: it used to print "✓ synced" and exit 0). A dry run only
    // previews, and the watch loop keeps retrying, so neither fails here.
    let fail_one_shot = incomplete && !report.dry_run && !report.will_enter_watch;
    if fail_one_shot {
        Err(incomplete_error(&report))
    } else {
        Ok(())
    }
}

fn incomplete_error(report: &PassReport<'_>) -> String {
    crate::exit::with_code(
        crate::exit::INCOMPLETE,
        format!(
            "sync incomplete: {} \u{2014} fix the cause and re-run{}",
            match incomplete_summary(report.failed, report.conflicts, report.held_back, report.read_only_kept) {
                s if s.is_empty() && report.delete_refused => "--delete refused on a changed remote folder".to_string(),
                s => s,
            },
            if report.delete_refused {
                " (`bb sync --rebind` accepts the new remote folder before --delete may trash anything)"
            } else if report.held_back > 0 {
                " (`--allow-mass-delete` confirms the held-back removals)"
            } else if report.conflicts > 0 {
                " (`--force` overwrites conflicts with the local copy)"
            } else if report.read_only_kept > 0 {
                " (save each read-only file's edit under a new name; sync uploads it as a new file)"
            } else {
                ""
            }
        ),
    )
}

fn print_json(report: &PassReport<'_>) {
    let json_out = serde_json::json!({
        "ok": !report.incomplete(),
        "uploaded": report.uploaded,
        "downloaded": report.downloaded,
        "failed": report.failed,
        "conflicts": report.conflicts,
        "deleted": report.deletes,
        "held_back": report.held_back,
        "read_only_kept": report.read_only_kept,
        "skipped": report.skipped,
        "index_written": report.index_report.written,
        "index_deferred": report.index_report.deferred,
        "index_partial": report.index_report.partial,
        // L14: rows left out of the shared index, by reason.
        "index_skipped": report.index_report.skipped,
        "index_skip_reasons": report.index_report.skip_reasons,
        "bytes": report.total_bytes,
        "elapsed_secs": report.elapsed.as_secs_f64(),
        "transfer_upload": report.upload.to_json(),
        "transfer_download": report.download.to_json(),
    });
    println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
}

fn print_quiet(report: &PassReport<'_>) {
    println!(
        "{} up {} down {} conflict{}{}",
        report.uploaded,
        report.downloaded,
        report.conflicts,
        if report.failed > 0 {
            format!(" {} failed", report.failed)
        } else {
            String::new()
        },
        if report.deletes > 0 {
            format!(" {} del", report.deletes)
        } else {
            String::new()
        },
    );
}

fn print_rich(report: &PassReport<'_>, incomplete: bool) {
    let (mark, verdict) = if incomplete {
        (
            "!".custom_color(verdict_colour(incomplete)),
            "incomplete".custom_color(verdict_colour(incomplete)),
        )
    } else {
        (
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            "synced".custom_color(crate::colors::GREEN_OK),
        )
    };
    println!(
        "  {} {} {} {} {} {}",
        mark,
        verdict,
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        counts_line(report),
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        meta_line(report).custom_color(crate::colors::INK_DIM),
    );
    if report.verbose {
        for line in detail_lines(report) {
            println!("{}", view_line::paint(&line, Role::Dim));
        }
    }
}

/// An incomplete pass is a failure: the CLI's error colour, never amber (spec §6.5).
fn verdict_colour(incomplete: bool) -> colored::CustomColor {
    if incomplete {
        crate::colors::RED_ERR
    } else {
        crate::colors::GREEN_OK
    }
}

/// `-v`: one detail line per direction that moved something, led by `↑` or `↓` (spec §6.5).
fn detail_lines(report: &PassReport<'_>) -> Vec<String> {
    [("\u{2191}", &report.upload), ("\u{2193}", &report.download)]
        .into_iter()
        .filter(|(_, s)| s.chunks_done > 0)
        .map(|(arrow, s)| format!("    {arrow} {}", view_line::verbose_detail(s).trim_start()))
        .collect()
}

fn counts_line(report: &PassReport<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if report.uploaded > 0 {
        parts.push(format!(
            "{}",
            format!("{} \u{2191}", report.uploaded).custom_color(crate::colors::GREEN_OK)
        ));
    }
    if report.downloaded > 0 {
        parts.push(format!(
            "{}",
            format!("{} \u{2193}", report.downloaded).custom_color(crate::colors::CYAN)
        ));
    }
    if report.conflicts > 0 {
        parts.push(format!(
            "{}",
            format!("{} \u{26A1}", report.conflicts).custom_color(crate::colors::AMBER)
        ));
    }
    if report.failed > 0 {
        parts.push(format!(
            "{}",
            format!("{} failed", report.failed).custom_color(crate::colors::RED_ERR)
        ));
    }
    if report.held_back > 0 {
        parts.push(format!(
            "{}",
            format!("{} held back", report.held_back).custom_color(crate::colors::AMBER)
        ));
    }
    if report.read_only_kept > 0 {
        parts.push(format!(
            "{}",
            format!("{} read-only", report.read_only_kept).custom_color(crate::colors::AMBER)
        ));
    }
    if report.deletes > 0 {
        parts.push(format!(
            "{}",
            format!("{} \u{2717}", report.deletes).custom_color(crate::colors::RED_ERR)
        ));
    }

    if parts.is_empty() {
        "no changes".to_string()
    } else {
        parts.join(" ")
    }
}

/// `{size} · avg {Mbps} · {time}` (spec §5, §6.5): the one Mbps rule and the readable durations.
fn meta_line(report: &PassReport<'_>) -> String {
    let elapsed_ms = report.elapsed.as_millis() as u64;
    let mut meta_parts: Vec<String> = Vec::new();
    if report.total_bytes > 0 {
        meta_parts.push(ui::human_size(report.total_bytes));
        if elapsed_ms > 0 {
            meta_parts.push(format!("avg {}", mbps(report.total_bytes, elapsed_ms)));
        }
    }
    meta_parts.push(duration(elapsed_ms));
    meta_parts.join(" \u{00b7} ")
}

#[cfg(test)]
mod end_line_tests {
    use super::*;

    fn report<'a>(index: &'a crate::shared_folder_index::IndexRepairReport, bytes: u64, ms: u64) -> PassReport<'a> {
        let empty = || transfer_sink(Way::Upload, false, false).summary();
        PassReport {
            uploaded: 1,
            downloaded: 0,
            conflicts: 0,
            failed: 0,
            skipped: 0,
            deletes: 0,
            held_back: 0,
            read_only_kept: 0,
            total_bytes: bytes,
            elapsed: std::time::Duration::from_millis(ms),
            index_report: index,
            dry_run: false,
            will_enter_watch: false,
            delete_refused: false,
            upload: empty(),
            download: empty(),
            verbose: true,
        }
    }

    /// Spec §9.2 `sync_end_lines`: Mbps and the §5 durations; `! incomplete` in the error colour.
    #[test]
    fn sync_end_lines() {
        let index = crate::shared_folder_index::IndexRepairReport::default();
        assert_eq!(
            meta_line(&report(&index, 12_300_000, 3_000)),
            "12.3 MB · avg 32.8 Mbps · 3.0s"
        );
        assert_eq!(
            meta_line(&report(&index, 12_300_000, 92_100)),
            "12.3 MB · avg 1.1 Mbps · 1m 32s"
        );
        assert_eq!(meta_line(&report(&index, 0, 400)), "0.4s");
        assert_eq!(verdict_colour(true), crate::colors::RED_ERR);
        assert_eq!(verdict_colour(false), crate::colors::GREEN_OK);
    }

    /// `-v`: a direction that moved nothing has no detail line.
    #[test]
    fn verbose_detail_lines_name_each_direction_that_moved() {
        let index = crate::shared_folder_index::IndexRepairReport::default();
        let mut r = report(&index, 1, 1);
        assert!(detail_lines(&r).is_empty());
        r.upload.chunks_done = 3;
        r.upload.peak = 2;
        let lines = detail_lines(&r);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("    \u{2191} 3 chunks · up to 2 at once · "),
            "{lines:?}"
        );
    }
}
