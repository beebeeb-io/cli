use super::*;
use beebeeb_core::kdf::derive_master_key;

fn test_master_key() -> MasterKey {
    derive_master_key("file-request-cli-test", b"sixteen-byte-salt").unwrap()
}

#[test]
fn parse_expiry_units() {
    assert_eq!(parse_expiry_secs("7d").unwrap(), 7 * 24 * 3600);
    assert_eq!(parse_expiry_secs("24h").unwrap(), 24 * 3600);
    assert_eq!(parse_expiry_secs("30m").unwrap(), 30 * 60);
    assert_eq!(parse_expiry_secs("2w").unwrap(), 2 * 7 * 24 * 3600);
    assert_eq!(parse_expiry_secs("5").unwrap(), 5 * 24 * 3600); // bare → days
    assert!(parse_expiry_secs("0d").is_err());
    assert!(parse_expiry_secs("abc").is_err());
}

#[test]
fn parse_sizes() {
    assert_eq!(parse_size_bytes("1000").unwrap(), 1000);
    assert_eq!(parse_size_bytes("1KB").unwrap(), 1000);
    assert_eq!(parse_size_bytes("1KiB").unwrap(), 1024);
    assert_eq!(parse_size_bytes("100MB").unwrap(), 100_000_000);
    assert_eq!(parse_size_bytes("2GiB").unwrap(), 2 * 1024 * 1024 * 1024);
    assert!(parse_size_bytes("notasize").is_err());
}

#[test]
fn link_assembly_roundtrips_through_parse() {
    // build_link + parse_request_link are inverses for token + R_pub.
    let (_priv, r_pub) = generate_request_keypair();
    let request_url = "https://app.beebeeb.io/r/ABCtoken_123-xyz";
    let link = build_link(request_url, &r_pub);
    assert!(link.contains("/r/ABCtoken_123-xyz#"));

    let parsed = parse_request_link(&link).unwrap();
    assert_eq!(parsed.token, "ABCtoken_123-xyz");
    assert_eq!(parsed.r_pub, r_pub);
}

#[test]
fn parse_link_rejects_missing_fragment() {
    assert!(parse_request_link("https://app.beebeeb.io/r/tok").is_err());
    assert!(parse_request_link("https://app.beebeeb.io/r/tok#").is_err());
}

#[test]
fn keypair_wrap_unwrap_roundtrip_matches_create_then_list() {
    // Mirrors the create → list flow: wrap R_priv at create, then unwrap it
    // from the list response (base64) and re-derive R_pub for the link.
    let mk = test_master_key();
    let (r_priv, r_pub) = generate_request_keypair();

    let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();
    let wrapped_b64 = b64std().encode(&wrapped);
    let nonce_b64 = b64std().encode(&nonce);

    // Simulate the list endpoint echoing these back.
    let row = serde_json::json!({
        "request_url": "https://app.beebeeb.io/r/tok123",
        "wrapped_private_key": wrapped_b64,
        "wrap_nonce": nonce_b64,
    });
    let link = rebuild_link(&mk, &row).expect("link rebuilds");
    let expected = build_link("https://app.beebeeb.io/r/tok123", &r_pub);
    assert_eq!(link, expected);
}

