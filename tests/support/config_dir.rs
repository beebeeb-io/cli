//! Where `bb` keeps its state under a scratch `HOME` (LD-0145): the one place the tests that name a state file or
//! folder get that path from, instead of each assuming Linux's `~/.config/beebeeb`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// `dirs::config_dir()/beebeeb` of a `bb` run with `HOME=home` and `XDG_CONFIG_HOME=home/.config` (what every
/// harness sets): `$HOME/Library/Application Support` on macOS, `$XDG_CONFIG_HOME` elsewhere. The scratch homes
/// write the login to both, so only a test that names a state file or folder needs this.
pub fn config_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/beebeeb")
    } else {
        home.join(".config/beebeeb")
    }
}
