//! The transfer lines (task 2333, spec §6.3–§6.6): a snapshot and a terminal
//! width in, plain text out. Colour is applied afterwards (`paint*`), so it
//! never changes a width, and the tests assert plain strings.

use colored::Colorize;
use unicode_width::UnicodeWidthStr;

use super::units::{bytes, clean_name, duration, eta, fit_name, mbps, pad, speed};
use crate::transfer_stats::{ChunkRecord, Direction, FileView, Held, PoolCount, Route, Snapshot, Summary, Tally};

const SEP: &str = " \u{00B7} ";
/// The run line's bar is at most this wide, and dropped below the minimum (spec §6.6).
const MAX_BAR: usize = 30;
const MIN_BAR: usize = 10;
/// A fitted name keeps at least this many columns, however narrow the terminal.
const MIN_NAME: usize = 12;
/// The per-file end line's name column (spec §6.5).
const NAME_COLUMN: usize = 40;
/// Everything on the per-file end line besides the name: `  ✓ `, two gaps, 8 + 11 + 7 columns.
const END_LINE_FIXED: usize = 36;

/// What a line may use: one column fewer than the terminal, so it never auto-wraps.
fn budget(columns: u16) -> usize {
    (columns as usize).max(1) - 1
}

fn fits(line: &str, columns: u16) -> bool {
    line.width() <= budget(columns)
}

/// `segments` joined after `prefix`, dropping from the end until the line fits; the first stays.
fn drop_from_end(prefix: &str, mut segments: Vec<String>, columns: u16) -> String {
    while segments.len() > 1 && !fits(&format!("{prefix}{}", segments.join(SEP)), columns) {
        segments.pop();
    }
    format!("{prefix}{}", segments.join(SEP))
}

fn count(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn retries(n: u32) -> String {
    count(n, "retry", "retries")
}

fn percent(done: u64, total: u64) -> u128 {
    if total == 0 {
        100
    } else {
        done as u128 * 100 / total as u128
    }
}

fn bar(done: u64, total: u64, width: usize) -> String {
    let filled = if total == 0 {
        width
    } else {
        (done as u128 * width as u128 / total as u128).min(width as u128) as usize
    };
    format!("{}{}", "\u{2501}".repeat(filled), "\u{2500}".repeat(width - filled))
}

#[derive(PartialEq, Clone, Copy)]
enum Seg {
    Bytes,
    Files,
    Speed,
    Eta,
}

/// The run line (spec §6.3, §6.6).
pub(crate) fn run_line(s: &Snapshot, columns: u16) -> String {
    if !s.plan_final {
        let segments = vec![
            bytes(s.bytes_done),
            count(s.files_done, "file", "files"),
            speed(&s.current),
        ];
        return drop_from_end("  ", segments, columns);
    }
    let pct = percent(s.bytes_done, s.bytes_total);
    let mut segs: Vec<(Seg, String)> = vec![(
        Seg::Bytes,
        format!("{} / {}", bytes(s.bytes_done), bytes(s.bytes_total)),
    )];
    if s.files_total > 1 {
        segs.push((Seg::Files, format!("{}/{} files", s.files_done, s.files_total)));
    }
    segs.push((Seg::Speed, speed(&s.current)));
    segs.push((Seg::Eta, format!("eta {}", eta(s.eta_secs))));
    let text = |segs: &[(Seg, String)]| {
        let parts: Vec<&str> = segs.iter().map(|(_, t)| t.as_str()).collect();
        format!("{pct:>3}% {}", parts.join(SEP))
    };
    let t = text(&segs);
    let bar_width = budget(columns).saturating_sub(2 + t.width() + 1).min(MAX_BAR);
    if bar_width >= MIN_BAR {
        return format!("  {} {t}", bar(s.bytes_done, s.bytes_total, bar_width));
    }
    for drop in [Seg::Files, Seg::Speed, Seg::Eta] {
        if fits(&format!("  {}", text(&segs)), columns) {
            break;
        }
        segs.retain(|(kind, _)| *kind != drop);
    }
    if !fits(&format!("  {}", text(&segs)), columns) {
        segs[0].1 = bytes(s.bytes_done);
    }
    format!("  {}", text(&segs))
}

/// A file in flight (spec §6.3). `single`: the run has one file, so its bytes are on the run line.
pub(crate) fn file_line(f: &FileView, single: bool, columns: u16) -> String {
    if single {
        let width = budget(columns).saturating_sub(4).max(MIN_NAME);
        return format!("    {}", fit_name(&f.name, width));
    }
    let tail = format!("  {} / {}", bytes(f.bytes_done), bytes(f.bytes_total));
    let width = budget(columns).saturating_sub(4 + tail.width()).max(MIN_NAME);
    format!("    {}{tail}", fit_name(&f.name, width))
}

/// Verbose: chunks, in flight, average, the oldest chunk's age (spec §6.3).
pub(crate) fn stats_line(s: &Snapshot, columns: u16) -> String {
    let avg = s
        .avg_bytes_per_sec
        .map(crate::ui::human_speed_network)
        .unwrap_or_else(|| "\u{2014}".to_string());
    let mut segments = vec![
        format!("chunks {}/{}", s.chunks_done, s.chunks_total),
        format!("{}/{} in flight", s.in_flight, s.limit),
        format!("avg {avg}"),
    ];
    if let Some(age) = s.oldest_in_flight_ms {
        segments.push(format!("oldest {}", duration(age)));
    }
    drop_from_end("    ", segments, columns)
}

/// The pool part of a line (Guus, 2026-11-06): the storage country the client received, `pool Germany`, or
/// `pools Germany, Finland` when chunks went to several countries. Nothing when no country was received: never a
/// guess, and never the pool's id, a city, a datacenter code or a provider. The server's text is cleaned like a
/// name, so it cannot send codes to the terminal.
fn pool_segments(pools: &[PoolCount]) -> Vec<String> {
    let mut countries: Vec<String> = Vec::new();
    for country in pools.iter().filter_map(|p| p.label.country.as_deref()).map(clean_name) {
        if !countries.contains(&country) {
            countries.push(country);
        }
    }
    match countries.as_slice() {
        [] => Vec::new(),
        [one] => vec![format!("pool {one}")],
        many => vec![format!("pools {}", many.join(", "))],
    }
}

/// Verbose: the pool, the routes, the retries, the chunk size (spec §6.3).
pub(crate) fn pool_line(s: &Snapshot, columns: u16) -> String {
    let mut segments = pool_segments(&s.pools);
    segments.push(format!("direct {}", s.direct));
    segments.push(format!("api {}", s.api));
    segments.push(retries(s.retries));
    if let Some(f) = s.active.last() {
        segments.push(format!("chunk {}", bytes(f.chunk_size)));
    }
    drop_from_end("    ", segments, columns)
}

fn route(r: Route) -> &'static str {
    match r {
        Route::Direct => "direct",
        Route::Api => "api",
    }
}

