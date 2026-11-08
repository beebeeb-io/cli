//! `bb billing usage`: the per-region storage breakdown that sits on top of
//! `show` (task 0486). Aggregated client-side over `GET /files`, root level
//! only, and labelled approximate for that reason.
//!
//! Owns: `usage`.
//! Must not: render the plan (see `billing::show`).

use super::*;

/// `bb billing usage` — storage usage with a per-region breakdown. See the
/// module doc comment above ("`usage` (task 0486, plan Task 18)") for the
/// approximate-breakdown deviation.
pub async fn usage() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // Quiet mode prints only used/quota/percentage. Check it BEFORE fetching
    // the file listing — the region breakdown is the only thing that needs
    // it, and `list_files(None)` follows every pagination cursor, so an
    // account with many root entries would otherwise pay for a full listing
    // walk (and risk the hard-cap warning) just to throw the result away
    // (Codex review, cli PR #26).
    if ui::is_quiet() {
        let usage = api.get_billing_usage().await?;
        let used = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
        let quota = usage.get("quota_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
        let pct = if quota > 0 { used as f64 / quota as f64 } else { 0.0 };
        println!("{}", format_storage_si(used));
        println!("{}", format_storage_si(quota));
        println!("{:.2}%", pct * 100.0);
        return Ok(());
    }

    let (usage_res, files_res) = tokio::join!(api.get_billing_usage(), api.list_files(None));
    let usage = usage_res?;

    let used = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let quota = usage.get("quota_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let pct = if quota > 0 { used as f64 / quota as f64 } else { 0.0 };

    // Local per-region aggregation over root-level files only — see the
    // module doc comment (eng-0486) for why this is approximate.
    let region_breakdown_failed = files_res.is_err();
    let (by_region, file_count) = files_res
        .ok()
        .and_then(|body| body.get("files").and_then(|v| v.as_array()).cloned())
        .map(|files| aggregate_by_region(&files))
        .unwrap_or_default();

    if ui::is_json() {
        let regions = region_rows_json(&by_region);
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "used_bytes": used,
                "quota_bytes": quota,
                "percentage": pct,
                "file_count": file_count,
                "by_region": regions,
                "by_region_note": "approximate — top-level files only, no /billing/usage/by-region route yet",
                "by_region_unavailable": region_breakdown_failed,
            }))
            .unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    println!();
    println!("  {}", "STORAGE".custom_color(crate::colors::AMBER));
    println!(
        "  {} / {}  ({:.0}%)",
        format_storage_si(used).custom_color(crate::colors::INK),
        format_storage_si(quota).custom_color(crate::colors::INK),
        pct * 100.0,
    );
    println!(
        "            {}",
        ui::quota_bar(used.max(0) as u64, quota.max(0) as u64, 40)
    );
    println!();
    println!(
        "  {} {}",
        "files  ".custom_color(crate::colors::INK_SAGE),
        file_count.to_string().custom_color(crate::colors::INK)
    );
    if region_breakdown_failed {
        println!(
            "  {} {}",
            "region ".custom_color(crate::colors::INK_SAGE),
            "unavailable — could not list files for the breakdown".custom_color(crate::colors::RED_ERR)
        );
    } else {
        println!(
            "  {} {}",
            "region ".custom_color(crate::colors::INK_SAGE),
            "approximate — top-level files only".custom_color(crate::colors::INK_DIM)
        );
        for ((_region, country), bytes) in &by_region {
            println!(
                "           {}    {}",
                country.custom_color(crate::colors::INK_DIM),
                format_storage_si(*bytes).custom_color(crate::colors::INK),
            );
        }
    }
    println!();
    Ok(())
}
