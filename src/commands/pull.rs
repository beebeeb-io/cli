//! `bb pull` — download one file (by id prefix or path) or a whole folder.
//!
//! Owns: the command entry point ([`run`]), argument resolution (id vs path),
//! overwrite confirmation, the folder walk and single-file download, and the
//! `--zip` archive writer.
//! Must not: decide what exists remotely (see `crate::api`) or how bytes are
//! decrypted (see `crate::download`).
//! Tests: `resolve::failure_tests` (version-change retry, ask-once) and
//! `zip::tests` (hostile names, collision suffixes, plaintext-byte progress).

use colored::Colorize;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

/// The user's pull policy, grouped so the entry point does not take a row of
/// bare booleans.
#[derive(Clone, Copy, Debug, Default)]
pub struct PullOptions {
    /// Download a folder as a zip archive.
    pub zip: bool,
    /// Overwrite an existing local file without asking.
    pub force: bool,
}

mod error;
use error::*;

mod resolve;
use resolve::*;
pub(crate) use resolve::resolve_file_arg;

mod folder;
use folder::*;

// ---------------------------------------------------------------------------
// --zip: download folder as a zip archive
// ---------------------------------------------------------------------------

mod zip;
use zip::*;

mod run;
pub use run::run;

#[cfg(test)]
mod options_tests {
    use super::PullOptions;

    /// The defaults match `bb pull <file>`: write in place, ask before overwrite.
    #[test]
    fn default_pull_is_in_place_and_asks_before_overwrite() {
        let o = PullOptions::default();
        assert!(!o.zip);
        assert!(!o.force);
    }

    /// `--zip` and `--force` are independent flags.
    #[test]
    fn zip_and_force_are_independent() {
        assert!(PullOptions { zip: true, force: false }.zip);
        assert!(PullOptions { zip: false, force: true }.force);
        assert!(PullOptions { zip: true, force: true }.force);
    }
}