#[test]
fn send_seal_then_owner_open_recovers_file() {
    // End-to-end crypto without a server: owner creates a keypair, uploader
    // seals a file to R_pub, owner unwraps R_priv and decrypts name + body.
    let mk = test_master_key();
    let (r_priv, r_pub) = generate_request_keypair();
    let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();

    let body = b"the quick brown fox jumps over the lazy dog, twice over.".repeat(4);
    let (metadata, chunks) = seal_file_for_request(&r_pub, "secret.txt", Some("text/plain"), &body, 16).unwrap();

    // Parse what the uploader would POST.
    let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    let e_pub_bytes = b64std()
        .decode(meta["sender_ephemeral_pubkey"].as_str().unwrap())
        .unwrap();
    let mut e_pub = [0u8; 32];
    e_pub.copy_from_slice(&e_pub_bytes);
    let wrapped_key = b64std().decode(meta["wrapped_key"].as_str().unwrap()).unwrap();
    let name_encrypted = meta["name_encrypted"].as_str().unwrap();
    assert_eq!(meta["size_bytes"].as_i64().unwrap(), body.len() as i64);

    // Owner side: unwrap R_priv, open the content key, decrypt name + chunks.
    let recovered_priv = file_request::unwrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &wrapped, &nonce).unwrap();
    let content_key =
        file_request::open_request_upload(&recovered_priv, &e_pub, REQUEST_SEAL_CONTEXT, &wrapped_key).unwrap();
    let file_key = crate::crypto::key_from_bytes(&content_key);

    // Decrypt filename.
    let blob: beebeeb_types::EncryptedBlob = serde_json::from_str(name_encrypted).unwrap();
    let decrypted_meta = beebeeb_core::encrypt::decrypt_metadata(&file_key, &blob).unwrap();
    let meta_json: serde_json::Value = serde_json::from_str(&decrypted_meta).unwrap();
    assert_eq!(meta_json["name"].as_str().unwrap(), "secret.txt");

    // Decrypt the body via the shared raw-chunk path.
    let mut all = Vec::new();
    for (_idx, c) in &chunks {
        all.extend_from_slice(c);
    }
    let plaintext = crate::crypto::try_decrypt_all_chunks(
        &file_key,
        "00000000-0000-0000-0000-000000000000",
        beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        &all,
        chunks.len() as u32,
    )
    .expect("decrypts");
    assert_eq!(plaintext, body);
}

