use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::ui;

// Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
// See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

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

    let key_from_string = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());
    let key_from_binary = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

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
            if let (Ok(nonce), Ok(ciphertext)) = (b64.decode(&web_blob.nonce), b64.decode(&web_blob.ciphertext)) {
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
    let file_uuid: uuid::Uuid = file_id_str.parse().map_err(|e| format!("invalid file ID: {e}"))?;
    let key_from_binary = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

    crate::crypto::try_decrypt_all_chunks(
        &key_from_binary,
        file_id_str,
        beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        encrypted_bytes,
        chunk_count,
    )
}

/// Encrypt all chunks with the string-UUID key, returning serialized blobs.
fn encrypt_chunks_with_string_key(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    plaintext: &[u8],
) -> Result<Vec<(u32, Vec<u8>)>, String> {
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());

    let plan = beebeeb_types::plan_chunks(plaintext.len() as u64, beebeeb_types::ChunkProfile::Cli);
    let chunk_size = plan.chunk_size_bytes as usize;

    let mut encrypted_chunks = Vec::new();

    if plaintext.is_empty() {
        let bytes = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, &[])
            .map_err(|e| format!("encrypt empty chunk: {e}"))?;
        encrypted_chunks.push((0, bytes));
    } else {
        for (i, chunk) in plaintext.chunks(chunk_size).enumerate() {
            let bytes = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, chunk)
                .map_err(|e| format!("encrypt chunk {i}: {e}"))?;
            encrypted_chunks.push((i as u32, bytes));
        }
    }

    Ok(encrypted_chunks)
}

