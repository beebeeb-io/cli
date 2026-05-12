use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

/// 1 MiB chunk size (matches beebeeb_types::CHUNK_SIZE and push.rs)
const CHUNK_SIZE: usize = 1024 * 1024;

/// Which UUID key derivation a file's name_encrypted was encrypted with.
#[derive(Debug, PartialEq)]
enum KeyDerivation {
    /// `file_id.to_string().as_bytes()` — 36-byte string form (web-compatible)
    StringUuid,
    /// `file_uuid.as_bytes()` — 16-byte binary form (old CLI-only)
    BinaryUuid,
    /// Plaintext — not encrypted at all (legacy server-created folders)
    Plaintext,
}

/// Test which key derivation was used for a file's name_encrypted.
/// Returns (derivation, decrypted_name).
fn detect_key_derivation(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name_encrypted_str: &str,
) -> Option<(KeyDerivation, String)> {
    use base64::Engine as _;
    use beebeeb_types::EncryptedBlob;

    // Plaintext fast path
    if !name_encrypted_str.starts_with('{') {
        return Some((KeyDerivation::Plaintext, name_encrypted_str.to_string()));
    }

    let file_uuid: uuid::Uuid = file_id_str.parse().ok()?;

    let key_from_string =
        beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());
    let key_from_binary =
        beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

    // Try string-UUID key first (the "correct" / web-compatible derivation)
    for (file_key, derivation) in [
        (&key_from_string, KeyDerivation::StringUuid),
        (&key_from_binary, KeyDerivation::BinaryUuid),
    ] {
        // Format 1: Rust-native EncryptedBlob
        if let Ok(blob) = serde_json::from_str::<EncryptedBlob>(name_encrypted_str) {
            if let Ok(name) = beebeeb_core::encrypt::decrypt_metadata(file_key, &blob) {
                return Some((derivation, unwrap_metadata_json(&name)));
            }
        }

        // Format 2: Web-app blob (base64 nonce + ciphertext)
        #[derive(serde::Deserialize)]
        struct WebAppBlob {
            nonce: String,
            ciphertext: String,
        }
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
                    return Some((derivation, unwrap_metadata_json(&name)));
                }
            }
        }
    }

    None
}

/// Unwrap metadata JSON the same way crypto.rs does.
fn unwrap_metadata_json(decrypted: &str) -> String {
    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(decrypted) {
        if let Some(name) = meta.get("name").and_then(|v| v.as_str()) {
            return name.to_string();
        }
    }
    decrypted.to_string()
}

/// Try decrypting chunks with the binary-UUID key specifically.
/// Returns the plaintext on success.
fn decrypt_chunks_with_binary_key(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    encrypted_bytes: &[u8],
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    let file_uuid: uuid::Uuid = file_id_str
        .parse()
        .map_err(|e| format!("invalid file ID: {e}"))?;
    let key_from_binary =
        beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

    crate::crypto::try_decrypt_all_chunks(&key_from_binary, encrypted_bytes, chunk_count)
}

/// Encrypt all chunks with the string-UUID key, returning serialized blobs.
fn encrypt_chunks_with_string_key(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    plaintext: &[u8],
) -> Result<Vec<(u32, Vec<u8>)>, String> {
    let file_key =
        beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());

    let mut encrypted_chunks = Vec::new();

    if plaintext.is_empty() {
        let blob = beebeeb_core::encrypt::encrypt_chunk(&file_key, &[])
            .map_err(|e| format!("failed to encrypt empty chunk: {e}"))?;
        let serialized = serde_json::to_vec(&blob)
            .map_err(|e| format!("failed to serialize chunk: {e}"))?;
        encrypted_chunks.push((0, serialized));
    } else {
        for (i, chunk) in plaintext.chunks(CHUNK_SIZE).enumerate() {
            let blob = beebeeb_core::encrypt::encrypt_chunk(&file_key, chunk)
                .map_err(|e| format!("failed to encrypt chunk {i}: {e}"))?;
            let serialized = serde_json::to_vec(&blob)
                .map_err(|e| format!("failed to serialize chunk {i}: {e}"))?;
            encrypted_chunks.push((i as u32, serialized));
        }
    }

    Ok(encrypted_chunks)
}

/// Encrypt a filename with the string-UUID key.
fn encrypt_name_with_string_key(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name: &str,
) -> Result<String, String> {
    let file_key =
        beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());
    let blob = beebeeb_core::encrypt::encrypt_metadata(&file_key, name)
        .map_err(|e| format!("failed to encrypt name: {e}"))?;
    serde_json::to_string(&blob)
        .map_err(|e| format!("failed to serialize name blob: {e}"))
}

