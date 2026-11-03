//! P2-1 (2326 review): a request row's rename over the mount takes core's
//! `RenameTarget` — the string-convention key and the pin the current name
//! records — never the content key `C` the anonymous uploader holds. The
//! ladder is the one `bb mv` and WebDAV MOVE use: write target, then rename
//! target, then a refusal (read-only for an upper-repair name, unknown key
//! otherwise).
//!
//! Split out of `fuse_impl/tests.rs` (file-size ratchet): one file per concern.

use super::*;
use crate::test_server::{Mock, Shared, shared, spawn_on_own_thread};
use beebeeb_core::chunk_stream::ExpectedChunkFormat;

const REQ: &str = "11111111-2222-4333-8444-555555555555";
const ROW_ID: &str = "99999999-8888-7777-6666-555555555555";
const ROW_INO: u64 = 30;

fn master() -> beebeeb_core::kdf::MasterKey {
    beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32])
}

fn setup(mock: Mock) -> (BeebeebFsState, Shared) {
    let mock = shared(mock);
    let url = spawn_on_own_thread(mock.clone());
    let fs = BeebeebFsState::new(
        ApiClient::new_for_test(url),
        master(),
        std::time::Duration::from_secs(60),
    );
    (fs, mock)
}

fn entry(file_id: &str, name: &str, read_only: bool) -> InodeEntry {
    InodeEntry {
        file_id: Some(file_id.to_string()),
        key_uuid: file_id.parse().ok(),
        name: name.to_string(),
        is_dir: false,
        // A real upload's geometry: one chunk, sealed under the request's
        // content key `C`. The mount's write check probes it exactly as in
        // production; a listing without geometry would skip the probe and
        // prove only the seal branch.
        size: 0,
        chunk_count: 1,
        modified: SystemTime::UNIX_EPOCH,
        parent_ino: ROOT_INO,
        pin: Ok(ExpectedChunkFormat::Unrecorded),
        read_only,
    }
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A request row as the mount lists it: content under `C`, name sealed
/// under `name_key`, and the owner's request private key served by
/// `GET /file-requests`, sealed with the 2236 (row-id) seal.
fn request_row_mock(id: &str, name: &str, name_key: &beebeeb_core::kdf::FileKey) -> Mock {
    let content = [42u8; 32];
    let private = [31u8; 32];
    let public = beebeeb_core::opaque::derive_x25519_public(&private);
    let (wrapped, nonce) = beebeeb_core::file_request::wrap_request_private(&master(), b"", &private)
        .expect("wrap the request private key");
    let seal = beebeeb_core::file_request::seal_to_request(&public, id.as_bytes(), &content)
        .expect("seal the content key to the request");
    let sealed_name =
        beebeeb_core::encrypt::encrypt_name_for(name_key, name, Some("text/plain"), ExpectedChunkFormat::Unrecorded)
            .expect("row name");
    let mut mock = Mock {
        requests_doc: serde_json::json!({"file_requests": [{
            "id": REQ,
            "wrapped_private_key": b64(&wrapped),
            "wrap_nonce": b64(&nonce),
        }]}),
        ..Default::default()
    };
    mock.files.insert(
        id.to_string(),
        serde_json::json!({
            "id": id,
            "parent_id": null,
            "name_encrypted": sealed_name,
            "is_folder": false,
            "size_bytes": 0,
            "chunk_count": 1,
            "file_request_id": REQ,
            "sender_ephemeral_pubkey": b64(&seal.e_pub),
            "wrapped_content_key": b64(&seal.wrapped_key),
        }),
    );
    // Chunk 0 of the version, sealed under the request's content key `C` — the
    // key the uploader holds and the owner only reaches through the request.
    mock.downloads.insert(
        id.to_string(),
        beebeeb_core::encrypt::encrypt_chunk_raw(&beebeeb_core::kdf::FileKey::from_bytes(content), b"")
            .expect("chunk 0 under C"),
    );
    mock
}

/// P2-1: a pre-2236 request row (name under the uploader-held `C`) is
/// renamed over the mount: the new name is sealed under the
/// string-convention key and the pin the current name records — never
/// under `C`.
#[test]
fn a_request_row_is_renamed_under_the_string_key_never_the_content_key() {
    let content_key = beebeeb_core::kdf::FileKey::from_bytes([42u8; 32]);
    let (mut fs, mock) = setup(request_row_mock(ROW_ID, "request.txt", &content_key));
    fs.inodes.insert(ROW_INO, entry(ROW_ID, "request.txt", false));
    fs.id_to_ino.insert(ROW_ID.to_string(), ROW_INO);
    let ino = ROW_INO;

    fs.rename_stored(
        ino,
        ROW_ID,
        ROW_ID.parse().ok(),
        "request.txt",
        ROOT_INO,
        ROOT_INO,
        "renamed.txt",
        None,
    )
    .expect("a request row is renamed, not refused as read-only");

    let m = mock.lock().unwrap();
    assert_eq!(m.events, vec![format!("move:{ROW_ID}")]);
    let sent = m.move_bodies[0].1["name_encrypted"]
        .as_str()
        .expect("the move carries a sealed name");
    let string_key = beebeeb_core::kdf::derive_file_key(&master(), ROW_ID.as_bytes());
    let after = beebeeb_core::encrypt::decrypt_name_metadata_with_key(&string_key, sent)
        .expect("the new name opens under the string-convention key");
    assert_eq!(after.name, "renamed.txt");
    assert_eq!(
        after.chunk_format,
        ExpectedChunkFormat::Unrecorded,
        "the rename carries the pin the current name records"
    );
    assert!(
        beebeeb_core::encrypt::decrypt_name_metadata_with_key(&content_key, sent).is_err(),
        "the uploader's content key must not open the owner's new name"
    );
}

/// P2-1: an upper-repair request name is read-only — the rename is refused
/// and nothing is moved.
#[test]
fn an_upper_repair_request_row_is_refused() {
    const UPPER: &str = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";
    let upper_key = beebeeb_core::kdf::derive_file_key(&master(), UPPER.to_uppercase().as_bytes());
    let (mut fs, mock) = setup(request_row_mock(UPPER, "request.txt", &upper_key));
    fs.inodes.insert(ROW_INO, entry(UPPER, "request.txt", true));
    fs.id_to_ino.insert(UPPER.to_string(), ROW_INO);

    let renamed = fs.rename_stored(
        ROW_INO,
        UPPER,
        UPPER.parse().ok(),
        "request.txt",
        ROOT_INO,
        ROOT_INO,
        "renamed.txt",
        None,
    );
    assert_eq!(renamed, Err(Errno::EPERM));
    assert!(
        mock.lock().unwrap().events.is_empty(),
        "a read-only request row sent a move"
    );
}
