//! Shared crypto helpers for the CLI.
//!
//! The Beebeeb server stores `name_encrypted` in three formats depending on
//! which client uploaded the file:
//!
//! 1. **Rust/core EncryptedBlob** — JSON with `cipher_suite`, `nonce` (byte
//!    array), `ciphertext` (byte array).  Produced by the CLI and any client
//!    using `beebeeb-core` via serde.
//!
//! 2. **Web-app blob** — JSON with `nonce` and `ciphertext` as base64 strings,
//!    no `cipher_suite` field.  Produced by the web client which calls the WASM
//!    `encrypt_metadata` function and base64-encodes the raw `Uint8Array`s.
//!
//! 3. **Plaintext** — a bare string (not JSON at all).  Legacy entries or
//!    server-created folders that predate client-side encryption.
//!
//! [`decrypt_name`] tries all three in order and returns the decrypted filename.

use base64::Engine as _;
use beebeeb_types::EncryptedBlob;

/// Intermediate struct for deserializing the web-app blob format where nonce
/// and ciphertext are base64-encoded strings instead of byte arrays.
#[derive(serde::Deserialize)]
struct WebAppBlob {
    nonce: String,
    ciphertext: String,
}

/// Decrypt a `name_encrypted` value from the API, handling all three server
/// formats (Rust EncryptedBlob, web-app base64 blob, plaintext fallback).
///
/// Returns `Some(name)` on success, `None` only if the value looks encrypted
/// but decryption fails (wrong key, corrupt data).
pub fn decrypt_name(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> Option<String> {
    // Format 3 (fast path): Plaintext — not JSON at all.
    if !name_encrypted_str.starts_with('{') {
        return Some(name_encrypted_str.to_string());
    }

    let file_uuid: uuid::Uuid = file_id_str.parse().ok()?;

    // The web app derives file keys using the UUID *string* as UTF-8 bytes
    // (TextEncoder.encode(fileId)), while the CLI/core uses the UUID's 16-byte
    // binary form (uuid.as_bytes()). We must try both derivations since files
    // can originate from either client.
    let key_from_string = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());
    let key_from_binary = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

    // Try each key derivation against each blob format.
    for file_key in [&key_from_string, &key_from_binary] {
        // Format 1: Rust-native EncryptedBlob (cipher_suite + byte-array fields).
        if let Ok(blob) = serde_json::from_str::<EncryptedBlob>(name_encrypted_str) {
            if let Ok(name) = beebeeb_core::encrypt::decrypt_metadata(file_key, &blob) {
                return Some(unwrap_metadata_json(&name));
            }
        }

        // Format 2: Web-app blob (nonce + ciphertext as base64 strings).
        if let Ok(web_blob) = serde_json::from_str::<WebAppBlob>(name_encrypted_str) {
            let b64 = base64::engine::general_purpose::STANDARD;
            if let (Ok(nonce), Ok(ciphertext)) =
                (b64.decode(&web_blob.nonce), b64.decode(&web_blob.ciphertext))
            {
                let blob = EncryptedBlob {
                    cipher_suite: beebeeb_types::CipherSuite::V1Aes256Gcm,
                    nonce,
                    ciphertext,
                };
                if let Ok(name) = beebeeb_core::encrypt::decrypt_metadata(file_key, &blob) {
                    return Some(unwrap_metadata_json(&name));
                }
            }
        }
    }

    None
}

/// The web app's "new ZK-safe" format encrypts a JSON object like
/// `{"name":"report.pdf","mime_type":"application/pdf"}` instead of a bare
/// filename string. If the decrypted plaintext is JSON with a `name` field,
/// extract it; otherwise return the string as-is (legacy bare-filename format).
fn unwrap_metadata_json(decrypted: &str) -> String {
    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(decrypted) {
        if let Some(name) = meta.get("name").and_then(|v| v.as_str()) {
            return name.to_string();
        }
    }
    decrypted.to_string()
}
