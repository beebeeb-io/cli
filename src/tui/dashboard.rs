use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::state::TuiState;

// ── Brand colors as ratatui Color::Rgb ──────────────────────────────────────

const AMBER: Color = Color::Rgb(245, 184, 0);
const GREEN_OK: Color = Color::Rgb(143, 193, 139);
const RED_ERR: Color = Color::Rgb(224, 122, 106);
const INK: Color = Color::Rgb(233, 230, 221);
const INK_DIM: Color = Color::Rgb(106, 101, 91);

/// Render the full-screen dashboard view.
pub fn render(f: &mut Frame, state: &TuiState) {
    let area = f.area();

    // Layout: session list (flex) | hints bar (1)
    let chunks = Layout::vertical([
        Constraint::Min(1),    // session list with border
        Constraint::Length(1), // bottom hints
    ])
    .split(area);

    render_sessions(f, chunks[0], state);
    render_hints(f, chunks[1]);
}

/// Session list with amber border and BEEBEEB SYNC title.
fn render_sessions(f: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            " BEEBEEB SYNC ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if state.sessions.is_empty() {
        let msg = Line::from(vec![
            Span::raw("  "),
            Span::styled("No active sync sessions", Style::default().fg(INK_DIM)),
        ]);
        f.render_widget(Paragraph::new(msg), inner);
        return;
    }

    // Each session takes 2 lines, plus a blank line between sessions.
    let mut lines: Vec<Line> = Vec::new();
    for (i, session) in state.sessions.iter().enumerate() {
        let is_selected = i == state.selected_session;
        let selector = if is_selected { "\u{25b6}" } else { " " };

        let (dot, dot_color) = match session.status.as_str() {
            "watching" | "idle" => ("\u{25CF}", GREEN_OK),
            "syncing" | "uploading" | "downloading" => ("\u{25C6}", AMBER),
            "error" => ("\u{25CF}", RED_ERR),
            "stopped" => ("\u{25CB}", INK_DIM),
            _ => ("\u{25CF}", INK_DIM),
        };

        let status_color = dot_color;

        // Line 1: selector + dot + name + "hostname -> remote_path" dim + status
        let mut line1_spans = vec![
            Span::raw(" "),
            Span::styled(selector, Style::default().fg(if is_selected { AMBER } else { INK_DIM })),
            Span::raw(" "),
            Span::styled(dot, Style::default().fg(dot_color)),
            Span::raw(" "),
            Span::styled(
                session.name.as_str(),
                Style::default()
                    .fg(INK)
                    .add_modifier(if is_selected { Modifier::BOLD } else { Modifier::empty() }),
            ),
        ];

        if !session.device_name.is_empty() {
            line1_spans.push(Span::styled("  ", Style::default()));
            line1_spans.push(Span::styled(session.device_name.as_str(), Style::default().fg(INK_DIM)));
            line1_spans.push(Span::styled(" \u{2192} ", Style::default().fg(INK_DIM)));
            line1_spans.push(Span::styled(session.remote_path.as_str(), Style::default().fg(INK_DIM)));
        }

        line1_spans.push(Span::styled("  ", Style::default()));
        line1_spans.push(Span::styled(session.status.as_str(), Style::default().fg(status_color)));

        lines.push(Line::from(line1_spans));

        // Line 2: file count + last heartbeat
        let mut detail_parts: Vec<String> = Vec::new();
        if session.files_synced > 0 {
            detail_parts.push(format!("{} files", session.files_synced));
        }
        if session.bytes_synced > 0 {
            detail_parts.push(format_size_binary(session.bytes_synced));
        }
        if let Some(ref hb) = session.last_heartbeat {
            detail_parts.push(format!("heartbeat {hb}"));
        }

        if let Some(ref file) = session.current_file {
            let speed_str = session
                .speed_bps
                .map(|s| format!(" \u{00b7} {}/s", format_size_binary(s)))
                .unwrap_or_default();
            detail_parts.push(format!("uploading {file}{speed_str}"));
        }

        let detail_str = if detail_parts.is_empty() {
            "\u{2014}".to_string() // em dash
        } else {
            detail_parts.join(" \u{00b7} ")
        };

        lines.push(Line::from(vec![
            Span::raw("       "),
            Span::styled(detail_str, Style::default().fg(INK_DIM)),
        ]));

        // Blank line between sessions (except after last).
        if i + 1 < state.sessions.len() {
            lines.push(Line::raw(""));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// Bottom hints bar.
fn render_hints(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled("Tab", Style::default().fg(AMBER)),
        Span::styled(" compact", Style::default().fg(INK_DIM)),
        Span::styled(" \u{00b7} ", Style::default().fg(INK_DIM)),
        Span::styled("\u{2191}\u{2193}", Style::default().fg(AMBER)),
        Span::styled(" navigate", Style::default().fg(INK_DIM)),
        Span::styled(" \u{00b7} ", Style::default().fg(INK_DIM)),
        Span::styled("q", Style::default().fg(AMBER)),
        Span::styled(" quit", Style::default().fg(INK_DIM)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Format bytes using binary units (1024 base).
fn format_size_binary(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes == 0 {
        return "\u{2014}".to_string();
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
