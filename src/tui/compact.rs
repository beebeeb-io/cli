use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::state::{FileEventStatus, SyncStatus, TuiState};

// ── Brand colors as ratatui Color::Rgb ──────────────────────────────────────

const AMBER: Color = Color::Rgb(245, 184, 0);
const GREEN_OK: Color = Color::Rgb(143, 193, 139);
const RED_ERR: Color = Color::Rgb(224, 122, 106);
const INK: Color = Color::Rgb(233, 230, 221);
const INK_DIM: Color = Color::Rgb(106, 101, 91);
const CYAN: Color = Color::Rgb(127, 184, 209);

/// Render the compact view into the given frame.
pub fn render(f: &mut Frame, state: &TuiState) {
    let area = f.area();

    // Layout: hints (1) | file log (flex) | separator (1) | status bar (1)
    let chunks = Layout::vertical([
        Constraint::Length(1), // keybinding hints
        Constraint::Min(1),    // scrolling file log
        Constraint::Length(1), // amber separator
        Constraint::Length(1), // status bar
    ])
    .split(area);

    render_hints(f, chunks[0]);
    render_file_log(f, chunks[1], state);
    render_separator(f, chunks[2]);
    render_status_bar(f, chunks[3], state);
}

/// Row 0: keybinding hints.
fn render_hints(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled("Tab", Style::default().fg(AMBER)),
        Span::styled(" dashboard", Style::default().fg(INK_DIM)),
        Span::styled(" \u{00b7} ", Style::default().fg(INK_DIM)),
        Span::styled("d", Style::default().fg(AMBER)),
        Span::styled(" daemonize", Style::default().fg(INK_DIM)),
        Span::styled(" \u{00b7} ", Style::default().fg(INK_DIM)),
        Span::styled("p", Style::default().fg(AMBER)),
        Span::styled(" pause", Style::default().fg(INK_DIM)),
        Span::styled(" \u{00b7} ", Style::default().fg(INK_DIM)),
        Span::styled("q", Style::default().fg(AMBER)),
        Span::styled(" quit", Style::default().fg(INK_DIM)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Row 1: scrolling file event log.
fn render_file_log(f: &mut Frame, area: Rect, state: &TuiState) {
    let height = area.height as usize;
    if height == 0 {
        return;
    }

    // Take the last `height` events.
    let start = state.file_log.len().saturating_sub(height);
    let visible: Vec<Line> = state
        .file_log
        .iter()
        .skip(start)
        .map(|evt| {
            let (icon, icon_color) = match evt.status {
                FileEventStatus::Uploading => ("\u{2191}", CYAN),   // arrow up
                FileEventStatus::Downloading => ("\u{2193}", CYAN), // arrow down
                FileEventStatus::Done => ("\u{2713}", GREEN_OK),    // checkmark
                FileEventStatus::Error => ("\u{2717}", RED_ERR),    // X mark
                FileEventStatus::Skipped => ("-", INK_DIM),
            };
            let size_str = format_size_binary(evt.size);
            Line::from(vec![
                Span::raw("  "),
                Span::styled(evt.timestamp.as_str(), Style::default().fg(INK_DIM)),
                Span::raw(" "),
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::raw(" "),
                Span::styled(evt.filename.as_str(), Style::default().fg(INK)),
                Span::raw(" "),
                Span::styled(format!("({})", size_str), Style::default().fg(INK_DIM)),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(visible), area);
}

/// Row 2: amber separator line.
fn render_separator(f: &mut Frame, area: Rect) {
    let width = area.width as usize;
    let line = Line::from(Span::styled("\u{2501}".repeat(width), Style::default().fg(AMBER)));
    f.render_widget(Paragraph::new(line), area);
}

/// Row 3: status bar with colored dot, session name, file count, status text.
fn render_status_bar(f: &mut Frame, area: Rect, state: &TuiState) {
    let (dot, dot_color) = match state.sync_status {
        SyncStatus::Watching => ("\u{25CF}", GREEN_OK),
        SyncStatus::Syncing => ("\u{25CF}", AMBER),
        SyncStatus::Paused => ("\u{25CF}", INK_DIM),
        SyncStatus::Error => ("\u{25CF}", RED_ERR),
    };

    let files_str = format!("{} files", state.local_file_count);
    let bytes_str = format_size_binary(state.local_total_bytes);

    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled(dot, Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(
            state.session_name.as_str(),
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" \u{00b7} ", Style::default().fg(INK_DIM)),
        Span::styled(files_str, Style::default().fg(INK_DIM)),
        Span::styled(" \u{00b7} ", Style::default().fg(INK_DIM)),
        Span::styled(bytes_str, Style::default().fg(INK_DIM)),
        Span::styled(" \u{00b7} ", Style::default().fg(INK_DIM)),
        Span::styled(state.status_text.as_str(), Style::default().fg(INK_DIM)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Format bytes using binary units (1024 base) to match the plan.
fn format_size_binary(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes == 0 {
        return "\u{2014}".to_string(); // em dash
    }
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
