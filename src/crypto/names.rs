//! Filename decryption (web-app blob, JSON envelope, core formats) split out of `crypto.rs`.

use super::*;

/// Intermediate struct for deserializing the web-app blob format where nonce
/// and ciphertext are base64-encoded strings instead of byte arrays.
#[derive(serde::Deserialize)]
pub(super) struct WebAppBlob {
    pub(super) nonce: String,
    pub(super) ciphertext: String,
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
/// uppercase repair convention? The one name verdict for every surface (the
/// resolver, `bb mv`/`push` paths, WebDAV, the mount), whichever key the row
/// was reached by; `owned_keys::write_check` then checks the content as well.
pub fn name_is_upper_repair(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> bool {
    decrypt_name_with_convention(master_key, file_id_str, name_encrypted_str)
        .is_some_and(|(_, convention)| convention == beebeeb_core::kdf::OwnerKeyConvention::UpperRepair)
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
pub(super) fn unwrap_metadata_json(decrypted: &str) -> String {
    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(decrypted) {
        if let Some(name) = meta.get("name").and_then(|v| v.as_str()) {
            return name.to_string();
        }
    }
    decrypted.to_string()
}
