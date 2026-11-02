use super::*;

/// SF-0002 r3 (K6): the notice every sync path prints for a kept edit of a
/// read-only row, word for word, and the pass summary counts it as kept,
/// not failed.
#[test]
fn a_read_only_edit_is_a_notice_with_guidance() {
    assert_eq!(
        read_only_notice("Scans/receipt.pdf"),
        "Scans/receipt.pdf: This file was saved by an older version of the iOS app. You can open and \
         download it. To change it, save a copy. Your edit stays on this computer: save it under a new name \
         and sync uploads that as a new file."
    );
    assert_eq!(incomplete_summary(0, 0, 0, 1), "1 read-only edit kept");
    assert_eq!(
        incomplete_summary(1, 0, 0, 2),
        "1 failed \u{00b7} 2 read-only edits kept"
    );
}

/// Make a unique scratch directory under the system temp dir (no extra
/// test-only dependency). Caller removes it.
fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bb-sync-test-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &Path, rel: &str, contents: &[u8]) -> LocalFile {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, contents).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    LocalFile {
        mtime,
        size: meta.len(),
        content_hash: compute_file_hash(&path).unwrap(),
    }
}

/// A manifest entry matching the given local file (the post-sync state).
fn entry_for(local: &LocalFile) -> FileEntry {
    FileEntry {
        remote_id: Uuid::new_v4(),
        remote_version: None,
        last_mtime: local.mtime,
        last_size: local.size,
        last_sync: Utc::now(),
        content_hash: Some(local.content_hash.clone()),
    }
}

fn remote_file() -> RemoteFile {
    RemoteFile {
        id: Uuid::new_v4(),
        resolved: None,
        chunk_count: 1,
        updated_at: Utc::now(),
        pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
    }
}

/// Listing trusted, and the server reports EVERY manifest entry trashed.
fn plan_all(state: &SyncState) -> AbsencePlan {
    AbsencePlan {
        tombstones: state.files.values().map(|e| e.remote_id).collect(),
        untrusted: None,
    }
}

/// Listing trusted, but the server reports nothing trashed.
fn plan_none() -> AbsencePlan {
    AbsencePlan {
        tombstones: HashSet::new(),
        untrusted: None,
    }
}

/// `n` previously-synced files `f0.txt`.. on disk and in the manifest.
fn synced_files(dir: &Path, n: usize) -> (SyncState, HashMap<String, LocalFile>) {
    let mut state = SyncState::default();
    let mut local_files = HashMap::new();
    for i in 0..n {
        let rel = format!("f{i}.txt");
        let local = write_file(dir, &rel, format!("content {i}").as_bytes());
        state.files.insert(rel.clone(), entry_for(&local));
        local_files.insert(rel, local);
    }
    (state, local_files)
}

fn recovered_names(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.join(crate::sync_safety::RECOVERY_DIR)];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                out.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

// ── pure decision function ─────────────────────────────────────────────

