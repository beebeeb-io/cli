//! Chunk decryption (pinned v2 frames, JSON and raw) split out of `crypto.rs`.

use super::*;

/// Decrypt all chunks of a downloaded file under its read `pin`
/// ([`content_pin`]).
///
/// `Unrecorded` (a legacy file): handles both CLI-format (JSON `EncryptedBlob`)
/// and web-app-format (raw `nonce|ciphertext` binary) chunk storage, and both
/// UUID key derivations (binary vs string). The server streams chunk blobs
/// back-to-back. The format is never trusted from the bytes alone (a random raw
/// nonce starts with `{` about 1 time in 256): the first byte only picks which
/// parser is tried first, and the other is the fallback. Every chunk is
/// AEAD-authenticated, so a wrong parse fails and can never yield wrong
/// plaintext.
///
/// `V2`: only core's string-UUID key and raw frames, opened as v2.
///
/// Returns the reassembled plaintext on success. Callers that know the file's
/// plaintext chunk size (the `X-Chunk-Size` download header) should use
/// [`decrypt_file_chunks_with_chunk_size`].
pub fn decrypt_file_chunks(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, CryptoError> {
    decrypt_file_chunks_with_chunk_size(master_key, file_id_str, pin, encrypted_bytes, chunk_count, None)
}

/// [`decrypt_file_chunks`] with the file's uniform plaintext chunk size, when
/// the caller has it. For raw multi-chunk files this is the exact wire frame
/// size (`chunk_size + 28`); without it the frame size is recovered by
/// AEAD-validated probing (see [`decrypt_raw_chunks`]).
pub fn decrypt_file_chunks_with_chunk_size(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    encrypted_bytes: &[u8],
    chunk_count: u32,
    chunk_size: Option<u64>,
) -> Result<Vec<u8>, CryptoError> {
    decrypt_file_chunks_owned(
        master_key,
        file_id_str,
        pin,
        None,
        encrypted_bytes,
        chunk_count,
        chunk_size,
    )
    .map(|(plaintext, _)| plaintext)
}

/// [`decrypt_file_chunks_with_chunk_size`] with an explicit first candidate key
/// (owner rows resolved through [`crate::owned_keys`]) and the id convention
/// that finally opened the content, when one of core's owner helpers was used
/// (SF-0002).
///
/// The existing keys are tried first, exactly as before. Only when the first
/// chunk fails under all of them does core's
/// [`beebeeb_core::kdf::owner_file_key_for_content`] pick the per-version
/// read-only repair key, authenticated against that version's first frame.
#[allow(clippy::too_many_arguments)]
pub fn decrypt_file_chunks_owned(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    explicit_key: Option<&FileKey>,
    encrypted_bytes: &[u8],
    chunk_count: u32,
    chunk_size: Option<u64>,
) -> Result<(Vec<u8>, Option<beebeeb_core::kdf::OwnerKeyConvention>), CryptoError> {
    let file_uuid: uuid::Uuid = file_id_str.parse().map_err(|e| CryptoError::InvalidId(format!("invalid file ID: {e}")))?;

    // A v2 file is written by core, which derives its key from the UUID STRING:
    // that key only. A legacy file may be under either derivation: binary-UUID
    // first (CLI origin), then string-UUID (web origin). An explicit owner key
    // (a shared-folder or owner-wrap key resolved from the row) authenticates
    // the content on its own: try ONLY that key. Falling back to the owner's
    // derived or repair keys would let a shared reader open content that is not
    // sealed under the key the row handed it (SF-0002 review).
    let keys: Vec<FileKey> = match explicit_key {
        Some(key) => vec![key_copy(key)],
        None => match pin {
            ExpectedChunkFormat::V2 => {
                vec![beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes())]
            }
            ExpectedChunkFormat::Unrecorded => vec![
                beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes()),
                beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes()),
            ],
        },
    };

    // Attempt decryption with each key. The first successful full decryption wins.
    for file_key in &keys {
        if let Ok(plaintext) =
            try_decrypt_all_chunks_with_chunk_size(file_key, file_id_str, pin, encrypted_bytes, chunk_count, chunk_size)
        {
            return Ok((plaintext, None));
        }
    }

    // SF-0002: the first chunk failed under every written convention. Ask core
    // which read-only owner convention authenticates THIS version, then retry
    // that version under the returned key. A split file (name under the
    // uppercase convention, this version under the string one, or the reverse)
    // resolves per version. Only the owner's own (master-derived) path may take
    // this fallback; a row opened with an explicit key must never switch to an
    // owner key.
    if explicit_key.is_some() {
        return Err(CryptoError::Decrypt(match pin {
            ExpectedChunkFormat::V2 => format!(
                "failed to decrypt file {file_id_str}: its metadata records chunk format v2 and the content does not open under the key this row supplied"
            ),
            ExpectedChunkFormat::Unrecorded => {
                format!(
                    "failed to decrypt file {file_id_str}: the content does not authenticate under the key this row supplied"
                )
            }
        }));
    }
    for len in first_frame_candidates(encrypted_bytes, chunk_count, chunk_size) {
        let Some((key, convention)) = owner_content_key(master_key, file_id_str, &encrypted_bytes[..len]) else {
            continue;
        };
        if let Ok(plaintext) =
            try_decrypt_all_chunks_with_chunk_size(&key, file_id_str, pin, encrypted_bytes, chunk_count, chunk_size)
        {
            return Ok((plaintext, Some(convention)));
        }
    }

    Err(CryptoError::Decrypt(match pin {
        ExpectedChunkFormat::V2 => format!(
            "failed to decrypt file {file_id_str}: its metadata records chunk format v2 and the content does not open as v2"
        ),
        ExpectedChunkFormat::Unrecorded => {
            format!("failed to decrypt file {file_id_str}: no known owner key derivation authenticates its content")
        }
    }))
}

