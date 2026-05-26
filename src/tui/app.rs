use std::io;
use std::time::Duration;

use crossterm::event::KeyCode;
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use super::compact;
use super::dashboard;
use super::events::{TuiAction, TuiEvent, is_ctrl_c, poll_key};
use super::state::{SyncStatus, TuiState, TuiView};

/// Run the TUI app loop.
///
/// Reads events from the given receiver (sent by the sync task) and from the
/// keyboard. Returns a `TuiAction` indicating why the loop exited.
pub fn run(
    mut rx: mpsc::UnboundedReceiver<TuiEvent>,
    session_name: String,
    file_count: u64,
    total_bytes: u64,
) -> Result<TuiAction, String> {
    // Enter raw mode + alternate screen.
    enable_raw_mode().map_err(|e| format!("enable raw mode: {e}"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| format!("alternate screen: {e}"))?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| format!("terminal init: {e}"))?;

    let mut state = TuiState::new(session_name, file_count, total_bytes);

    let result = event_loop(&mut terminal, &mut state, &mut rx);

    // Restore terminal — always, even on error.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    result
}

/// The inner event loop. Separated so cleanup always runs.
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut TuiState,
    rx: &mut mpsc::UnboundedReceiver<TuiEvent>,
) -> Result<TuiAction, String> {
    loop {
        // Draw the current view.
        terminal
            .draw(|f| match state.view {
                TuiView::Compact => compact::render(f, state),
                TuiView::Dashboard => dashboard::render(f, state),
            })
            .map_err(|e| format!("draw: {e}"))?;

        // Poll for keyboard events (non-blocking, 50ms timeout for ~20fps).
        if let Some(key) = poll_key(Duration::from_millis(50)) {
            if is_ctrl_c(&key) {
                return Ok(TuiAction::Quit);
            }
            match key.code {
                KeyCode::Char('q') => return Ok(TuiAction::Quit),
                KeyCode::Char('d') => return Ok(TuiAction::Daemonize),
                KeyCode::Char('p') => {
                    state.paused = !state.paused;
                    if state.paused {
                        state.sync_status = SyncStatus::Paused;
                        state.status_text = "paused".to_string();
                    } else {
                        state.sync_status = SyncStatus::Watching;
                        state.status_text = "watching for changes".to_string();
                    }
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    state.view = match state.view {
                        TuiView::Compact => TuiView::Dashboard,
                        TuiView::Dashboard => TuiView::Compact,
                    };
                }
                KeyCode::Up => {
                    if state.view == TuiView::Dashboard && state.selected_session > 0 {
                        state.selected_session -= 1;
                    }
                }
                KeyCode::Down => {
                    if state.view == TuiView::Dashboard {
                        let max = state.sessions.len().saturating_sub(1);
                        if state.selected_session < max {
                            state.selected_session += 1;
                        }
                    }
                }
                _ => {}
            }
        }

        // Drain all pending channel events (non-blocking).
        loop {
            match rx.try_recv() {
                Ok(event) => match event {
                    TuiEvent::SyncUpdate(file_event) => {
                        state.push_event(file_event);
                    }
                    TuiEvent::SyncStarted(_count) => {
                        state.sync_status = SyncStatus::Syncing;
                        state.status_text = "syncing".to_string();
                    }
                    TuiEvent::SyncFinished {
                        file_count,
                        total_bytes,
                    } => {
                        state.local_file_count = file_count;
                        state.local_total_bytes = total_bytes;
                        if !state.paused {
                            state.sync_status = SyncStatus::Watching;
                            state.status_text = "watching for changes".to_string();
                        }
                    }
                    TuiEvent::SessionsRefresh(sessions) => {
                        state.sessions = sessions;
                        // Clamp selection.
                        if state.selected_session >= state.sessions.len() && !state.sessions.is_empty() {
                            state.selected_session = state.sessions.len() - 1;
                        }
                    }
                    TuiEvent::SyncError(msg) => {
                        state.sync_status = SyncStatus::Error;
                        state.status_text = msg;
                    }
                    TuiEvent::Key(_) | TuiEvent::Tick => {
                        // These are handled above via poll_key; ignore if they
                        // appear in the channel.
                    }
                },
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // Sync task exited — quit the TUI.
                    return Ok(TuiAction::Quit);
                }
            }
        }
    }
}