/// Counters for the summary line.
struct RepairStats {
    files_repaired: u32,
    folders_repaired: u32,
    already_correct: u32,
    skipped: u32,
    errors: u32,
}

pub async fn run(dry_run: bool) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let master_key = load_master_key()?;

    println!(
        "  {} {}",
        "repair".custom_color(crate::colors::AMBER),
        if dry_run {
            "(dry run) scanning vault for binary-UUID files..."
                .custom_color(crate::colors::INK_DIM)
        } else {
            "scanning vault for binary-UUID files..."
                .custom_color(crate::colors::INK_DIM)
        },
    );

    // Collect all files recursively
    let all_items = collect_all_files(&api, None).await?;

    if all_items.is_empty() {
        println!(
            "  {}",
            "No files in vault.".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    }

    let mut stats = RepairStats {
        files_repaired: 0,
        folders_repaired: 0,
        already_correct: 0,
        skipped: 0,
        errors: 0,
    };

    for item in &all_items {
        let file_id = match item.get("id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => continue,
        };
        let name_encrypted = match item.get("name_encrypted").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => continue,
        };
        let is_folder = item
            .get("is_folder")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let chunk_count = item
            .get("chunk_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(1) as u32;

        let detection = detect_key_derivation(&master_key, file_id, name_encrypted);

        match detection {
            Some((KeyDerivation::StringUuid, _)) | Some((KeyDerivation::Plaintext, _)) => {
                stats.already_correct += 1;
            }
            Some((KeyDerivation::BinaryUuid, decrypted_name)) => {
                if dry_run {
                    let kind = if is_folder { "folder" } else { "file" };
                    println!(
                        "  {} {} {} {}",
                        "would repair:".custom_color(crate::colors::AMBER),
                        decrypted_name.custom_color(crate::colors::INK),
                        format!("({file_id})").custom_color(crate::colors::INK_DIM),
                        kind.custom_color(crate::colors::INK_SAGE),
                    );
                    if is_folder {
                        stats.folders_repaired += 1;
                    } else {
                        stats.files_repaired += 1;
                    }
                } else if is_folder {
                    match repair_folder(&api, &master_key, file_id, &decrypted_name).await {
                        Ok(()) => {
                            stats.folders_repaired += 1;
                        }
                        Err(e) => {
                            eprintln!(
                                "  {} {} {}",
                                "error:".custom_color(crate::colors::RED_ERR),
                                decrypted_name.custom_color(crate::colors::INK),
                                e.custom_color(crate::colors::INK_DIM),
                            );
                            stats.errors += 1;
                        }
                    }
                } else {
                    match repair_file(
                        &api,
                        &master_key,
                        file_id,
                        &decrypted_name,
                        chunk_count,
                    )
                    .await
                    {
                        Ok(()) => {
                            stats.files_repaired += 1;
                        }
                        Err(e) => {
                            eprintln!(
                                "  {} {} {}",
                                "error:".custom_color(crate::colors::RED_ERR),
                                decrypted_name.custom_color(crate::colors::INK),
                                e.custom_color(crate::colors::INK_DIM),
                            );
                            stats.errors += 1;
                        }
                    }
                }
            }
            None => {
                stats.skipped += 1;
            }
        }
    }

    // Summary
    if stats.files_repaired == 0 && stats.folders_repaired == 0 {
        if stats.skipped > 0 {
            println!(
                "  {} ({} skipped — unknown format)",
                "All files are already compatible."
                    .custom_color(crate::colors::GREEN_OK),
                stats.skipped,
            );
        } else {
            println!(
                "  {}",
                "All files are already compatible."
                    .custom_color(crate::colors::GREEN_OK),
            );
        }
    } else {
        let verb = if dry_run { "Would repair" } else { "Repaired" };
        let mut parts = Vec::new();
        if stats.files_repaired > 0 {
            parts.push(format!(
                "{} file{}",
                stats.files_repaired,
                if stats.files_repaired == 1 { "" } else { "s" }
            ));
        }
        if stats.folders_repaired > 0 {
            parts.push(format!(
                "{} folder{}",
                stats.folders_repaired,
                if stats.folders_repaired == 1 { "" } else { "s" }
            ));
        }
        let mut summary = format!("{verb} {}.", parts.join(", "));
        summary.push_str(&format!(" {} already correct.", stats.already_correct));
        if stats.errors > 0 {
            summary.push_str(&format!(" {} errors.", stats.errors));
        }
        println!(
            "  {}",
            summary.custom_color(if dry_run {
                crate::colors::AMBER
            } else {
                crate::colors::GREEN_OK
            }),
        );
    }

    Ok(())
}

