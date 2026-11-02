//! L12/P2-8: a request row is content-under-`C`, name derived or under `C`. A
//! plaintext name authenticates under neither, and no path may accept it as if
//! it were a name: the row is refused with its own reason.
use super::*;

#[tokio::test]
async fn a_request_row_with_a_plaintext_name_is_refused() {
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
    let err = resolver
        .resolve_owner_row(&api, &MasterKey::from_bytes([7u8; 32]), row, id, owned_keys::ItemKind::File)
        .await
        .err()
        .expect("a plaintext name proves nothing");
    assert_eq!(err, REQUEST_NAME_NOT_ENCRYPTED);
}
