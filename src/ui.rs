// UI rendering module — output modes, box drawing, formatting helpers.
//
// Every command should call `ui::init()` once at startup (main.rs does this).
// Then use `is_rich()`, `is_json()`, `is_quiet()` to branch on output mode.

use std::sync::OnceLock;

use colored::{ColoredString, Colorize};

use crate::colors;

// ── Output mode ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Full colors, box drawing, progress bars.
    Rich,
    /// Structured JSON on stdout — no ANSI, no decorations.
    Json,
    /// Minimal text, no progress, no colors.
    Quiet,
}

static MODE: OnceLock<OutputMode> = OnceLock::new();

/// Initialise the global output mode.  Call once from `main()`.
///
/// Precedence: `--json` > `--quiet` > rich (default).
/// If `--no-color` or `NO_COLOR` env var is set, colored output is disabled
/// globally via the `colored` crate's control mechanism.
pub fn init(json: bool, quiet: bool, no_color: bool) {
    let mode = if json {
        OutputMode::Json
    } else if quiet {
        OutputMode::Quiet
    } else {
        OutputMode::Rich
    };
    let _ = MODE.set(mode);

    // Respect NO_COLOR (https://no-color.org/) and --no-color flag.
    if no_color || std::env::var("NO_COLOR").is_ok() || mode == OutputMode::Json {
        colored::control::set_override(false);
    }
}

#[inline]
pub fn is_rich() -> bool {
    MODE.get().copied().unwrap_or(OutputMode::Rich) == OutputMode::Rich
}

#[inline]
pub fn is_json() -> bool {
    MODE.get().copied() == Some(OutputMode::Json)
}

#[inline]
pub fn is_quiet() -> bool {
    MODE.get().copied() == Some(OutputMode::Quiet)
}

// ── Box drawing ──────────────────────────────────────────────────────────────

/// Top border with an embedded title: `╭─ Title ───────╮`
pub fn box_header(title: &str, width: usize) -> ColoredString {
    let inner = width.saturating_sub(2); // space inside the box edges
    let label = format!(" {} ", title);
    let fill_len = inner.saturating_sub(label.len() + 1); // +1 for leading ─
    let line = format!("╭─{}{}╮", label, "─".repeat(fill_len));
    line.custom_color(colors::AMBER)
}

/// Content line padded to `width`: `│ content          │`
pub fn box_line(content: &str, width: usize) -> ColoredString {
    let inner = width.saturating_sub(4); // 2 border chars + 2 padding spaces
    let visible_len = strip_ansi(content).chars().count();
    let pad = inner.saturating_sub(visible_len);
    let line = format!("│ {}{} │", content, " ".repeat(pad));
    line.custom_color(colors::AMBER)
}

/// Bottom border: `╰──────────────────╯`
pub fn box_footer(width: usize) -> ColoredString {
    let inner = width.saturating_sub(2);
    let line = format!("╰{}╯", "─".repeat(inner));
    line.custom_color(colors::AMBER)
}

// ── Time formatting ──────────────────────────────────────────────────────────

/// Convert an ISO 8601 timestamp to a human-friendly relative string.
///
/// Examples: "just now", "2m ago", "1h ago", "yesterday", "3 days ago", "May 5"
pub fn relative_time(iso_str: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso_str) else {
        return iso_str.to_string();
    };
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(dt);

    let secs = delta.num_seconds();
    if secs < 0 {
        // Future timestamp — show "in Xh", "in 2 days", etc.
        let future_secs = (-secs) as u64;
        if future_secs < 60 {
            return "in <1m".to_string();
        }
        let mins = future_secs / 60;
        if mins < 60 {
            return format!("in {}m", mins);
        }
        let hours = mins / 60;
        if hours < 24 {
            return format!("in {}h", hours);
        }
        let days = hours / 24;
        if days == 1 {
            return "in 1 day".to_string();
        }
        if days < 30 {
            return format!("in {} days", days);
        }
        return dt.format("%b %-d").to_string();
    }
    if secs < 60 {
        return "just now".to_string();
    }

    let mins = delta.num_minutes();
    if mins < 60 {
        return format!("{}m ago", mins);
    }

    let hours = delta.num_hours();
    if hours < 24 {
        return format!("{}h ago", hours);
    }

    let days = delta.num_days();
    if days == 1 {
        return "yesterday".to_string();
    }
    if days < 7 {
        return format!("{} days ago", days);
    }

    // Older than a week — show "Mon D" (e.g. "May 5").
    dt.format("%b %-d").to_string()
}

