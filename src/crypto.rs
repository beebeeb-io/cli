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
        if let Some(name) = decrypt_name_with_key(file_key, name_encrypted_str) {
            return Some(name);
        }
    }

    None
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

/// Decrypt all chunks of a downloaded file, handling both CLI-format (JSON
/// `EncryptedBlob`) and web-app-format (raw `nonce|ciphertext` binary) chunk
/// storage, and both UUID key derivation methods (binary vs string).
///
/// The server streams chunk blobs back-to-back. For CLI uploads each chunk is
/// a JSON object; for web uploads each chunk is raw `nonce (12) | ciphertext`.
/// The format is never trusted from the bytes alone (a random raw nonce starts
/// with `{` about 1 time in 256): the first byte only picks which parser is
/// tried first, and the other is the fallback. Every chunk is AEAD-authenticated,
/// so a wrong parse fails and can never yield wrong plaintext.
///
/// Returns the reassembled plaintext on success. Callers that know the file's
/// plaintext chunk size (the `X-Chunk-Size` download header) should use
/// [`decrypt_file_chunks_with_chunk_size`].
pub fn decrypt_file_chunks(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    decrypt_file_chunks_with_chunk_size(master_key, file_id_str, encrypted_bytes, chunk_count, None)
}

/// [`decrypt_file_chunks`] with the file's uniform plaintext chunk size, when
/// the caller has it. For raw multi-chunk files this is the exact wire frame
/// size (`chunk_size + 28`); without it the frame size is recovered by
/// AEAD-validated probing (see [`decrypt_raw_chunks`]).
pub fn decrypt_file_chunks_with_chunk_size(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    encrypted_bytes: &[u8],
    chunk_count: u32,
    chunk_size: Option<u64>,
) -> Result<Vec<u8>, String> {
    let file_uuid: uuid::Uuid = file_id_str.parse().map_err(|e| format!("invalid file ID: {e}"))?;

    // Try binary-UUID key first (CLI origin), then string-UUID key (web origin).
    let key_from_binary = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());
    let key_from_string = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());

    // Attempt decryption with each key. The first successful full decryption wins.
    for file_key in [&key_from_binary, &key_from_string] {
        match try_decrypt_all_chunks_with_chunk_size(file_key, encrypted_bytes, chunk_count, chunk_size) {
            Ok(plaintext) => return Ok(plaintext),
            Err(_) => continue,
        }
    }

    Err(format!(
        "failed to decrypt file {file_id_str}: neither binary-UUID nor string-UUID key derivation succeeded"
    ))
}

/// Try to decrypt all chunks with a given file key. The chunk format is NOT
/// detected from the bytes: the first byte only orders the two parsers (JSON
/// blob vs raw `nonce|ciphertext`), and the other one is the fallback.
pub fn try_decrypt_all_chunks(
    file_key: &beebeeb_core::kdf::FileKey,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    try_decrypt_all_chunks_with_chunk_size(file_key, encrypted_bytes, chunk_count, None)
}

/// [`try_decrypt_all_chunks`] with the file's uniform plaintext chunk size, when
/// known (`X-Chunk-Size`). Only the raw parser uses it.
pub fn try_decrypt_all_chunks_with_chunk_size(
    file_key: &beebeeb_core::kdf::FileKey,
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

    // The first byte is only a HINT (task 1759): a raw chunk starts with a
    // random nonce, so ~1 in 256 raw files begin with 0x7b ('{') and would be
    // misrouted to the JSON parser. Try the hinted format first, then the
    // other. A false success is impossible: every chunk is AEAD-authenticated,
    // so a wrong parse can only fail, never yield wrong plaintext.
    let raw = |k: &beebeeb_core::kdf::FileKey| decrypt_raw_chunks(k, encrypted_bytes, chunk_count, chunk_size);
    let json = |k: &beebeeb_core::kdf::FileKey| decrypt_json_chunks(k, encrypted_bytes, chunk_count);
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

        // Wiped on drop: only the copy appended to `plaintext` (which the caller owns) survives.
        let decrypted = zeroize::Zeroizing::new(
            beebeeb_core::encrypt::decrypt_chunk(file_key, &blob).map_err(|e| format!("decrypt chunk {i}: {e}"))?,
        );
        plaintext.extend_from_slice(&decrypted);
    }

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

