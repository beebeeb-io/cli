//! Public command routing. Network/update fixtures live in update.rs; these
//! invoke the actual CLI without touching the user's profile or executable.
use std::process::{Command, Stdio};

#[test]
fn update_help_is_available_without_authentication() {
    let output = Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(["update", "--help"])
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env(
            "HOME",
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("update-command-no-profile"),
        )
        .env(
            "XDG_CONFIG_HOME",
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("update-command-no-config"),
        )
        .env("PATH", "")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Update bb to the latest release"));
}

#[test]
#[cfg(debug_assertions)]
fn explicit_update_is_not_silently_disabled_by_auto_update_opt_out() {
    let output = Command::new(env!("CARGO_BIN_EXE_bb"))
        .arg("update")
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env(
            "HOME",
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("update-command-no-profile"),
        )
        .env(
            "XDG_CONFIG_HOME",
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("update-command-no-config"),
        )
        .env("PATH", "")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("development builds cannot self-update"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
