//! `bb mount` — FUSE filesystem for the Beebeeb vault.
//!
//! # Prerequisites
//!
//! Build with `--features fuse`:
//!
//! ```sh
//! # macOS: install macFUSE kernel extension first (reboot required)
//! brew install macfuse
//! # then reboot, enable kernel extension in System Settings → Privacy
//! cargo build --features fuse
//!
//! # Linux:
//! apt install libfuse3-dev   # or equivalent for your distro
//! cargo build --features fuse
//! ```
//!
//! # Day 1 scope (read-only)
//!
//! - `readdir`  → list directory, decrypt names, build inode table
//! - `lookup`   → resolve filename → inode + attributes
//! - `getattr`  → return size, timestamps, permissions
//! - `open`     → accept open requests for regular files
//! - `read`     → download + decrypt on first access, serve from memory cache
//!
//! # Day 2 scope (write operations)
//!
//! - `create`   → allocate inode, start write buffer
//! - `write`    → accumulate data in memory buffer (commit-on-close model)
//! - `release`  → encrypt buffer in 1 MiB chunks → upload → update inode table
//! - `mkdir`    → encrypt folder name → API createFolder → new inode
//! - `unlink`   → trash file via API → remove from inode table
//! - `rmdir`    → trash empty folder → remove from inode table
//! - `rename`   → re-encrypt name if changed → move_file API call
//! - `setattr`  → chmod/chown no-op; truncation resizes write buffer
//!
//! ## Write-buffer design
//!
//! We use "commit on close" — writes are buffered in memory and uploaded only
//! when the last file descriptor is closed (`release`). This is the same model
//! macOS Finder uses with SMB/AFP network mounts. It means mid-file content is
//! never visible on the server, and partial writes never leave corrupt data.
//!
//! Small files (<500 MiB) are buffered fully in memory. There is no disk-backed
//! temp-file path today — very large files will use proportionally more RSS.
//!
//! ## Key-UUID vs file_id
//!
//! Per-file AES-256-GCM keys are derived from a UUID:
//! `file_key = HKDF(master_key, uuid.as_bytes())`.
//!
//! For files pulled from the server the UUID = the file's id. For files we
//! create via FUSE, we mint a random UUID (`key_uuid`) before uploading and
//! propose it as the file's id; L13 (SF-0001): every answer must echo that id
//! exactly, or the upload is abandoned and the inode records no server id. So
//! `key_uuid` and `file_id` are the same id for every file the mount writes.

#[cfg(not(feature = "fuse"))]
use std::path::PathBuf;

#[cfg(any(feature = "fuse", test))]
const ROOT_INO: u64 = 1;

#[cfg(any(feature = "fuse", test))]
fn rename_parent_target(
    parent: u64,
    newparent: u64,
    entry: Option<(bool, Option<&str>)>,
) -> Result<Option<Option<uuid::Uuid>>, super::move_target::ParentError> {
    use super::move_target::{DestinationParent, parent_target};
    let target = if newparent == ROOT_INO {
        DestinationParent::Root
    } else {
        match entry {
            Some((true, id)) => DestinationParent::Folder(id),
            Some((false, _)) => DestinationParent::NotFolder,
            None => DestinationParent::Missing,
        }
    };
    parent_target(parent != newparent, target)
}

// ─── Stub (no FUSE feature) ───────────────────────────────────────────────────
//
// Release binaries (cargo-dist, `dist-workspace.toml`) are built WITHOUT the
// `fuse` feature, and no FUSE-enabled release asset is published. So this stub
// must not walk the user through installing macFUSE / libfuse3 (a kernel
// extension this binary can never use) or point at a download that does not
// exist. It says what is true and points at `bb webdav`, which ships in every
// release binary. Pinned by `tests/mount_availability.rs`.

#[cfg(not(feature = "fuse"))]
const NO_FUSE_ERR: &str =
    "bb mount is not available in this build (release binaries ship without FUSE). Use `bb webdav` instead.";

#[cfg(not(feature = "fuse"))]
pub async fn run(_mountpoint: PathBuf, _foreground: bool, _cache_ttl: u64) -> Result<(), String> {
    use colored::Colorize;

    println!();
    println!(
        "  {} {}",
        "◆".custom_color(crate::colors::AMBER),
        "This bb binary was built without FUSE, so it cannot mount."
            .bold()
            .custom_color(crate::colors::INK),
    );
    println!();
    println!(
        "  {}",
        "To open your vault as a drive, use WebDAV — it is included in every bb release:"
            .custom_color(crate::colors::INK_DIM),
    );
    println!("    {}", "bb webdav".custom_color(crate::colors::AMBER));
    println!(
        "  {}",
        "then connect Finder, Explorer, rclone or Cyberduck to the address it prints."
            .custom_color(crate::colors::INK_DIM),
    );
    println!();
    println!(
        "  {}",
        "FUSE mount is only available when you build bb from source:".custom_color(crate::colors::INK_DIM),
    );
    #[cfg(target_os = "macos")]
    println!(
        "    {}",
        "brew install --cask macfuse   # kernel extension, reboot required".custom_color(crate::colors::INK_DIM),
    );
    #[cfg(target_os = "linux")]
    println!(
        "    {}",
        "apt install libfuse3-dev      # or your distro's libfuse3 headers".custom_color(crate::colors::INK_DIM),
    );
    println!(
        "    {}",
        "cargo install --git https://github.com/beebeeb-io/cli --features fuse".custom_color(crate::colors::INK_DIM),
    );
    println!();
    Err(NO_FUSE_ERR.to_string())
}

