//! Why a pull failed and what the retry loop may do about it: only a version
//! change is retried, everything else is reported as-is.
//!
//! Owns: `PullFailure` and its conversions from `String`, `&str` and
//! `crate::download::DownloadError`, plus the `String` unwrap the CLI prints.
//! Must not: decide when to retry (see `pull::run`).
//! Tests: `pull::resolve::failure_tests`.

/// Why one attempt at `bb pull` ended without a file. Only a version change is a decision: the
/// file is read again from fresh metadata, once.
pub(super) enum PullFailure {
    /// The file's version changed while it was downloading (nothing was written).
    Changed(String),
    Other(String),
}

impl From<String> for PullFailure {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl From<&str> for PullFailure {
    fn from(message: &str) -> Self {
        Self::Other(message.to_string())
    }
}

impl From<crate::api::ApiError> for PullFailure {
    fn from(e: crate::api::ApiError) -> Self {
        Self::Other(e.into())
    }
}

impl From<crate::download::DownloadError> for PullFailure {
    fn from(e: crate::download::DownloadError) -> Self {
        match e {
            crate::download::DownloadError::Changed(m) => Self::Changed(m),
            other => Self::Other(other.into()),
        }
    }
}

impl From<PullFailure> for String {
    fn from(e: PullFailure) -> String {
        match e {
            PullFailure::Changed(m) | PullFailure::Other(m) => m,
        }
    }
}
