//! Which machine this is, and which local folders it never backs up.

use std::path::{Path, PathBuf};

use beebeeb_types::backup::Category;

/// Test seams (like `BB_PENDING_UPLOADS_PATH`): the files the machine id and
/// the pretty hostname are read from.
const MACHINE_ID_ENV: &str = "BB_BACKUP_MACHINE_ID_FILE";
const MACHINE_INFO_ENV: &str = "BB_BACKUP_MACHINE_INFO_FILE";

/// The machine id files, in order (systemd's, then D-Bus's older copy).
const MACHINE_ID_FILES: [&str; 2] = ["/etc/machine-id", "/var/lib/dbus/machine-id"];

/// This machine's backup device id: core's `device_id_from_machine_id` of the
/// OS's stable machine id. The raw id never leaves this function.
pub(crate) fn device_id() -> Result<String, String> {
    let files: Vec<PathBuf> = match std::env::var_os(MACHINE_ID_ENV) {
        Some(p) => vec![PathBuf::from(p)],
        None if cfg!(target_os = "linux") => MACHINE_ID_FILES.iter().map(PathBuf::from).collect(),
        None => {
            return Err(
                "`bb backup` runs on Linux: it needs the machine id in /etc/machine-id. On Windows and \
                        macOS the Beebeeb desktop app backs up folders."
                    .to_string(),
            );
        }
    };
    for file in &files {
        if let Ok(text) = std::fs::read_to_string(file)
            && !text.trim().is_empty()
        {
            return Ok(beebeeb_core::backup::device_id_from_machine_id(&text));
        }
    }
    Err(
        "this machine has no machine id (/etc/machine-id is missing or empty); create one with \
         `systemd-machine-id-setup`"
            .to_string(),
    )
}

/// The machine's user-facing name: `PRETTY_HOSTNAME` from `/etc/machine-info`,
/// else the hostname (spec §4.2 rule 2). Core makes it a folder name.
pub(crate) fn device_name() -> String {
    let info = std::env::var_os(MACHINE_INFO_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/machine-info"));
    if let Some(pretty) = std::fs::read_to_string(info).ok().and_then(|t| pretty_hostname(&t)) {
        return pretty;
    }
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `PRETTY_HOSTNAME=` of a machine-info file, unquoted, if set and not empty.
fn pretty_hostname(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.trim().strip_prefix("PRETTY_HOSTNAME=")?.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// The marker a `bb sync` folder holds at its top.
const SYNC_MARKER: &str = ".bb-sync.json";

/// The CLI's own folders on this machine, which a backup never enters: its
/// state dir (the session and key live there) and every Beebeeb FUSE mount
/// (`bb mount`; its files are the vault itself).
pub(crate) fn excluded_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![canonical_or_same(&super::state::state_dir())];
    if let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") {
        dirs.extend(beebeeb_mounts(&mounts).into_iter().map(|m| canonical_or_same(&m)));
    }
    dirs
}

/// Mount points of `bb mount` in a `/proc/self/mounts` text (its fsname is
/// `beebeeb`, its type `fuse.beebeeb`).
fn beebeeb_mounts(mounts: &str) -> Vec<PathBuf> {
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (source, target, fstype) = (fields.next()?, fields.next()?, fields.next()?);
            (source == "beebeeb" || fstype == "fuse.beebeeb").then(|| unescape_mount_field(target))
        })
        .collect()
}

/// A `/proc/self/mounts` field with the kernel's octal escapes (`\040` space, `\011` tab, `\012` newline,
/// `\134` backslash) turned back into their bytes.
fn unescape_mount_field(field: &str) -> PathBuf {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'\\')
            .then(|| bytes.get(i + 1..i + 4))
            .flatten()
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

