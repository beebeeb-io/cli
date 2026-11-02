//! Content pins and per-row key material split out of `crypto.rs`.

use super::*;

/// Resolve the owner's content key for ONE stored version from its first chunk
/// frame (SF-0002). Only call this after the existing key failed to
/// authenticate that version's first chunk: it is the read-only repair path,
/// and a returned [`beebeeb_core::kdf::OwnerKeyConvention::UpperRepair`] means
/// the version must not be rewritten (no new version under this id).
pub fn owner_content_key(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    first_chunk_frame: &[u8],
) -> Option<(FileKey, beebeeb_core::kdf::OwnerKeyConvention)> {
    beebeeb_core::kdf::owner_file_key_for_content(master_key, file_id_str, first_chunk_frame).ok()
}

/// SF-0002 write check: does `frame` open as chunk 0 of a version under `key`?
/// The same reader core's per-version owner resolver uses (frame 0 decides
/// the format: v1 legacy or v2, either `is_last`); only the verdict is kept and
/// the opened bytes are wiped.
pub fn first_frame_opens(key: &FileKey, file_id_str: &str, frame: &[u8]) -> bool {
    ChunkDecryptor::for_push_with_file_key(key_copy(key), file_id_str, ExpectedChunkFormat::Unrecorded)
        .push_frame(frame)
        .map(|chunk| zeroize::Zeroizing::new(chunk.data))
        .is_ok()
}

/// SF-0002 write check (round 3, K1): the frame (`nonce || ciphertext`) of a
/// legacy CLI JSON-blob chunk 0 at the start of `prefix`, when one whole
/// `EncryptedBlob` is there. The same parse the read path's JSON parser does
/// (`decrypt_json_chunks`); nothing is authenticated here.
pub fn first_json_blob_frame(prefix: &[u8]) -> Option<Vec<u8>> {
    if prefix.first() != Some(&b'{') {
        return None;
    }
    let blob = serde_json::Deserializer::from_slice(prefix)
        .into_iter::<EncryptedBlob>()
        .next()?
        .ok()?;
    let mut frame = blob.nonce;
    frame.extend_from_slice(&blob.ciphertext);
    Some(frame)
}

/// AES-256-GCM nonce length in bytes.
pub(super) const NONCE_LEN: usize = 12;

/// AES-256-GCM authentication tag length in bytes.
pub(super) const TAG_LEN: usize = 16;

/// The chunk format a file's content must be read with: what its AUTHENTICATED
/// (encrypted) metadata records, via core's `decrypt_file_metadata` (task
/// 1908). Never taken from a server field. A plaintext or undecryptable name is
/// an error, not a guess: it carries no pin, and reading it as `Unrecorded`
/// would let a server re-enable frame-0 detection for a v2 file (core round 3,
/// P1-B).
pub fn content_pin(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> Result<ExpectedChunkFormat, CryptoError> {
    match beebeeb_core::encrypt::decrypt_file_metadata(master_key, file_id_str, name_encrypted_str) {
        Ok(m) => Ok(m.chunk_format),
        Err(e) => {
            // SF-0002: the name may be sealed under the read-only uppercase
            // convention. Resolve it through core's owner helper, then read the
            // pin under that key. A row that still does not authenticate keeps
            // the original error (never guessed).
            if let Ok((key, _)) =
                beebeeb_core::kdf::owner_file_key_for_name(master_key, file_id_str, name_encrypted_str)
            {
                if let Ok(m) = beebeeb_core::encrypt::decrypt_file_metadata_with_key(&key, name_encrypted_str) {
                    return Ok(m.chunk_format);
                }
            }
            Err(pin_error(file_id_str, e))
        }
    }
}

/// [`content_pin`] under a known file key (file-request content keys).
pub fn content_pin_with_key(
    file_key: &FileKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> Result<ExpectedChunkFormat, CryptoError> {
    beebeeb_core::encrypt::decrypt_file_metadata_with_key(file_key, name_encrypted_str)
        .map(|m| m.chunk_format)
        .map_err(|e| pin_error(file_id_str, e))
}

/// The read pin of a file row: under the request content key for a
/// file-request upload, the master-derived key otherwise.
pub fn file_row_pin(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
    content_key: Option<&FileKey>,
) -> Result<ExpectedChunkFormat, CryptoError> {
    match content_key {
        Some(c) => content_pin_with_key(c, file_id_str, name_encrypted_str),
        None => content_pin(master_key, file_id_str, name_encrypted_str),
    }
}

pub(super) fn pin_error(file_id: &str, e: impl std::fmt::Display) -> CryptoError {
    CryptoError::Metadata(format!(
        "cannot read the encrypted metadata of file {file_id} ({e}), so its chunk format is unknown; refusing to guess"
    ))
}

/// A second owned copy of `file_key` (core's `FileKey` is not `Clone`). The
/// by-value bytes it passes through are wiped on drop (SF-0002 round 2, C6);
/// `FileKey::from_bytes` wipes its own argument and the copy zeroizes on drop.
pub(crate) fn key_copy(file_key: &FileKey) -> FileKey {
    let bytes = zeroize::Zeroizing::new(*file_key.as_bytes());
    FileKey::from_bytes(*bytes)
}

#[cfg(test)]
mod key_copy_tests {
    use super::*;

    /// P2-12: the named write sites pass a key by reference through key_copy,
    /// which wipes the bytes it passes by value. A bare
    /// FileKey::from_bytes(*k.as_bytes()) leaves an unwiped stack copy of a
    /// file key, so the sources are checked for it.
    #[test]
    fn the_named_write_sites_hold_no_unwiped_stack_copy() {
        for (name, src) in [
            ("commands/mv.rs", include_str!("../commands/mv.rs")),
            ("commands/webdav/verbs.rs", include_str!("../commands/webdav/verbs.rs")),
            ("owned_keys.rs", include_str!("../owned_keys.rs")),
        ] {
            for banned in [
                "FileKey::from_bytes(*target.key().as_bytes())",
                "FileKey::from_bytes(*keys.name_key.as_bytes())",
                "FileKey::from_bytes(*src.key().as_bytes())",
            ] {
                assert!(!src.contains(banned), "{name} keeps an unwiped copy: {banned}");
            }
        }
    }

    /// Copying keeps the source: a wipe-in-place would break every caller that
    /// still needs the key it copied.
    #[test]
    fn key_copy_leaves_the_source_key_intact() {
        let source = FileKey::from_bytes([9u8; 32]);
        let copy = key_copy(&source);
        assert_eq!(copy.as_bytes(), source.as_bytes());
        assert_eq!(source.as_bytes(), &[9u8; 32]);
    }
}
