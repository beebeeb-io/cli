//! Where `bb` keeps its state under a scratch `HOME` (LD-0145): the one place the tests get that path from, instead of
//! each assuming Linux's `~/.config/beebeeb`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The config dir of a `bb` run with `HOME=home`: every harness sets `BB_CONFIG_DIR=home/.config/beebeeb` (and
/// `XDG_CONFIG_HOME=home/.config`), on every platform, because `dirs` ignores HOME on Windows.
pub fn config_dir(home: &Path) -> PathBuf {
    home.join(".config/beebeeb")
}

/// Both places a scratch home writes its login to, so one `home` serves either platform's lookup.
pub fn login_dirs(home: &Path) -> [PathBuf; 2] {
    [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ]
}
