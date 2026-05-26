use std::collections::VecDeque;

/// Maximum number of file events to keep in the scrollback buffer.
const MAX_LOG_ENTRIES: usize = 500;

/// Which view the TUI is currently displaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiView {
    Compact,
    Dashboard,
}

/// The status of a single file event in the sync log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEventStatus {
    Uploading,
    Downloading,
    Done,
    Error,
    Skipped,
}

/// A single file sync event shown in the compact view log.
#[derive(Debug, Clone)]
pub struct SyncFileEvent {
    pub status: FileEventStatus,
    pub filename: String,
    pub size: u64,
    pub timestamp: String,
}

/// Status of a remote sync session (fetched from server).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
    pub device_name: String,
    pub remote_path: String,
    pub status: String,
    pub files_synced: u64,
    pub bytes_synced: u64,
    pub last_heartbeat: Option<String>,
    pub current_file: Option<String>,
    pub speed_bps: Option<u64>,
}

/// Overall sync status for the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Watching,
    Syncing,
    Paused,
    Error,
}

/// Full TUI state, updated via events from the sync task.
pub struct TuiState {
    /// Current view.
    pub view: TuiView,
    /// Scrolling file event log (most recent at end).
    pub file_log: VecDeque<SyncFileEvent>,
    /// Remote session list (refreshed periodically).
    pub sessions: Vec<SessionInfo>,
    /// Selected session index for dashboard navigation.
    pub selected_session: usize,
    /// Number of files tracked locally.
    pub local_file_count: u64,
    /// Total bytes tracked locally.
    pub local_total_bytes: u64,
    /// Current sync status.
    pub sync_status: SyncStatus,
    /// Session name for this sync pair.
    pub session_name: String,
    /// Whether the sync is paused.
    pub paused: bool,
    /// Status text for the bottom bar.
    pub status_text: String,
}

impl TuiState {
    pub fn new(session_name: String, file_count: u64, total_bytes: u64) -> Self {
        Self {
            view: TuiView::Compact,
            file_log: VecDeque::with_capacity(MAX_LOG_ENTRIES),
            sessions: Vec::new(),
            selected_session: 0,
            local_file_count: file_count,
            local_total_bytes: total_bytes,
            sync_status: SyncStatus::Watching,
            session_name,
            paused: false,
            status_text: "watching for changes".to_string(),
        }
    }

    /// Push a file event into the log, evicting old entries if needed.
    pub fn push_event(&mut self, event: SyncFileEvent) {
        if self.file_log.len() >= MAX_LOG_ENTRIES {
            self.file_log.pop_front();
        }
        self.file_log.push_back(event);
    }
}
