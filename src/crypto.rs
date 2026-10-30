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
//! [`decrypt_file_chunks`] handles all combinations transparently. The format is
//! never trusted from the first byte alone (a raw nonce starts with `{` 1 time
//! in 256): both parsers are tried, and AEAD authentication is the arbiter.
//!
//! **Chunk format pin (tasks 1908 / 1943).** Every file-content frame is opened
//! by core's pin-aware reader ([`ChunkDecryptor`], [`open_chunk`]) under the
//! [`ExpectedChunkFormat`] the file's AUTHENTICATED metadata records
//! ([`content_pin`]), never a server field and never the generic
//! `decrypt_chunk(file_key, blob)`. A v2-pinned file opens only as v2: one
//! key (core's string-UUID derivation), raw frames, no JSON parser and no v1
//! fallback. The legacy combinations above apply only to `Unrecorded` files.

use base64::Engine as _;
use beebeeb_core::chunk_stream::{ChunkDecryptor, ChunkFormat, ExpectedChunkFormat, open_chunk};
use beebeeb_core::kdf::FileKey;
use beebeeb_types::EncryptedBlob;

/// Intermediate struct for deserializing the web-app blob format where nonce
/// and ciphertext are base64-encoded strings instead of byte arrays.
#[derive(serde::Deserialize)]
struct WebAppBlob {
    nonce: String,
    ciphertext: String,
}

/// Plain, user-facing explanation for a file that the SF-0002 read-only
/// repair convention could open (iOS Files-app create sealed under an
/// uppercase id). Shared by every CLI surface that refuses a write.
pub const OWNER_REPAIR_READ_ONLY: &str =
    "This file was saved by an older version of the iOS app. You can open and download it. To change it, save a copy.";

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
    decrypt_name_with_convention(master_key, file_id_str, name_encrypted_str).map(|(name, _)| name)
}

/// [`decrypt_name`] that also reports which id convention authenticated the
/// name (SF-0002). A returned [`beebeeb_core::kdf::OwnerKeyConvention::UpperRepair`]
/// means the row was sealed by the iOS File Provider under an uppercase id:
/// it is READ-ONLY on this client (see [`OWNER_REPAIR_READ_ONLY`]).
///
/// The client's existing decrypt paths run first and are unchanged. Only when
/// they all fail for an owner row is core's
/// [`beebeeb_core::kdf::owner_file_key_for_name`] asked for the read-only
/// repair key, so no ordinary row changes behaviour.
pub fn decrypt_name_with_convention(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> Option<(String, beebeeb_core::kdf::OwnerKeyConvention)> {
    use beebeeb_core::kdf::OwnerKeyConvention;

    // Format 3 (fast path): Plaintext — not JSON at all.
    if !name_encrypted_str.starts_with('{') {
        return Some((name_encrypted_str.to_string(), OwnerKeyConvention::String));
    }

    // Core first (task 1943): it also opens envelopes sealed under the 1908
    // name subkey, which every v2 file's metadata is. The paths below stay for
    // the legacy blobs core does not parse.
    if let Ok(meta) = beebeeb_core::encrypt::decrypt_name_metadata(master_key, file_id_str, name_encrypted_str) {
        return Some((meta.name, OwnerKeyConvention::String));
    }

    if let Ok(file_uuid) = file_id_str.parse::<uuid::Uuid>() {
        // The web app derives file keys using the UUID *string* as UTF-8 bytes
        // (TextEncoder.encode(fileId)), while the CLI/core uses the UUID's 16-byte
        // binary form (uuid.as_bytes()). We must try both derivations since files
        // can originate from either client.
        let key_from_string = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());
        let key_from_binary = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

        // Try each key derivation against each blob format.
        for (file_key, convention) in [
            (&key_from_string, OwnerKeyConvention::String),
            (&key_from_binary, OwnerKeyConvention::Binary),
        ] {
            if let Some(name) = decrypt_name_with_key(file_key, name_encrypted_str) {
                return Some((name, convention));
            }
        }
    }

    // SF-0002: read-only repair. Only the owner can derive this key, and only
    // when the name still fails under both written conventions above.
    if let Ok((file_key, convention)) =
        beebeeb_core::kdf::owner_file_key_for_name(master_key, file_id_str, name_encrypted_str)
    {
        if let Some(name) = decrypt_name_with_key(&file_key, name_encrypted_str) {
            return Some((name, convention));
        }
    }

    None
}

