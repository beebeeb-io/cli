//! The typed download failure and the stats a caller reports.

/// Why a streaming download did not produce a file. Nearly every failure is just a message; the two
/// that callers must tell apart are typed, because a daemon that matched message text once
/// crash-looped on a message nobody had listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// The bytes could not be authenticated: this client cannot read the file right now. The
    /// remote copy is untouched, and a later pass may read it (task 1916).
    Unreadable(String),
    /// The file's version changed while it was downloading; nothing was written. Only fresh
    /// metadata (chunk count, read pin) can say what the file is now.
    Changed(String),
    /// Anything else (network, server, disk, an authorization answer).
    Other(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Unreadable(m) | Self::Changed(m) | Self::Other(m)) = self;
        f.write_str(m)
    }
}

impl From<String> for DownloadError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl From<crate::api::ApiError> for DownloadError {
    fn from(e: crate::api::ApiError) -> Self {
        Self::Other(e.into())
    }
}

impl From<DownloadError> for String {
    fn from(e: DownloadError) -> String {
        e.to_string()
    }
}

/// Outcome of a streaming download.
pub struct DownloadStats {
    pub plaintext_bytes: u64,
    pub encrypted_bytes: u64,
    /// `"api"` (the file came through the API), `"direct"` (every chunk straight
    /// from object storage through presigned links, task 2173) or `"mixed"`
    /// (direct, with some chunks through the pinned per-chunk API route).
    pub transport: &'static str,
}
