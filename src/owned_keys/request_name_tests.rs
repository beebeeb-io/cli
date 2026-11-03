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

/// §15/A9 (2326 review P3-1): an upper-repair request row RESOLVES as a
/// request row — content under `C`, name under the read-only uppercase
/// convention — and core hands back NO rename target for it. The integration
/// test only sees the refusal, which `read_only` alone can produce; this pins
/// the resolution, so a core that returns a rename target for an upper-repair
/// name turns the row writable and is caught here.
#[tokio::test]
async fn an_upper_repair_request_row_resolves_with_no_rename_target() {
    use beebeeb_core::chunk_stream::ExpectedChunkFormat;
    const REQ: &str = "11111111-2222-4333-8444-555555555555";
    // A separate id whose uppercase form is a different derivation.
    const UPPER: &str = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";
    let master = MasterKey::from_bytes([7u8; 32]);
    let content = beebeeb_core::kdf::derive_file_key(&master, b"request content");
    let private = [31u8; 32];
    let public = beebeeb_core::opaque::derive_x25519_public(&private);
    let (wrapped, nonce) =
        beebeeb_core::file_request::wrap_request_private(&master, b"", &private).expect("wrap the request private key");
    let seal = beebeeb_core::file_request::seal_to_request(&public, UPPER.as_bytes(), content.as_bytes())
        .expect("seal the content key to the request");
    let upper_key = beebeeb_core::kdf::derive_file_key(&master, UPPER.to_uppercase().as_bytes());
    let sealed_name = beebeeb_core::encrypt::encrypt_name_for(
        &upper_key,
        "request.txt",
        Some("text/plain"),
        ExpectedChunkFormat::Unrecorded,
    )
    .expect("row name");
    let mut mock = crate::test_server::Mock {
        requests_doc: serde_json::json!({"file_requests": [{
            "id": REQ,
            "wrapped_private_key": B64.encode(wrapped),
            "wrap_nonce": B64.encode(nonce),
        }]}),
        ..Default::default()
    };
    mock.files.insert(
        UPPER.to_string(),
        serde_json::json!({
            "id": UPPER,
            "name_encrypted": sealed_name,
            "is_folder": false,
            "chunk_count": 1,
            "size_bytes": 12,
        }),
    );
    let api = ApiClient::new_for_test(crate::test_server::spawn(crate::test_server::shared(mock)).await);
    let row = serde_json::json!({
        "id": UPPER,
        "name_encrypted": sealed_name,
        "is_folder": false,
        "chunk_count": 1,
        "size_bytes": 12,
        "acted_via_share_id": Value::Null,
        "file_request_id": REQ,
        "sender_ephemeral_pubkey": B64.encode(seal.e_pub),
        "wrapped_content_key": B64.encode(seal.wrapped_key),
    });
    let resolved = OwnedKeyResolver::new()
        .resolve_owner_row(&api, &master, row, UPPER, owned_keys::ItemKind::File)
        .await
        .expect("an upper-repair request row still resolves");
    assert_eq!(resolved.name, "request.txt");
    assert_eq!(
        resolved.origin,
        KeyOrigin::RequestContent,
        "the row resolves through core's request reader, not as an owner row"
    );
    assert!(resolved.read_only, "the upper-repair name proves no writable key");
    assert!(
        resolved.rename_target.is_none(),
        "an upper-repair name has no seal target: a rename must not invent one"
    );
    assert!(
        resolved.write_target.is_none(),
        "a request row never takes the owner write path"
    );
}
