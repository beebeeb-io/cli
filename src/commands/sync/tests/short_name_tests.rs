//! Task 2408: a remote move applied locally (`reconcile_moves`) never renames
//! a file by a name that Windows resolves to a different entry (an 8.3 short
//! name: `REPORT~1.PDF` for `Report for 2026.pdf`). Such a move is held for
//! conflict handling, as for an occupied destination. The first two tests run
//! on every host; the Windows ones build a real 8.3 alias.

use super::*;

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bb-sync-short-name-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The manifest entry of a file synced with exactly `contents` (remote id `id`).
fn entry_with(id: Uuid, contents: &[u8], path: &Path) -> FileEntry {
    let meta = std::fs::metadata(path).unwrap();
    FileEntry {
        remote_id: id,
        remote_version: None,
        last_mtime: meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        last_size: contents.len() as u64,
        last_sync: Utc::now(),
        content_hash: Some(compute_file_hash(path).unwrap()),
    }
}

/// The remote side: file `id` now lives at `rel`.
fn moved_to(rel: &str, id: Uuid) -> HashMap<String, RemoteFile> {
    HashMap::from([(
        rel.to_string(),
        RemoteFile {
            id,
            resolved: None,
            chunk_count: 1,
            updated_at: Utc::now(),
            pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
        },
    )])
}

fn held(old: &str, new: &str) -> HashSet<String> {
    HashSet::from([old.to_string(), new.to_string()])
}

/// Control (every host): an unchanged local file follows its remote move.
#[test]
fn a_remote_move_renames_the_unchanged_local_file() {
    let root = scratch_dir("moves");
    let id = Uuid::new_v4();
    std::fs::write(root.join("old.txt"), b"body").unwrap();
    let mut state = SyncState::default();
    state
        .files
        .insert("old.txt".into(), entry_with(id, b"body", &root.join("old.txt")));
    let local = walk_local_files(&root).unwrap();

    let blocked = reconcile_moves(&root, &mut state, &local, &moved_to("moved/new.txt", id), false).unwrap();

    assert!(blocked.is_empty(), "{blocked:?}");
    assert_eq!(std::fs::read(root.join("moved").join("new.txt")).unwrap(), b"body");
    assert!(!root.join("old.txt").exists());
    assert!(state.files.contains_key("moved/new.txt") && !state.files.contains_key("old.txt"));
    let _ = std::fs::remove_dir_all(&root);
}

/// Control (every host): a destination that already exists is held, and
/// nothing is renamed.
#[test]
fn a_remote_move_onto_an_existing_file_is_held() {
    let root = scratch_dir("occupied");
    let id = Uuid::new_v4();
    std::fs::write(root.join("old.txt"), b"body").unwrap();
    std::fs::write(root.join("new.txt"), b"mine").unwrap();
    let mut state = SyncState::default();
    state
        .files
        .insert("old.txt".into(), entry_with(id, b"body", &root.join("old.txt")));
    let local = walk_local_files(&root).unwrap();

    let blocked = reconcile_moves(&root, &mut state, &local, &moved_to("new.txt", id), false).unwrap();

    assert_eq!(blocked, held("old.txt", "new.txt"));
    assert_eq!(std::fs::read(root.join("old.txt")).unwrap(), b"body");
    assert_eq!(std::fs::read(root.join("new.txt")).unwrap(), b"mine");
    assert!(state.files.contains_key("old.txt"));
    let _ = std::fs::remove_dir_all(&root);
}

/// The remote move's destination folder is a short name that reaches the
/// different local folder `Photos 2026`: held, not renamed into that folder,
/// and not a failed pass either.
#[cfg(windows)]
#[test]
fn a_remote_move_into_a_folder_a_short_name_reaches_is_held() {
    use crate::short_name_test_support::{Scratch, aliased_folder, entries};
    let scratch = Scratch::new("sync-move-folder");
    let root = scratch.path();
    let Some(alias) = aliased_folder(root, "Photos 2026") else {
        return;
    };
    let id = Uuid::new_v4();
    std::fs::write(root.join("old.jpg"), b"body").unwrap();
    let mut state = SyncState::default();
    state
        .files
        .insert("old.jpg".into(), entry_with(id, b"body", &root.join("old.jpg")));
    let local = walk_local_files(root).unwrap();
    let new = format!("{alias}/new.jpg");

    let blocked = reconcile_moves(root, &mut state, &local, &moved_to(&new, id), false)
        .expect("a name conflict is held for conflict handling, not a failed pass");

    assert_eq!(blocked, held("old.jpg", &new));
    assert_eq!(std::fs::read(root.join("old.jpg")).unwrap(), b"body");
    assert!(
        entries(&root.join("Photos 2026")).is_empty(),
        "the file was moved into the folder the short name reaches"
    );
    assert!(state.files.contains_key("old.jpg") && !state.files.contains_key(&new));
}

/// The manifest's old path is a short name that now reaches the different
/// local file `Report for 2026.pdf` (same bytes, so the content check alone
/// would pass): held, and that file is not renamed.
#[cfg(windows)]
#[test]
fn a_remote_move_from_a_short_name_is_held() {
    use crate::short_name_test_support::{Scratch, aliased_file, entries};
    let scratch = Scratch::new("sync-move-source");
    let root = scratch.path();
    let Some(alias) = aliased_file(root, "Report for 2026.pdf", b"same bytes") else {
        return;
    };
    let id = Uuid::new_v4();
    let mut state = SyncState::default();
    state.files.insert(
        alias.clone(),
        entry_with(id, b"same bytes", &root.join("Report for 2026.pdf")),
    );
    let local = walk_local_files(root).unwrap();

    let blocked = reconcile_moves(root, &mut state, &local, &moved_to("moved.pdf", id), false)
        .expect("a name conflict is held for conflict handling, not a failed pass");

    assert_eq!(blocked, held(&alias, "moved.pdf"));
    assert_eq!(std::fs::read(root.join("Report for 2026.pdf")).unwrap(), b"same bytes");
    assert_eq!(entries(root), ["Report for 2026.pdf"]);
    assert!(state.files.contains_key(&alias));
}
