//! Windows test support for 8.3 short names (task 2408): a scratch folder and
//! the short name the volume gave an entry.
//!
//! Windows also resolves a name through a folder's 8.3 short names
//! (`REPORT~1.PDF` for `Report for 2026.pdf`). Each test that uses
//! [`short_name`] first shows that its fixture aliases, so a pass is never a
//! check that could not fail. A volume that makes no 8.3 names skips with the
//! reason printed; with `CI` set it fails instead.

use std::path::{Path, PathBuf};

/// A private scratch directory under the system temp dir, removed on drop.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bb-short-name-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The 8.3 short name the volume gave `path`'s last component, or `None`
/// after printing why (with `CI` set, a missing short name fails instead).
pub(crate) fn short_name(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt as _;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, length: u32) -> u32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut buffer = vec![0u16; 1024];
    // SAFETY: `wide` is NUL-terminated; `buffer` holds the length passed.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) };
    assert!(
        written != 0 && (written as usize) < buffer.len(),
        "GetShortPathNameW failed for {}",
        path.display()
    );
    let short = String::from_utf16(&buffer[..written as usize]).expect("short path is UTF-16");
    let last = short.rsplit('\\').next().unwrap_or(&short).to_owned();
    let long = path.file_name().expect("a file name").to_string_lossy();
    if last != long {
        return Some(last);
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "{}: the volume made no 8.3 short name, so this test proves nothing here",
        path.display()
    );
    eprintln!("skipped: {} has no 8.3 short name", path.display());
    None
}

/// Write `contents` to `dir/long` and return the 8.3 short name that reaches
/// it, after showing that it does (reading through the short name returns
/// `contents`). `None`: the volume makes no 8.3 names (see [`short_name`]).
pub(crate) fn aliased_file(dir: &Path, long: &str, contents: &[u8]) -> Option<String> {
    let path = dir.join(long);
    std::fs::write(&path, contents).expect("write the fixture");
    let alias = short_name(&path)?;
    assert_eq!(
        std::fs::read(dir.join(&alias)).expect("read through the short name"),
        contents,
        "the fixture must alias: {alias} has to reach {long}"
    );
    Some(alias)
}

/// Create the folder `dir/long` and return the 8.3 short name that reaches it,
/// after showing that it does. `None`: the volume makes no 8.3 names.
pub(crate) fn aliased_folder(dir: &Path, long: &str) -> Option<String> {
    let path = dir.join(long);
    std::fs::create_dir_all(&path).expect("create the fixture folder");
    let alias = short_name(&path)?;
    let marker = path.join("bb-alias-probe");
    std::fs::write(&marker, b"probe").expect("write the probe");
    assert!(
        dir.join(&alias).join("bb-alias-probe").exists(),
        "the fixture must alias: {alias} has to reach {long}"
    );
    std::fs::remove_file(&marker).expect("remove the probe");
    Some(alias)
}

/// The names in `dir`, sorted.
pub(crate) fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("list the folder")
        .map(|entry| entry.expect("an entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
