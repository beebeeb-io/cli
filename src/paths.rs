//! The one place the cli's config directory is decided.
//!
//! Owns: `config_dir`, `CONFIG_DIR_OVERRIDE`. Must not: be bypassed; the source sweep below fails on any other
//! `dirs::config_dir()` call in `src/`.

use std::path::PathBuf;

/// Names the cli's config directory itself (the `beebeeb` dir, used as given). Set by the test harnesses, because
/// on Windows `dirs` asks the Known Folders API and ignores `HOME` / `XDG_CONFIG_HOME`.
pub const CONFIG_DIR_OVERRIDE: &str = "BB_CONFIG_DIR";

/// `BB_CONFIG_DIR` when set and non-empty, else the platform config dir joined with `beebeeb`.
pub fn config_dir() -> PathBuf {
    resolve(std::env::var_os(CONFIG_DIR_OVERRIDE), dirs::config_dir())
}

/// The platform's config dir (what a service would read), ignoring `BB_CONFIG_DIR`; for the service guard only.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn platform_config_dir() -> Option<PathBuf> {
    dirs::config_dir()
}

fn resolve(over: Option<std::ffi::OsString>, platform: Option<PathBuf>) -> PathBuf {
    match over {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => platform.unwrap_or_else(|| PathBuf::from(".")).join("beebeeb"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_override_is_used_as_given_and_empty_means_unset() {
        let plat = Some(PathBuf::from("/plat"));
        assert_eq!(resolve(Some("/x/y".into()), plat.clone()), Path::new("/x/y"));
        assert_eq!(resolve(Some("".into()), plat.clone()), Path::new("/plat/beebeeb"));
        assert_eq!(resolve(None, plat), Path::new("/plat/beebeeb"));
        assert_eq!(resolve(None, None), Path::new("./beebeeb"));
    }

    fn scan(dir: &Path, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                scan(&p, hits);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("paths.rs") {
                let text = std::fs::read_to_string(&p).unwrap();
                for (i, l) in text.lines().enumerate() {
                    let t = l.trim_start();
                    if !t.starts_with("//") && l.contains(concat!("dirs::config", "_dir()")) {
                        hits.push(format!("{}:{}", p.display(), i + 1));
                    }
                }
            }
        }
    }

    #[test]
    fn no_dirs_config_dir_call_outside_paths_rs() {
        let mut hits = Vec::new();
        scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut hits);
        assert!(
            hits.is_empty(),
            "{} bypass(es) of paths::config_dir(): {hits:?}",
            hits.len()
        );
    }
}
