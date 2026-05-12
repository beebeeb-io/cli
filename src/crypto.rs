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
//!
//! Chunk content has a similar cross-client problem:
//!
//! - **CLI chunks** are stored as JSON-serialized `EncryptedBlob` objects.
//! - **Web-app chunks** are stored as raw binary: `nonce (12 bytes) | ciphertext`.
//!
//! Additionally, the file key used to encrypt chunks may have been derived from
//! either the UUID's 16-byte binary form (CLI) or the UUID string as UTF-8 bytes
//! (web app).
//!
//! [`decrypt_file_chunks`] handles all combinations transparently.

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

/// AES-256-GCM nonce length in bytes.
const NONCE_LEN: usize = 12;
/// AES-256-GCM authentication tag length in bytes.
const TAG_LEN: usize = 16;

/// Decrypt all chunks of a downloaded file, handling both CLI-format (JSON
/// `EncryptedBlob`) and web-app-format (raw `nonce|ciphertext` binary) chunk
/// storage, and both UUID key derivation methods (binary vs string).
///
/// The server streams chunk blobs back-to-back. For CLI uploads each chunk is
/// a JSON object; for web uploads each chunk is raw `nonce (12) | ciphertext`.
/// This function detects the format from the first byte (`{` = JSON, otherwise
/// raw binary) and applies the matching parser for all chunks.
///
/// Returns the reassembled plaintext on success.
pub fn decrypt_file_chunks(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    let file_uuid: uuid::Uuid = file_id_str
        .parse()
        .map_err(|e| format!("invalid file ID: {e}"))?;

    // Try binary-UUID key first (CLI origin), then string-UUID key (web origin).
    let key_from_binary = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());
    let key_from_string = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());

    // Attempt decryption with each key. The first successful full decryption wins.
    for file_key in [&key_from_binary, &key_from_string] {
        match try_decrypt_all_chunks(file_key, encrypted_bytes, chunk_count) {
            Ok(plaintext) => return Ok(plaintext),
            Err(_) => continue,
        }
    }

    Err(format!(
        "failed to decrypt file {file_id_str}: neither binary-UUID nor string-UUID key derivation succeeded"
    ))
}

/// Try to decrypt all chunks with a given file key. Detects the chunk format
/// from the first byte of the buffer and parses accordingly.
pub fn try_decrypt_all_chunks(
    file_key: &beebeeb_core::kdf::FileKey,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    if encrypted_bytes.is_empty() && chunk_count == 0 {
        return Ok(Vec::new());
    }
    if encrypted_bytes.is_empty() {
        return Err("no data but chunk_count > 0".to_string());
    }

    // Detect format: JSON blobs start with '{', raw binary does not.
    if encrypted_bytes[0] == b'{' {
        decrypt_json_chunks(file_key, encrypted_bytes, chunk_count)
    } else {
        decrypt_raw_chunks(file_key, encrypted_bytes, chunk_count)
    }
}

/// Parse and decrypt CLI-format chunks: concatenated JSON `EncryptedBlob` objects.
fn decrypt_json_chunks(
    file_key: &beebeeb_core::kdf::FileKey,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    let mut plaintext = Vec::new();
    let mut offset = 0;

    for i in 0..chunk_count {
        if offset >= encrypted_bytes.len() {
            return Err(format!(
                "unexpected end of data at chunk {i}/{chunk_count} (offset {offset})"
            ));
        }

        let remaining = &encrypted_bytes[offset..];
        let mut de =
            serde_json::Deserializer::from_slice(remaining).into_iter::<EncryptedBlob>();

        let blob = match de.next() {
            Some(Ok(b)) => b,
            Some(Err(e)) => return Err(format!("parse chunk {i}: {e}")),
            None => return Err(format!("no data for chunk {i}")),
        };
        offset += de.byte_offset();

        let decrypted = beebeeb_core::encrypt::decrypt_chunk(file_key, &blob)
            .map_err(|e| format!("decrypt chunk {i}: {e}"))?;
        plaintext.extend_from_slice(&decrypted);
    }

    Ok(plaintext)
}

/// Parse and decrypt web-app-format chunks: raw `nonce (12 bytes) | ciphertext`.
///
/// The server knows each chunk's stored size and streams them back-to-back.
/// With a single-chunk file the entire download is one chunk. For multi-chunk
/// files the server returns them sequentially; since AES-256-GCM ciphertext is
/// `plaintext_len + 16` bytes (tag), and the nonce is 12 bytes, each raw chunk
/// is `12 + plaintext_len + 16` bytes. We split evenly for equal-sized chunks
/// and handle the potentially shorter last chunk.
fn decrypt_raw_chunks(
    file_key: &beebeeb_core::kdf::FileKey,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    let total = encrypted_bytes.len();
    let count = chunk_count as usize;

    if count == 0 {
        return Ok(Vec::new());
    }

    // Minimum valid chunk: NONCE_LEN + TAG_LEN (empty plaintext encrypted).
    let min_chunk_overhead = NONCE_LEN + TAG_LEN;
    if total < count * min_chunk_overhead {
        return Err(format!(
            "raw data too short: {total} bytes for {count} chunks (minimum {})",
            count * min_chunk_overhead
        ));
    }

    // For single-chunk files, the entire buffer is the chunk.
    // For multi-chunk files, split evenly — all chunks except the last have
    // the same encrypted size; the last may be shorter.
    let chunk_enc_size = if count == 1 {
        total
    } else {
        // Each chunk has the same plaintext size except the last, so the
        // encrypted size (nonce + ciphertext + tag) is uniform for all but
        // the last. We compute it by dividing the total evenly among the
        // first N-1 chunks.
        total / count
    };

    let mut plaintext = Vec::with_capacity(total);
    let mut offset = 0;

    for i in 0..count {
        let remaining = total - offset;
        // Last chunk takes whatever is left; earlier chunks take the uniform size.
        let this_chunk_size = if i == count - 1 {
            remaining
        } else {
            chunk_enc_size
        };

        if this_chunk_size < min_chunk_overhead {
            return Err(format!(
                "chunk {i} too short: {this_chunk_size} bytes (minimum {min_chunk_overhead})"
            ));
        }

        let chunk_data = &encrypted_bytes[offset..offset + this_chunk_size];
        let nonce = &chunk_data[..NONCE_LEN];
        let ciphertext = &chunk_data[NONCE_LEN..];

        let blob = EncryptedBlob {
            cipher_suite: beebeeb_types::CipherSuite::V1Aes256Gcm,
            nonce: nonce.to_vec(),
            ciphertext: ciphertext.to_vec(),
        };

        let decrypted = beebeeb_core::encrypt::decrypt_chunk(file_key, &blob)
            .map_err(|e| format!("decrypt raw chunk {i}: {e}"))?;
        plaintext.extend_from_slice(&decrypted);

        offset += this_chunk_size;
    }

    Ok(plaintext)
}
