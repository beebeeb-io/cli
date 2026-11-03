//! Content pins and per-row key material split out of `crypto.rs`.

use super::*;
use zeroize::Zeroize;

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

/// A second owned copy of `file_key` (core's `FileKey` is not `Clone`).
pub(crate) fn key_copy(file_key: &FileKey) -> FileKey {
    key_from_bytes(file_key.as_bytes())
}

/// Build a `FileKey` from bytes already in hand (task 2275 review-2, N5).
///
/// This is the one place in the CLI that spells the by-value copy core's
/// `FileKey::from_bytes` takes: the bytes are copied into a `Zeroizing`
/// buffer, the buffer is wiped before this returns, and core wipes the
/// argument it received. Every other site passes a reference here, so a key
/// array never sits un-wiped in a caller's frame.
pub(crate) fn key_from_bytes(bytes: &[u8; 32]) -> FileKey {
    let mut copy = zeroize::Zeroizing::new(*bytes);
    let key = FileKey::from_bytes(*copy);
    copy.zeroize();
    key
}

/// Build a `MasterKey` from raw bytes (R4-D).
///
/// Core takes the 32 bytes by value; this helper copies them into a
/// `Zeroizing` buffer, hands core a copy, and wipes its own buffer. The caller
/// never holds an un-wiped master-key array in its frame.
pub(crate) fn master_key_from_bytes(bytes: &[u8]) -> Result<beebeeb_core::kdf::MasterKey, String> {
    if bytes.len() != 32 {
        return Err(format!("master key must be 32 bytes, got {}", bytes.len()));
    }
    let mut copy = zeroize::Zeroizing::new([0u8; 32]);
    copy.copy_from_slice(bytes);
    let key = beebeeb_core::kdf::MasterKey::from_bytes(*copy);
    copy.zeroize();
    Ok(key)
}

#[cfg(test)]
mod key_copy_tests {
    use super::*;

    /// N5: no write site spells core's by-value key copy itself. Every
    /// `FileKey` built from bytes goes through [`key_from_bytes`] (or
    /// [`key_copy`]), which copies into a `Zeroizing` buffer and wipes it; a
    /// bare `FileKey::from_bytes(*k.as_bytes())` leaves a stack copy whose wipe
    /// depends on core's callee and cannot be checked from here.
    /// Drop `#[cfg(test)] mod ... { ... }` blocks so the scan sees only code
    /// that ships. Test modules may spell any key construction they like.
    /// The item a `#[cfg(test)]` attribute applies to. R4-D: only a `mod <name>
    /// { ... }` body is dropped; `mod <name>;` and every non-mod item stay.
    enum CfgTestItem {
        ModBlock { name: String, body_start: usize },
        ModDecl { name: String },
        Other,
    }

    fn cfg_test_item(src: &str, at: usize) -> CfgTestItem {
        let mut offset = at + "#[cfg(test)]".len();
        let mut rest = &src[offset..];
        loop {
            let trimmed = rest.trim_start();
            offset += rest.len() - trimmed.len();
            rest = trimmed;
            // A second attribute (`#[path = ...]`) still belongs to this item.
            if let Some(stripped) = rest.strip_prefix("#[") {
                let Some(end) = stripped.find(']') else {
                    return CfgTestItem::Other;
                };
                offset += "#[".len() + end + 1;
                rest = &stripped[end + 1..];
                continue;
            }
            break;
        }
        for vis in ["pub(crate) ", "pub(super) ", "pub "] {
            if let Some(stripped) = rest.strip_prefix(vis) {
                offset += vis.len();
                rest = stripped;
                break;
            }
        }
        let Some(body) = rest.strip_prefix("mod ") else {
            return CfgTestItem::Other;
        };
        offset += "mod ".len();
        let ident_len = body
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(body.len());
        let name = body[..ident_len].to_string();
        if name.is_empty() {
            return CfgTestItem::Other;
        }
        let after = &body[ident_len..];
        let trimmed = after.trim_start();
        offset += ident_len + (after.len() - trimmed.len());
        match trimmed.chars().next() {
            Some('{') => CfgTestItem::ModBlock {
                name,
                body_start: offset,
            },
            Some(';') => CfgTestItem::ModDecl { name },
            _ => CfgTestItem::Other,
        }
    }

