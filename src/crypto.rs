//! Shared crypto helpers for the CLI.
//!
//! ## Name decryption — delegates to the core single source of truth
//!
//! The server stores `name_encrypted` in several shapes depending on which
//! client wrote the file (native `EncryptedBlob` JSON, web base64
//! `{nonce,ciphertext}` blob, or bare plaintext), and the per-file key may have
//! been derived from either the UUID's 16-byte binary form (legacy CLI) or the
//! UUID string as UTF-8 bytes (web / current CLI). **All of that logic now lives
//! once in [`beebeeb_core::encrypt::decrypt_name`]** — the CLI's [`decrypt_name`]
//! and [`decrypt_name_with_key`] are thin wrappers over it so there is a single
//! implementation shared by every client (CLI, desktop, mobile, web). They keep
//! the CLI's `Option<String>` return shape (the call sites expect it) by mapping
//! core's `Result` with `.ok()`.
//!
//! ## Chunk content — handled here (a separate cross-client problem)
//!
//! - **CLI chunks** are stored as JSON-serialized `EncryptedBlob` objects.
//! - **Web-app chunks** are stored as raw binary: `nonce (12 bytes) | ciphertext`.
//!
//! Additionally, the file key used to encrypt chunks may have been derived from
//! either the UUID's 16-byte binary form (CLI) or the UUID string as UTF-8 bytes
//! (web app).
//!
//! [`decrypt_file_chunks`] handles all combinations transparently.

use beebeeb_types::EncryptedBlob;

