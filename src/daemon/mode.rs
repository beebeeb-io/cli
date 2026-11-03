//! Which transport the detached daemon child was started for.
//!
//! `--transport api` must survive into the child (and beat an inherited
//! `BB_TRANSPORT=auto`), so the flag is carried as a value rather than a bool.

/// The transport a daemon was started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonMode {
    /// `--transport auto` (or no flag): the child decides.
    Default,
    /// `--transport api` was given explicitly.
    ApiForced,
}

impl From<bool> for DaemonMode {
    fn from(api_forced: bool) -> Self {
        if api_forced { Self::ApiForced } else { Self::Default }
    }
}

impl DaemonMode {
    /// The `BB_TRANSPORT` value the child should inherit.
    pub fn transport(self) -> &'static str {
        match self {
            Self::Default => "auto",
            Self::ApiForced => "api",
        }
    }

    /// True when the run forced the API transport.
    pub fn is_api_forced(self) -> bool {
        matches!(self, Self::ApiForced)
    }
}

#[cfg(test)]
mod tests {
    use super::DaemonMode;

    #[test]
    fn the_flag_maps_to_a_mode_and_the_mode_maps_to_bb_transport() {
        assert_eq!(DaemonMode::from(false), DaemonMode::Default);
        assert_eq!(DaemonMode::from(true), DaemonMode::ApiForced);
        assert_eq!(DaemonMode::Default.transport(), "auto");
        assert_eq!(DaemonMode::ApiForced.transport(), "api");
        assert!(!DaemonMode::Default.is_api_forced());
        assert!(DaemonMode::ApiForced.is_api_forced());
    }
}
