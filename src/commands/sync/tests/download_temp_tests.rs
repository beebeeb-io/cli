//! Task 2420: `bb sync` excludes exactly the temp files the download writers
//! recorded, sweeps a recorded temp only once its writer is gone, and syncs a
//! user's own `.tmp` / `.bbtmp` files like any other.

use super::*;
use crate::download::AtomicFile;

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bb-sync-dl-temp-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A download whose process was killed mid-write: the temp stays on disk and
/// the writer is gone.
fn killed_download(out: &Path) {
    let mut writer = AtomicFile::create(out).unwrap();
    writer.write_all(b"partial").unwrap();
    writer.simulate_kill();
}

/// A download still running: the writer is held by the caller.
fn running_download(out: &Path) -> AtomicFile {
    let mut writer = AtomicFile::create(out).unwrap();
    writer.write_all(b"partial").unwrap();
    writer
}

fn sweep(_root: &Path) {
    crate::temp_record::sweep();
}

/// Each test gets its own record directory (the override is per thread).
fn own_record_dir(tag: &str) -> PathBuf {
    let dir = scratch_dir(&format!("record-{tag}"));
    crate::temp_record::DIR_OVERRIDE.with(|d| *d.borrow_mut() = Some(dir.clone()));
    dir
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

const USER_SHAPES: [&str; 3] = ["foo.tmp", "x.0123456789abcdef.tmp", "y.bbtmp"];

#[test]
fn a_killed_pulls_plain_tmp_is_excluded_then_swept() {
    let root = scratch_dir("plain");
    let record = own_record_dir("plain");
    killed_download(&root.join("big.bin"));
    assert!(root.join("big.bin.tmp").exists());
    let local = walk_local_files(&root).unwrap();
    assert!(
        names(&local).is_empty(),
        "the leftover temp would upload: {:?}",
        names(&local)
    );
    sweep(&root);
    assert!(!root.join("big.bin.tmp").exists(), "the dead writer's temp stays");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&record);
}

#[test]
fn a_users_own_temp_shaped_files_sync_and_survive() {
    let root = scratch_dir("user");
    let record = own_record_dir("user");
    for n in USER_SHAPES {
        std::fs::write(root.join(n), b"mine").unwrap();
        age(&root.join(n), 3600);
    }
    let local = walk_local_files(&root).unwrap();
    assert_eq!(names(&local), ["foo.tmp", "x.0123456789abcdef.tmp", "y.bbtmp"]);
    sweep(&root);
    for n in USER_SHAPES {
        assert!(root.join(n).exists(), "{n} was deleted");
    }
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&record);
}

#[test]
fn a_known_file_of_a_temp_shape_is_not_seen_as_deleted() {
    let root = scratch_dir("known");
    let record = own_record_dir("known");
    for n in USER_SHAPES {
        std::fs::write(root.join(n), b"mine").unwrap();
    }
    let local = walk_local_files(&root).unwrap();
    for n in USER_SHAPES {
        assert!(
            local.contains_key(n),
            "{n} is missing from the local view: the planner reads it as deleted"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&record);
}

#[test]
fn a_running_writers_temp_is_not_swept_even_when_old() {
    let root = scratch_dir("alive");
    let record = own_record_dir("alive");
    std::fs::write(root.join("a.bin.tmp"), b"user's").unwrap(); // forces the random form
    let writer = running_download(&root.join("a.bin"));
    let temp: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap() != "a.bin.tmp")
        .collect();
    assert_eq!(temp.len(), 1, "{temp:?}");
    age(&temp[0], 7200);
    sweep(&root);
    assert!(temp[0].exists(), "a live writer's temp was removed");
    assert!(root.join("a.bin.tmp").exists());
    assert!(names(&walk_local_files(&root).unwrap()) == ["a.bin.tmp"]);
    drop(writer);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&record);
}

#[test]
fn a_corrupt_or_missing_record_means_no_exclusions_and_no_deletions() {
    let root = scratch_dir("corrupt");
    let record = own_record_dir("corrupt");
    for n in USER_SHAPES {
        std::fs::write(root.join(n), b"mine").unwrap();
    }
    std::fs::write(root.join("big.bin.tmp"), b"unrecorded").unwrap();
    // Garbage entries, one of them naming a user's file only by accident of a bad shape.
    std::fs::write(record.join("a.json"), b"{not json").unwrap();
    std::fs::write(record.join("b.json"), b"").unwrap();
    std::fs::write(record.join("c.json"), br#"{"v":1,"path":"relative/foo.tmp"}"#).unwrap();
    std::fs::write(record.join("d.json"), br#"{"v":7,"path":"/etc/passwd"}"#).unwrap();
    assert_eq!(walk_local_files(&root).unwrap().len(), 4);
    sweep(&root);
    for n in USER_SHAPES.iter().chain(&["big.bin.tmp"]) {
        assert!(root.join(n).exists(), "{n} was deleted on a corrupt record");
    }
    // A record directory that is not a directory, and one that does not exist.
    std::fs::remove_dir_all(&record).unwrap();
    assert_eq!(walk_local_files(&root).unwrap().len(), 4);
    sweep(&root);
    std::fs::write(&record, b"a file, not a directory").unwrap();
    assert_eq!(walk_local_files(&root).unwrap().len(), 4);
    sweep(&root);
    assert!(root.join("big.bin.tmp").exists());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&record);
}

/// An entry whose temp is a link is dropped, never followed.
#[cfg(unix)]
#[test]
fn a_recorded_link_is_never_followed() {
    let root = scratch_dir("link");
    let record = own_record_dir("link");
    let target = root.join("precious.txt");
    std::fs::write(&target, b"mine").unwrap();
    let link = root.join("x.tmp");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let handle = std::fs::File::open(&target).unwrap();
    crate::temp_record::TempRecord::register(&link, &handle)
        .unwrap()
        .simulate_kill();
    sweep(&root);
    assert!(target.exists(), "the sweep followed a link");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&record);
}