#[cfg(not(feature = "fuse"))]
pub async fn unmount(_mountpoint: PathBuf) -> Result<(), String> {
    Err(NO_FUSE_ERR.replace("bb mount", "bb unmount"))
}

// ─── Full implementation (fuse feature enabled) ───────────────────────────────

#[cfg(feature = "fuse")]
mod fuse_impl;


/// L13 (SF-0001): the folder id to record after a create — the one this client
/// minted, confirmed against the answer's echo. Kept outside the FUSE module so
/// the decision is unit-tested without a FUSE mount.
#[cfg_attr(not(feature = "fuse"), allow(dead_code))]
fn folder_create_reply(resp: &serde_json::Value, folder_uuid: uuid::Uuid) -> Result<String, String> {
    crate::item_id::verify_minted_echo(folder_uuid, crate::item_id::echoed_id(resp))
        .map(|id| id.to_string())
        .map_err(|e| e.to_string())
}

/// What a FUSE release uploaded: the id this client minted (it derives the
/// content key AND names the file on the server: one id) and the chunk count.
#[cfg(any(feature = "fuse", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UploadedFile {
    id: uuid::Uuid,
    chunk_count: u32,
}

/// Encrypt and upload `plaintext` as a NEW file under an id minted here (the
/// FUSE `release` path). L13 (SF-0001): the init must echo that id exactly
/// before any chunk is PUT, and the completion must echo it too. On either
/// mismatch the session just opened is abandoned, by the minted id (never the
/// echoed one), and the error is the plain changed-id message; the caller then
/// records no server id. Kept outside the FUSE module so it is tested without a
/// mount.
#[cfg(any(feature = "fuse", test))]
async fn upload_new_file(
    api: &crate::api::ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    name: &str,
    plaintext: &[u8],
    parent_id: Option<uuid::Uuid>,
) -> Result<UploadedFile, String> {
    let minted = crate::item_id::Proposed::Minted(crate::item_id::mint());
    let key_uuid = minted.id();
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, key_uuid.to_string().as_bytes());

    // Encrypt the filename (MIME type is part of the encrypted envelope).
    let mime = beebeeb_core::media::guess_mime_type(name);
    let name_enc = beebeeb_core::encrypt::encrypt_name(master_key, &key_uuid.to_string(), name, mime)
        .map_err(|e| format!("encrypt name: {e}"))?;

    // Encrypt content using the adaptive chunk-size ladder.
    let plan = beebeeb_types::plan_chunks(plaintext.len() as u64, beebeeb_types::ChunkProfile::Cli);
    let chunk_size = plan.chunk_size_bytes as usize;
    let chunks: Vec<&[u8]> = if plaintext.is_empty() {
        vec![&[]] // at least one (empty) chunk
    } else {
        plaintext.chunks(chunk_size).collect()
    };
    let mut encrypted_chunks: Vec<Vec<u8>> = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let bytes = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, chunk)
            .map_err(|e| format!("encrypt chunk {i}: {e}"))?;
        encrypted_chunks.push(bytes);
    }
    let chunk_count = encrypted_chunks.len() as i32;
    let is_media = beebeeb_core::media::is_media(mime);

    // V2 upload: init (open session) → chunks (by session) → complete. A new
    // file, so no version replace (`base_version_number` None). The v2 init
    // takes the PLAINTEXT size; the server recomputes the stored total.
    let init = api
        .upload_init(
            Some(key_uuid),
            &name_enc,
            parent_id,
            plaintext.len() as i64,
            chunk_size as i64,
            chunk_count,
            is_media,
            None,
        )
        .await?;
    if let Err(e) = minted.verify(Some(&init.file_id)) {
        let _ = api.upload_abandon(&key_uuid.to_string()).await;
        return Err(e.to_string());
    }

    for (idx, data) in encrypted_chunks.into_iter().enumerate() {
        api.upload_chunk(&init.upload_session_id, idx as u32, bytes::Bytes::from(data))
            .await?;
    }

    let completed = api.upload_complete(&init.upload_session_id).await?;
    if let Err(e) = minted.verify(crate::item_id::echoed_id(&completed)) {
        let _ = api.upload_abandon(&key_uuid.to_string()).await;
        return Err(e.to_string());
    }

    Ok(UploadedFile {
        id: key_uuid,
        chunk_count: chunk_count as u32,
    })
}

#[cfg(feature = "fuse")]
pub use fuse_impl::{run, unmount};

#[cfg(test)]
mod parent_target_tests {
    use super::*;
    use crate::commands::move_target::ParentError;