    fn shipping_code(src: &str) -> String {
        let mut out = String::new();
        let mut rest = src;
        while let Some(at) = rest.find("#[cfg(test)]") {
            match cfg_test_item(rest, at) {
                CfgTestItem::ModBlock { body_start, .. } => {
                    // Only a module BODY is test-only. A `mod x;` declaration
                    // continues into a shipping file that is classified by
                    // `cfg_test_modules`/`is_test_file`, never by stripping the
                    // next block after it (R4-D).
                    out.push_str(&rest[..at]);
                    let bytes = rest.as_bytes();
                    let mut depth = 0usize;
                    let mut j = body_start;
                    while j < bytes.len() {
                        match bytes[j] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    j += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    rest = &rest[j..];
                }
                _ => {
                    // A cfg(test) item that is not a module body stays; keep its
                    // attribute in the output and scan on after it.
                    out.push_str(&rest[..at + "#[cfg(test)]".len()]);
                    rest = &rest[at + "#[cfg(test)]".len()..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn cfg_test_modules(files: &[(String, String)]) -> std::collections::HashSet<String> {
        let mut mods = std::collections::HashSet::new();
        for (_, src) in files {
            let mut rest = src.as_str();
            while let Some(at) = rest.find("#[cfg(test)]") {
                if let CfgTestItem::ModBlock { name, .. } | CfgTestItem::ModDecl { name } = cfg_test_item(rest, at) {
                    mods.insert(name);
                }
                rest = &rest[at + "#[cfg(test)]".len()..];
            }
        }
        mods
    }

    fn is_test_file(name: &str, test_mods: &std::collections::HashSet<String>) -> bool {
        let stem = name.rsplit('/').next().unwrap_or(name).trim_end_matches(".rs");
        name.split('/').any(|seg| seg == "tests") || stem.ends_with("_tests") || test_mods.contains(stem)
    }

    fn rust_files(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("read src") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let name = path
                    .strip_prefix(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((name, std::fs::read_to_string(&path).expect("read source")));
            }
        }
    }

    /// N5: scan ALL of `src/` (not a hand list) and forbid core's by-value
    /// `FileKey::from_bytes(` outside this file. A key array held by value is
    /// also forbidden: core wipes the argument it receives, but an array in a
    /// caller's frame is not wiped when that call returns.
    #[test]
    fn no_shipping_site_spells_its_own_by_value_key_copy() {
        let mut files = Vec::new();
        rust_files(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        assert!(files.len() > 40, "scanned the whole crate: {}", files.len());
        let test_mods = cfg_test_modules(&files);
        for (name, src) in &files {
            if name == "crypto/pins.rs" || is_test_file(name, &test_mods) {
                continue; // the blessed copy / test-only source, asserted below
            }
            let code = shipping_code(src);
            assert!(
                !code.contains("FileKey::from_bytes("),
                "{name} spells core's by-value key copy; use crypto::key_from_bytes / key_copy"
            );
            assert!(
                !code.contains("let key: [u8; 32] ="),
                "{name} holds a key array by value; wrap the array in Zeroizing and pass a reference"
            );
            // R4-D: a master key is never built by value in a caller's frame.
            assert!(
                !code.contains("MasterKey::from_bytes("),
                "{name} spells core's by-value master-key copy; use crypto::master_key_from_bytes"
            );
            let mut lines = code.lines();
            while let Some(line) = lines.next() {
                if !line.contains("[0u8; 32]") {
                    continue;
                }
                let window: String = std::iter::once(line)
                    .chain(lines.clone().take(5))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    !line.contains("let mut arr") && !window.contains("MasterKey::from_bytes("),
                    "{name} holds a master-key array by value; use crypto::master_key_from_bytes"
                );
            }
        }
        // The one blessed copy is this file's helper, into a buffer it wipes.
        let pins = include_str!("pins.rs");
        assert!(
            pins.contains("let mut copy = zeroize::Zeroizing::new(*bytes);"),
            "key_from_bytes copies into a Zeroizing buffer"
        );
        assert!(pins.contains("copy.zeroize();"), "key_from_bytes wipes that buffer");
        // Split so this assertion's own text is not a second match.
        let helper = concat!("let key = FileKey::from_bytes(*", "copy);");
        assert_eq!(
            pins.matches(helper).count(),
            1,
            "exactly one by-value construction in prod"
        );
    }

    /// The helper keeps the key it was given.
    #[test]
    fn key_from_bytes_keeps_the_key() {
        let key = key_from_bytes(&[7u8; 32]);
        assert_eq!(key.as_bytes(), &[7u8; 32]);
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
