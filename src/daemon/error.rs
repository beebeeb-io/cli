//! The typed daemon failure.
//!
//! Every message is carried verbatim; the two classes callers could ever need
//! to tell apart are a guard refusal (the run would touch the wrong home or
//! config) and everything else.

/// Why a daemon operation did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonError {
    /// The real-home / config guard refused to touch the service manager.
    Refused(String),
    /// Anything else (I/O, a service-manager command that failed).
    Other(String),
}

impl DaemonError {
    /// True when the message contains `pat` (tests and callers that only have a message).
    pub fn contains(&self, pat: &str) -> bool {
        self.to_string().contains(pat)
    }
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) | Self::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for DaemonError {}

impl From<String> for DaemonError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl From<&str> for DaemonError {
    fn from(message: &str) -> Self {
        Self::Other(message.to_string())
    }
}

impl From<std::io::Error> for DaemonError {
    fn from(e: std::io::Error) -> Self {
        Self::Other(e.to_string())
    }
}

impl From<DaemonError> for String {
    fn from(e: DaemonError) -> String {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::DaemonError;

    #[test]
    fn every_variant_carries_its_message_to_the_string_exit_path() {
        for e in [
            DaemonError::Refused("not this home".to_string()),
            DaemonError::Other("launchctl failed".to_string()),
        ] {
            let shown = e.to_string();
            assert_eq!(String::from(e), shown);
            assert!(shown == "not this home" || shown == "launchctl failed");
        }
    }

    #[test]
    fn contains_sees_the_message() {
        assert!(DaemonError::Refused("refusing to touch".to_string()).contains("refusing"));
        assert!(!DaemonError::Other("boom".to_string()).contains("refusing"));
    }
}
