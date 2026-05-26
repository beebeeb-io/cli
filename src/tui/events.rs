use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

use super::state::{SessionInfo, SyncFileEvent};

/// Events consumed by the TUI app loop.
pub enum TuiEvent {
    /// A keyboard/mouse event from crossterm.
    Key(KeyEvent),
    /// Timer tick for redraw.
    Tick,
    /// A file sync event from the watcher.
    SyncUpdate(SyncFileEvent),
    /// Batch update: sync started (count of files being synced).
    SyncStarted(u32),
    /// Batch update: sync finished, new totals.
    SyncFinished { file_count: u64, total_bytes: u64 },
    /// Refreshed session list from the server.
    SessionsRefresh(Vec<SessionInfo>),
    /// An error message from the sync task.
    SyncError(String),
}

/// Action returned from the TUI to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiAction {
    Quit,
    Daemonize,
}

/// Poll for a crossterm key event with a timeout.
/// Returns `Some(KeyEvent)` if a key was pressed, `None` on timeout.
pub fn poll_key(timeout: Duration) -> Option<KeyEvent> {
    if event::poll(timeout).ok()? {
        if let Event::Key(key) = event::read().ok()? {
            // Ignore key release events on platforms that send them.
            if key.kind == crossterm::event::KeyEventKind::Press {
                return Some(key);
            }
        }
    }
    None
}

/// Check if a key event is Ctrl+C.
pub fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}
