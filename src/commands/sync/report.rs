//! One pass's summary: the json, quiet and rich output and the one-shot exit code.

use super::*;

/// The pass summary: the json / quiet / rich output and the one-shot exit code.
#[allow(clippy::too_many_arguments)]
pub(super) fn report_pass(
    up_count: u32,
    down_count: u32,
    conflicts: u32,
    failed: u32,
    skipped: u32,
    deletes: u32,
    held_back: u32,
    read_only_kept: u32,
    total_bytes: u64,
    elapsed: std::time::Duration,
    index_report: &crate::shared_folder_index::IndexRepairReport,
    dry_run: bool,
    will_enter_watch: bool,
    delete_refused: bool,
) -> Result<(), String> {
    let elapsed_secs = elapsed.as_secs_f64();

    // A pass that left uploads failed or conflicts unresolved is not "synced".
    // A one-shot run (`--once`, json/quiet, …) exits 3 so scripts and cron jobs
    // notice (flow-6: it used to print "✓ synced" and exit 0). A dry run only
    // previews, and the watch loop keeps retrying, so neither fails here.
    let incomplete = failed > 0 || conflicts > 0 || held_back > 0 || read_only_kept > 0 || delete_refused;
    let incomplete_err = || {
        crate::exit::with_code(
            crate::exit::INCOMPLETE,
            format!(
                "sync incomplete: {} \u{2014} fix the cause and re-run{}",
                match incomplete_summary(failed, conflicts, held_back, read_only_kept) {
                    s if s.is_empty() && delete_refused => "--delete refused on a changed remote folder".to_string(),
                    s => s,
                },
                if delete_refused {
                    " (`bb sync --rebind` accepts the new remote folder before --delete may trash anything)"
                } else if held_back > 0 {
                    " (`--allow-mass-delete` confirms the held-back removals)"
                } else if conflicts > 0 {
                    " (`--force` overwrites conflicts with the local copy)"
                } else if read_only_kept > 0 {
                    " (save each read-only file's edit under a new name; sync uploads it as a new file)"
                } else {
                    ""
                }
            ),
        )
    };
    let fail_one_shot = incomplete && !dry_run && !will_enter_watch;

    if ui::is_json() {
        let json_out = serde_json::json!({
            "ok": !incomplete,
            "uploaded": up_count,
            "downloaded": down_count,
            "failed": failed,
            "conflicts": conflicts,
            "deleted": deletes,
            "held_back": held_back,
            "read_only_kept": read_only_kept,
            "skipped": skipped,
            "index_written": index_report.written,
            "index_deferred": index_report.deferred,
            "index_partial": index_report.partial,
            "bytes": total_bytes,
            "elapsed_secs": elapsed_secs,
        });
        println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
        return if fail_one_shot { Err(incomplete_err()) } else { Ok(()) };
    }

    if ui::is_quiet() {
        println!(
            "{} up {} down {} conflict{}{}",
            up_count,
            down_count,
            conflicts,
            if failed > 0 {
                format!(" {failed} failed")
            } else {
                String::new()
            },
            if deletes > 0 {
                format!(" {deletes} del")
            } else {
                String::new()
            },
        );
        return if fail_one_shot { Err(incomplete_err()) } else { Ok(()) };
    }

    println!();
    // Rich summary: ✓ synced · 2 ↑ 1 ↓ 1 ⚡ · 3.3 MB · avg 45 MB/s · 1.8s
    let avg_speed = if elapsed_secs > 0.0 {
        total_bytes as f64 / elapsed_secs
    } else {
        0.0
    };

    let mut parts: Vec<String> = Vec::new();
    if up_count > 0 {
        parts.push(format!(
            "{}",
            format!("{up_count} \u{2191}").custom_color(crate::colors::GREEN_OK)
        ));
    }
    if down_count > 0 {
        parts.push(format!(
            "{}",
            format!("{down_count} \u{2193}").custom_color(crate::colors::CYAN)
        ));
    }
    if conflicts > 0 {
        parts.push(format!(
            "{}",
            format!("{conflicts} \u{26A1}").custom_color(crate::colors::AMBER)
        ));
    }
    if failed > 0 {
        parts.push(format!(
            "{}",
            format!("{failed} failed").custom_color(crate::colors::RED_ERR)
        ));
    }
    if held_back > 0 {
        parts.push(format!(
            "{}",
            format!("{held_back} held back").custom_color(crate::colors::AMBER)
        ));
    }
    if read_only_kept > 0 {
        parts.push(format!(
            "{}",
            format!("{read_only_kept} read-only").custom_color(crate::colors::AMBER)
        ));
    }
    if deletes > 0 {
        parts.push(format!(
            "{}",
            format!("{deletes} \u{2717}").custom_color(crate::colors::RED_ERR)
        ));
    }

    let counts_str = if parts.is_empty() {
        "no changes".to_string()
    } else {
        parts.join(" ")
    };

    let mut meta_parts: Vec<String> = Vec::new();
    if total_bytes > 0 {
        meta_parts.push(ui::human_size(total_bytes));
    }
    if avg_speed > 0.0 && total_bytes > 0 {
        meta_parts.push(format!("avg {}", ui::human_speed(avg_speed)));
    }
    meta_parts.push(format!("{:.1}s", elapsed_secs));

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
        counts_str,
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        meta_parts.join(" \u{00b7} ").custom_color(crate::colors::INK_DIM),
    );

    if fail_one_shot {
        return Err(incomplete_err());
    }
    Ok(())
}
