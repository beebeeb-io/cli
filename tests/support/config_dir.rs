//! Where `bb` keeps its state under a scratch `HOME` (LD-0145): the one place the tests get that path from, instead of
//! each assuming Linux's `~/.config/beebeeb`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// `dirs::config_dir()/beebeeb` of a `bb` run with `HOME=home` and `XDG_CONFIG_HOME=home/.config` (what every harness
/// sets): `$HOME/Library/Application Support` on macOS, `$XDG_CONFIG_HOME` on other unix. (`dirs` answers `%APPDATA%`
/// on Windows; these tests are unix-only.)
pub fn config_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/beebeeb")
    } else {
        home.join(".config/beebeeb")
    }
}

/// Both places a scratch home writes its login to, so one `home` serves either platform's lookup.
pub fn login_dirs(home: &Path) -> [PathBuf; 2] {
    [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ]
}