fn decrypt_raw_frame(file_key: &beebeeb_core::kdf::FileKey, frame: &[u8], index: usize) -> Result<Vec<u8>, String> {
    // Core's raw decrypt takes the frame as-is (no to_vec copies) and rejects
    // frames shorter than nonce + tag instead of panicking on the slice.
    beebeeb_core::encrypt::decrypt_chunk_raw(file_key, frame).map_err(|e| format!("decrypt raw chunk {index}: {e}"))
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
/// key, so a wrong guess costs time and can never produce wrong plaintext.
fn raw_frame_size(
    file_key: &beebeeb_core::kdf::FileKey,
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
    file_key: &'a beebeeb_core::kdf::FileKey,
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
        decrypt_raw_frame(self.file_key, &self.data[..f as usize], 0)
            .map(zeroize::Zeroizing::new) // wiped on drop; only the verdict is kept
            .is_ok()
    }
}

/// Parse and decrypt web-app-format chunks: raw `nonce (12 bytes) | ciphertext`.
///
/// Chunks are stored back-to-back with no length prefix. Every chunk but the
/// last is exactly `chunk_size + 28` bytes (nonce + ciphertext + tag); the last
/// is the remainder and is normally SHORTER, so the frame size cannot be
/// derived as `total / count` (task 1759 round 2). It is taken from `chunk_size`
/// when the caller knows it, otherwise recovered by AEAD-validated probing
/// (see [`raw_frame_size`]).
fn decrypt_raw_chunks(
    file_key: &beebeeb_core::kdf::FileKey,
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
        raw_frame_size(file_key, encrypted_bytes, count, chunk_size)?
    };

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

        let decrypted = zeroize::Zeroizing::new(decrypt_raw_frame(
            file_key,
            &encrypted_bytes[offset..offset + this_chunk_size],
            i,
        )?);
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
        assert_eq!(try_decrypt_all_chunks(&fk, &frame, 1).unwrap(), pt);
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
        assert_eq!(try_decrypt_all_chunks(&fk, &all, 2).unwrap(), b"AAAAAAAABBBBBBBB");
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
        assert_eq!(try_decrypt_all_chunks(&fk, &wire, 4).unwrap(), pt);
        assert_eq!(decrypt_file_chunks(&m, FID, &wire, 4).unwrap(), pt);
    }

    /// Same, with a '{' first nonce byte on frame 0: both defects at once.
    #[test]
    fn raw_multi_chunk_uneven_last_chunk_with_brace_first_byte_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt: Vec<u8> = (0..(3 * 64 + 10)).map(|i| (i % 251) as u8).collect();
        let wire = raw_file(&fk, &pt, 64, true);
        assert_eq!(wire[0], b'{');
        assert_eq!(try_decrypt_all_chunks(&fk, &wire, 4).unwrap(), pt);
        assert_eq!(decrypt_file_chunks(&m, FID, &wire, 4).unwrap(), pt);
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
            assert_eq!(try_decrypt_all_chunks(&fk, &wire, 3).unwrap(), pt, "chunk size {cs}");
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
        assert_eq!(try_decrypt_all_chunks(&fk, &wire, 2).unwrap(), pt);
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
            try_decrypt_all_chunks_with_chunk_size(&fk, &wire, 6, Some(100)).unwrap(),
            pt
        );
        assert_eq!(
            try_decrypt_all_chunks_with_chunk_size(&fk, &wire, 6, Some(4096)).unwrap(),
            pt
        );
        assert_eq!(
            decrypt_file_chunks_with_chunk_size(&m, FID, &wire, 6, Some(100)).unwrap(),
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
        assert!(try_decrypt_all_chunks(&derive_file_key(&m, b"other"), &wire, 4).is_err());
        let mid = 64 + 28 + 20;
        wire[mid] ^= 0xff;
        assert!(try_decrypt_all_chunks(&fk, &wire, 4).is_err());
    }

    /// P2-c: when neither parser works, the error names BOTH reasons.
    #[test]
    fn failure_reports_both_parser_errors() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let junk = vec![0x41u8; 200];
        let err = try_decrypt_all_chunks(&fk, &junk, 2).unwrap_err();
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
        assert!(try_decrypt_all_chunks(&other, &frame, 1).is_err());
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
}
