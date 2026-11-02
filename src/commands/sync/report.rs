//! One pass's summary: the json, quiet and rich output and the one-shot exit code.

use super::*;

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
}

impl PassReport<'_> {
    /// A pass that left uploads failed or conflicts unresolved is not "synced".
    fn incomplete(&self) -> bool {
        self.failed > 0
            || self.conflicts > 0
            || self.held_back > 0
            || self.read_only_kept > 0
            || self.delete_refused
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
            match incomplete_summary(
                report.failed,
                report.conflicts,
                report.held_back,
                report.read_only_kept
            ) {
                s if s.is_empty() && report.delete_refused =>
                    "--delete refused on a changed remote folder".to_string(),
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
            "!".custom_color(crate::colors::AMBER),
            "incomplete".custom_color(crate::colors::AMBER),
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

fn meta_line(report: &PassReport<'_>) -> String {
    let elapsed_secs = report.elapsed.as_secs_f64();
    let avg_speed = if elapsed_secs > 0.0 {
        report.total_bytes as f64 / elapsed_secs
    } else {
        0.0
    };

    let mut meta_parts: Vec<String> = Vec::new();
    if report.total_bytes > 0 {
        meta_parts.push(ui::human_size(report.total_bytes));
    }
    if avg_speed > 0.0 && report.total_bytes > 0 {
        meta_parts.push(format!("avg {}", ui::human_speed(avg_speed)));
    }
    meta_parts.push(format!("{:.1}s", elapsed_secs));
    meta_parts.join(" \u{00b7} ")
}
