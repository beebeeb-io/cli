use super::*;
use crate::test_server::{Mock, Scratch, Shared, shared, spawn};

const REL: &str = "payload.bin";

/// Upload the scratch file through `do_upload` as the NEW copy of the remote
/// `old` (its key is derived, so it is not versioned: the old one is to be
/// trashed). SF-0002 r3: `old` is resolved as the walk resolves it, and its
/// version is served, so the write check proves it writable first.
async fn upload_over(mut mock: Mock, s: &Scratch, old: Uuid) -> (Result<(), String>, SyncState, Shared) {
    let (path, size, _mtime) = s.file(4096);
    let mtime = std::fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let local = LocalFile {
        mtime,
        size,
        content_hash: compute_file_hash(&path).unwrap(),
    };
    let master = Arc::new(beebeeb_core::kdf::MasterKey::from_bytes([5u8; 32]));
    let old_version = b"the old remote's content";
    let old_key = || beebeeb_core::kdf::derive_file_key(&master, old.to_string().as_bytes());
    mock.downloads.insert(
        old.to_string(),
        beebeeb_core::encrypt::encrypt_chunk_raw(&old_key(), old_version).unwrap(),
    );
    let remote = RemoteFile {
        id: old,
        resolved: Some(ResolvedCliKey {
            key: Arc::new(old_key()),
            origin: KeyOrigin::Derived,
            canonical: serde_json::json!({
                "id": old.to_string(),
                "size_bytes": old_version.len(),
                "chunk_count": 1,
            }),
            pending_owner_wrap: None,
            shared_root_id: None,
            name: REL.to_string(),
            read_only: false,
        }),
        chunk_count: 1,
        updated_at: Utc::now(),
        pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
    };
    let mock = shared(mock);
    let api = ApiClient::new_for_test(spawn(mock.clone()).await);
    let mut state = SyncState::default();
    let result = do_upload(
        &api,
        master,
        &s.dir,
        REL,
        &local,
        Some(remote),
        Uuid::new_v4(),
        &HashMap::new(),
        &mut state,
        false,
    )
    .await;
    (result, state, mock)
}

/// L5: the new copy's init echoes another id. The old remote is NOT
/// trashed, nothing is PUT, and the state keeps no entry for the path.
#[tokio::test]
async fn an_init_echo_mismatch_leaves_the_old_remote_untrashed() {
    let s = Scratch::new("l5-init");
    let old = Uuid::new_v4();
    let (result, state, mock) = upload_over(
        Mock {
            init_echo: Some(Uuid::new_v4().to_string()),
            ..Default::default()
        },
        &s,
        old,
    )
    .await;
    let err = result.expect_err("the upload aborts");
    assert!(err.contains(crate::item_id::CHANGED_ID_MESSAGE), "{err}");
    let m = mock.lock().unwrap();
    assert!(m.trashed.is_empty(), "the old remote is not trashed: {:?}", m.trashed);
    assert!(m.chunk_puts.is_empty());
    assert!(!state.files.contains_key(REL));
}

/// L5: the completion answer names another item. The old remote is still
/// not trashed.
#[tokio::test]
async fn a_completion_echo_mismatch_leaves_the_old_remote_untrashed() {
    let s = Scratch::new("l5-complete");
    let old = Uuid::new_v4();
    let (result, state, mock) = upload_over(
        Mock {
            complete_echo: Some(Uuid::new_v4().to_string()),
            ..Default::default()
        },
        &s,
        old,
    )
    .await;
    result.expect_err("the upload aborts");
    let m = mock.lock().unwrap();
    assert_eq!(m.completed.len(), 1);
    assert!(m.trashed.is_empty(), "the old remote is not trashed: {:?}", m.trashed);
    assert!(!state.files.contains_key(REL));
}

/// Normal flow: the old remote is trashed, once, AFTER the new copy
/// completed under the client's own id, which the state records.
#[tokio::test]
async fn the_old_remote_is_trashed_only_after_the_new_copy_completed() {
    let s = Scratch::new("l5-ok");
    let old = Uuid::new_v4();
    let (result, state, mock) = upload_over(Mock::default(), &s, old).await;
    result.expect("uploads");
    let m = mock.lock().unwrap();
    assert_eq!(m.trashed, vec![old.to_string()]);
    let complete = m
        .events
        .iter()
        .position(|e| e.starts_with("complete:"))
        .expect("completed");
    let trash = m
        .events
        .iter()
        .position(|e| *e == format!("trash:{old}"))
        .expect("trashed");
    assert!(complete < trash, "trash after completion: {:?}", m.events);
    assert_eq!(state.files[REL].remote_id.to_string(), m.inits[0]);
}
