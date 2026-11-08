//! Units and names for the transfer lines (task 2333, spec §5, §6.6): bytes,
//! Mbps, durations, the ETA, and file names fitted to a width. Plain text only.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::transfer_stats::Speed;

/// Bytes in the CLI's decimal units; `0 B` where the table formatter prints `—`
/// (in a progress line, "nothing yet" is not "unknown").
pub(crate) fn bytes(b: u64) -> String {
    if b == 0 {
        "0 B".to_string()
    } else {
        crate::ui::human_size(b)
    }
}

/// `bytes` over `ms` in the one Mbps rule (`ui::human_speed_network`); `—` with no time.
pub(crate) fn mbps(bytes: u64, ms: u64) -> String {
    if ms == 0 {
        return "\u{2014}".to_string();
    }
    crate::ui::human_speed_network(bytes as f64 * 1000.0 / ms as f64)
}

/// The current speed for the run line (spec §6.3).
pub(crate) fn speed(s: &Speed) -> String {
    match s {
        Speed::NoRate | Speed::Idle => "\u{2014} Mbps".to_string(),
        Speed::Stalled => "stalled".to_string(),
        Speed::BytesPerSec(r) => crate::ui::human_speed_network(*r),
    }
}

/// A duration (spec §5): one decimal of seconds under 59.95 s; otherwise whole
/// seconds first (half up), then `{m}m {ss}s` or `{h}h {mm}m {ss}s`. Never `1m 60s`.
pub(crate) fn duration(ms: u64) -> String {
    if ms < 59_950 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let s = (ms + 500) / 1000;
    let (h, m, ss) = (s / 3600, (s % 3600) / 60, s % 60);
    if h == 0 {
        format!("{m}m {ss:02}s")
    } else {
        format!("{h}h {m:02}m {ss:02}s")
    }
}

/// The ETA (spec §5): seconds rounded up first, then split; `—` when hidden.
pub(crate) fn eta(secs: Option<f64>) -> String {
    let Some(secs) = secs.filter(|s| s.is_finite()) else {
        return "\u{2014}".to_string();
    };
    let s = secs.max(0.0).ceil() as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else if s < 360_000 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else {
        ">99h".to_string()
    }
}

/// A name as it may reach a terminal: control characters (a newline, a tab, the
/// escape that starts a colour code) become `?`, so a file name cannot break the
/// live lines or send codes to the terminal.
pub(crate) fn clean_name(name: &str) -> String {
    name.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// `clean_name`, then fitted to `width` display columns with a middle ellipsis
/// that keeps the extension (spec §6.6): `tail = ⌈(width − 1) × 0.4⌉` columns.
pub(crate) fn fit_name(name: &str, width: usize) -> String {
    let name = clean_name(name);
    if name.width() <= width {
        return name;
    }
    if width == 0 {
        return String::new();
    }
    let tail_w = ((width - 1) as f64 * 0.4).ceil() as usize;
    let head_w = width - 1 - tail_w;
    let head = take_columns(name.chars(), head_w);
    let tail: String = take_columns(name.chars().rev(), tail_w).chars().rev().collect();
    format!("{head}\u{2026}{tail}")
}

/// `text` padded with spaces to `width` display columns.
pub(crate) fn pad(text: &str, width: usize) -> String {
    let w = text.width();
    format!("{text}{}", " ".repeat(width.saturating_sub(w)))
}

fn take_columns(chars: impl Iterator<Item = char>, columns: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in chars {
        let w = c.width().unwrap_or(0);
        if used + w > columns {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}