#[test]
fn receive_decrypts_request_uploaded_file_row() {
    // Full receiving path: an uploader seals a file; the server stores the
    // request fields on the file row; the owner's RequestKeyResolver recovers
    // the content key from a `GET /files`-shaped row and decrypts name + body.
    let mk = test_master_key();
    let request_id = "11111111-2222-3333-4444-555555555555".parse::<uuid::Uuid>().unwrap();
    let (r_priv, r_pub) = generate_request_keypair();
    let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();

    // Uploader side (mirrors `bb request send`).
    let body = b"received over a file request, sealed to the request key.".repeat(3);
    let (metadata, chunks) =
        seal_file_for_request(&r_pub, "received.bin", Some("application/octet-stream"), &body, 24).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();

    // Server-shaped file row (what GET /files returns for a request upload).
    let file_row = serde_json::json!({
        "id": "99999999-8888-7777-6666-555555555555",
        "name_encrypted": meta["name_encrypted"].as_str().unwrap(),
        "chunk_count": chunks.len(),
        "is_folder": false,
        "file_request_id": request_id.to_string(),
        "sender_ephemeral_pubkey": meta["sender_ephemeral_pubkey"].as_str().unwrap(),
        "wrapped_content_key": meta["wrapped_key"].as_str().unwrap(),
    });
    assert!(is_request_upload(&file_row));

    // A normal file row must NOT be treated as a request upload.
    let normal_row = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000000",
        "name_encrypted": "{}",
        "file_request_id": serde_json::Value::Null,
    });
    assert!(!is_request_upload(&normal_row));

    // Owner side: build a resolver seeded with the wrapped private key (as the
    // list endpoint would return it) and recover the content key.
    let mut resolver = RequestKeyResolver {
        wrapped: std::collections::HashMap::from([(
            request_id,
            (b64std().encode(&wrapped), b64std().encode(&nonce)),
        )]),
        privs: std::collections::HashMap::new(),
    };
    let content_key = resolver.content_key(&mk, &file_row).expect("recovers content key");

    // Name decrypts via the request content key.
    let name =
        crate::crypto::decrypt_name_with_key(&content_key, file_row["name_encrypted"].as_str().unwrap()).unwrap();
    assert_eq!(name, "received.bin");

    // Body decrypts via the shared raw-chunk path.
    let mut all = Vec::new();
    for (_idx, c) in &chunks {
        all.extend_from_slice(c);
    }
    let plaintext = crate::crypto::try_decrypt_all_chunks(
        &content_key,
        "00000000-0000-0000-0000-000000000000",
        beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        &all,
        chunks.len() as u32,
    )
    .unwrap();
    assert_eq!(plaintext, body);

    // R_priv is cached after the first resolve.
    assert!(resolver.privs.contains_key(&request_id));

    // An unknown request id yields no key (graceful, not a panic).
    let orphan_row = serde_json::json!({
        "file_request_id": "deadbeef-0000-0000-0000-000000000000",
        "sender_ephemeral_pubkey": meta["sender_ephemeral_pubkey"].as_str().unwrap(),
        "wrapped_content_key": meta["wrapped_key"].as_str().unwrap(),
    });
    assert!(resolver.content_key(&mk, &orphan_row).is_none());
}
/// P2-8 (review-2): a row core refuses as a foreign id is refused. bcc7320
/// fell back to the local legacy reader on ANY core error, so this row
/// resolved under its content key; it must not.
#[test]
fn a_foreign_id_request_row_is_refused_not_resolved_locally() {
    let mk = test_master_key();
    let request_id = "11111111-2222-3333-4444-555555555555".parse::<uuid::Uuid>().unwrap();
    let (r_priv, r_pub) = generate_request_keypair();
    let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();
    let (metadata, _chunks) = seal_file_for_request(&r_pub, "foreign.bin", None, b"foreign id", 16).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    let foreign_id = "33333333-3333-8333-8333-333333333333";
    assert_eq!(
        beebeeb_core::item_id::item_class(foreign_id).unwrap(),
        beebeeb_core::item_id::ItemClass::Foreign
    );
    let file_row = serde_json::json!({
        "id": foreign_id,
        "name_encrypted": meta["name_encrypted"].as_str().unwrap(),
        "is_folder": false,
        "file_request_id": request_id.to_string(),
        "sender_ephemeral_pubkey": meta["sender_ephemeral_pubkey"].as_str().unwrap(),
        "wrapped_content_key": meta["wrapped_key"].as_str().unwrap(),
    });
    let mut resolver = RequestKeyResolver {
        wrapped: std::collections::HashMap::from([(
            request_id,
            (b64std().encode(&wrapped), b64std().encode(&nonce)),
        )]),
        privs: std::collections::HashMap::new(),
    };
    assert!(
        resolver.row_keys(&mk, &file_row).is_none(),
        "a foreign-id request row is refused by core and must not fall back"
    );
}
/// R5 (run 14): the pre-2236 request upload every shipped CLI and web client
/// wrote — empty seal and wrap contexts, owner-class id — must still open.
/// Core's bound-envelope reader refuses that shape, so the legacy reader
/// behind the class and name checks is what resolves it.
#[test]
fn a_pre_2236_request_upload_opens_through_the_legacy_reader() {
    let mk = test_master_key();
    let request_id = "11111111-2222-3333-4444-555555555555".parse::<uuid::Uuid>().unwrap();
    let (r_priv, r_pub) = generate_request_keypair();
    let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();
    let (metadata, _chunks) = seal_file_for_request(&r_pub, "legacy.bin", None, b"pre-2236", 16).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    let file_row = serde_json::json!({
        "id": "99999999-8888-7777-6666-555555555555",
        "name_encrypted": meta["name_encrypted"].as_str().unwrap(),
        "is_folder": false,
        "file_request_id": request_id.to_string(),
        "sender_ephemeral_pubkey": meta["sender_ephemeral_pubkey"].as_str().unwrap(),
        "wrapped_content_key": meta["wrapped_key"].as_str().unwrap(),
    });
    let mut resolver = RequestKeyResolver {
        wrapped: std::collections::HashMap::from([(
            request_id,
            (b64std().encode(&wrapped), b64std().encode(&nonce)),
        )]),
        privs: std::collections::HashMap::new(),
    };
    let keys = resolver
        .row_keys(&mk, &file_row)
        .expect("a pre-2236 request upload opens through the legacy reader");
    assert_eq!(keys.name.name, "legacy.bin");
}
/// R5 (run 14): the fallback is gated, not "any core error". An owner-class
/// row with a plaintext name proves nothing core could have accepted, so it
/// stays refused even though its content key would open.
#[test]
fn a_plaintext_named_request_row_is_refused_not_resolved_locally() {
    let mk = test_master_key();
    let request_id = "11111111-2222-3333-4444-555555555555".parse::<uuid::Uuid>().unwrap();
    let (r_priv, r_pub) = generate_request_keypair();
    let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();
    let (metadata, _chunks) = seal_file_for_request(&r_pub, "plain.bin", None, b"plain", 16).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    let file_row = serde_json::json!({
        "id": "55555555-5555-4555-8555-555555555555",
        "name_encrypted": "plain.bin",
        "is_folder": false,
        "file_request_id": request_id.to_string(),
        "sender_ephemeral_pubkey": meta["sender_ephemeral_pubkey"].as_str().unwrap(),
        "wrapped_content_key": meta["wrapped_key"].as_str().unwrap(),
    });
    let mut resolver = RequestKeyResolver {
        wrapped: std::collections::HashMap::from([(
            request_id,
            (b64std().encode(&wrapped), b64std().encode(&nonce)),
        )]),
        privs: std::collections::HashMap::new(),
    };
    assert!(
        resolver.row_keys(&mk, &file_row).is_none(),
        "a plaintext-named request row must not resolve through the fallback"
    );
}
/// P2-8 (review-2): `content_key` feeds the pull folder walk and
/// `pull --zip`, so it refuses a plaintext-named owner row and a foreign
/// id just as the resolver does. bcc7320 returned the content key for both.
#[test]
fn content_key_refuses_a_plaintext_name_and_a_foreign_id() {
    let mk = test_master_key();
    let request_id = "11111111-2222-3333-4444-555555555555".parse::<uuid::Uuid>().unwrap();
    let (r_priv, r_pub) = generate_request_keypair();
    let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();
    let (metadata, _chunks) =
        seal_file_for_request(&r_pub, "received.bin", None, b"content", 16).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    let request_fields = serde_json::json!({
        "file_request_id": request_id.to_string(),
        "sender_ephemeral_pubkey": meta["sender_ephemeral_pubkey"].as_str().unwrap(),
        "wrapped_content_key": meta["wrapped_key"].as_str().unwrap(),
    });
    let mut resolver = RequestKeyResolver {
        wrapped: std::collections::HashMap::from([(
            request_id,
            (b64std().encode(&wrapped), b64std().encode(&nonce)),
        )]),
        privs: std::collections::HashMap::new(),
    };

    // An owner-class row with a plaintext name: nothing to authenticate it.
    let mut plaintext_row = request_fields.clone();
    plaintext_row["id"] = serde_json::json!("55555555-5555-4555-8555-555555555555");
    plaintext_row["name_encrypted"] = serde_json::json!("received.bin");
    assert!(
        resolver.content_key(&mk, &plaintext_row).is_none(),
        "a plaintext-named row gets no content key"
    );

    // A foreign id is never a request row, even with a sealed name.
    let mut foreign_row = request_fields;
    foreign_row["id"] = serde_json::json!("33333333-3333-8333-8333-333333333333");
    foreign_row["name_encrypted"] = meta["name_encrypted"].clone();
    assert!(
        resolver.content_key(&mk, &foreign_row).is_none(),
        "a foreign-id row gets no content key"
    );
}
