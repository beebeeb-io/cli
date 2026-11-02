//! Filename decryption: core's readers plus the CLI's SF-0002 convention wrapper.

/// Plain, user-facing explanation for a file that the SF-0002 read-only
/// repair convention could open (iOS Files-app create sealed under an
/// uppercase id). Shared by every CLI surface that refuses a write.
pub const OWNER_REPAIR_READ_ONLY: &str =
    "This file was saved by an older version of the iOS app. You can open and download it. To change it, save a copy.";

#[cfg(test)]
pub use beebeeb_core::encrypt::decrypt_name_with_key;

/// Is this row's stored name a plaintext value rather than an envelope?
/// (P2-10: such a name proves nothing about the row's key.)
pub fn name_is_plaintext(canonical: &serde_json::Value) -> bool {
    canonical["name_encrypted"]
        .as_str()
        .is_some_and(|enc| !enc.starts_with('{'))
}

/// Decrypt a `name_encrypted` value from the API.
///
/// Returns `Some(name)` on success, `None` only if the value looks encrypted
/// but decryption fails (wrong key, corrupt data). All blob formats and key
/// derivations live in core (`beebeeb_core::encrypt`); this is the SF-0002
/// convention wrapper around them.
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
/// Core answers the format matrix (name subkey, native `EncryptedBlob`, web
/// base64 blob) for one key. It is asked once per key derivation so the caller
/// still learns which derivation authenticated: the web app derives from the
/// UUID *string* bytes (`TextEncoder.encode(fileId)`), the CLI from the
/// 16-byte binary form. Only when every ordinary path fails for an owner row
/// is core's [`beebeeb_core::kdf::owner_file_key_for_name`] asked for the
/// read-only repair key, so no ordinary row changes behaviour.
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

    let key_from_string = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());
    if let Ok(meta) = beebeeb_core::encrypt::decrypt_name_metadata_with_key(&key_from_string, name_encrypted_str) {
        return Some((meta.name, OwnerKeyConvention::String));
    }

    if let Ok(file_uuid) = file_id_str.parse::<uuid::Uuid>() {
        let key_from_binary = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());
        if let Ok(meta) = beebeeb_core::encrypt::decrypt_name_metadata_with_key(&key_from_binary, name_encrypted_str) {
            return Some((meta.name, OwnerKeyConvention::Binary));
        }
    }

    // SF-0002: read-only repair. Only the owner can derive this key, and only
    // when the name still fails under both written conventions above.
    if let Ok((file_key, convention)) =
        beebeeb_core::kdf::owner_file_key_for_name(master_key, file_id_str, name_encrypted_str)
    {
        if let Ok(meta) = beebeeb_core::encrypt::decrypt_name_metadata_with_key(&file_key, name_encrypted_str) {
            return Some((meta.name, convention));
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
/// [`decrypt_name`] (format matrix in core + both UUID-key derivations + the
/// plaintext fast-path + the SF-0002 repair convention); `None` for an item
/// that looks encrypted but won't decrypt. Result order matches the input.
#[cfg(test)]
pub fn decrypt_names(master_key: &beebeeb_core::kdf::MasterKey, items: &[(&str, &str)]) -> Vec<Option<String>> {
    use rayon::prelude::*;
    items
        .par_iter()
        .map(|(file_id, name_encrypted)| decrypt_name(master_key, file_id, name_encrypted))
        .collect()
}