/// SF-0002: does this row's NAME authenticate only under the read-only
/// uppercase repair convention? The flag a mount entry carries; a caller that
/// can also read content must check chunk 0 as well before a write.
#[cfg_attr(not(feature = "fuse"), allow(dead_code))] // only the mount path asks
pub fn name_is_upper_repair(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> bool {
    decrypt_name_with_convention(master_key, file_id_str, name_encrypted_str)
        .is_some_and(|(_, convention)| convention == beebeeb_core::kdf::OwnerKeyConvention::UpperRepair)
}

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

/// Batch [`decrypt_name`] — decrypt many `(file_id, name_encrypted)` pairs in
/// PARALLEL (rayon, task 0810). Per-item semantics are IDENTICAL to
/// [`decrypt_name`] (all three blob formats + both UUID-key derivations + the
/// plaintext fast-path); `None` for an item that looks encrypted but won't
/// decrypt. Result order matches the input.
///
/// Deliberately wraps the CLI's own all-format `decrypt_name`, NOT
/// `beebeeb_core::encrypt::decrypt_names`: core only parses the Rust
/// `EncryptedBlob` (number-array) format. Current web AND mobile emit exactly
/// that format, so core would handle them — but the CLI also reads the LEGACY
/// base64 `WebAppBlob` (format 2) from OLD web uploads, which core can't parse.
/// Keeping the CLI's all-format coverage is what keeps `bb ls`/`bb search` output
/// byte-identical for those legacy files.
#[cfg(test)]
pub fn decrypt_names(master_key: &beebeeb_core::kdf::MasterKey, items: &[(&str, &str)]) -> Vec<Option<String>> {
    use rayon::prelude::*;
    items
        .par_iter()
        .map(|(file_id, name_encrypted)| decrypt_name(master_key, file_id, name_encrypted))
        .collect()
}

/// Decrypt a `name_encrypted` value with a **known** file key (rather than one
/// derived from the master key + file id). Used for file-request uploads, whose
/// content key `C` is recovered via the request-key path — the filename is then
/// encrypted directly under `FileKey(C)`, not under a master-derived key.
///
/// Handles both server blob formats (Rust `EncryptedBlob` JSON and the web-app
/// base64 `{nonce, ciphertext}` blob) and the plaintext fast-path.
pub fn decrypt_name_with_key(file_key: &beebeeb_core::kdf::FileKey, name_encrypted_str: &str) -> Option<String> {
    // Plaintext fast path — not JSON at all.
    if !name_encrypted_str.starts_with('{') {
        return Some(name_encrypted_str.to_string());
    }

    // Core first: name-subkey envelopes (task 1943), then the legacy formats.
    if let Ok(meta) = beebeeb_core::encrypt::decrypt_name_metadata_with_key(file_key, name_encrypted_str) {
        return Some(meta.name);
    }

    // Format 1: Rust-native EncryptedBlob (cipher_suite + byte-array fields).
    if let Ok(blob) = serde_json::from_str::<EncryptedBlob>(name_encrypted_str) {
        if let Ok(name) = beebeeb_core::encrypt::decrypt_metadata(file_key, &blob) {
            return Some(unwrap_metadata_json(&name));
        }
    }

    // Format 2: Web-app blob (nonce + ciphertext as base64 strings).
    if let Ok(web_blob) = serde_json::from_str::<WebAppBlob>(name_encrypted_str) {
        let b64 = base64::engine::general_purpose::STANDARD;
        if let (Ok(nonce), Ok(ciphertext)) = (b64.decode(&web_blob.nonce), b64.decode(&web_blob.ciphertext)) {
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
) -> Result<ExpectedChunkFormat, String> {
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
) -> Result<ExpectedChunkFormat, String> {
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
) -> Result<ExpectedChunkFormat, String> {
    match content_key {
        Some(c) => content_pin_with_key(c, file_id_str, name_encrypted_str),
        None => content_pin(master_key, file_id_str, name_encrypted_str),
    }
}

fn pin_error(file_id: &str, e: impl std::fmt::Display) -> String {
    format!(
        "cannot read the encrypted metadata of file {file_id} ({e}), so its chunk format is unknown; refusing to guess"
    )
}

/// A second owned handle on a file key, for the decryptor that takes one by value.
fn key_copy(file_key: &FileKey) -> FileKey {
    FileKey::from_bytes(*file_key.as_bytes())
}

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
) -> Result<Vec<u8>, String> {
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
) -> Result<Vec<u8>, String> {
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
) -> Result<(Vec<u8>, Option<beebeeb_core::kdf::OwnerKeyConvention>), String> {
    let file_uuid: uuid::Uuid = file_id_str.parse().map_err(|e| format!("invalid file ID: {e}"))?;

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
        return Err(match pin {
            ExpectedChunkFormat::V2 => format!(
                "failed to decrypt file {file_id_str}: its metadata records chunk format v2 and the content does not open under the key this row supplied"
            ),
            ExpectedChunkFormat::Unrecorded => {
                format!(
                    "failed to decrypt file {file_id_str}: the content does not authenticate under the key this row supplied"
                )
            }
        });
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

    Err(match pin {
        ExpectedChunkFormat::V2 => format!(
            "failed to decrypt file {file_id_str}: its metadata records chunk format v2 and the content does not open as v2"
        ),
        ExpectedChunkFormat::Unrecorded => {
            format!("failed to decrypt file {file_id_str}: no known owner key derivation authenticates its content")
        }
    })
}

/// Candidate lengths for the first wire frame, used only to let core's
/// per-version owner resolver authenticate chunk 0 (SF-0002). Ordered most
/// likely first; duplicates are skipped. A single-frame file is the whole
/// payload; otherwise `X-Chunk-Size` is authoritative, and the last resort is
/// the average frame size for equal frames (the iOS File Provider wrote 1 MiB
/// frames) and the total itself.
fn first_frame_candidates(encrypted: &[u8], chunk_count: u32, chunk_size: Option<u64>) -> Vec<usize> {
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
) -> Result<Vec<u8>, String> {
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
) -> Result<Vec<u8>, String> {
    if encrypted_bytes.is_empty() && chunk_count == 0 {
        return Ok(Vec::new());
    }
    if encrypted_bytes.is_empty() {
        return Err("no data but chunk_count > 0".to_string());
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
            format!("raw parser: {raw_err}; json parser: {json_err}")
        }),
    }
}

