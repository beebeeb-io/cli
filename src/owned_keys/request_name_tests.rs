//! L12/P2-9: request-row-ness is decided by RESOLUTION, never by the fields a
//! server sends. A plaintext name authenticates under neither the bound reader
//! nor the R5 reader, so the row is not a request row: it falls back to the
//! plaintext-owner path (read-only, no seal target) and a rename refuses with
//! [`RENAME_UNKNOWN_KEY`](crate::owned_keys::RENAME_UNKNOWN_KEY).
use super::*;

#[tokio::test]
async fn a_request_row_with_a_plaintext_name_resolves_read_only_and_is_never_written() {
    let api = ApiClient::new_for_test("http://127.0.0.1:9".to_string());
    let id = "33333333-3333-4333-8333-333333333333";
    let row = serde_json::json!({
        "id": id,
        "name_encrypted": "request.txt",
        "file_request_id": "req-1",
        "sender_ephemeral_pubkey": "pub",
        "wrapped_content_key": "wrap"
    });
    let mut resolver = OwnedKeyResolver::new();
    let resolved = resolver
        .resolve_owner_row(
            &api,
            &MasterKey::from_bytes([7u8; 32]),
            row,
            id,
            owned_keys::ItemKind::File,
        )
        .await
        .expect("a plaintext name is not a request row, it is a plaintext owner row");
    assert_eq!(resolved.name, "request.txt");
    assert!(resolved.read_only, "nothing authenticates a plaintext name");
    assert!(resolved.write_target.is_none(), "no seal target for a guess");
    assert_eq!(resolved.origin, KeyOrigin::Derived);
}