#[test]
fn classify_prior_synced_unchanged_with_tombstone_is_remote_delete() {
    let dir = scratch_dir("classify-del");
    let local = write_file(&dir, "a.txt", b"hello");
    let entry = entry_for(&local);
    assert_eq!(
        classify_remote_absent(Some(&local), Some(&entry), true),
        RemoteAbsent::DeleteLocal,
        "a prior-synced, byte-identical local file the server reports trashed is a remote delete"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn classify_prior_synced_modified_is_readd() {
    let dir = scratch_dir("classify-readd");
    let original = write_file(&dir, "a.txt", b"hello");
    let entry = entry_for(&original);
    // Simulate an offline edit: same path, different bytes → different hash.
    let edited = write_file(&dir, "a.txt", b"hello, edited offline");
    assert_ne!(edited.content_hash, entry.content_hash.clone().unwrap());
    assert_eq!(
        classify_remote_absent(Some(&edited), Some(&entry), true),
        RemoteAbsent::ReAddLocal,
        "a locally-modified file must NOT be deleted on remote-absence"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn classify_new_local_is_not_a_delete() {
    let dir = scratch_dir("classify-new");
    let local = write_file(&dir, "fresh.txt", b"brand new");
    assert_eq!(
        classify_remote_absent(Some(&local), None, true),
        RemoteAbsent::NotADelete,
        "a genuinely-new local file (no manifest entry) is an upload, not a delete"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn local_matches_entry_uses_hash_then_size_mtime() {
    let dir = scratch_dir("match");
    let local = write_file(&dir, "a.txt", b"data");
    let mut entry = entry_for(&local);
    assert!(local_matches_entry(&local, &entry));
    // Hash mismatch → not a match (even if size/mtime line up).
    entry.content_hash = Some("deadbeef".into());
    assert!(!local_matches_entry(&local, &entry));
    // No stored hash → fall back to (mtime, size).
    entry.content_hash = None;
    assert!(local_matches_entry(&local, &entry));
    entry.last_size += 1;
    assert!(!local_matches_entry(&local, &entry));
    std::fs::remove_dir_all(&dir).ok();
}

// ── reconcile helper (used by both watch loops) ────────────────────────

/// (a) prior-synced + remote gone + local byte-identical
///     ⇒ local removed, manifest entry removed, nothing re-uploaded.
#[test]
fn reconcile_removes_byte_identical_remote_delete() {
    let dir = scratch_dir("reconcile-del");
    let local = write_file(&dir, "a.txt", b"hello");
    let mut state = SyncState::default();
    state.files.insert("a.txt".to_string(), entry_for(&local));

    let mut local_files = HashMap::new();
    local_files.insert("a.txt".to_string(), local);
    let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

    let plan = plan_all(&state);
    let removed =
        reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

    assert_eq!(removed, vec!["a.txt".to_string()]);
    assert!(!dir.join("a.txt").exists(), "local mirror must leave the sync folder");
    assert_eq!(recovered_names(&dir), vec!["a.txt".to_string()], "and be recoverable");
    assert!(!state.files.contains_key("a.txt"), "manifest entry must be dropped");
    std::fs::remove_dir_all(&dir).ok();
}

/// (b) prior-synced + remote gone but local MODIFIED offline
///     ⇒ NOT deleted, manifest entry retained (re-add path).
#[test]
fn reconcile_keeps_offline_modified_file() {
    let dir = scratch_dir("reconcile-keep");
    let original = write_file(&dir, "a.txt", b"hello");
    let entry = entry_for(&original);
    // Edit offline AFTER the manifest snapshot.
    let edited = write_file(&dir, "a.txt", b"hello, locally edited");

    let mut state = SyncState::default();
    state.files.insert("a.txt".to_string(), entry);

    let mut local_files = HashMap::new();
    local_files.insert("a.txt".to_string(), edited);
    let remote_files: HashMap<String, RemoteFile> = HashMap::new();

    let plan = plan_all(&state);
    let removed =
        reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

    assert!(removed.is_empty(), "an offline-edited file must NOT be removed");
    assert!(dir.join("a.txt").exists(), "local edit must survive reconciliation");
    assert!(
        state.files.contains_key("a.txt"),
        "manifest entry retained so the upload pass re-adds it"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// (c) genuinely-new local file (no manifest entry)
///     ⇒ untouched by reconciliation (it is uploaded by the normal pass).
#[test]
fn reconcile_ignores_new_local_file() {
    let dir = scratch_dir("reconcile-new");
    let local = write_file(&dir, "new.txt", b"fresh content");

    let mut state = SyncState::default(); // no manifest entry for new.txt
    let mut local_files = HashMap::new();
    local_files.insert("new.txt".to_string(), local);
    let remote_files: HashMap<String, RemoteFile> = HashMap::new();

    let plan = plan_all(&state);
    let removed =
        reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

    assert!(removed.is_empty(), "a new local file is never a remote delete");
    assert!(dir.join("new.txt").exists(), "new local file must remain on disk");
    std::fs::remove_dir_all(&dir).ok();
}

/// A file still present remotely is never touched by reconciliation.
#[test]
fn reconcile_leaves_present_remote_alone() {
    let dir = scratch_dir("reconcile-present");
    let local = write_file(&dir, "a.txt", b"hello");
    let mut state = SyncState::default();
    state.files.insert("a.txt".to_string(), entry_for(&local));

    let mut local_files = HashMap::new();
    local_files.insert("a.txt".to_string(), local);
    let mut remote_files = HashMap::new();
    remote_files.insert("a.txt".to_string(), remote_file()); // still on server

    let plan = plan_all(&state);
    let removed =
        reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

    assert!(removed.is_empty());
    assert!(dir.join("a.txt").exists());
    assert!(state.files.contains_key("a.txt"));
    std::fs::remove_dir_all(&dir).ok();
}

/// Manifest entry whose local file is already gone (deleted both sides):
/// no local unlink needed, but the stale manifest entry is dropped.
#[test]
fn reconcile_drops_manifest_when_local_already_gone() {
    let dir = scratch_dir("reconcile-gone");
    // Build an entry for a file that does NOT exist on disk.
    let phantom = LocalFile {
        mtime: 0,
        size: 5,
        content_hash: "00".into(),
    };
    let mut state = SyncState::default();
    state.files.insert("gone.txt".to_string(), entry_for(&phantom));

    let local_files: HashMap<String, LocalFile> = HashMap::new(); // not on disk
    let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // not on server

    let plan = plan_all(&state);
    let removed =
        reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

    assert_eq!(removed, vec!["gone.txt".to_string()]);
    assert!(!state.files.contains_key("gone.txt"), "stale manifest entry dropped");
    std::fs::remove_dir_all(&dir).ok();
}

/// dry_run must be side-effect-free: no unlink, no manifest mutation.
#[test]
fn reconcile_dry_run_is_side_effect_free() {
    let dir = scratch_dir("reconcile-dry");
    let local = write_file(&dir, "a.txt", b"hello");
    let mut state = SyncState::default();
    state.files.insert("a.txt".to_string(), entry_for(&local));

    let mut local_files = HashMap::new();
    local_files.insert("a.txt".to_string(), local);
    let remote_files: HashMap<String, RemoteFile> = HashMap::new();

    let plan = plan_all(&state);
    let removed =
        reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, true).removed;

    // It still reports the rel, but touches nothing.
    assert_eq!(removed, vec!["a.txt".to_string()]);
    assert!(dir.join("a.txt").exists(), "dry-run must not unlink");
    assert!(state.files.contains_key("a.txt"), "dry-run must not mutate manifest");
    std::fs::remove_dir_all(&dir).ok();
}

// ── reconcile → upload-snapshot prune (the watch-loop integration) ─────

/// Models exactly what both watch loops do after a remote delete: capture the
/// `removed` Vec, prune those keys from the pre-reconcile `resync_local`
/// snapshot, then run the change-detection that drives the upload loop. The
/// deleted file must be ABSENT from the pruned snapshot, so no upload is even
/// considered for it (the BLOCKER: a stale snapshot re-uploaded the unlinked
/// file every cycle, resurrecting the remote delete and erroring on the
/// now-missing path).
#[test]
fn reconcile_then_prune_drops_deleted_key_from_upload_snapshot() {
    let dir = scratch_dir("prune-snapshot");
    let local = write_file(&dir, "a.txt", b"hello");
    let mut state = SyncState::default();
    state.files.insert("a.txt".to_string(), entry_for(&local));

    // The pre-reconcile snapshot the upload loop iterates.
    let mut resync_local = HashMap::new();
    resync_local.insert("a.txt".to_string(), local);
    let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

    let plan = plan_all(&state);
    let removed =
        reconcile_remote_deletions(&dir, &mut state, &resync_local, &remote_files, &plan, false, false).removed;
    assert_eq!(removed, vec!["a.txt".to_string()]);
    // Local mirror + manifest entry are gone after reconcile.
    assert!(!dir.join("a.txt").exists());
    assert!(!state.files.contains_key("a.txt"));

    // The fix: prune removed keys from the snapshot BEFORE the upload loop.
    for rel in &removed {
        resync_local.remove(rel);
    }
    assert!(
        !resync_local.contains_key("a.txt"),
        "deleted file must be absent from the pruned upload snapshot"
    );

    // Replay the watch upload loop's decision over the pruned snapshot: it
    // must consider ZERO uploads (nothing left to resurrect).
    let mut would_upload: Vec<String> = Vec::new();
    for (rel, l) in &resync_local {
        let remote_present = remote_files.contains_key(rel);
        let changed = match state.files.get(rel) {
            Some(p) => local_changed_vs_manifest(l, p),
            None => !remote_present,
        };
        if changed {
            would_upload.push(rel.clone());
        }
    }
    assert!(
        would_upload.is_empty(),
        "no upload may be attempted for the just-deleted file; got {would_upload:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Data-safety invariant + the HIGH change-detection fix together: a
/// SAME-SIZE offline edit (identical byte length, different content → different
/// hash) of a remote-deleted file must (a) survive reconciliation (ReAddLocal,
/// never deleted) and (b) be detected as changed by `local_changed_vs_manifest`
/// so the watch upload loop re-uploads it. A bare `(mtime,size)` compare —
/// the old watch-loop bug — would have missed it and silently lost the edit.
#[test]
fn same_size_offline_edit_survives_and_is_re_uploaded() {
    let dir = scratch_dir("same-size-edit");
    // 5-byte original, manifest snapshot taken against it.
    let original = write_file(&dir, "a.txt", b"aaaaa");
    let entry = entry_for(&original);
    // Offline edit: SAME 5-byte size, different content → different hash.
    // Force the mtime to match the manifest so a (mtime,size) compare can't
    // tell them apart — only the content hash distinguishes them.
    let mut edited = write_file(&dir, "a.txt", b"bbbbb");
    edited.mtime = entry.last_mtime;
    assert_eq!(edited.size, original.size, "test premise: identical size");
    assert_eq!(edited.mtime, entry.last_mtime, "test premise: identical mtime");
    assert_ne!(edited.content_hash, original.content_hash, "content differs");

    // (a) reconcile must KEEP it (ReAddLocal), not delete it.
    let mut state = SyncState::default();
    state.files.insert("a.txt".to_string(), entry.clone());
    let mut resync_local = HashMap::new();
    resync_local.insert("a.txt".to_string(), edited.clone());
    let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

    let plan = plan_all(&state);
    let removed =
        reconcile_remote_deletions(&dir, &mut state, &resync_local, &remote_files, &plan, false, false).removed;
    assert!(removed.is_empty(), "an offline edit must never be deleted");
    assert!(dir.join("a.txt").exists(), "the local edit must survive on disk");
    assert!(state.files.contains_key("a.txt"), "manifest entry retained for re-add");

    // Pruning removes nothing (it wasn't deleted), so the key stays.
    for rel in &removed {
        resync_local.remove(rel);
    }
    assert!(resync_local.contains_key("a.txt"));

    // (b) the change detector that gates the upload loop must flag it.
    assert!(
        local_changed_vs_manifest(&edited, &entry),
        "a same-size/same-mtime content edit MUST be detected as changed (hash compare)"
    );
    // And the bare (mtime,size) compare the old watch loop used would NOT
    // have — proving the bug this fix closes.
    let old_buggy_compare = edited.mtime != entry.last_mtime || edited.size != entry.last_size;
    assert!(
        !old_buggy_compare,
        "the old (mtime,size)-only watch compare misses this edit — that was the bug"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ── tasks 1915/1916: absence is not deletion ───────────────────────────

#[test]
fn classify_identical_without_tombstone_is_kept_not_deleted() {
    let dir = scratch_dir("classify-unconfirmed");
    let local = write_file(&dir, "a.txt", b"hello");
    let entry = entry_for(&local);
    assert_eq!(
        classify_remote_absent(Some(&local), Some(&entry), false),
        RemoteAbsent::KeepUnconfirmed,
        "absent from the listing but no deletion record: keep"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn classify_edited_file_is_readded_whether_or_not_tombstoned() {
    let dir = scratch_dir("classify-edit-both");
    let original = write_file(&dir, "a.txt", b"hello");
    let entry = entry_for(&original);
    let edited = write_file(&dir, "a.txt", b"hello again, different");
    for tomb in [true, false] {
        assert_eq!(
            classify_remote_absent(Some(&edited), Some(&entry), tomb),
            RemoteAbsent::ReAddLocal
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn classify_missing_local_only_forgets_the_manifest_entry() {
    let dir = scratch_dir("classify-forget");
    let phantom = write_file(&dir, "p.txt", b"x");
    let entry = entry_for(&phantom);
    for tomb in [true, false] {
        assert_eq!(
            classify_remote_absent(None, Some(&entry), tomb),
            RemoteAbsent::ForgetStale
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// No tombstone => nothing is touched, however absent the file looks.
#[test]
fn reconcile_keeps_a_file_without_a_tombstone() {
    let dir = scratch_dir("reconcile-no-tomb");
    let (mut state, local_files) = synced_files(&dir, 3);
    let remote_files: HashMap<String, RemoteFile> = HashMap::new();

    let r = reconcile_remote_deletions(
        &dir,
        &mut state,
        &local_files,
        &remote_files,
        &plan_none(),
        false,
        false,
    );

    assert!(r.removed.is_empty());
    assert_eq!(state.files.len(), 3, "manifest untouched");
    for i in 0..3 {
        assert!(dir.join(format!("f{i}.txt")).exists());
    }
    assert!(recovered_names(&dir).is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

/// A tombstone removes THAT file only, and into the recoverable folder.
#[test]
fn reconcile_moves_only_the_tombstoned_file_to_recovery() {
    let dir = scratch_dir("reconcile-one-tomb");
    let (mut state, local_files) = synced_files(&dir, 3);
    let plan = AbsencePlan {
        tombstones: [state.files["f1.txt"].remote_id].into_iter().collect(),
        untrusted: None,
    };
    let remote_files: HashMap<String, RemoteFile> = HashMap::new();

    let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

    assert_eq!(r.removed, vec!["f1.txt".to_string()]);
    assert!(!dir.join("f1.txt").exists());
    assert!(dir.join("f0.txt").exists() && dir.join("f2.txt").exists());
    assert!(!state.files.contains_key("f1.txt") && state.files.len() == 2);
    assert_eq!(
        recovered_names(&dir),
        vec!["f1.txt".to_string()],
        "moved aside, not unlinked"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 60 tombstones of 60 tracked is over max(50, 10 %): remove nothing.
#[test]
fn reconcile_breaker_holds_back_a_mass_delete() {
    let dir = scratch_dir("reconcile-breaker");
    let (mut state, local_files) = synced_files(&dir, 60);
    let plan = plan_all(&state);
    let remote_files: HashMap<String, RemoteFile> = HashMap::new();

    let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

    assert!(
        r.notices
            .iter()
            .any(|n| n.contains("mass delete held back") && n.contains("--allow-mass-delete")),
        "the trip must be explained: {:?}",
        r.notices
    );
    assert!(r.removed.is_empty());
    assert_eq!(state.files.len(), 60);
    for i in 0..60 {
        assert!(dir.join(format!("f{i}.txt")).exists(), "f{i}.txt must survive");
    }

    // Explicit confirmation lets it through - still recoverably.
    let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, true, false);
    assert_eq!(r.removed.len(), 60);
    assert!(!r.notices.iter().any(|n| n.contains("mass delete held back")));
    assert_eq!(recovered_names(&dir).len(), 60);
    std::fs::remove_dir_all(&dir).ok();
}

/// Exactly the limit (50) is allowed; the breaker trips strictly above it.
#[test]
fn reconcile_breaker_allows_exactly_the_limit() {
    let dir = scratch_dir("reconcile-breaker-edge");
    let (mut state, local_files) = synced_files(&dir, 60);
    let plan = AbsencePlan {
        tombstones: (0..50).map(|i| state.files[&format!("f{i}.txt")].remote_id).collect(),
        untrusted: None,
    };
    let remote_files: HashMap<String, RemoteFile> = HashMap::new();

    let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

    assert_eq!(
        r.removed.len(),
        50,
        "50 tombstones of 60 tracked is within max(50, 10%)"
    );
    assert!(!r.notices.iter().any(|n| n.contains("mass delete held back")));
    assert_eq!(state.files.len(), 10);
    std::fs::remove_dir_all(&dir).ok();
}

/// The scan can be hours old: a file edited since must be kept, not moved.
#[test]
fn reconcile_keeps_a_file_edited_after_the_scan() {
    let dir = scratch_dir("reconcile-edited-since");
    let (mut state, local_files) = synced_files(&dir, 1);
    // `local_files` (the scan) still says identical; the disk has moved on.
    std::fs::write(dir.join("f0.txt"), b"typed after the scan began").unwrap();
    let plan = plan_all(&state);
    let remote_files: HashMap<String, RemoteFile> = HashMap::new();

    let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

    assert!(r.removed.is_empty());
    assert_eq!(
        std::fs::read(dir.join("f0.txt")).unwrap(),
        b"typed after the scan began"
    );
    assert!(state.files.contains_key("f0.txt"));
    assert!(recovered_names(&dir).is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recovery_folder_is_never_sync_input() {
    assert!(is_ignored_path(Path::new(
        "/home/u/Docs/.bb-trash/20261017-120000/a.txt"
    )));
    assert!(is_ignored_path(Path::new("/home/u/Docs/.bb-trash")));
    assert!(!is_ignored_path(Path::new("/home/u/Docs/a.txt")));
}

#[test]
fn root_is_unreconciled_only_when_bound_to_another_folder() {
    let bound = Uuid::new_v4();
    let mut state = SyncState::default();
    assert!(
        !root_unreconciled(&state, bound),
        "an unbound directory binds on its first sync"
    );
    state.remote_folder_id = Some(bound);
    assert!(!root_unreconciled(&state, bound));
    assert!(
        root_unreconciled(&state, Uuid::new_v4()),
        "bound to another folder: removals stay blocked"
    );
}

#[test]
fn decrypt_failures_are_told_apart_from_other_errors() {
    assert!(is_decrypt_failure("decrypt chunk 0: aead error"));
    assert!(is_decrypt_failure("key derivation failed"));
    assert!(!is_decrypt_failure("download failed (500): oops"));
    assert!(!is_decrypt_failure("download truncated: chunk 1/3"));
}

#[test]
fn a_409_is_a_file_still_being_uploaded_and_nothing_else_is() {
    assert!(is_still_uploading(
        "download failed (409 Conflict): replace_in_progress"
    ));
    assert!(is_still_uploading("upload is still in progress"));
    // Random ids in an error text must not decide it (a uuid may contain "409").
    assert!(!is_still_uploading(
        "decrypt chunk 0 of 7b0c1a52-3f4e-4409-9c61-2f8e5a1d0b37: aead error"
    ));
    assert!(!is_still_uploading(
        "download failed (500 Internal Server Error): 409 widgets"
    ));
    assert!(!is_still_uploading("download failed (403 Forbidden)"));
}

#[test]
fn a_typed_download_failure_ends_one_files_turn_not_the_pass() {
    use crate::download::DownloadError;
    // A chunk that never authenticates: the same outcome as a decrypt failure on the API path.
    assert_eq!(
        DownloadOutcome::of_failure(&DownloadError::Unreadable("x".into())),
        Some(DownloadOutcome::Unreadable)
    );
    // The version changed under the download: not this pass; the next one reads the new version.
    assert_eq!(
        DownloadOutcome::of_failure(&DownloadError::Changed("x".into())),
        Some(DownloadOutcome::Deferred)
    );
    // Everything else is the caller's, as before (an expired session must still stop the pass).
    assert_eq!(DownloadOutcome::of_failure(&DownloadError::Other("x".into())), None);
}

// ── walk_remote: what the listing could not show us ────────────────────

/// In-process `GET /api/v1/files` that answers every request with `entries`.
async fn mock_listing(entries: Vec<serde_json::Value>) -> ApiClient {
    use axum::routing::get;
    let app = axum::Router::new().route(
        "/api/v1/files",
        get(move || {
            let entries = entries.clone();
            async move { axum::Json(serde_json::json!({ "files": entries })) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    ApiClient::new_for_test(format!("http://{addr}"))
}

fn entry(name: &str) -> serde_json::Value {
    serde_json::json!({
        "id": Uuid::new_v4().to_string(),
        "name_encrypted": name,
        "is_folder": false,
        "chunk_count": 1,
        "updated_at": "2020-01-01T00:00:00+00:00",
    })
}

async fn walk(api: &ApiClient) -> (HashMap<String, RemoteFile>, crate::sync_safety::ListingHealth) {
    let mk = beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32]);
    let mut files = HashMap::new();
    let mut folders = HashMap::new();
    let mut health = crate::sync_safety::ListingHealth::default();
    walk_remote(
        api,
        &mk,
        Uuid::new_v4(),
        "",
        &mut files,
        &mut folders,
        &mut health,
        &mut OwnedKeyResolver::new(),
        None,
    )
    .await
    .unwrap();
    (files, health)
}

#[tokio::test]
async fn walk_remote_reports_a_complete_listing_as_healthy() {
    let api = mock_listing(vec![entry("a.txt"), entry("b.txt")]).await;
    let (files, health) = walk(&api).await;
    assert_eq!(files.len(), 2);
    assert_eq!(health, crate::sync_safety::ListingHealth::default());
}

#[tokio::test]
async fn walk_remote_counts_names_it_cannot_read() {
    // `{...` is not a plaintext name and not a valid blob: undecryptable.
    // `../x` and `a/b` are unsafe as local path segments.
    let api = mock_listing(vec![
        entry("ok.txt"),
        entry("{not-a-blob"),
        entry("../escape.txt"),
        entry("a/b.txt"),
    ])
    .await;
    let (files, health) = walk(&api).await;
    assert_eq!(files.keys().cloned().collect::<Vec<_>>(), vec!["ok.txt".to_string()]);
    assert_eq!(health.unreadable, 3, "three entries were skipped");
    assert!(!health.truncated);
}

#[tokio::test]
async fn walk_remote_flags_a_listing_cut_short_by_the_cap() {
    // 50,001 entries in one page: the client's 50,000 safety cap cuts it.
    let entries: Vec<_> = (0..50_001).map(|i| entry(&format!("f{i}.txt"))).collect();
    let api = mock_listing(entries).await;
    let (files, health) = walk(&api).await;
    assert_eq!(files.len(), 50_000);
    assert!(health.truncated, "a capped listing must be marked incomplete");
}