/// Encrypt a filename with the string-UUID key (includes MIME type in envelope).
fn encrypt_name_with_string_key(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id_str: &str,
    name: &str,
    mime_type: Option<&str>,
) -> Result<String, String> {
    beebeeb_core::encrypt::encrypt_name(master_key, file_id_str, name, mime_type)
        .map_err(|e| format!("failed to encrypt name: {e}"))
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

    if !ui::is_json() && !ui::is_quiet() {
        let mode = if dry_run { " (dry run)" } else { "" };
        println!(
            "  {} {}{}",
            "repair".custom_color(crate::colors::AMBER),
            "scanning vault for binary-UUID files...".custom_color(crate::colors::INK_DIM),
            mode.custom_color(crate::colors::AMBER),
        );
    }

    // Collect all files recursively
    let all_items = collect_all_files(&api, None).await?;

    if all_items.is_empty() {
        if !ui::is_json() && !ui::is_quiet() {
            println!("  {}", "No files in vault.".custom_color(crate::colors::INK_DIM),);
        }
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
        let is_folder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let chunk_count = item.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;

        let detection = detect_key_derivation(&master_key, file_id, name_encrypted);

        match detection {
            Some((KeyDerivation::StringUuid, ref name)) | Some((KeyDerivation::Plaintext, ref name)) => {
                // Optionally show skipped files in verbose mode (not quiet/json)
                // For now just count them silently to reduce noise.
                let _ = name;
                stats.already_correct += 1;
            }
            Some((KeyDerivation::BinaryUuid, decrypted_name)) => {
                if dry_run {
                    let kind = if is_folder { "folder" } else { "file" };
                    if !ui::is_json() && !ui::is_quiet() {
                        println!(
                            "  {} {} {}",
                            "\u{00b7}".custom_color(crate::colors::AMBER),
                            format!("would repair: {decrypted_name}").custom_color(crate::colors::INK),
                            kind.custom_color(crate::colors::INK_DIM),
                        );
                    }
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
                                "\u{2717}".custom_color(crate::colors::RED_ERR),
                                decrypted_name.custom_color(crate::colors::INK),
                                e.custom_color(crate::colors::INK_DIM),
                            );
                            stats.errors += 1;
                        }
                    }
                } else {
                    match repair_file(&api, &master_key, file_id, &decrypted_name, chunk_count).await {
                        Ok(()) => {
                            stats.files_repaired += 1;
                        }
                        Err(e) => {
                            eprintln!(
                                "  {} {} {}",
                                "\u{2717}".custom_color(crate::colors::RED_ERR),
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

    // JSON output
    if ui::is_json() {
        let json_out = serde_json::json!({
            "files_repaired": stats.files_repaired,
            "folders_repaired": stats.folders_repaired,
            "already_correct": stats.already_correct,
            "skipped": stats.skipped,
            "errors": stats.errors,
            "dry_run": dry_run,
        });
        println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
        return Ok(());
    }

    if ui::is_quiet() {
        if stats.files_repaired > 0 || stats.folders_repaired > 0 {
            let verb = if dry_run { "would repair" } else { "repaired" };
            println!(
                "{verb} {} files {} folders",
                stats.files_repaired, stats.folders_repaired,
            );
        }
        return Ok(());
    }

    // Rich summary
    println!();
    if stats.files_repaired == 0 && stats.folders_repaired == 0 {
        println!(
            "  {} {} {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            "All files are already compatible.".custom_color(crate::colors::GREEN_OK),
            if stats.skipped > 0 {
                format!("({} skipped \u{2014} unknown format)", stats.skipped).custom_color(crate::colors::INK_DIM)
            } else {
                "".to_string().custom_color(crate::colors::INK_DIM)
            },
        );
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
        let main_summary = format!("{verb} {}.", parts.join(", "));
        let already = format!("{} already correct.", stats.already_correct);
        let errors = if stats.errors > 0 {
            format!(" {} errors.", stats.errors)
        } else {
            String::new()
        };

        let icon = if dry_run {
            "\u{00b7}".custom_color(crate::colors::AMBER)
        } else {
            "\u{2713}".custom_color(crate::colors::GREEN_OK)
        };
        let summary_color = if dry_run {
            crate::colors::AMBER
        } else {
            crate::colors::GREEN_OK
        };
        println!(
            "  {} {} {} {}",
            icon,
            main_summary.custom_color(summary_color),
            already.custom_color(crate::colors::INK_DIM),
            errors.custom_color(crate::colors::RED_ERR),
        );
    }

    Ok(())
}

/// Recursively collect all files and folders from the vault.
async fn collect_all_files(api: &ApiClient, parent_id: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
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
        let is_folder = file.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);

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
    let new_name_encrypted = encrypt_name_with_string_key(master_key, file_id, decrypted_name, None)?;
    api.move_file(file_id, Some(&new_name_encrypted), None).await?;

    if !ui::is_json() && !ui::is_quiet() {
        println!(
            "  {} {}",
            "\u{2713} repaired:".custom_color(crate::colors::GREEN_OK),
            decrypted_name.custom_color(crate::colors::INK),
        );
    }

    Ok(())
}

/// Repair a file: download, decrypt with binary key, re-encrypt with string key,
/// and write the result as a NEW VERSION of the same file (same id, so links
/// and shares keep pointing at it) via the V2 chunked upload route.
///
/// L13 (SF-0001): the write proposes the file's own id, and the init and the
/// completion must both echo it exactly; on a mismatch the session just opened
/// is abandoned (which reverts an in-flight version, never the file) and nothing
/// is sealed or PUT under the echoed id. The original is never trashed: a
/// trash-then-create could leave it in the trash with nothing in its place
/// (the current server also refuses an init for a trashed id), and a
/// trash-after-completion would trash the repaired file itself, which keeps
/// the same id.
async fn repair_file(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id: &str,
    decrypted_name: &str,
    chunk_count: u32,
) -> Result<(), String> {
    // Get full metadata (need parent_id). The plaintext mime_type column
    // was dropped server-side — the server no longer returns it, and the
    // encrypted MIME type lives inside `name_encrypted`. We re-encrypt with
    // None and let the client recover MIME from the filename if it ever needs
    // it.
    let file_meta = api.get_file(file_id).await?;
    let parent_id = file_meta
        .get("parent_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Step 0: the read pin from the file's authenticated metadata (task 1943).
    // Repair rewrites a legacy file as v1, so a v2-pinned file is never touched:
    // that would silently downgrade it (and core never writes v2 under the
    // binary-UUID key anyway).
    let name_encrypted = file_meta
        .get("name_encrypted")
        .and_then(|v| v.as_str())
        .ok_or("server response missing name_encrypted")?;
    let pin = crate::crypto::content_pin(master_key, file_id, name_encrypted)?;
    if pin != beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded {
        return Err("its metadata records chunk format v2; a v2 file is never rewritten by repair".to_string());
    }

    // Step 1: Download encrypted content
    let encrypted_bytes = api.download_file(file_id).await?;

    // Step 2: Decrypt with binary-UUID key
    let plaintext = decrypt_chunks_with_binary_key(master_key, file_id, &encrypted_bytes, chunk_count)?;

    // Step 3: Re-encrypt name + chunks with string-UUID key
    let new_name_encrypted = encrypt_name_with_string_key(master_key, file_id, decrypted_name, None)?;
    let new_chunks = encrypt_chunks_with_string_key(master_key, file_id, &plaintext)?;

    // Step 4: write it as a new version of the SAME file (see the doc comment:
    // never a trash). The file's current version is the optimistic-concurrency
    // base, so an edit made elsewhere since the download is a stale-version 409,
    // never overwritten with the older repaired content.
    let file_uuid: uuid::Uuid = file_id.parse().map_err(|e| format!("invalid file ID: {e}"))?;
    let listed = crate::item_id::Proposed::Listed(file_uuid);
    let parent_uuid: Option<uuid::Uuid> = parent_id.as_deref().and_then(|s| s.parse().ok());
    let base_version = crate::api::current_version_number(&file_meta).unwrap_or(1) as i32;

    // Derive the chunk plan that matches what encrypt_chunks_with_string_key used,
    // so the init hint and the actual chunk layout agree.
    let plan = beebeeb_types::plan_chunks(plaintext.len() as u64, beebeeb_types::ChunkProfile::Cli);
    let new_chunk_count = new_chunks.len() as i32;
    let chunk_size_bytes = plan.chunk_size_bytes as i64;
    let is_media = beebeeb_core::media::is_media(beebeeb_core::media::guess_mime_type(decrypted_name));

    let init = api
        .upload_init(
            Some(file_uuid),
            &new_name_encrypted,
            parent_uuid,
            plaintext.len() as i64,
            chunk_size_bytes,
            new_chunk_count,
            is_media,
            Some(base_version),
        )
        .await?;
    // L13: before any chunk is PUT under it.
    if let Err(e) = listed.verify(Some(&init.file_id)) {
        let _ = api.upload_abandon(file_id).await;
        return Err(e.to_string());
    }

    // Upload each pre-encrypted chunk.
    for (idx, data) in &new_chunks {
        api.upload_chunk(&init.upload_session_id, *idx, bytes::Bytes::from(data.clone()))
            .await?;
    }

    // Finalise the upload session; the answer must name the same file.
    let completed = api.upload_complete(&init.upload_session_id).await?;
    if let Err(e) = listed.verify(crate::item_id::echoed_id(&completed)) {
        let _ = api.upload_abandon(file_id).await;
        return Err(e.to_string());
    }

    if !ui::is_json() && !ui::is_quiet() {
        println!(
            "  {} {}",
            "\u{2713} repaired:".custom_color(crate::colors::GREEN_OK),
            decrypted_name.custom_color(crate::colors::INK),
        );
    }

    Ok(())
}

#[cfg(test)]
mod l13_repair_tests {
    use super::*;
    use crate::test_server::{Mock, Shared, shared, spawn};
    use beebeeb_core::chunk_stream::ExpectedChunkFormat;
    use serde_json::json;
    use uuid::Uuid;

    const PLAINTEXT: &[u8] = b"repair me";
    const BASE_VERSION: i64 = 3;

    fn master() -> beebeeb_core::kdf::MasterKey {
        beebeeb_core::kdf::MasterKey::from_bytes([6u8; 32])
    }

    /// A file as an old CLI left it (name and content under the binary-UUID
    /// key), at version `BASE_VERSION`, in the mock.
    fn legacy_file(mut mock: Mock, id: Uuid) -> Mock {
        let key = beebeeb_core::kdf::derive_file_key(&master(), id.as_bytes());
        let name = beebeeb_core::encrypt::encrypt_name_for(&key, "notes.txt", None, ExpectedChunkFormat::Unrecorded)
            .expect("legacy name");
        mock.files.insert(
            id.to_string(),
            json!({
                "id": id,
                "parent_id": null,
                "name_encrypted": name,
                "download_version": { "version_number": BASE_VERSION },
            }),
        );
        mock.downloads.insert(
            id.to_string(),
            beebeeb_core::encrypt::encrypt_chunk_raw(&key, PLAINTEXT).expect("legacy chunk"),
        );
        mock
    }

    async fn repair(mock: Mock, id: Uuid) -> (Result<(), String>, Shared) {
        let mock = shared(legacy_file(mock, id));
        let api = ApiClient::new_for_test(spawn(mock.clone()).await);
        (
            repair_file(&api, &master(), &id.to_string(), "notes.txt", 1).await,
            mock,
        )
    }

    /// L2: the re-encrypted write's init echoes another id. The original is not
    /// trashed, no chunk is PUT, nothing completes, the session it opened is
    /// ended once by the file's own id, and the message is plain.
    #[tokio::test]
    async fn a_repair_whose_init_echoes_another_id_leaves_the_original_alone() {
        let id = Uuid::new_v4();
        let (result, mock) = repair(
            Mock {
                init_echo: Some(Uuid::new_v4().to_string()),
                ..Default::default()
            },
            id,
        )
        .await;
        assert_eq!(result, Err(crate::item_id::CHANGED_ID_MESSAGE.to_string()));
        let m = mock.lock().unwrap();
        assert!(m.trashed.is_empty(), "the original is not trashed: {:?}", m.trashed);
        assert_eq!(m.inits, vec![id.to_string()], "the file's own id was proposed");
        assert!(
            m.chunk_puts.is_empty(),
            "no chunk under the echoed id: {:?}",
            m.chunk_puts
        );
        assert!(m.completed.is_empty());
        assert_eq!(m.abandons, vec![id.to_string()], "ended once, by the file's id");
    }

    /// L2: the completion answer names another item. The repair fails with the
    /// plain message; the original is still not trashed.
    #[tokio::test]
    async fn a_repair_whose_completion_echoes_another_id_fails() {
        let id = Uuid::new_v4();
        let (result, mock) = repair(
            Mock {
                complete_echo: Some(Uuid::new_v4().to_string()),
                ..Default::default()
            },
            id,
        )
        .await;
        assert_eq!(result, Err(crate::item_id::CHANGED_ID_MESSAGE.to_string()));
        let m = mock.lock().unwrap();
        assert!(m.trashed.is_empty(), "the original is not trashed: {:?}", m.trashed);
        assert_eq!(m.completed.len(), 1, "the check runs on the completion answer");
        assert_eq!(m.abandons, vec![id.to_string()], "ended once, by the file's id");
    }

    /// Normal flow: the repaired content is written as a new version of the
    /// SAME file (its current version as the base), under the string-UUID key,
    /// and nothing is trashed.
    #[tokio::test]
    async fn a_repair_writes_a_new_version_of_the_same_file() {
        let id = Uuid::new_v4();
        let (result, mock) = repair(Mock::default(), id).await;
        result.expect("repairs");
        let m = mock.lock().unwrap();
        assert_eq!(m.inits, vec![id.to_string()]);
        assert_eq!(m.init_bodies[0]["base_version_number"], json!(BASE_VERSION));
        assert!(m.trashed.is_empty(), "never trashed: {:?}", m.trashed);
        assert_eq!(m.completed.len(), 1);
        assert!(m.abandons.is_empty());

        let (derivation, name) =
            detect_key_derivation(&master(), &id.to_string(), &m.init_names[0]).expect("the new name decrypts");
        assert_eq!((derivation, name.as_str()), (KeyDerivation::StringUuid, "notes.txt"));
        let chunk = m.chunk_bodies.values().next().expect("one chunk");
        let string_key = beebeeb_core::kdf::derive_file_key(&master(), id.to_string().as_bytes());
        let plain = crate::crypto::try_decrypt_all_chunks(
            &string_key,
            &id.to_string(),
            ExpectedChunkFormat::Unrecorded,
            chunk,
            1,
        )
        .expect("the new content decrypts under the string-UUID key");
        assert_eq!(plain, PLAINTEXT);
    }
}