/// Candidate lengths for the first wire frame, used only to let core's
/// per-version owner resolver authenticate chunk 0 (SF-0002). Ordered most
/// likely first; duplicates are skipped. A single-frame file is the whole
/// payload; otherwise `X-Chunk-Size` is authoritative, and the last resort is
/// the average frame size for equal frames (the iOS File Provider wrote 1 MiB
/// frames) and the total itself.
pub(super) fn first_frame_candidates(encrypted: &[u8], chunk_count: u32, chunk_size: Option<u64>) -> Vec<usize> {
    const NONCE_AND_TAG: u64 = 28;
    const FILE_PROVIDER_CHUNK: u64 = 1024 * 1024;
    let total = encrypted.len();
    let mut out: Vec<usize> = Vec::new();
    let mut push = |v: u64| {
        if let Ok(v) = usize::try_from(v) {
            if v > 0 && v <= total && !out.contains(&v) {
                out.push(v);
            }
        }
    };
    if chunk_count <= 1 {
        push(total as u64);
        return out;
    }
    if let Some(cs) = chunk_size {
        push(cs.saturating_add(NONCE_AND_TAG));
    }
    if let Some(avg) = total.checked_div(chunk_count as usize) {
        push(avg as u64);
    }
    push(FILE_PROVIDER_CHUNK.saturating_add(NONCE_AND_TAG));
    push(total as u64);
    out
}