/// Decrypt a `name_encrypted` value from the API, handling every server format
/// (native `EncryptedBlob`, web-app base64 blob, plaintext fallback) and both
/// UUID key derivations.
///
/// Thin wrapper over the consolidated [`beebeeb_core::encrypt::decrypt_name`]
/// (the single source of truth). Returns `Some(name)` on success, `None` only if
/// the value looks encrypted but no (key, format) combination decrypts it.
pub fn decrypt_name(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> Option<String> {
    beebeeb_core::encrypt::decrypt_name(master_key, file_id_str, name_encrypted_str).ok()
}

/// Decrypt a `name_encrypted` value with a **known** file key (rather than one
/// derived from the master key + file id). Used for file-request uploads, whose
/// content key `C` is recovered via the request-key path — the filename is then
/// encrypted directly under `FileKey(C)`, not under a master-derived key.
///
/// Thin wrapper over [`beebeeb_core::encrypt::decrypt_name_with_key`]; handles
/// both server blob formats and the plaintext fast-path.
pub fn decrypt_name_with_key(file_key: &beebeeb_core::kdf::FileKey, name_encrypted_str: &str) -> Option<String> {
    beebeeb_core::encrypt::decrypt_name_with_key(file_key, name_encrypted_str)
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
    let file_uuid: uuid::Uuid = file_id_str.parse().map_err(|e| format!("invalid file ID: {e}"))?;

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
        let mut de = serde_json::Deserializer::from_slice(remaining).into_iter::<EncryptedBlob>();

        let blob = match de.next() {
            Some(Ok(b)) => b,
            Some(Err(e)) => return Err(format!("parse chunk {i}: {e}")),
            None => return Err(format!("no data for chunk {i}")),
        };
        offset += de.byte_offset();

        let decrypted =
            beebeeb_core::encrypt::decrypt_chunk(file_key, &blob).map_err(|e| format!("decrypt chunk {i}: {e}"))?;
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
        let this_chunk_size = if i == count - 1 { remaining } else { chunk_enc_size };

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

        let decrypted =
            beebeeb_core::encrypt::decrypt_chunk(file_key, &blob).map_err(|e| format!("decrypt raw chunk {i}: {e}"))?;
        plaintext.extend_from_slice(&decrypted);

        offset += this_chunk_size;
    }

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use beebeeb_core::chunk_stream::ChunkDecryptor;
    use beebeeb_core::encrypt::{encrypt_chunk, encrypt_chunk_raw};
    use beebeeb_core::kdf::{MasterKey, derive_file_key, derive_master_key};

    // A fixed file id used across the legacy/dual-key tests.
    const FID: &str = "3e15382b-1111-2222-3333-444455556666";

    fn mk() -> MasterKey {
        derive_master_key("test-password", b"test-salt-16bytes").unwrap()
    }

    /// Modern raw format, key derived from the UUID **string** (current CLI +
    /// web + post-`bb repair`): the common single-chunk case.
    #[test]
    fn raw_string_uuid_single_chunk_roundtrip() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"hello raw world";
        let frame = encrypt_chunk_raw(&fk, pt).unwrap();
        let out = decrypt_file_chunks(&m, FID, &frame, 1).unwrap();
        assert_eq!(out, pt);
    }

    /// LEGACY pre-`bb repair` file: key derived from the 16-byte **binary**
    /// UUID. `decrypt_file_chunks` must fall back to the binary-UUID key — this
    /// is the dual-key path the streaming download relies on for legacy files.
    #[test]
    fn raw_binary_uuid_legacy_dual_key_fallback() {
        let m = mk();
        let uuid: uuid::Uuid = FID.parse().unwrap();
        let fk = derive_file_key(&m, uuid.as_bytes());
        let pt = b"legacy binary-uuid file body";
        let frame = encrypt_chunk_raw(&fk, pt).unwrap();
        let out = decrypt_file_chunks(&m, FID, &frame, 1).unwrap();
        assert_eq!(out, pt);
    }

    /// LEGACY CLI JSON-blob chunk format (first byte `{`): detected and
    /// decrypted via the buffered path. The streaming download peeks the first
    /// byte and routes these here unchanged.
    #[test]
    fn json_blob_legacy_format_detected_and_decrypted() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"legacy json blob chunk";
        let blob = encrypt_chunk(&fk, pt).unwrap();
        let json = serde_json::to_vec(&blob).unwrap();
        assert_eq!(json[0], b'{', "JSON-blob detection relies on a leading brace");
        let out = decrypt_file_chunks(&m, FID, &json, 1).unwrap();
        assert_eq!(out, pt);
    }

    /// JSON-blob format combined with the binary-UUID legacy key — both legacy
    /// dimensions at once (the worst-case pre-repair file).
    #[test]
    fn json_blob_with_binary_uuid_key() {
        let m = mk();
        let uuid: uuid::Uuid = FID.parse().unwrap();
        let fk = derive_file_key(&m, uuid.as_bytes());
        let pt = b"json + binary uuid";
        let blob = encrypt_chunk(&fk, pt).unwrap();
        let json = serde_json::to_vec(&blob).unwrap();
        let out = decrypt_file_chunks(&m, FID, &json, 1).unwrap();
        assert_eq!(out, pt);
    }

    /// Multi-chunk raw roundtrip with uniform (exact-multiple) chunks — the
    /// `total / count` framing the streaming + buffered paths share.
    #[test]
    fn raw_multi_chunk_uniform_roundtrip() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let chunk = vec![0xABu8; 4096];
        let mut payload = Vec::new();
        for _ in 0..3 {
            payload.extend_from_slice(&encrypt_chunk_raw(&fk, &chunk).unwrap());
        }
        let out = decrypt_file_chunks(&m, FID, &payload, 3).unwrap();
        let mut expected = Vec::new();
        for _ in 0..3 {
            expected.extend_from_slice(&chunk);
        }
        assert_eq!(out, expected);
    }

    /// A wrong master key fails cleanly (no panic) after trying both
    /// derivations.
    #[test]
    fn wrong_key_fails_cleanly() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let frame = encrypt_chunk_raw(&fk, b"data").unwrap();
        let wrong = derive_master_key("other-password", b"other-salt-16byte").unwrap();
        assert!(decrypt_file_chunks(&wrong, FID, &frame, 1).is_err());
    }

    /// The streaming download decryptor (`ChunkDecryptor::for_push`, string-UUID
    /// key) must produce exactly what the buffered path produces for a modern
    /// raw file — guarantees the streaming rewire is behaviour-preserving.
    #[test]
    fn streaming_string_key_matches_buffered() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"streaming equals buffered";
        let frame = encrypt_chunk_raw(&fk, pt).unwrap();
        let mut dec = ChunkDecryptor::for_push(&m, FID);
        let streamed = dec.push_frame(&frame).unwrap().data;
        let buffered = decrypt_file_chunks(&m, FID, &frame, 1).unwrap();
        assert_eq!(streamed, buffered);
        assert_eq!(streamed, pt);
    }

    /// `decrypt_name` handles the plaintext fallback (non-JSON value).
    #[test]
    fn decrypt_name_plaintext_passthrough() {
        let m = mk();
        assert_eq!(decrypt_name(&m, FID, "Documents").as_deref(), Some("Documents"));
    }
}
