use super::*;
use beebeeb_core::kdf::derive_file_key;

const ID: &str = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";

fn master() -> MasterKey {
    MasterKey::from_bytes([7u8; 32])
}

fn frame(key: &FileKey, plaintext: &[u8]) -> Vec<u8> {
    beebeeb_core::encrypt::encrypt_chunk_raw(key, plaintext).unwrap()
}

/// SF-0002 round 2 (C3): geometry that gives no frame-0 candidate is
/// Unknown. A single-chunk row whose size the listing did not carry, read
/// as a prefix that is not the whole body, has no candidate length; the
/// mount's `size_bytes = 0` default gives a 28-byte candidate that cannot
/// authenticate a longer frame. Neither may pass as writable.
#[test]
fn missing_geometry_is_unknown_not_writable() {
    let m = master();
    let wire = frame(&derive_file_key(&m, ID.as_bytes()), b"an ordinary single-chunk version");
    let prefix = &wire[..28];
    assert_eq!(
        first_chunk_check(&m, ID, None, prefix, None, 1, None, false),
        WriteCheck::Unknown,
        "no size, partial body"
    );
    assert_eq!(
        first_chunk_check(&m, ID, None, prefix, Some(0), 1, None, false),
        WriteCheck::Unknown,
        "the mount's size 0 for a size the listing did not carry"
    );
    assert_eq!(
        first_chunk_check(&m, ID, None, &[], Some(4), 1, None, true),
        WriteCheck::Unknown,
        "an empty body"
    );
}

/// No key opens chunk 0: Unknown (a version this client cannot read is
/// neither proof of the repair convention nor of a writable row).
#[test]
fn a_frame_no_key_opens_is_unknown() {
    let m = master();
    let stranger = FileKey::from_bytes([3u8; 32]);
    let wire = frame(&stranger, b"sealed under a key the owner does not hold");
    let size = Some(wire.len() as u64 - 28);
    assert_eq!(
        first_chunk_check(&m, ID, None, &wire, size, 1, None, true),
        WriteCheck::Unknown
    );
    let other = FileKey::from_bytes([4u8; 32]);
    assert_eq!(
        first_chunk_check(&m, ID, Some(&other), &wire, size, 1, None, true),
        WriteCheck::Unknown,
        "a row key that does not open the frame proves nothing"
    );
}

/// SF-0002 round 2 (C4): a folder is never probed. The API here is a
/// closed port, so any request would make the answer Unknown: Writable
/// proves nothing was asked. Its name still decides (a read-only name is
/// UpperRepair), and the same target as a file is Unknown (the control that
/// shows the closed port answers).
#[tokio::test]
async fn a_folder_is_decided_by_its_name_alone() {
    let api = ApiClient::new_for_test("http://127.0.0.1:9".to_string());
    let m = master();
    let target = |is_folder, name_read_only| WriteTarget {
        file_id: ID,
        is_folder,
        name_read_only,
        row_key: None,
        size_bytes: None,
        chunk_count: 1,
    };
    assert_eq!(write_check(&api, &m, &target(true, false)).await, WriteCheck::Writable);
    assert_eq!(
        write_check(&api, &m, &target(true, true)).await,
        WriteCheck::UpperRepair
    );
    assert_eq!(write_check(&api, &m, &target(false, false)).await, WriteCheck::Unknown);
}

/// Round 3 (K1): a legacy CLI JSON-blob chunk 0 is writable once blob 0
/// authenticates under the owner's binary or string key or the row's own
/// key. A blob no write-capable key opens, or one cut short, is Unknown; a
/// JSON blob is never read as the repair convention.
#[test]
fn a_json_blob_chunk0_is_writable_once_it_authenticates() {
    let m = master();
    let uuid = uuid::Uuid::parse_str(ID).unwrap();
    let json =
        |key: &FileKey, pt: &[u8]| serde_json::to_vec(&beebeeb_core::encrypt::encrypt_chunk(key, pt).unwrap()).unwrap();
    let pt = b"a legacy CLI chunk";
    let size = Some(pt.len() as u64);
    let check = |row_key: Option<&FileKey>, body: &[u8], complete: bool| {
        first_chunk_check(&m, ID, row_key, body, size, 1, None, complete)
    };
    for key in [derive_file_key(&m, uuid.as_bytes()), derive_file_key(&m, ID.as_bytes())] {
        assert_eq!(check(None, &json(&key, pt), true), WriteCheck::Writable);
    }
    let item = FileKey::from_bytes([5u8; 32]);
    let shared = json(&item, pt);
    assert_eq!(
        check(Some(&item), &shared, true),
        WriteCheck::Writable,
        "the row's own key"
    );
    assert_eq!(
        check(None, &shared, true),
        WriteCheck::Unknown,
        "no write-capable key opens it"
    );
    let upper = json(&derive_file_key(&m, ID.to_ascii_uppercase().as_bytes()), pt);
    assert_eq!(
        check(None, &upper, true),
        WriteCheck::Unknown,
        "never the repair convention"
    );
    let binary = json(&derive_file_key(&m, uuid.as_bytes()), pt);
    assert_eq!(
        check(None, &binary[..binary.len() / 2], false),
        WriteCheck::Unknown,
        "cut short"
    );

    // Multi-chunk: blob 0 followed by the rest of the version.
    let key = derive_file_key(&m, uuid.as_bytes());
    let mut multi = json(&key, &[1u8; 300]);
    multi.extend(json(&key, &[2u8; 300]));
    multi.extend(json(&key, &[3u8; 5]));
    assert_eq!(
        first_chunk_check(&m, ID, None, &multi, Some(605), 3, None, true),
        WriteCheck::Writable
    );
}