/// Verbose: one confirmed chunk (spec §6.4). `multi_file`: the run has several files, so the name leads.
pub(crate) fn chunk_line(r: &ChunkRecord, multi_file: bool, columns: u16) -> String {
    let mut parts = vec![format!("chunk {}/{}", r.index + 1, r.chunk_count), bytes(r.bytes)];
    match r.held {
        Held::AlreadyStored => parts.push("already on the server".to_string()),
        Held::Transferred => {
            parts.push(route(r.route).to_string());
            parts.push(duration(r.duration_ms));
            parts.push(mbps(r.bytes, r.duration_ms));
            parts.push(retries(r.retries));
        }
    }
    let rest = parts.join(SEP);
    if !multi_file {
        return format!("    {rest}");
    }
    let width = budget(columns)
        .saturating_sub(4 + SEP.width() + rest.width())
        .max(MIN_NAME);
    format!("    {}{SEP}{rest}", fit_name(&r.file_name, width))
}

/// Verbose: a failed attempt's chunks are counted again by its retry (spec §6.4).
pub(crate) fn rollback_line(name: &str, t: Tally, d: Direction) -> String {
    let why = match d {
        Direction::Upload => "after the upload restarted",
        Direction::Download => "after the download moved to the API",
    };
    format!(
        "    {}: {} counted again {why}",
        clean_name(name),
        count(t.chunks, "chunk", "chunks")
    )
}

/// The four columns of a per-file end line, padded (spec §6.5).
fn push_file_fields(name: &str, size: u64, transferred: u64, wall_ms: u64, columns: u16) -> [String; 4] {
    let width = budget(columns)
        .saturating_sub(END_LINE_FIXED)
        .clamp(MIN_NAME, NAME_COLUMN);
    [
        pad(&fit_name(name, width), width),
        format!("{:>8}", bytes(size)),
        format!("{:>11}", mbps(transferred, wall_ms)),
        format!("{:>7}", duration(wall_ms)),
    ]
}