/// Recursively collect all files and folders from the vault.
async fn collect_all_files(
    api: &ApiClient,
    parent_id: Option<&str>,
) -> Result<Vec<serde_json::Value>, String> {
    let listing = api.list_files(parent_id).await?;

    let files = listing
        .as_array()
        .or_else(|| listing.get("files").and_then(|f| f.as_array()));

    let Some(files) = files else {
        return Ok(Vec::new());
    };

    let mut result = Vec::new();

    for file in files {
        let file_id = file.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let is_folder = file
            .get("is_folder")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        result.push(file.clone());

        if is_folder && !file_id.is_empty() {
            match Box::pin(collect_all_files(api, Some(file_id))).await {
                Ok(children) => result.extend(children),
                Err(e) => {
                    eprintln!(
                        "  {} listing folder {}: {}",
                        "warning:".custom_color(crate::colors::AMBER),
                        file_id,
                        e,
                    );
                }
            }
        }
    }

    Ok(result)
}

/// Repair a folder: just update name_encrypted via PATCH.
async fn repair_folder(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id: &str,
    decrypted_name: &str,
) -> Result<(), String> {
    println!(
        "  {} {} {}",
        "repairing:".custom_color(crate::colors::AMBER),
        decrypted_name.custom_color(crate::colors::INK),
        format!("({file_id})").custom_color(crate::colors::INK_DIM),
    );

    let new_name_encrypted =
        encrypt_name_with_string_key(master_key, file_id, decrypted_name)?;
    api.move_file(file_id, Some(&new_name_encrypted), None)
        .await?;

    println!(
        "  {} {}",
        "repaired:".custom_color(crate::colors::GREEN_OK),
        decrypted_name.custom_color(crate::colors::INK),
    );

    Ok(())
}

/// Repair a file: download, decrypt with binary key, re-encrypt with string key,
/// trash the old version, and re-upload.
async fn repair_file(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id: &str,
    decrypted_name: &str,
    chunk_count: u32,
) -> Result<(), String> {
    println!(
        "  {} {} {}",
        "repairing:".custom_color(crate::colors::AMBER),
        decrypted_name.custom_color(crate::colors::INK),
        format!("({file_id})").custom_color(crate::colors::INK_DIM),
    );

    // Get full metadata (need parent_id, mime_type, size)
    let file_meta = api.get_file(file_id).await?;
    let parent_id = file_meta
        .get("parent_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mime_type = file_meta
        .get("mime_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Step 1: Download encrypted content
    let encrypted_bytes = api.download_file(file_id).await?;

    // Step 2: Decrypt with binary-UUID key
    let plaintext =
        decrypt_chunks_with_binary_key(master_key, file_id, &encrypted_bytes, chunk_count)?;

    // Step 3: Re-encrypt name + chunks with string-UUID key
    let new_name_encrypted =
        encrypt_name_with_string_key(master_key, file_id, decrypted_name)?;
    let new_chunks =
        encrypt_chunks_with_string_key(master_key, file_id, &plaintext)?;

    let total_encrypted_size: i64 = new_chunks
        .iter()
        .map(|(_, data)| data.len() as i64)
        .sum();

    // Step 4: Trash old file
    api.trash_file(file_id).await?;

    // Step 5: Re-upload with the same file_id so links/shares are preserved
    let file_uuid: uuid::Uuid = file_id
        .parse()
        .map_err(|e| format!("invalid file ID: {e}"))?;

    let mut metadata = serde_json::json!({
        "name_encrypted": new_name_encrypted,
        "size_bytes": total_encrypted_size,
        "file_id": file_uuid,
    });
    if let Some(pid) = &parent_id {
        if let Ok(parent_uuid) = pid.parse::<uuid::Uuid>() {
            metadata["parent_id"] = serde_json::json!(parent_uuid);
        }
    }
    if let Some(mime) = &mime_type {
        metadata["mime_type"] = serde_json::json!(mime);
    }

    let metadata_json = serde_json::to_string(&metadata)
        .map_err(|e| format!("failed to serialize metadata: {e}"))?;

    api.upload_encrypted(&metadata_json, &new_chunks).await?;

    println!(
        "  {} {}",
        "repaired:".custom_color(crate::colors::GREEN_OK),
        decrypted_name.custom_color(crate::colors::INK),
    );

    Ok(())
}
