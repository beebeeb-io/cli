//! `scan` items split out of `sync.rs`.

use super::*;

pub(super) fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

pub(super) fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
    let config = load_config();
    let mk_b64 = config.master_key.ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(
        b64()
            .decode(&mk_b64)
            .map_err(|e| format!("invalid master key in config: {e}"))?,
    );
    if mk_bytes.len() != 32 {
        return Err(format!("master key must be 32 bytes, got {}", mk_bytes.len()));
    }
    crate::crypto::master_key_from_bytes(&mk_bytes)
}

pub(super) fn rel_path_str(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .collect::<Vec<_>>()
        .join("/")
}

/// Build an amber-accented scan spinner with a live `{pos}`-driven counter,
/// or `None` when progress is suppressed (non-TTY / json / quiet). Draws to
/// stderr so it never corrupts stdout output.
pub(super) fn make_scan_spinner(active: bool, label: &str, unit: &str) -> Option<indicatif::ProgressBar> {
    if !active {
        return None;
    }
    let pb = indicatif::ProgressBar::new_spinner();
    let template = format!("  {{spinner:.214}} {label} · {{pos}} {unit}");
    pb.set_style(
        indicatif::ProgressStyle::with_template(&template)
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner()),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    Some(pb)
}

/// Walk the local tree like [`walk_local_files`], but with two scan-phase
/// optimisations: a live spinner counter, and skip-hashing.
///
/// Skip-hashing: hashing every file with SHA-256 is the bulk of the scan cost.
/// When `rehash` is false and a file's `(mtime, size)` match the prior sync
/// state, the stored `content_hash` is reused and the read+hash is skipped.
/// `--rehash` forces a full hash of every file (catches same-size edits).
pub(super) fn scan_local_files(
    root: &Path,
    prior: &HashMap<String, FileEntry>,
    rehash: bool,
    scan: Option<&indicatif::ProgressBar>,
) -> Result<HashMap<String, LocalFile>, String> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size = meta.len();
        let rel_str = rel_path_str(rel);

        // Reuse the stored hash when unchanged (unless --rehash forces a read).
        let reused = if rehash {
            None
        } else {
            prior.get(&rel_str).and_then(|p| {
                if p.last_mtime == mtime && p.last_size == size {
                    p.content_hash.clone()
                } else {
                    None
                }
            })
        };
        let content_hash = match reused {
            Some(h) => h,
            None => compute_file_hash(path)?,
        };

        if let Some(pb) = scan {
            pb.inc(1);
        }
        out.insert(
            rel_str,
            LocalFile {
                mtime,
                size,
                content_hash,
            },
        );
    }
    Ok(out)
}

pub(super) fn walk_local_files(root: &Path) -> Result<HashMap<String, LocalFile>, String> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let content_hash = compute_file_hash(path)?;
        out.insert(
            rel_path_str(rel),
            LocalFile {
                mtime,
                size: meta.len(),
                content_hash,
            },
        );
    }
    Ok(out)
}

pub(super) fn walk_local_folders(root: &Path) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() || path == root {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        out.insert(rel_path_str(rel));
    }
    Ok(out)
}

pub(super) fn remote_version(remote: &RemoteFile) -> Option<i64> {
    remote
        .resolved
        .as_ref()
        .filter(|r| !crate::owned_keys::is_derived_origin(r.origin))
        .and_then(|r| crate::api::current_version_number(&r.canonical))
}

pub(super) fn remote_changed(remote: &RemoteFile, prior: &FileEntry) -> bool {
    if remote.id != prior.remote_id {
        return true;
    }
    match (remote_version(remote), prior.remote_version) {
        (Some(current), Some(previous)) => current > previous,
        _ => remote.updated_at > prior.last_sync + chrono::Duration::seconds(1),
    }
}

pub(super) fn replacement_version(row: &serde_json::Value) -> Result<i32, String> {
    crate::api::current_version_number(row)
        .and_then(|v| i32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| "missing or invalid replacement version".into())
}

pub(super) fn listing_healthy(health: &crate::sync_safety::ListingHealth) -> bool {
    health.unreadable == 0 && !health.truncated
}

pub(super) fn foreign_missing(state: &SyncState, rel: &str, remote: &HashMap<String, RemoteFile>) -> bool {
    !remote.contains_key(rel)
        && state
            .files
            .get(rel)
            .is_some_and(|p| state.owned_keys.contains_key(&p.remote_id.to_string()))
}

