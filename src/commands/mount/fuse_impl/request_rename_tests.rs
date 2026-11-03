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
        request_class: true,
    }
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Insert one row the uploader sent through `REQ`: content under `C`, name
/// sealed under `name_key`, `chunk_count` chunks declared by the listing.
fn add_request_row(mock: &mut Mock, id: &str, name: &str, name_key: &beebeeb_core::kdf::FileKey, chunk_count: u32) {
    let content = [42u8; 32];
    let private = [31u8; 32];
    let public = beebeeb_core::opaque::derive_x25519_public(&private);
    let (wrapped, nonce) = beebeeb_core::file_request::wrap_request_private(&master(), b"", &private)
        .expect("wrap the request private key");
    let seal = beebeeb_core::file_request::seal_to_request(&public, id.as_bytes(), &content)
        .expect("seal the content key to the request");
    let sealed_name =
        beebeeb_core::encrypt::encrypt_name_for(name_key, name, Some("text/plain"), ExpectedChunkFormat::V2)
            .expect("row name");
    mock.requests_doc = serde_json::json!({"file_requests": [{
        "id": REQ,
        "wrapped_private_key": b64(&wrapped),
        "wrap_nonce": b64(&nonce),
    }]});
    mock.files.insert(
        id.to_string(),
        serde_json::json!({
            "id": id,
            "parent_id": null,
            "name_encrypted": sealed_name,
            "is_folder": false,
            "size_bytes": 0,
            "chunk_count": chunk_count,
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
}

/// A request row as the mount lists it (one chunk, real geometry).
fn request_row_mock(id: &str, name: &str, name_key: &beebeeb_core::kdf::FileKey) -> Mock {
    let mut mock = Mock::default();
    add_request_row(&mut mock, id, name, name_key, 1);
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
        ExpectedChunkFormat::V2,
        "the rename carries the pin the current name records, not a hard-coded default"
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

/// P3-2 (2326 review2): the same upper-repair name, but the listing did not
/// flag the row read-only. The write check cannot see the name (the inode
/// carries no such verdict), so the refusal must come from the resolver's
/// upper-repair verdict having no `RenameTarget` — the `else` arm of the
/// ladder, which the read-only inode above never reaches.
#[test]
fn an_upper_repair_name_is_refused_even_when_the_listing_flagged_it_writable() {
    const UPPER: &str = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";
    let upper_key = beebeeb_core::kdf::derive_file_key(&master(), UPPER.to_uppercase().as_bytes());
    let (mut fs, mock) = setup(request_row_mock(UPPER, "request.txt", &upper_key));
    fs.inodes.insert(ROW_INO, entry(UPPER, "request.txt", false));
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
        "an upper-repair name sent a move"
    );
}

/// P3-1 (2326 review3): the overwrite site passes no key — only the owner
/// conventions may prove a version writable. A request row's content is sealed
/// under `C`, which no owner derivation produces, so the check cannot decide:
/// EAGAIN, and nothing is uploaded and nothing is trashed. Passing the resolved
/// key here would make it `Writable` and the edit would trash the sender's row.
#[test]
fn an_overwrite_of_a_request_row_is_refused_before_any_upload_or_trash() {
    let content_key = beebeeb_core::kdf::FileKey::from_bytes([42u8; 32]);
    let (mut fs, mock) = setup(request_row_mock(ROW_ID, "request.txt", &content_key));
    fs.inodes.insert(ROW_INO, entry(ROW_ID, "request.txt", false));
    fs.id_to_ino.insert(ROW_ID.to_string(), ROW_INO);

    assert_eq!(fs.commit_overwrite(ROW_INO, b"edited".to_vec()), Err(Errno::EAGAIN));
    let m = mock.lock().unwrap();
    assert!(m.inits.is_empty(), "an overwrite uploaded: {:?}", m.events);
    assert!(
        m.trashed.is_empty(),
        "an overwrite trashed the request row: {:?}",
        m.events
    );
    assert!(m.events.is_empty(), "an overwrite changed the server: {:?}", m.events);
}

/// P3-1: the create-over-an-existing-name site is unkeyed too: the request row
/// there is refused with EAGAIN and stays in the inode table.
#[test]
fn a_create_over_a_request_row_is_refused_and_keeps_it() {
    let content_key = beebeeb_core::kdf::FileKey::from_bytes([42u8; 32]);
    let (mut fs, mock) = setup(request_row_mock(ROW_ID, "request.txt", &content_key));
    fs.inodes.insert(ROW_INO, entry(ROW_ID, "request.txt", false));
    fs.id_to_ino.insert(ROW_ID.to_string(), ROW_INO);
    fs.dir_cache.insert(
        ROOT_INO,
        CachedDir {
            children: vec![ROW_INO],
            expires_at: std::time::Instant::now() + Duration::from_secs(60),
        },
    );

    assert_eq!(fs.take_replaced_child(ROOT_INO, "request.txt"), Err(Errno::EAGAIN));
    assert!(fs.inodes.contains_key(&ROW_INO), "the refused row's inode was dropped");
    let m = mock.lock().unwrap();
    assert!(m.inits.is_empty(), "a replace uploaded: {:?}", m.events);
    assert!(
        m.trashed.is_empty(),
        "a replace trashed the request row: {:?}",
        m.events
    );
}

const CONFLICT_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const CONFLICT_INO: u64 = 31;

/// P3-1: the third unkeyed site is the row a rename lands on. The writable
/// source is the request row itself (it renames through core's `RenameTarget`);
/// the target name is held by another request row, whose check is unkeyed:
/// EAGAIN, no move and nothing trashed.
#[test]
fn a_rename_onto_a_request_row_is_refused_before_the_move() {
    let content_key = beebeeb_core::kdf::FileKey::from_bytes([42u8; 32]);
    let mut mock = request_row_mock(ROW_ID, "request.txt", &content_key);
    add_request_row(&mut mock, CONFLICT_ID, "taken.txt", &content_key, 1);
    let (mut fs, mock) = setup(mock);
    fs.inodes.insert(ROW_INO, entry(ROW_ID, "request.txt", false));
    fs.id_to_ino.insert(ROW_ID.to_string(), ROW_INO);
    fs.inodes.insert(CONFLICT_INO, entry(CONFLICT_ID, "taken.txt", false));
    fs.id_to_ino.insert(CONFLICT_ID.to_string(), CONFLICT_INO);
    fs.dir_cache.insert(
        ROOT_INO,
        CachedDir {
            children: vec![ROW_INO, CONFLICT_INO],
            expires_at: std::time::Instant::now() + Duration::from_secs(60),
        },
    );

    let renamed = fs.rename_stored(
        ROW_INO,
        ROW_ID,
        ROW_ID.parse().ok(),
        "request.txt",
        ROOT_INO,
        ROOT_INO,
        "taken.txt",
        None,
    );
    assert_eq!(renamed, Err(Errno::EAGAIN));
    let m = mock.lock().unwrap();
    assert!(
        m.events.is_empty(),
        "a refused rename changed the server: {:?}",
        m.events
    );
    assert_eq!(fs.inodes[&ROW_INO].name, "request.txt", "the source was renamed anyway");
    assert_eq!(fs.inodes[&CONFLICT_INO].file_id.as_deref(), Some(CONFLICT_ID));
}

/// P3-3 (2326 review3): the server declares the empty request row with 0
/// chunks, so the write check skips the probe and the row would be `Writable`.
/// The resolution is request-class, so a content write is still refused: an
/// edit would upload a new owner file and trash the sender's row.
#[test]
fn an_empty_request_row_overwrite_is_refused_and_trashes_nothing() {
    let content_key = beebeeb_core::kdf::FileKey::from_bytes([42u8; 32]);
    let mut mock = Mock::default();
    add_request_row(&mut mock, ROW_ID, "request.txt", &content_key, 0);
    let (mut fs, mock) = setup(mock);
    let mut inode = entry(ROW_ID, "request.txt", false);
    inode.chunk_count = 0;
    fs.inodes.insert(ROW_INO, inode);
    fs.id_to_ino.insert(ROW_ID.to_string(), ROW_INO);

    assert_eq!(fs.commit_overwrite(ROW_INO, b"edited".to_vec()), Err(Errno::EPERM));
    let m = mock.lock().unwrap();
    assert!(
        m.inits.is_empty(),
        "an empty request row was uploaded over: {:?}",
        m.events
    );
    assert!(m.trashed.is_empty(), "the sender's row was trashed: {:?}", m.events);
    drop(m);
    assert_eq!(fs.inodes[&ROW_INO].file_id.as_deref(), Some(ROW_ID));
}