/// Parse and decrypt CLI-format chunks: concatenated JSON `EncryptedBlob`
/// objects (legacy, `Unrecorded` files only). Each blob is the frame
/// `nonce || ciphertext`, opened by core's pin-aware reader as one stream.
fn decrypt_json_chunks(
    file_key: &FileKey,
    file_id_str: &str,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    let mut decryptor =
        ChunkDecryptor::for_push_with_file_key(key_copy(file_key), file_id_str, ExpectedChunkFormat::Unrecorded);
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

        let mut frame = blob.nonce;
        frame.extend_from_slice(&blob.ciphertext);
        // Wiped on drop: only the copy appended to `plaintext` (which the caller owns) survives.
        let opened = zeroize::Zeroizing::new(
            decryptor
                .push_frame(&frame)
                .map_err(|e| format!("decrypt chunk {i}: {e}"))?
                .data,
        );
        plaintext.extend_from_slice(&opened);
    }
    decryptor.finish().map_err(|e| format!("incomplete file: {e}"))?;

    Ok(plaintext)
}

/// Upper bound on the AEAD work spent probing for the frame size when the
/// caller has no chunk-size metadata (bytes of ciphertext authenticated). This
/// bounds the exhaustive scan (step 4 of [`raw_frame_size`]) only: the known
/// size and the power-of-two ladder always run in full.
const FRAME_PROBE_BUDGET: u64 = 64 * 1024 * 1024;

/// Plaintext chunk sizes the platform has ever emitted as powers of two: 1 KiB
/// up to 256 MiB (`beebeeb_types` chunk planner is 4 MiB..256 MiB; 1 MiB is the
/// pre-V2 fixed size). Probed first when no chunk size is known.
fn ladder_frame_sizes() -> impl Iterator<Item = u64> {
    (10..=28u32).map(|k| (1u64 << k) + FRAME_OVERHEAD)
}

/// Per-frame AEAD overhead: `nonce (12) + tag (16)`.
const FRAME_OVERHEAD: u64 = (NONCE_LEN + TAG_LEN) as u64;

/// Does `frame` authenticate as frame 0 (not last) of a multi-chunk file under a
/// format `pin` allows? Core's random-access opener; only the verdict is kept.
/// A v2 frame 0's own nonce is the upload's version id.
fn opens_as_first_frame(file_key: &FileKey, file_id_str: &str, pin: ExpectedChunkFormat, frame: &[u8]) -> bool {
    let formats: &[ChunkFormat] = match pin {
        ExpectedChunkFormat::V2 => &[ChunkFormat::V2],
        ExpectedChunkFormat::Unrecorded => &[ChunkFormat::V2, ChunkFormat::V1Legacy],
    };
    let version_id = frame.get(..NONCE_LEN).unwrap_or_default();
    formats.iter().any(|&format| {
        open_chunk(file_key, file_id_str, version_id, 0, false, format, frame)
            .map(zeroize::Zeroizing::new) // wiped on drop; only the verdict is kept
            .is_ok()
    })
}

/// Work out the wire frame size of the first `count - 1` frames of a raw
/// multi-chunk file (every frame but the last is `chunk_size + 28` bytes; the
/// last holds the remainder and is usually shorter).
///
/// The wire carries no per-chunk lengths, so the size comes from, in order:
/// 1. `chunk_size` (the `X-Chunk-Size` header), when the caller has it;
/// 2. the powers of two the planner emits (1 KiB..256 MiB);
/// 3. the uniform split `ceil(total / count)` (every chunk the same size);
/// 4. every other size the byte count allows, until the scan budget
///    [`FRAME_PROBE_BUDGET`] (ciphertext bytes authenticated) is spent.
///
/// A candidate is accepted only if the first frame AUTHENTICATES under the file
/// key (in a format the pin allows), so a wrong guess costs time and can never
/// produce wrong plaintext.
fn raw_frame_size(
    file_key: &FileKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    data: &[u8],
    count: usize,
    chunk_size: Option<u64>,
) -> Result<usize, String> {
    let total = data.len() as u64;
    let n = count as u64;
    // total = (n-1)*F + last, 28 <= last <= F  =>  F in [ceil(total/n), (total-28)/(n-1)].
    let lo = total.div_ceil(n);
    let hi = total.saturating_sub(FRAME_OVERHEAD) / (n - 1);
    if lo > hi {
        return Err(format!(
            "raw data of {total} bytes cannot be split into {count} chunks of >= {FRAME_OVERHEAD} bytes"
        ));
    }

    let mut probe = FrameProbe {
        file_key,
        file_id_str,
        pin,
        data,
        lo,
        hi,
        tried: Vec::new(),
        spent: 0,
    };

    if let Some(cs) = chunk_size.and_then(|cs| cs.checked_add(FRAME_OVERHEAD))
        && probe.accepts(cs)
    {
        return Ok(cs as usize);
    }
    for f in ladder_frame_sizes() {
        if probe.accepts(f) {
            return Ok(f as usize);
        }
    }
    if probe.accepts(lo) {
        return Ok(lo as usize);
    }
    let mut f = lo;
    while f <= hi && probe.spent < FRAME_PROBE_BUDGET {
        if probe.accepts(f) {
            return Ok(f as usize);
        }
        f += 1;
    }
    Err(format!(
        "no chunk size splits {total} bytes into {count} authenticating chunks \
         (tried {} candidate frame sizes)",
        probe.tried.len()
    ))
}