/// Round 3 (K2): a legacy multi-chunk raw file served without X-Chunk-Size,
/// whose chunk size is not a power of two and whose last chunk is shorter,
/// is found by the read path's frame-size probe: writable under the owner's
/// key, read-only under the uppercase-id key.
#[test]
fn a_legacy_multi_chunk_file_without_a_chunk_size_is_probed() {
    let m = master();
    let pt: Vec<u8> = (0..3 * 3000 + 77).map(|i| (i % 251) as u8).collect();
    let wire = |key: &FileKey| -> Vec<u8> { pt.chunks(3000).flat_map(|c| frame(key, c)).collect() };
    let size = Some(pt.len() as u64);
    let ordinary = wire(&derive_file_key(&m, ID.as_bytes()));
    assert_eq!(
        first_chunk_check(&m, ID, None, &ordinary, size, 4, None, true),
        WriteCheck::Writable
    );
    let sealed = wire(&derive_file_key(&m, ID.to_ascii_uppercase().as_bytes()));
    assert_eq!(
        first_chunk_check(&m, ID, None, &sealed, size, 4, None, true),
        WriteCheck::UpperRepair
    );
    let item = FileKey::from_bytes([5u8; 32]);
    assert_eq!(
        first_chunk_check(&m, ID, Some(&item), &wire(&item), size, 4, None, true),
        WriteCheck::Writable,
        "the row's own key, probed the same way"
    );
}

/// Round 3 (K2), through the real probe: chunk 0 is 2 MiB (longer than the
/// ordinary prefix) and the server sends no X-Chunk-Size. The check reads
/// the feasible frame lengths in a second request and decides Writable.
#[tokio::test]
async fn a_frame_longer_than_the_first_prefix_is_found_by_a_second_read() {
    let m = master();
    let key = derive_file_key(&m, ID.as_bytes());
    let pt: Vec<u8> = (0..2 * 1024 * 1024 + 100).map(|i| (i % 251) as u8).collect();
    let wire: Vec<u8> = pt.chunks(2 * 1024 * 1024).flat_map(|c| frame(&key, c)).collect();
    let mock = crate::test_server::shared(crate::test_server::Mock::default());
    mock.lock().unwrap().downloads.insert(ID.to_string(), wire);
    let api = ApiClient::new_for_test(crate::test_server::spawn(mock.clone()).await);
    let target = WriteTarget {
        file_id: ID,
        is_folder: false,
        name_read_only: false,
        row_key: None,
        size_bytes: Some(pt.len() as u64),
        chunk_count: 2,
    };
    assert_eq!(write_check(&api, &m, &target).await, WriteCheck::Writable);
}

/// The controls: the canonical owner key and a row's own key prove a
/// version writable; the owner's uppercase-id key is read-only, also when it
/// is the row's own key (an owner wrap of the uppercase key).
#[test]
fn proven_answers_with_full_geometry() {
    let m = master();
    let pt = b"single chunk";
    let size = Some(pt.len() as u64);
    let ordinary = frame(&derive_file_key(&m, ID.as_bytes()), pt);
    assert_eq!(
        first_chunk_check(&m, ID, None, &ordinary, size, 1, None, false),
        WriteCheck::Writable
    );
    let item = FileKey::from_bytes([5u8; 32]);
    let shared = frame(&item, pt);
    assert_eq!(
        first_chunk_check(&m, ID, Some(&item), &shared, size, 1, None, false),
        WriteCheck::Writable
    );
    let upper = derive_file_key(&m, ID.to_ascii_uppercase().as_bytes());
    let sealed = frame(&upper, pt);
    assert_eq!(
        first_chunk_check(&m, ID, None, &sealed, size, 1, None, false),
        WriteCheck::UpperRepair
    );
    assert_eq!(
        first_chunk_check(&m, ID, Some(&upper), &sealed, size, 1, None, false),
        WriteCheck::UpperRepair
    );
}