/// `  ✓ {name}  {size}  {speed}  {time}` (the name never touches the size, spec §6.5), each field given to `paint`
/// with its role after it was fitted, so colour never changes a width.
fn push_file_line_with(
    paint: impl Fn(&str, Role) -> String,
    name: &str,
    size: u64,
    transferred: u64,
    wall_ms: u64,
    columns: u16,
) -> String {
    let [n, s, v, t] = push_file_fields(name, size, transferred, wall_ms, columns);
    format!(
        "  {} {}  {}  {}  {}",
        paint("\u{2713}", Role::Ok),
        paint(&n, Role::Ink),
        paint(&s, Role::Dim),
        paint(&v, Role::Ok),
        paint(&t, Role::Dim)
    )
}

/// The end line in today's colours: the mark and the speed green, the name ink, the rest dim.
pub(crate) fn paint_push_file_line(name: &str, size: u64, transferred: u64, wall_ms: u64, columns: u16) -> String {
    push_file_line_with(paint, name, size, transferred, wall_ms, columns)
}

/// Tests: the same line, uncoloured.
#[cfg(test)]
pub(crate) fn push_file_line(name: &str, size: u64, transferred: u64, wall_ms: u64, columns: u16) -> String {
    push_file_line_with(|text, _| text.to_string(), name, size, transferred, wall_ms, columns)
}

/// `{n} file[s] {verb}[ · {k} {label}] · {size} · avg {avg} · {time} total`, without the mark (spec §6.5).
pub(crate) fn summary_line(verb: &str, files_ok: u32, (label, k): (&str, u32), size: u64, s: &Summary) -> String {
    let mut out = format!("{} {verb}", count(files_ok, "file", "files"));
    if k > 0 {
        out.push_str(&format!("{SEP}{k} {label}"));
    }
    out.push_str(&format!(
        "{SEP}{}{SEP}avg {}{SEP}{} total",
        bytes(size),
        mbps(s.bytes_transferred, s.duration_ms),
        duration(s.duration_ms)
    ));
    out
}

/// The verbose summary's detail line (spec §6.5).
pub(crate) fn verbose_detail(s: &Summary) -> String {
    let mut chunks = count(s.chunks_done, "chunk", "chunks");
    if s.chunks_already_stored > 0 {
        chunks.push_str(&format!(" ({} already on the server)", s.chunks_already_stored));
    }
    let mut segments = vec![chunks];
    match s.peak {
        0 => {}
        1 => segments.push("1 at a time".to_string()),
        k => segments.push(format!("up to {k} at once")),
    }
    segments.push(format!("direct {}", s.direct));
    segments.push(format!("api {}", s.api));
    segments.push(retries(s.retries));
    segments.extend(pool_segments(&s.pools));
    format!("    {}", segments.join(SEP))
}

/// Colours for the transfer lines (`crate::colors` only).
#[derive(Clone, Copy)]
pub(crate) enum Role {
    /// The filled part of a bar: brand amber, as today's bars.
    Bar,
    Dim,
    Ok,
    /// A failure mark: the CLI's error colour, never amber (spec §6.5).
    Warn,
    Ink,
}

pub(crate) fn paint(text: &str, role: Role) -> String {
    let colour = match role {
        Role::Bar => crate::colors::AMBER,
        Role::Dim => crate::colors::INK_DIM,
        Role::Ok => crate::colors::GREEN_OK,
        Role::Warn => crate::colors::RED_ERR,
        Role::Ink => crate::colors::INK,
    };
    text.custom_color(colour).to_string()
}

/// A run line with its bar painted: filled cells amber, empty cells dim, the rest as is.
pub(crate) fn paint_bar(line: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    let mut run_kind: Option<char> = None;
    let flush = |out: &mut String, run: &mut String, kind: Option<char>| {
        match kind {
            Some('\u{2501}') => out.push_str(&paint(run, Role::Bar)),
            Some('\u{2500}') => out.push_str(&paint(run, Role::Dim)),
            _ => out.push_str(run),
        }
        run.clear();
    };
    for c in line.chars() {
        let kind = matches!(c, '\u{2501}' | '\u{2500}').then_some(c);
        if kind != run_kind {
            flush(&mut out, &mut run, run_kind);
            run_kind = kind;
        }
        run.push(c);
    }
    flush(&mut out, &mut run, run_kind);
    out
}