/// Whether `rel` below `root` reaches a DIFFERENT existing entry through
/// another of its names: on Windows an 8.3 short name, `REPORT~1.PDF` for
/// `Report for 2026.pdf` (task 2408). Renaming from or into such a path would
/// act on that other entry, so it counts as occupied. `Err`: the check could
/// not finish, or `rel` is not a relative path of plain names.
fn reaches_another_entry(root: &Path, rel: &str) -> std::io::Result<bool> {
    crate::safe_path::short_name_alias(root, Path::new(rel)).map(|conflict| conflict.is_some())
}

/// Follow identity across remote moves. Only an unchanged local mirror may be
/// renamed; dirty paths, occupied destinations and paths that reach a
/// different entry through an 8.3 short name are held for conflict handling.
pub(super) fn reconcile_moves(
    root: &Path,
    state: &mut SyncState,
    local: &HashMap<String, LocalFile>,
    remote: &HashMap<String, RemoteFile>,
    dry_run: bool,
) -> Result<HashSet<String>, String> {
    let mut blocked = HashSet::new();
    for (new, r) in remote {
        let old = state
            .files
            .iter()
            .find(|(path, e)| *path != new && e.remote_id == r.id)
            .map(|(p, e)| (p.clone(), e.clone()));
        let Some((old, entry)) = old else {
            continue;
        };
        if local.contains_key(new)
            || local.get(&old).is_some_and(|l| !local_matches_entry(l, &entry))
            // A path the check refuses is refused by `safe_rel_join` below, as before.
            || reaches_another_entry(root, &old).unwrap_or(false)
            || reaches_another_entry(root, new).unwrap_or(false)
        {
            blocked.insert(old);
            blocked.insert(new.clone());
            continue;
        }
        if dry_run {
            continue;
        }
        let from =
            crate::safe_path::safe_rel_join(root, &old, crate::safe_path::Rules::Host).map_err(|e| e.to_string())?;
        let to =
            crate::safe_path::safe_rel_join(root, new, crate::safe_path::Rules::Host).map_err(|e| e.to_string())?;
        if from.exists() {
            // Recheck bytes immediately before a local move, as for tombstones.
            if compute_file_hash(&from).ok().as_ref() != entry.content_hash.as_ref() {
                blocked.insert(old);
                blocked.insert(new.clone());
                continue;
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // Never overwrite a destination which appeared after the scan, nor
            // move into a folder that now reaches another one (checked again
            // here: hashing a large file takes a while; a check that cannot
            // finish holds the move).
            if to.exists() || reaches_another_entry(root, new).unwrap_or(true) {
                blocked.insert(old);
                blocked.insert(new.clone());
                continue;
            }
            std::fs::rename(from, to).map_err(|e| e.to_string())?;
        }
        state.files.remove(&old);
        state.files.insert(new.clone(), entry);
    }
    Ok(blocked)
}

/// Apply incoming files through the same downloader during an existing watch
/// resync. Local edits keep precedence until the existing conflict flow resolves.
pub(super) async fn refresh_watch_reads(
    api: &ApiClient,
    master: &beebeeb_core::kdf::MasterKey,
    root: &Path,
    state: &mut SyncState,
    local: &mut HashMap<String, LocalFile>,
    remote: &HashMap<String, RemoteFile>,
    remote_root: Uuid,
) -> Result<HashSet<String>, String> {
    if root_unreconciled(state, remote_root) {
        return Ok(state.files.keys().cloned().collect());
    }
    let mut blocked = reconcile_moves(root, state, local, remote, false)?;
    *local = walk_local_files(root)?;
    for (rel, r) in remote {
        if blocked.contains(rel) {
            continue;
        }
        let prior = state.files.get(rel);
        let changed = prior.is_none_or(|p| remote_changed(r, p));
        let dirty = local
            .get(rel)
            .is_some_and(|l| prior.is_none_or(|p| local_changed_vs_manifest(l, p)));
        if changed && dirty {
            blocked.insert(rel.clone());
            continue;
        }
        if changed || !local.contains_key(rel) && prior.is_none() {
            if let Err(e) = do_download(api, master, root, rel, r, state, false, None).await {
                print_notice(&format!("{rel}: {e}"));
            }
        }
    }
    *local = walk_local_files(root)?;
    Ok(blocked)
}
