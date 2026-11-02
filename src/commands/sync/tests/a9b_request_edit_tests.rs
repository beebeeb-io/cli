use super::*;
use crate::test_server::{Mock, Scratch, Shared, shared, spawn};

const REL: &str = "payload.bin";
const REQUEST_ROW: &str = "33333333-3333-4333-8333-333333333333";

/// The watch loops' upload of a local edit of a request row (task 2275 L13/A9b,
/// R5): the row's content is read-only, so the edit is saved as a NEW owner item
/// under a core-minted id. The request row is neither versioned under `C` nor
/// trashed, and no chunk goes out under its id.
async fn watch_upload_of_request_edit(mut mock: Mock, s: &Scratch) -> (Result<(), String>, Shared) {
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
    // The request row's content is sealed under `C` (never the derived key).
    let content_key = beebeeb_core::kdf::FileKey::from_bytes([160u8; 32]);
    mock.downloads.insert(
        REQUEST_ROW.to_string(),
        beebeeb_core::encrypt::encrypt_chunk_raw(&content_key, b"the request upload").unwrap(),
    );
    let remote = RemoteFile {
        id: REQUEST_ROW.parse().unwrap(),
        resolved: Some(ResolvedCliKey {
            name_key: None,
            key: Arc::new(content_key),
            origin: KeyOrigin::RequestContent,
            canonical: serde_json::json!({
                "id": REQUEST_ROW,
                "size_bytes": 19,
                "chunk_count": 1,
                "version_number": 1,
            }),
            pending_owner_wrap: None,
            shared_root_id: None,
            name: REL.to_string(),
            read_only: false,
            write_target: None,
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
    (result, mock)
}

/// A9b (watch): the edit is uploaded as a new owner item, its init names a
/// different id than the request row and carries no version base, and the
/// request row is not trashed. No chunk is PUT under the request row's id.
#[tokio::test]
async fn a_request_row_edit_in_the_watch_loop_is_a_new_owner_item() {
    let s = Scratch::new("a9b-watch");
    let (result, mock) = watch_upload_of_request_edit(Mock::default(), &s).await;
    result.expect("the edit uploads");
    let m = mock.lock().unwrap();
    assert_ne!(
        m.inits[0], REQUEST_ROW,
        "the edit is a new item, not a version of the request row"
    );
    assert!(
        m.init_bodies[0].get("base_version_number").is_none_or(|v| v.is_null()),
        "a new item carries no version base: {:?}",
        m.init_bodies[0]
    );
    assert!(m.trashed.is_empty(), "the request row is not trashed: {:?}", m.trashed);
    assert!(
        m.events.iter().all(|e| !e.contains(REQUEST_ROW)),
        "nothing is written under the request row: {:?}",
        m.events
    );
    // The new id is an owner (O) id, the class core mints for a new item.
    assert_eq!(
        beebeeb_core::item_id::item_class(&m.inits[0]).unwrap(),
        beebeeb_core::item_id::ItemClass::Owner
    );
}

/// P2-10: an O row whose name is plaintext is refused with its own sentence —
/// not the SF-0002 iOS-repair message, which describes a different convention.
#[tokio::test]
async fn a_plaintext_name_is_skipped_with_its_own_reason() {
    use crate::commands::sync::transfer::{Replace, replace_decision};
    use crate::owned_keys::ResolvedCliKey;
    use std::sync::Arc;

    let master = beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32]);
    let id = "11111111-1111-4111-8111-111111111111";
    let resolved = ResolvedCliKey {
        key: Arc::new(beebeeb_core::kdf::derive_file_key(&master, id.as_bytes())),
        name_key: None,
        origin: beebeeb_core::owned_keys::KeyOrigin::Derived,
        canonical: serde_json::json!({
            "id": id, "name_encrypted": "plain.txt", "size_bytes": 5, "chunk_count": 1
        }),
        pending_owner_wrap: None,
        shared_root_id: None,
        name: "plain.txt".to_string(),
        read_only: true,
        write_target: None,
    };
    let api = ApiClient::new_for_test("http://127.0.0.1:9".to_string());
    match replace_decision(&api, &master, Some(&resolved), true).await {
        Replace::Skip(reason) => {
            assert_eq!(reason, crate::owned_keys::PLAINTEXT_NAME_READ_ONLY);
            assert_ne!(reason, crate::crypto::OWNER_REPAIR_READ_ONLY);
        }
        _ => panic!("a plaintext-named row must be skipped"),
    }
}