    #[test]
    fn fuse_parent_target_root_folder_unchanged() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(rename_parent_target(2, ROOT_INO, None), Ok(Some(None)));
        assert_eq!(
            rename_parent_target(ROOT_INO, 2, Some((true, Some(&id.to_string())))),
            Ok(Some(Some(id)))
        );
        assert_eq!(rename_parent_target(2, 2, None), Ok(None));
    }

    #[test]
    fn fuse_parent_target_unknown_inode() {
        assert_eq!(rename_parent_target(ROOT_INO, 9, None), Err(ParentError::NotFound));
    }

    #[test]
    fn fuse_parent_target_missing_id() {
        assert_eq!(
            rename_parent_target(ROOT_INO, 9, Some((true, None))),
            Err(ParentError::MissingId)
        );
    }

    #[test]
    fn fuse_parent_target_invalid_id() {
        assert_eq!(
            rename_parent_target(ROOT_INO, 9, Some((true, Some("invalid")))),
            Err(ParentError::InvalidId)
        );
    }

    #[test]
    fn fuse_parent_target_file() {
        assert_eq!(
            rename_parent_target(ROOT_INO, 9, Some((false, Some("invalid")))),
            Err(ParentError::NotFolder)
        );
    }
}

#[cfg(test)]
mod item_id_echo_tests {
    use super::{UploadedFile, folder_create_reply, upload_new_file};
    use crate::test_server::{Mock, Shared, shared, spawn};
    use serde_json::json;

    fn master() -> beebeeb_core::kdf::MasterKey {
        beebeeb_core::kdf::MasterKey::from_bytes([4u8; 32])
    }

    async fn upload(mock: Mock) -> (Result<UploadedFile, String>, Shared) {
        let mock = shared(mock);
        let api = crate::api::ApiClient::new_for_test(spawn(mock.clone()).await);
        (upload_new_file(&api, &master(), "new.txt", b"hello", None).await, mock)
    }

    /// L1 (mount, new file): the init echoes another id. Zero chunk PUTs, no
    /// completion, the session it opened ended once by the minted id, and the
    /// plain message (the FUSE release turns it into EIO; the inode records
    /// nothing, see `fuse_impl::truncate_tests`).
    #[tokio::test]
    async fn new_file_whose_init_echoes_another_id_puts_nothing() {
        let (result, mock) = upload(Mock {
            init_echo: Some(uuid::Uuid::new_v4().to_string()),
            ..Default::default()
        })
        .await;
        assert_eq!(result, Err(crate::item_id::CHANGED_ID_MESSAGE.to_string()));
        let m = mock.lock().unwrap();
        assert_eq!(m.inits.len(), 1);
        assert!(
            m.chunk_puts.is_empty(),
            "no chunk under the echoed id: {:?}",
            m.chunk_puts
        );
        assert!(m.completed.is_empty(), "never completed");
        assert_eq!(m.abandons, m.inits, "ended once, by the minted id");
    }

    /// L1 (mount, new file): the completion answer names another item. The
    /// upload fails with the plain message and the session is ended once.
    #[tokio::test]
    async fn new_file_whose_completion_echoes_another_id_fails() {
        let (result, mock) = upload(Mock {
            complete_echo: Some(uuid::Uuid::new_v4().to_string()),
            ..Default::default()
        })
        .await;
        assert_eq!(result, Err(crate::item_id::CHANGED_ID_MESSAGE.to_string()));
        let m = mock.lock().unwrap();
        assert_eq!(m.completed.len(), 1, "the check runs on the completion answer");
        assert_eq!(m.abandons, m.inits, "ended once, by the minted id");
    }

    /// Normal flow: the minted v4 id is proposed, echoed, and returned.
    #[tokio::test]
    async fn new_file_echoing_the_minted_id_is_uploaded_under_it() {
        let (result, mock) = upload(Mock::default()).await;
        let uploaded = result.expect("uploads");
        let m = mock.lock().unwrap();
        assert_eq!(m.inits, vec![uploaded.id.to_string()]);
        assert_eq!(uploaded.id.get_version(), Some(uuid::Version::Random));
        assert_eq!(uploaded.chunk_count, 1);
        assert_eq!(m.chunk_puts.len(), 1);
        assert!(m.abandons.is_empty());
    }

    /// A8 (mount): the folder id recorded after a create is the client's own,
    /// confirmed against the answer. A different echo aborts (the FUSE mkdir
    /// turns this error into EIO); no id from the answer is ever used.
    #[test]
    fn folder_create_echoing_a_different_id_aborts() {
        let mine = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let err = folder_create_reply(&json!({"id": other}), mine).expect_err("must abort");
        assert_eq!(err, crate::item_id::CHANGED_ID_MESSAGE);
    }

    #[test]
    fn folder_create_echoing_the_clients_id_is_recorded() {
        let mine = uuid::Uuid::new_v4();
        assert_eq!(folder_create_reply(&json!({"id": mine}), mine), Ok(mine.to_string()));
    }

    #[test]
    fn folder_create_without_an_echo_aborts() {
        let mine = uuid::Uuid::new_v4();
        assert_eq!(
            folder_create_reply(&json!({}), mine),
            Err(crate::item_id::CHANGED_ID_MESSAGE.to_string())
        );
    }
}
