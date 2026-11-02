//! The typed upload failure.
//!
//! `Interrupted` is the one outcome callers must tell apart: a shutdown flag
//! stopped the upload, so the file is "remaining", not failed. Everything else
//! is a message.

/// Why an upload did not produce a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadError {
    /// The shared shutdown flag was set (Ctrl-C / daemon stop).
    Interrupted,
    /// Anything else (network, server, disk, an authorization answer).
    Other(String),
}

impl UploadError {
    /// True when the upload stopped because the caller asked it to.
    pub fn is_interrupted(&self) -> bool {
        matches!(self, Self::Interrupted)
    }
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupted => f.write_str(super::INTERRUPTED),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl From<String> for UploadError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl From<&str> for UploadError {
    fn from(message: &str) -> Self {
        Self::Other(message.to_string())
    }
}

impl From<crate::api::ApiError> for UploadError {
    fn from(e: crate::api::ApiError) -> Self {
        Self::Other(e.into())
    }
}

impl From<UploadError> for String {
    fn from(e: UploadError) -> String {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_the_message_and_interrupted_reads_as_the_sentinel() {
        assert_eq!(UploadError::Other("boom".into()).to_string(), "boom");
        assert_eq!(UploadError::Interrupted.to_string(), super::super::INTERRUPTED);
        assert!(UploadError::Interrupted.is_interrupted());
        assert!(!UploadError::Other("boom".into()).is_interrupted());
        assert_eq!(String::from(UploadError::Other("x".into())), "x");
    }
}