// ── File icons ───────────────────────────────────────────────────────────────

/// Return an emoji icon for a file based on its extension (or folder state).
pub fn file_icon(name: &str, is_folder: bool) -> &'static str {
    if is_folder {
        return "\u{1F4C1}"; // 📁
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        // Images
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tiff" | "heic" => {
            "\u{1F5BC}" // 🖼
        }
        // Video
        "mp4" | "mov" | "avi" | "mkv" | "webm" | "flv" | "wmv" | "m4v" => {
            "\u{1F3AC}" // 🎬
        }
        // Audio
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" => {
            "\u{1F3B5}" // 🎵
        }
        // Documents
        "pdf" | "doc" | "docx" | "txt" | "rtf" | "odt" | "md" | "pages" => {
            "\u{1F4C4}" // 📄
        }
        // Archives
        "zip" | "tar" | "gz" | "rar" | "7z" | "bz2" | "xz" | "zst" | "dmg" | "iso" => {
            "\u{1F4E6}" // 📦
        }
        // Spreadsheets
        "xls" | "xlsx" | "csv" | "ods" | "numbers" | "tsv" => {
            "\u{1F4CA}" // 📊
        }
        // Everything else
        _ => "\u{1F4CE}", // 📎
    }
}

// ── Size / speed formatting ──────────────────────────────────────────────────

/// Format a byte count as a human-readable size: "3.4 MB", "293 kB", "\u{2014}" for 0.
pub fn human_size(bytes: u64) -> String {
    if bytes == 0 {
        return "\u{2014}".to_string(); // —
    }
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    const TB: f64 = 1_000_000_000_000.0;
    let b = bytes as f64;
    if b < KB {
        format!("{} B", bytes)
    } else if b < MB {
        format!("{:.0} kB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else if b < TB {
        format!("{:.2} GB", b / GB)
    } else {
        format!("{:.2} TB", b / TB)
    }
}

/// Format bytes-per-second as a human-readable speed: "48.2 MB/s".
pub fn human_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 {
        return "\u{2014}".to_string(); // —
    }
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    if bytes_per_sec < KB {
        format!("{:.0} B/s", bytes_per_sec)
    } else if bytes_per_sec < MB {
        format!("{:.1} kB/s", bytes_per_sec / KB)
    } else if bytes_per_sec < GB {
        format!("{:.1} MB/s", bytes_per_sec / MB)
    } else {
        format!("{:.1} GB/s", bytes_per_sec / GB)
    }
}

// ── Progress bars ────────────────────────────────────────────────────────────

/// A speed bar: amber filled `━` against dim `━`.
pub fn speed_bar(value: f64, max: f64, width: usize) -> String {
    let ratio = if max > 0.0 { (value / max).clamp(0.0, 1.0) } else { 0.0 };
    let filled = (ratio * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    format!(
        "{}{}",
        "━".repeat(filled).custom_color(colors::AMBER),
        "━".repeat(empty).custom_color(colors::INK_DIM),
    )
}

/// A quota bar with color transitions: green (<50%), amber (50-80%), red (>80%).
pub fn quota_bar(used: u64, total: u64, width: usize) -> String {
    let ratio = if total > 0 {
        (used as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (ratio * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);

    let fill_color = if ratio < 0.5 {
        colors::GREEN_OK
    } else if ratio < 0.8 {
        colors::AMBER
    } else {
        colors::RED_ERR
    };

    format!(
        "{}{}",
        "━".repeat(filled).custom_color(fill_color),
        "━".repeat(empty).custom_color(colors::INK_DIM),
    )
}

// ── Speed verdict ────────────────────────────────────────────────────────────

/// Return a (label, color) pair rating a speed in Mbps.
pub fn speed_verdict(mbps: f64) -> (&'static str, colored::CustomColor) {
    if mbps > 30.0 {
        ("excellent", colors::GREEN_OK)
    } else if mbps > 10.0 {
        ("good", colors::GREEN_OK)
    } else if mbps > 3.0 {
        ("fair", colors::AMBER)
    } else {
        ("slow", colors::RED_ERR)
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Strip ANSI escape sequences from a string (for width calculations).
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
            continue;
        }
        result.push(c);
    }
    result
}