fn canonical_or_same(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Refuse a folder to back up that is inside one of the CLI's own folders:
/// its state dir, a `bb mount`, or a `bb sync` folder (the folder itself or
/// any folder above it holds `.bb-sync.json`). Those are copies of the vault
/// already, and the state dir holds the session.
pub(crate) fn check_source(source: &Path, excluded: &[PathBuf]) -> Result<(), String> {
    if let Some(dir) = excluded.iter().find(|d| source.starts_with(d)) {
        return Err(format!(
            "{} is inside {}, one of bb's own folders (its settings or a `bb mount`). Back up a folder outside it.",
            source.display(),
            dir.display()
        ));
    }
    if let Some(sync) = source.ancestors().find(|a| a.join(SYNC_MARKER).is_file()) {
        return Err(format!(
            "{} is inside {}, a `bb sync` folder: its files are already in your vault. Back up a folder outside it.",
            source.display(),
            sync.display()
        ));
    }
    Ok(())
}

/// The category a folder is backed up as: the fixed one when it is that XDG
/// user folder (Pictures, Documents, ...), else a custom one named by core
/// from the folder's own name, never equal to `taken` (the device's other
/// custom names).
pub(crate) fn category_for(source: &Path, taken: &[&str]) -> Category {
    let fixed = [
        (dirs::desktop_dir(), Category::Desktop),
        (dirs::document_dir(), Category::Documents),
        (dirs::picture_dir(), Category::Pictures),
        (dirs::audio_dir(), Category::Music),
        (dirs::video_dir(), Category::Videos),
        (dirs::download_dir(), Category::Downloads),
    ];
    let home = dirs::home_dir().map(|h| canonical_or_same(&h));
    for (dir, category) in fixed {
        // An XDG dir that is the home folder itself (a box without user dirs)
        // is no category.
        if let Some(dir) = dir.map(|d| canonical_or_same(&d))
            && Some(&dir) != home.as_ref()
            && dir == source
        {
            return category;
        }
    }
    let raw = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Category::Custom(beebeeb_core::backup::custom_category_name(&raw, taken))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pretty_hostname_wins_and_is_unquoted() {
        assert_eq!(
            pretty_hostname("PRETTY_HOSTNAME=\"Guus's NAS\"\nCHASSIS=server\n").as_deref(),
            Some("Guus's NAS")
        );
        assert_eq!(pretty_hostname("PRETTY_HOSTNAME=nas01").as_deref(), Some("nas01"));
        assert_eq!(pretty_hostname("PRETTY_HOSTNAME=\"\"\n"), None);
        assert_eq!(pretty_hostname("CHASSIS=server\n"), None);
    }

    #[test]
    fn only_beebeeb_mounts_are_excluded() {
        let mounts = "beebeeb /home/g/Vault\\040Drive fuse.beebeeb rw 0 0\n/dev/sda1 / ext4 rw 0 0\n";
        assert_eq!(beebeeb_mounts(mounts), vec![PathBuf::from("/home/g/Vault Drive")]);
        let odd = "beebeeb /home/g/a\\011b\\134c\\012d fuse.beebeeb rw 0 0\n";
        assert_eq!(beebeeb_mounts(odd), vec![PathBuf::from("/home/g/a\tb\\c\nd")]);
    }

    #[test]
    fn a_folder_inside_an_excluded_one_or_a_sync_folder_is_refused() {
        let root = std::env::temp_dir().join(format!("cli-backup-excl-{}", uuid::Uuid::new_v4()));
        let synced = root.join("synced/photos");
        let plain = root.join("plain");
        std::fs::create_dir_all(&synced).unwrap();
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(root.join("synced").join(SYNC_MARKER), b"{}").unwrap();
        let state = root.join("state");
        assert!(check_source(&plain, std::slice::from_ref(&state)).is_ok());
        let err = check_source(&state.join("x"), std::slice::from_ref(&state)).unwrap_err();
        assert!(err.contains("one of bb's own folders"), "{err}");
        let err = check_source(&synced, &[]).unwrap_err();
        assert!(err.contains("a `bb sync` folder"), "{err}");
    }
}
