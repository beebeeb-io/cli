//! Task 2420: `bb sync` never uploads the download writer's own temp files
//! (`{out}.tmp`, else `{out}.{16 hex}.tmp`), sweeps stale ones of that pattern at
//! sync start, and still syncs a user's own `.tmp` files.

use super::*;
use crate::download::AtomicFile;

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bb-sync-dl-temp-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn remote_of(paths: &[&str]) -> HashMap<String, RemoteFile> {
    paths
        .iter()
        .map(|p| {
            (
                p.to_string(),
                RemoteFile {
                    id: Uuid::new_v4(),
                    resolved: None,
                    chunk_count: 1,
                    updated_at: Utc::now(),
                    pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
                },
            )
        })
        .collect()
}

/// A download killed mid-write: the writer is never dropped or committed.
fn interrupted_download(out: &Path) {
    let mut writer = AtomicFile::create(out).unwrap();
    writer.write_all(b"partial").unwrap();
    std::mem::forget(writer);
}

fn age(path: &Path, secs: u64) {
    let when = SystemTime::now() - std::time::Duration::from_secs(secs);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn names(local: &HashMap<String, LocalFile>) -> Vec<String> {
    let mut v: Vec<String> = local.keys().cloned().collect();
    v.sort();
    v
}

#[test]
fn a_killed_downloads_temp_is_not_an_upload_candidate() {
    let root = scratch_dir("plain");
    std::fs::write(root.join("keep.txt"), b"mine").unwrap();
    interrupted_download(&root.join("report.pdf"));
    let mut local = walk_local_files(&root).unwrap();
    drop_download_temps(&mut local, &remote_of(&["report.pdf"]));
    assert_eq!(names(&local), ["keep.txt"], "{:?}", names(&local));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_random_temp_name_is_never_walked() {
    let root = scratch_dir("random");
    std::fs::write(root.join("a.txt.tmp"), b"user's own").unwrap();
    interrupted_download(&root.join("a.txt")); // `a.txt.tmp` is taken: a random name
    let local = walk_local_files(&root).unwrap();
    assert_eq!(names(&local), ["a.txt.tmp"], "{:?}", names(&local));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_users_own_tmp_files_still_sync() {
    let root = scratch_dir("user");
    std::fs::write(root.join("scratch.tmp"), b"x").unwrap();
    std::fs::write(root.join("data.1234.tmp"), b"x").unwrap();
    std::fs::create_dir_all(root.join("d")).unwrap();
    std::fs::write(root.join("d/notes.tmp"), b"x").unwrap();
    let mut local = walk_local_files(&root).unwrap();
    drop_download_temps(&mut local, &remote_of(&["other.txt"]));
    assert_eq!(names(&local), ["d/notes.tmp", "data.1234.tmp", "scratch.tmp"]);
    sweep_download_temps(&root, &remote_of(&["other.txt"]));
    assert!(root.join("scratch.tmp").exists() && root.join("d/notes.tmp").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_sweep_removes_only_stale_temps_of_our_pattern() {
    let root = scratch_dir("sweep");
    std::fs::create_dir_all(root.join("d")).unwrap();
    interrupted_download(&root.join("d/old.bin"));
    interrupted_download(&root.join("d/fresh.bin"));
    std::fs::write(root.join("user.tmp"), b"mine").unwrap();
    age(&root.join("d/old.bin.tmp"), 3600);
    age(&root.join("user.tmp"), 3600);
    sweep_download_temps(&root, &remote_of(&["d/old.bin", "d/fresh.bin"]));
    assert!(!root.join("d/old.bin.tmp").exists(), "stale own temp stays");
    assert!(
        root.join("d/fresh.bin.tmp").exists(),
        "a running download's temp was removed"
    );
    assert!(root.join("user.tmp").exists(), "a user's file was removed");
    let _ = std::fs::remove_dir_all(&root);
}