/// Candidate-frame-size prober for [`raw_frame_size`].
struct FrameProbe<'a> {
    file_key: &'a FileKey,
    file_id_str: &'a str,
    pin: ExpectedChunkFormat,
    data: &'a [u8],
    lo: u64,
    hi: u64,
    tried: Vec<u64>,
    /// Ciphertext bytes authenticated so far (against [`FRAME_PROBE_BUDGET`]).
    spent: u64,
}

impl FrameProbe<'_> {
    /// True if `f` is a feasible frame size (the byte count allows it) whose
    /// first frame authenticates. Each size is tried at most once.
    fn accepts(&mut self, f: u64) -> bool {
        if f < self.lo || f > self.hi || self.tried.contains(&f) {
            return false;
        }
        self.tried.push(f);
        self.spent = self.spent.saturating_add(f);
        opens_as_first_frame(self.file_key, self.file_id_str, self.pin, &self.data[..f as usize])
    }
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
fn decrypt_raw_chunks(
    file_key: &FileKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    encrypted_bytes: &[u8],
    chunk_count: u32,
    chunk_size: Option<u64>,
) -> Result<Vec<u8>, String> {
    let total = encrypted_bytes.len();
    let count = chunk_count as usize;

    if count == 0 {
        return Ok(Vec::new());
    }

    // Minimum valid chunk: NONCE_LEN + TAG_LEN (empty plaintext encrypted).
    let min_chunk_overhead = NONCE_LEN + TAG_LEN;
    let min_total = count
        .checked_mul(min_chunk_overhead)
        .ok_or_else(|| format!("chunk count {count} is too large"))?;
    if total < min_total {
        return Err(format!(
            "raw data too short: {total} bytes for {count} chunks (minimum {min_total})"
        ));
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
            return Err(format!(
                "chunk {i} has an invalid size: {this_chunk_size} bytes (minimum {min_chunk_overhead}, {remaining} left)"
            ));
        }

        let opened = zeroize::Zeroizing::new(
            decryptor
                .push_frame(&encrypted_bytes[offset..offset + this_chunk_size])
                .map_err(|e| format!("decrypt raw chunk {i}: {e}"))?
                .data,
        );
        plaintext.extend_from_slice(&opened);

        offset += this_chunk_size;
    }
    decryptor.finish().map_err(|e| format!("incomplete file: {e}"))?;

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use beebeeb_core::chunk_stream::ChunkDecryptor;
    use beebeeb_core::encrypt::{encrypt_chunk, encrypt_chunk_raw};
    use beebeeb_core::kdf::{MasterKey, OwnerKeyConvention, derive_file_key, derive_master_key};

    /// Task 1759: a raw-format file whose random nonce starts with 0x7b must
    /// still decrypt (was misrouted to the JSON parser: "key must be a string").
    #[test]
    fn raw_chunk_with_brace_first_byte_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"nonce starts with a brace";
        let frame = (0..100_000)
            .map(|_| encrypt_chunk_raw(&fk, pt).unwrap())
            .find(|f| f[0] == b'{')
            .expect("a nonce starting with 0x7b within 100k tries");
        assert_eq!(frame[0], 0x7b);
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &frame,
                1
            )
            .unwrap(),
            pt
        );
    }

    /// Multi-chunk variant: only the first chunk's nonce byte matters.
    #[test]
    fn raw_multi_chunk_with_brace_first_byte_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let first = (0..100_000)
            .map(|_| encrypt_chunk_raw(&fk, b"AAAAAAAA").unwrap())
            .find(|f| f[0] == b'{')
            .unwrap();
        let mut all = first;
        all.extend(encrypt_chunk_raw(&fk, b"BBBBBBBB").unwrap());
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &all,
                2
            )
            .unwrap(),
            b"AAAAAAAABBBBBBBB"
        );
    }

    /// Encrypt `plaintext` as the platform's raw multi-chunk wire format:
    /// uniform `chunk_size` plaintext chunks, the last one shorter. When
    /// `brace_first` is set, the FIRST frame is re-rolled until its random
    /// nonce starts with 0x7b ('{'), the byte that used to misroute the file.
    fn raw_file(fk: &beebeeb_core::kdf::FileKey, plaintext: &[u8], chunk_size: usize, brace_first: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, chunk) in plaintext.chunks(chunk_size).enumerate() {
            let frame = if i == 0 && brace_first {
                (0..100_000)
                    .map(|_| encrypt_chunk_raw(fk, chunk).unwrap())
                    .find(|f| f[0] == b'{')
                    .expect("a nonce starting with 0x7b within 100k tries")
            } else {
                encrypt_chunk_raw(fk, chunk).unwrap()
            };
            out.extend_from_slice(&frame);
        }
        out
    }

    /// Task 1759 round 2 (P1-a): a raw multi-chunk file whose LAST chunk is
    /// shorter than the rest, the normal case for any file that is not an
    /// exact multiple of the chunk size, must decrypt. The old splitter used
    /// `total / count` for every frame and mis-sized frame 0.
    #[test]
    fn raw_multi_chunk_uneven_last_chunk_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt: Vec<u8> = (0..(3 * 64 + 10)).map(|i| (i % 251) as u8).collect();
        let wire = raw_file(&fk, &pt, 64, false);
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .unwrap(),
            pt
        );
        assert_eq!(
            decrypt_file_chunks(
                &m,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .unwrap(),
            pt
        );
    }

    /// Same, with a '{' first nonce byte on frame 0: both defects at once.
    #[test]
    fn raw_multi_chunk_uneven_last_chunk_with_brace_first_byte_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt: Vec<u8> = (0..(3 * 64 + 10)).map(|i| (i % 251) as u8).collect();
        let wire = raw_file(&fk, &pt, 64, true);
        assert_eq!(wire[0], b'{');
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .unwrap(),
            pt
        );
        assert_eq!(
            decrypt_file_chunks(
                &m,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .unwrap(),
            pt
        );
    }

    /// Frame size recovered from the power-of-two ladder: a pre-V2 1 MiB chunk
    /// file with an uneven last chunk, and a 4 MiB (planner minimum) one.
    #[test]
    fn raw_multi_chunk_ladder_sizes_uneven_last_chunk() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        for cs in [1usize << 20, 4usize << 20] {
            let pt: Vec<u8> = (0..(2 * cs + 12_345)).map(|i| (i % 253) as u8).collect();
            let wire = raw_file(&fk, &pt, cs, false);
            assert_eq!(
                try_decrypt_all_chunks(
                    &fk,
                    FID,
                    beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                    &wire,
                    3
                )
                .unwrap(),
                pt,
                "chunk size {cs}"
            );
        }
    }

    /// Two chunks with a tiny last chunk: the widest feasible range, where the
    /// uniform split `total / 2` is wrong by a lot.
    #[test]
    fn raw_two_chunks_tiny_last_chunk() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = vec![7u8; 1000 + 1];
        let wire = raw_file(&fk, &pt, 1000, true);
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                2
            )
            .unwrap(),
            pt
        );
    }

    /// With the caller-supplied chunk size the frame is exact (no probing), and
    /// a WRONG hint is not trusted: it falls back to probing and still decrypts.
    #[test]
    fn raw_multi_chunk_chunk_size_hint_exact_and_wrong() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt: Vec<u8> = (0..(5 * 100 + 3)).map(|i| (i % 249) as u8).collect();
        let wire = raw_file(&fk, &pt, 100, true);
        assert_eq!(
            try_decrypt_all_chunks_with_chunk_size(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                6,
                Some(100)
            )
            .unwrap(),
            pt
        );
        assert_eq!(
            try_decrypt_all_chunks_with_chunk_size(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                6,
                Some(4096)
            )
            .unwrap(),
            pt
        );
        assert_eq!(
            decrypt_file_chunks_with_chunk_size(
                &m,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                6,
                Some(100)
            )
            .unwrap(),
            pt
        );
    }

    /// A raw file with a corrupted middle chunk fails (no partial plaintext),
    /// and a wrong key on an uneven multi-chunk file fails.
    #[test]
    fn raw_multi_chunk_corruption_and_wrong_key_fail() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = vec![9u8; 3 * 64 + 10];
        let mut wire = raw_file(&fk, &pt, 64, false);
        assert!(
            try_decrypt_all_chunks(
                &derive_file_key(&m, b"other"),
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .is_err()
        );
        let mid = 64 + 28 + 20;
        wire[mid] ^= 0xff;
        assert!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .is_err()
        );
    }

    /// P2-c: when neither parser works, the error names BOTH reasons.
    #[test]
    fn failure_reports_both_parser_errors() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let junk = vec![0x41u8; 200];
        let err = try_decrypt_all_chunks(
            &fk,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &junk,
            2,
        )
        .unwrap_err();
        assert!(err.contains("raw parser:") && err.contains("json parser:"), "{err}");
    }

    /// A wrong key must still fail on both parsers (no false success).
    #[test]
    fn brace_first_byte_wrong_key_still_fails() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let other = derive_file_key(&m, b"other");
        let frame = (0..100_000)
            .map(|_| encrypt_chunk_raw(&fk, b"x").unwrap())
            .find(|f| f[0] == b'{')
            .unwrap();
        assert!(
            try_decrypt_all_chunks(
                &other,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &frame,
                1
            )
            .is_err()
        );
    }

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
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &frame,
            1,
        )
        .unwrap();
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
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &frame,
            1,
        )
        .unwrap();
        assert_eq!(out, pt);
    }

    /// LEGACY CLI JSON-blob chunk format (first byte `{`): decrypted via the
    /// buffered path. The streaming download tries the raw frames first; when
    /// frame 0 fails to authenticate it hands the whole payload to this path.
    #[test]
    fn json_blob_legacy_format_detected_and_decrypted() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"legacy json blob chunk";
        let blob = encrypt_chunk(&fk, pt).unwrap();
        let json = serde_json::to_vec(&blob).unwrap();
        assert_eq!(json[0], b'{', "JSON-blob detection relies on a leading brace");
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &json,
            1,
        )
        .unwrap();
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
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &json,
            1,
        )
        .unwrap();
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
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &payload,
            3,
        )
        .unwrap();
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
        assert!(
            decrypt_file_chunks(
                &wrong,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &frame,
                1
            )
            .is_err()
        );
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
        let mut dec = ChunkDecryptor::for_push(&m, FID, beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded);
        let streamed = dec.push_frame(&frame).unwrap().data;
        let buffered = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &frame,
            1,
        )
        .unwrap();
        assert_eq!(streamed, buffered);
        assert_eq!(streamed, pt);
    }

    /// `decrypt_name` handles the plaintext fallback (non-JSON value).
    #[test]
    fn decrypt_name_plaintext_passthrough() {
        let m = mk();
        assert_eq!(decrypt_name(&m, FID, "Documents").as_deref(), Some("Documents"));
    }

    /// Batch `decrypt_names` is byte-identical to per-item `decrypt_name`, order-
    /// preserving, across encrypted / plaintext / undecryptable items (task 0810).
    #[test]
    fn decrypt_names_batch_matches_single() {
        use beebeeb_core::encrypt::encrypt_name;
        let m = mk();
        let id1 = "11111111-1111-2222-3333-444455556666";
        let id2 = "22222222-1111-2222-3333-444455556666";
        let id3 = "33333333-1111-2222-3333-444455556666";
        // id1: Rust-format encrypted name (string-UUID key form, as encrypt_name uses).
        let enc1 = encrypt_name(&m, id1, "report.pdf", Some("application/pdf")).unwrap();
        // id2: plaintext name (fast path). id3: well-formed-but-undecryptable blob.
        let enc2 = "Documents".to_string();
        let enc3 = r#"{"cipher_suite":"V1Aes256Gcm","nonce":[1,2,3],"ciphertext":[9,9,9,9]}"#.to_string();

        let items: Vec<(&str, &str)> = vec![(id1, enc1.as_str()), (id2, enc2.as_str()), (id3, enc3.as_str())];
        let batch = decrypt_names(&m, &items);
        assert_eq!(batch.len(), 3);
        for ((id, enc), got) in items.iter().zip(&batch) {
            assert_eq!(
                *got,
                decrypt_name(&m, id, enc),
                "batch item must equal single decrypt_name"
            );
        }
        assert_eq!(batch[0].as_deref(), Some("report.pdf"));
        assert_eq!(batch[1].as_deref(), Some("Documents"));
        assert_eq!(batch[2], None);
    }

    // ── Task 1943: cross-client vectors for the 1908 core read path ────────

    /// Regenerates `tests/fixtures/core_1908_vectors.json`'s `v2_file` with
    /// core's PUBLIC v2 writer path (a replace of a v2-pinned file:
    /// `with_recorded_pin(V2)` + `encrypt_name_with_chunk_format(V2)`). Run
    /// only on purpose (`cargo test regenerate_core_1908_v2_fixture -- --ignored`)
    /// and commit the output: the vector tests read the PINNED bytes, so a later
    /// core or CLI that drifts from them fails instead of round-tripping with
    /// itself. Core publishes no v2 content vector in `test-vectors/vectors.json`
    /// yet (v4 has only the v1 `chunk_encrypt_decrypt`).
    #[test]
    #[ignore]
    fn regenerate_core_1908_v2_fixture() {
        use beebeeb_core::chunk_stream::{ChunkEncryptor, ExpectedChunkFormat};
        let master = [0x42u8; 32];
        let m = MasterKey::from_bytes(master);
        let file_id = "1908c0de-0000-4000-8000-000000000943";
        // 3 chunks of 64 B plaintext + a 10 B tail: frame splitting, the last
        // flag and the version id all matter.
        let plaintext: Vec<u8> = (0..202u32).map(|i| (i * 7 + 3) as u8).collect();
        let chunk_size = 64u64;
        let mut enc = ChunkEncryptor::for_push_with_chunk_size(&m, file_id, plaintext.len() as u64, chunk_size)
            .unwrap()
            .with_recorded_pin(ExpectedChunkFormat::V2)
            .unwrap();
        let mut wire = Vec::new();
        for part in plaintext.chunks(chunk_size as usize) {
            wire.extend(enc.push_chunk(part).unwrap().data);
        }
        let summary = enc.finish().unwrap();
        let name_encrypted = beebeeb_core::encrypt::encrypt_name_with_chunk_format(
            &m,
            file_id,
            "core-1908-v2.txt",
            Some("text/plain"),
            ExpectedChunkFormat::V2,
        )
        .unwrap();
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let v2 = serde_json::json!({
            "master_key_hex": hex(&master),
            "file_id": file_id,
            "name": "core-1908-v2.txt",
            "mime_type": "text/plain",
            "name_encrypted": name_encrypted,
            "chunk_size": chunk_size,
            "chunk_count": summary.chunk_count,
            "wire_hex": hex(&wire),
            "plaintext_hex": hex(&plaintext),
        });
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/core_1908_vectors.json");
        let mut doc: serde_json::Value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        doc["v2_file"] = v2;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
        println!("wrote {}", path.display());
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// `tests/fixtures/core_1908_vectors.json` (see its `_provenance`).
    fn vectors() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/core_1908_vectors.json")).unwrap()
    }

    /// The v2 file of the fixture: (master key, file id, name_encrypted, wire,
    /// chunk count, chunk size, plaintext).
    fn v2_file() -> (MasterKey, String, String, Vec<u8>, u32, u64, Vec<u8>) {
        let v = &vectors()["v2_file"];
        let mut mk = [0u8; 32];
        mk.copy_from_slice(&unhex(v["master_key_hex"].as_str().unwrap()));
        (
            MasterKey::from_bytes(mk),
            v["file_id"].as_str().unwrap().to_string(),
            v["name_encrypted"].as_str().unwrap().to_string(),
            unhex(v["wire_hex"].as_str().unwrap()),
            v["chunk_count"].as_u64().unwrap() as u32,
            v["chunk_size"].as_u64().unwrap(),
            unhex(v["plaintext_hex"].as_str().unwrap()),
        )
    }

    /// Cross-client vector (task 1943): a file written by core's v2 writer opens
    /// through the CLI read path: its pin reads as V2, its name decrypts (name
    /// subkey), and its content decrypts with and without the chunk size.
    #[test]
    fn core_1908_v2_fixture_opens_through_the_pin_aware_reader() {
        let (m, fid, name_enc, wire, count, cs, plaintext) = v2_file();
        let pin = content_pin(&m, &fid, &name_enc).expect("the metadata authenticates");
        assert_eq!(pin, ExpectedChunkFormat::V2);
        assert_eq!(decrypt_name(&m, &fid, &name_enc).as_deref(), Some("core-1908-v2.txt"));
        assert_eq!(
            decrypt_file_chunks_with_chunk_size(&m, &fid, pin, &wire, count, Some(cs)).unwrap(),
            plaintext
        );
        // No X-Chunk-Size: the frame size is probed with core's v2 opener.
        assert_eq!(decrypt_file_chunks(&m, &fid, pin, &wire, count).unwrap(), plaintext);
    }

    /// Cross-client vector: core's v1 chunk vector (verbatim from core's
    /// `test-vectors/vectors.json`) still opens, as a legacy (`Unrecorded`)
    /// file. v1 binds no file id, so any id reads it.
    #[test]
    fn core_v1_chunk_vector_still_opens() {
        let v = &vectors()["v1_chunk_vector"];
        let mut key = [0u8; 32];
        key.copy_from_slice(&unhex(v["file_key_hex"].as_str().unwrap()));
        let mut frame = unhex(v["nonce_hex"].as_str().unwrap());
        frame.extend(unhex(v["ciphertext_hex"].as_str().unwrap()));
        let got = try_decrypt_all_chunks(
            &FileKey::from_bytes(key),
            FID,
            ExpectedChunkFormat::Unrecorded,
            &frame,
            1,
        );
        assert_eq!(got.unwrap(), unhex(v["plaintext_hex"].as_str().unwrap()));
    }

    /// Downgrade guard: under a V2 pin, v1 frames of the very same file key and
    /// plaintext do not open (no detection, no v1 fallback, no JSON parser),
    /// while the same bytes open as a legacy file.
    #[test]
    fn a_v2_pinned_file_refuses_v1_frames() {
        let (m, fid, _, _, _, _, plaintext) = v2_file();
        let fk = derive_file_key(&m, fid.as_bytes());
        let raw_v1 = encrypt_chunk_raw(&fk, &plaintext).unwrap();
        let json_v1 = serde_json::to_vec(&encrypt_chunk(&fk, &plaintext).unwrap()).unwrap();
        for (what, wire) in [("raw", &raw_v1), ("json", &json_v1)] {
            assert!(
                decrypt_file_chunks(&m, &fid, ExpectedChunkFormat::V2, wire, 1).is_err(),
                "{what} v1 frame opened under a v2 pin"
            );
            assert_eq!(
                decrypt_file_chunks(&m, &fid, ExpectedChunkFormat::Unrecorded, wire, 1).unwrap(),
                plaintext,
                "{what} control"
            );
        }
    }

    /// v2 truncation: the fixture without its last frame is rejected, though
    /// every frame that is there authenticates (core's `finish`).
    #[test]
    fn a_truncated_v2_file_is_rejected() {
        let (m, fid, _, wire, count, cs, _) = v2_file();
        // The first count-1 frames, all full: everything but the last frame.
        let cut = (count as usize - 1) * (cs as usize + 28);
        let err =
            decrypt_file_chunks_with_chunk_size(&m, &fid, ExpectedChunkFormat::V2, &wire[..cut], count - 1, Some(cs))
                .unwrap_err();
        assert!(err.contains("v2"), "{err}");
    }

    /// The pin comes only from authenticated metadata: v2 envelope → V2, a
    /// legacy envelope → Unrecorded, a plaintext name → refused (never guessed).
    #[test]
    fn content_pin_reads_authenticated_metadata_only() {
        let (m, fid, name_enc, ..) = v2_file();
        assert_eq!(content_pin(&m, &fid, &name_enc), Ok(ExpectedChunkFormat::V2));
        let legacy = beebeeb_core::encrypt::encrypt_name(&m, &fid, "old.txt", None).unwrap();
        assert_eq!(content_pin(&m, &fid, &legacy), Ok(ExpectedChunkFormat::Unrecorded));
        let err = content_pin(&m, &fid, "plain-name.txt").unwrap_err();
        assert!(err.contains("refusing to guess"), "{err}");
        // A key that does not open the envelope is no pin either.
        let other = MasterKey::from_bytes([0x24u8; 32]);
        assert!(content_pin(&other, &fid, &name_enc).is_err());
    }
    fn repair_vectors() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/sf_0002_owner_repair.json")).unwrap()
    }

    fn repair_master(v: &serde_json::Value) -> MasterKey {
        let mut mk = [0u8; 32];
        mk.copy_from_slice(&unhex(v["master_key_hex"].as_str().unwrap()));
        MasterKey::from_bytes(mk)
    }

    /// SF-0002: a row the old iOS File Provider sealed under the uppercase id
    /// opens through the ordinary readers (name, pin, content) and reports the
    /// read-only convention.
    #[test]
    fn owner_repair_v1_opens_and_reports_read_only() {
        let v = &repair_vectors()["upper_repair_v1"];
        let m = repair_master(v);
        let fid = v["file_id"].as_str().unwrap();
        let name_enc = v["name_encrypted"].as_str().unwrap();
        let frame = unhex(v["content_frame_hex"].as_str().unwrap());
        let plaintext = unhex(v["content_plaintext_hex"].as_str().unwrap());

        assert_eq!(
            decrypt_name_with_convention(&m, fid, name_enc),
            Some((
                v["expected_name"].as_str().unwrap().to_string(),
                OwnerKeyConvention::UpperRepair
            ))
        );
        assert_eq!(
            decrypt_name(&m, fid, name_enc).as_deref(),
            Some(v["expected_name"].as_str().unwrap())
        );
        let pin = content_pin(&m, fid, name_enc).expect("the repair metadata authenticates");
        assert_eq!(pin, ExpectedChunkFormat::Unrecorded);
        assert_eq!(decrypt_file_chunks(&m, fid, pin, &frame, 1).unwrap(), plaintext);
        assert_eq!(
            decrypt_file_chunks_owned(&m, fid, pin, None, &frame, 1, None).unwrap(),
            (plaintext, Some(OwnerKeyConvention::UpperRepair))
        );
    }

    /// SF-0002 split row: the name is sealed uppercase while each version uses
    /// its own convention. Both versions must open, per version.
    #[test]
    fn owner_repair_split_opens_every_version() {
        let v = &repair_vectors()["upper_repair_split_v1"];
        let m = repair_master(v);
        let fid = v["file_id"].as_str().unwrap();
        let name_enc = v["name_encrypted"].as_str().unwrap();
        assert_eq!(
            decrypt_name_with_convention(&m, fid, name_enc),
            Some((
                v["expected_name"].as_str().unwrap().to_string(),
                OwnerKeyConvention::UpperRepair
            ))
        );
        for (frame_key, pt_key) in [
            ("content_v1_frame_hex", "content_v1_plaintext_hex"),
            ("content_v2_frame_hex", "content_v2_plaintext_hex"),
        ] {
            let frame = unhex(v[frame_key].as_str().unwrap());
            let plaintext = unhex(v[pt_key].as_str().unwrap());
            let pin = content_pin(&m, fid, name_enc).expect("the repair metadata authenticates");
            let (out, _) = decrypt_file_chunks_owned(&m, fid, pin, None, &frame, 1, None)
                .unwrap_or_else(|e| panic!("{frame_key}: {e}"));
            assert_eq!(out, plaintext, "{frame_key}");
        }
    }

    /// The fallback is a fallback: an ordinary owner row still resolves under
    /// the string convention and is not marked read-only.
    #[test]
    fn an_ordinary_owner_row_is_not_read_only() {
        let m = mk();
        let name_enc = beebeeb_core::encrypt::encrypt_name(&m, FID, "ordinary.txt", None).unwrap();
        assert_eq!(
            decrypt_name_with_convention(&m, FID, &name_enc),
            Some(("ordinary.txt".to_string(), OwnerKeyConvention::String))
        );
    }

    /// SF-0002 (mount): the flag a mounted entry carries comes from the name's
    /// convention alone — true only for a name sealed under the uppercase id.
    #[test]
    fn the_mount_read_only_flag_follows_the_name_convention() {
        use beebeeb_core::chunk_stream::ExpectedChunkFormat;
        let m = mk();
        let id = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";
        let upper = beebeeb_core::kdf::derive_file_key(&m, id.to_uppercase().as_bytes());
        let normal = beebeeb_core::kdf::derive_file_key(&m, id.as_bytes());
        let sealed =
            beebeeb_core::encrypt::encrypt_name_for(&upper, "x.pdf", None, ExpectedChunkFormat::Unrecorded).unwrap();
        let ordinary =
            beebeeb_core::encrypt::encrypt_name_for(&normal, "x.pdf", None, ExpectedChunkFormat::Unrecorded).unwrap();
        assert!(name_is_upper_repair(&m, id, &sealed));
        assert!(!name_is_upper_repair(&m, id, &ordinary));
    }
}