/// Try to decrypt all chunks with a given file key under `pin`. For a legacy
/// (`Unrecorded`) file the chunk format is NOT detected from the bytes: the
/// first byte only orders the two parsers (JSON blob vs raw `nonce|ciphertext`),
/// and the other one is the fallback. A `V2` file has raw frames only.
pub fn try_decrypt_all_chunks(
    file_key: &FileKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, CryptoError> {
    try_decrypt_all_chunks_with_chunk_size(file_key, file_id_str, pin, encrypted_bytes, chunk_count, None)
}

/// [`try_decrypt_all_chunks`] with the file's uniform plaintext chunk size, when
/// known (`X-Chunk-Size`). Only the raw parser uses it.
pub fn try_decrypt_all_chunks_with_chunk_size(
    file_key: &FileKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    encrypted_bytes: &[u8],
    chunk_count: u32,
    chunk_size: Option<u64>,
) -> Result<Vec<u8>, CryptoError> {
    if encrypted_bytes.is_empty() && chunk_count == 0 {
        return Ok(Vec::new());
    }
    if encrypted_bytes.is_empty() {
        return Err(CryptoError::Malformed("no data but chunk_count > 0".to_string()));
    }
    if pin == ExpectedChunkFormat::V2 {
        // v2 frames are raw, always: no JSON parser to fall back to.
        return decrypt_raw_chunks(file_key, file_id_str, pin, encrypted_bytes, chunk_count, chunk_size);
    }

    // The first byte is only a HINT (task 1759): a raw chunk starts with a
    // random nonce, so ~1 in 256 raw files begin with 0x7b ('{') and would be
    // misrouted to the JSON parser. Try the hinted format first, then the
    // other. A false success is impossible: every chunk is AEAD-authenticated,
    // so a wrong parse can only fail, never yield wrong plaintext.
    let raw = |k: &FileKey| decrypt_raw_chunks(k, file_id_str, pin, encrypted_bytes, chunk_count, chunk_size);
    let json = |k: &FileKey| decrypt_json_chunks(k, file_id_str, encrypted_bytes, chunk_count);
    let raw_first = encrypted_bytes[0] != b'{';
    let attempt = |as_raw: bool| {
        if as_raw { raw(file_key) } else { json(file_key) }
    };
    match attempt(raw_first) {
        Ok(p) => Ok(p),
        Err(e1) => attempt(!raw_first).map_err(|e2| {
            let (raw_err, json_err) = if raw_first { (&e1, &e2) } else { (&e2, &e1) };
            CryptoError::Decrypt(format!("raw parser: {raw_err}; json parser: {json_err}"))
        }),
    }
}

/// Parse and decrypt CLI-format chunks: concatenated JSON `EncryptedBlob`
/// objects (legacy, `Unrecorded` files only). Each blob is the frame
/// `nonce || ciphertext`, opened by core's pin-aware reader as one stream.
pub(super) fn decrypt_json_chunks(
    file_key: &FileKey,
    file_id_str: &str,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, CryptoError> {
    let mut decryptor =
        ChunkDecryptor::for_push_with_file_key(key_copy(file_key), file_id_str, ExpectedChunkFormat::Unrecorded);
    let mut plaintext = Vec::new();
    let mut offset = 0;

    for i in 0..chunk_count {
        if offset >= encrypted_bytes.len() {
            return Err(CryptoError::Malformed(format!(
                "unexpected end of data at chunk {i}/{chunk_count} (offset {offset})"
            )));
        }

        let remaining = &encrypted_bytes[offset..];
        let mut de = serde_json::Deserializer::from_slice(remaining).into_iter::<EncryptedBlob>();

        let blob = match de.next() {
            Some(Ok(b)) => b,
            Some(Err(e)) => return Err(CryptoError::Malformed(format!("parse chunk {i}: {e}"))),
            None => return Err(CryptoError::Malformed(format!("no data for chunk {i}"))),
        };
        offset += de.byte_offset();

        let mut frame = blob.nonce;
        frame.extend_from_slice(&blob.ciphertext);
        // Wiped on drop: only the copy appended to `plaintext` (which the caller owns) survives.
        let opened = zeroize::Zeroizing::new(
            decryptor
                .push_frame(&frame)
                .map_err(|e| CryptoError::Decrypt(format!("decrypt chunk {i}: {e}")))?
                .data,
        );
        plaintext.extend_from_slice(&opened);
    }
    decryptor.finish().map_err(|e| CryptoError::Malformed(format!("incomplete file: {e}")))?;

    Ok(plaintext)
}

/// Parse and decrypt web-app-format chunks: raw `nonce (12 bytes) | ciphertext`.
///
/// Chunks are stored back-to-back with no length prefix. Every chunk but the
/// last is exactly `chunk_size + 28` bytes (nonce + ciphertext + tag); the last
/// is the remainder and is normally SHORTER, so the frame size cannot be
/// derived as `total / count` (task 1759 round 2). It is taken from `chunk_size`
/// when the caller knows it, otherwise recovered by AEAD-validated probing
/// (see [`raw_frame_size`]). The frames are then opened in order by core's
/// pin-aware reader as ONE stream, which also rejects a v2 file that is
/// reordered, mixes uploads, or ends before its last chunk.
pub(super) fn decrypt_raw_chunks(
    file_key: &FileKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    encrypted_bytes: &[u8],
    chunk_count: u32,
    chunk_size: Option<u64>,
) -> Result<Vec<u8>, CryptoError> {
    let total = encrypted_bytes.len();
    let count = chunk_count as usize;

    if count == 0 {
        return Ok(Vec::new());
    }

    // Minimum valid chunk: NONCE_LEN + TAG_LEN (empty plaintext encrypted).
    let min_chunk_overhead = NONCE_LEN + TAG_LEN;
    let min_total = count
        .checked_mul(min_chunk_overhead)
        .ok_or_else(|| CryptoError::Malformed(format!("chunk count {count} is too large")))?;
    if total < min_total {
        return Err(CryptoError::Malformed(format!(
            "raw data too short: {total} bytes for {count} chunks (minimum {min_total})"
        )));
    }

    // A single chunk is the whole buffer; otherwise find the uniform frame size.
    let frame_size = if count == 1 {
        total
    } else {
        raw_frame_size(file_key, file_id_str, pin, encrypted_bytes, count, chunk_size)?
    };

    let mut decryptor = ChunkDecryptor::for_push_with_file_key(key_copy(file_key), file_id_str, pin);
    let mut plaintext = Vec::with_capacity(total);
    let mut offset = 0;

    for i in 0..count {
        let remaining = total - offset;
        // Last chunk takes whatever is left; earlier chunks take the frame size.
        let this_chunk_size = if i == count - 1 { remaining } else { frame_size };

        if this_chunk_size < min_chunk_overhead || this_chunk_size > remaining {
            return Err(CryptoError::Malformed(format!(
                "chunk {i} has an invalid size: {this_chunk_size} bytes (minimum {min_chunk_overhead}, {remaining} left)"
            )));
        }

        let opened = zeroize::Zeroizing::new(
            decryptor
                .push_frame(&encrypted_bytes[offset..offset + this_chunk_size])
                .map_err(|e| CryptoError::Decrypt(format!("decrypt raw chunk {i}: {e}")))?
                .data,
        );
        plaintext.extend_from_slice(&opened);

        offset += this_chunk_size;
    }
    decryptor.finish().map_err(|e| CryptoError::Malformed(format!("incomplete file: {e}")))?;

    Ok(plaintext)
}
