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
mod fuse_impl {
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use base64::Engine as _;
    use colored::Colorize;
    use fuser::{
        BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
        LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry,
        ReplyOpen, Request, SessionACL, WriteFlags,
    };
    use zeroize::Zeroizing;

    use crate::api::ApiClient;
    use crate::config::load_config;

    // ── Constants ─────────────────────────────────────────────────────────────

    use super::{ROOT_INO, rename_parent_target};
    const ATTR_TTL: Duration = Duration::from_secs(5);
    /// Largest size a single `setattr(size = …)` may produce. The CLI has no
    /// mount-specific file limit, so this mirrors the buffered download ceiling
    /// in `download.rs`: a resize above it is refused with `EFBIG` before any
    /// allocation is attempted.
    const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024 * 1024;
    // Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
    // See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

    /// Permission bits for one inode: `0o700` for directories, `0o600` for
    /// files — owner only. The mount is the decrypted vault, so no other local
    /// account may list a directory or read a file. Every attribute path goes
    /// through here, so the mode cannot drift between `getattr`, `lookup`,
    /// `create` and `setattr` replies.
    fn node_perm(is_dir: bool) -> u16 {
        if is_dir { 0o700 } else { 0o600 }
    }

    // ── Truncate ──────────────────────────────────────────────────────────────

    /// Resize `existing` to `new_len` bytes: a grow zero-pads the tail, a
    /// shrink keeps the prefix. The first `min(len)` bytes are never changed,
    /// so a truncate cannot turn a file's content into zeros.
    fn resized(existing: &[u8], new_len: u64) -> Vec<u8> {
        resize_owned(existing.to_vec(), new_len)
    }

    /// [`resized`] for a buffer the caller already owns: a shrink truncates in
    /// place and a grow reuses the allocation, so a resident write buffer is
    /// resized without an extra copy.
    fn resize_owned(mut existing: Vec<u8>, new_len: u64) -> Vec<u8> {
        let new_len = usize::try_from(new_len).unwrap_or(usize::MAX);
        if new_len <= existing.len() {
            existing.truncate(new_len);
        } else {
            existing.resize(new_len, 0);
        }
        existing
    }

    /// Why a `setattr(size = …)` was refused. The errno is what the kernel (and
    /// therefore the application) sees.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ResizeError {
        /// The inode is not in the table: `ENOENT`, not `EIO`.
        NotFound,
        /// The requested size is above [`MAX_FILE_SIZE`]: `EFBIG`.
        TooLarge,
        /// The file's content could not be loaded, so resizing it would replace
        /// it with zeros: `EIO`, with nothing changed.
        Unavailable,
    }

    impl ResizeError {
        fn errno(self) -> Errno {
            match self {
                Self::NotFound => Errno::ENOENT,
                Self::TooLarge => Errno::EFBIG,
                Self::Unavailable => Errno::EIO,
            }
        }
    }

    /// What a `setattr(size = …)` resizes, decided before anything is read or
    /// marked dirty.
    ///
    /// A stored file's content must be loaded before it is resized: starting
    /// from an empty buffer and zero-filling replaces the file with zeros on
    /// the next `release()` (task PV-0007). Only [`Unchanged`](Self::Unchanged)
    /// leaves the file alone; every other outcome resizes and then marks the
    /// file dirty.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TruncateSource {
        /// The requested size is already the current size: nothing changes and
        /// `release()` uploads nothing.
        Unchanged,
        /// A write buffer is already resident (a create, a write, or an
        /// earlier resize): resize it in place.
        Buffer,
        /// The read cache holds the decrypted content at the inode's current
        /// size: resize that copy.
        Cache,
        /// Load the stored content (the same path `open`/`read` use), then
        /// resize it.
        Download,
        /// There is nothing to keep: a pending create, a directory, or a
        /// truncate to zero. Resize an empty buffer.
        Fresh,
        /// The file has stored content that is not resident (no buffer, no
        /// matching cache) and the requested size is above zero, so it cannot
        /// be resized without losing it: answer `EIO`.
        Unavailable,
    }

    /// Decide what a size request resizes. `known_size` is the length already
    /// known without a download (the resident buffer's, else the inode's).
    /// `cache_matches_size` says the read cache holds exactly the inode's
    /// current length — a cache left by an earlier read of a since-truncated
    /// file must never be treated as this file's content.
    ///
    /// Truncating to zero keeps nothing, so it never downloads — `O_TRUNC`
    /// and `truncate -s 0` stay as cheap as they are today.
    fn truncate_source(
        has_buffer: bool,
        has_stored_content: bool,
        cache_matches_size: bool,
        known_size: u64,
        new_size: u64,
    ) -> TruncateSource {
        if new_size == known_size {
            // Nothing to resize: the file must not become dirty.
            TruncateSource::Unchanged
        } else if has_buffer {
            TruncateSource::Buffer
        } else if new_size == 0 {
            // Truncating to zero keeps nothing, so it needs no content.
            TruncateSource::Fresh
        } else if cache_matches_size {
            TruncateSource::Cache
        } else if has_stored_content {
            TruncateSource::Download
        } else {
            // A file with no stored content and nothing resident (the state
            // after a failed release upload) cannot grow without inventing
            // bytes: only a genuinely new, empty file may start from nothing,
            // and that arrives as a pending create.
            TruncateSource::Unavailable
        }
    }

    // ── Inode table entry ─────────────────────────────────────────────────────

    #[derive(Clone)]
    struct InodeEntry {
        /// Server-assigned file ID, used for API calls (download, trash, move).
        file_id: Option<String>,
        /// UUID used for *key* derivation.  For server-side files this equals
        /// `file_id` (both are the server UUID).  For files we create via FUSE
        /// we generate a local UUID first; the server may assign a different ID.
        key_uuid: Option<uuid::Uuid>,
        name: String,
        is_dir: bool,
        size: u64,
        chunk_count: u32,
        modified: SystemTime,
        parent_ino: u64,
        /// The content's read pin from the file's authenticated metadata
        /// (`crypto::content_pin`, task 1943). `Err` for a directory, or for a
        /// file whose metadata cannot be read (its content is then refused).
        pin: Result<beebeeb_core::chunk_stream::ExpectedChunkFormat, String>,
        /// SF-0002: the name resolved under the read-only uppercase repair
        /// convention. A rename is refused with EPERM; the content is checked
        /// at rename time as well (the name alone does not decide).
        read_only: bool,
    }

    /// What a directory carries in [`InodeEntry::pin`].
    fn no_content() -> Result<beebeeb_core::chunk_stream::ExpectedChunkFormat, String> {
        Err("a directory has no content".to_string())
    }

    impl InodeEntry {
        fn file_attr(&self, ino: u64) -> FileAttr {
            let kind = if self.is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            FileAttr {
                ino: INodeNo(ino),
                size: self.size,
                blocks: self.size.div_ceil(512),
                atime: self.modified,
                mtime: self.modified,
                ctime: self.modified,
                crtime: self.modified,
                kind,
                // Owner-only: this is the decrypted vault.
                perm: node_perm(self.is_dir),
                nlink: if self.is_dir { 2 } else { 1 },
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
                rdev: 0,
                blksize: 4096,
                flags: 0,
            }
        }
    }

    // ── Write-buffer state ────────────────────────────────────────────────────

    /// Metadata we need at `release()` time for newly-created files.
    struct PendingCreate {
        parent_ino: u64,
        name: String,
        /// SF-0002 r3 (K3): the stored row this create replaces (a `create()`
        /// over an existing name). Trashed only after the new file's upload
        /// completed; a failed upload leaves it untouched.
        replaces: Option<String>,
    }

    /// What `release()` does with the buffer it takes off an inode. One pure
    /// decision, used both by `release()` and by the tests that assert what
    /// would be uploaded.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ReleaseUpload {
        /// No buffer, or a clean release: nothing is uploaded (`release()`
        /// case 3).
        Nothing,
        /// A brand-new file created via `create()` (`release()` case 1).
        NewFile,
        /// An existing file that was written to or resized (`release()` case 2).
        Overwrite,
    }

    fn release_upload(has_buffer: bool, is_pending_create: bool, is_dirty: bool) -> ReleaseUpload {
        if !has_buffer {
            ReleaseUpload::Nothing
        } else if is_pending_create {
            ReleaseUpload::NewFile
        } else if is_dirty {
            ReleaseUpload::Overwrite
        } else {
            ReleaseUpload::Nothing
        }
    }

    // ── Directory cache entry ─────────────────────────────────────────────────

    struct CachedDir {
        children: Vec<u64>,
        expires_at: std::time::Instant,
    }

    // ── Filesystem state ──────────────────────────────────────────────────────

    pub struct BeebeebFs {
        /// fuser 0.17+ calls the `Filesystem` methods through `&self` and
        /// requires the type to be `Send + Sync`. All mutable state lives
        /// behind one mutex; the default single-threaded session takes it
        /// once per request.
        inner: std::sync::Mutex<BeebeebFsState>,
    }

    struct BeebeebFsState {
        rt: tokio::runtime::Runtime,
        api: ApiClient,
        master_key: beebeeb_core::kdf::MasterKey,
        inodes: HashMap<u64, InodeEntry>,
        id_to_ino: HashMap<String, u64>,
        next_ino: u64,
        dir_cache: HashMap<u64, CachedDir>,
        /// Downloaded + decrypted file content (read cache).
        file_cache: HashMap<u64, Vec<u8>>,
        /// Write buffers keyed by inode number. Presence = file needs upload on `release`.
        write_buffers: HashMap<u64, Vec<u8>>,
        /// Metadata for brand-new files (created via `create`).
        pending_creates: HashMap<u64, PendingCreate>,
        /// Inodes that have been written to and need re-upload (existing files
        /// opened with O_TRUNC / O_WRONLY). Distinct from pending_creates.
        dirty_inodes: HashSet<u64>,
        /// SF-0002 r3 (K5): ids this mount uploaded, each under a key it minted
        /// (the canonical string-id key). Their write check is decided here,
        /// without a probe: an editor's atomic save never waits on a download.
        minted: HashSet<String>,
        cache_ttl: Duration,
    }

    impl BeebeebFs {
        pub fn new(api: ApiClient, master_key: beebeeb_core::kdf::MasterKey, cache_ttl: Duration) -> Self {
            Self {
                inner: std::sync::Mutex::new(BeebeebFsState::new(api, master_key, cache_ttl)),
            }
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, BeebeebFsState> {
            // A panic while holding the lock must not take the whole mount
            // down: the state itself is still consistent enough to continue.
            self.inner.lock().unwrap_or_else(|e| e.into_inner())
        }
    }

    impl BeebeebFsState {
        fn new(api: ApiClient, master_key: beebeeb_core::kdf::MasterKey, cache_ttl: Duration) -> Self {
            let mut fs = Self {
                rt: tokio::runtime::Runtime::new().expect("failed to create FUSE tokio runtime"),
                api,
                master_key,
                inodes: HashMap::new(),
                id_to_ino: HashMap::new(),
                next_ino: 2,
                dir_cache: HashMap::new(),
                file_cache: HashMap::new(),
                write_buffers: HashMap::new(),
                pending_creates: HashMap::new(),
                dirty_inodes: HashSet::new(),
                minted: HashSet::new(),
                cache_ttl,
            };
            fs.inodes.insert(
                ROOT_INO,
                InodeEntry {
                    file_id: None,
                    key_uuid: None,
                    name: "/".to_string(),
                    is_dir: true,
                    size: 0,
                    chunk_count: 0,
                    modified: SystemTime::now(),
                    parent_ino: ROOT_INO,
                    pin: no_content(),
                    read_only: false,
                },
            );
            fs
        }

        fn alloc_ino(&mut self) -> u64 {
            let ino = self.next_ino;
            self.next_ino += 1;
            ino
        }

        // ── Directory listing ─────────────────────────────────────────────────

        fn populate_dir(&mut self, dir_ino: u64) -> Vec<u64> {
            if let Some(cached) = self.dir_cache.get(&dir_ino) {
                if cached.expires_at > std::time::Instant::now() {
                    return cached.children.clone();
                }
            }

            let parent_id = self.inodes.get(&dir_ino).and_then(|e| e.file_id.clone());

            let resp = match self.rt.block_on(self.api.list_files(parent_id.as_deref())) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[mount] list_files: {e}");
                    return vec![];
                }
            };

            let files = match resp.get("files").and_then(|f| f.as_array()) {
                Some(f) => f.clone(),
                None => return vec![],
            };

            let mut children = Vec::new();

            for file in &files {
                let file_id = match file.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id.to_string(),
                    None => continue,
                };
                let is_folder = file.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
                let size_bytes = file.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                let chunk_count = file.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
                let modified = file
                    .get("updated_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| UNIX_EPOCH + Duration::from_secs(dt.timestamp().max(0) as u64))
                    .unwrap_or(SystemTime::now());

                let name_encrypted = file.get("name_encrypted").and_then(|v| v.as_str());
                let name = match name_encrypted {
                    Some(enc) => decrypt_name(&self.master_key, &file_id, enc),
                    None => format!("[{}]", &file_id[..8.min(file_id.len())]),
                };
                let read_only = name_encrypted
                    .is_some_and(|enc| crate::crypto::name_is_upper_repair(&self.master_key, &file_id, enc));
                let pin = match (is_folder, name_encrypted) {
                    (true, _) => no_content(),
                    (false, Some(enc)) => crate::crypto::content_pin(&self.master_key, &file_id, enc),
                    (false, None) => Err(format!("file {file_id} has no encrypted metadata")),
                };

                let ino = if let Some(&existing) = self.id_to_ino.get(&file_id) {
                    if let Some(entry) = self.inodes.get_mut(&existing) {
                        entry.name = name;
                        entry.size = size_bytes;
                        entry.chunk_count = chunk_count;
                        entry.modified = modified;
                        entry.pin = pin;
                        entry.read_only = read_only;
                    }
                    existing
                } else {
                    let ino = self.alloc_ino();
                    // For server files, key_uuid == file_id UUID.
                    let key_uuid = file_id.parse().ok();
                    self.id_to_ino.insert(file_id.clone(), ino);
                    self.inodes.insert(
                        ino,
                        InodeEntry {
                            file_id: Some(file_id),
                            key_uuid,
                            name,
                            is_dir: is_folder,
                            size: size_bytes,
                            chunk_count,
                            modified,
                            parent_ino: dir_ino,
                            pin,
                            read_only,
                        },
                    );
                    ino
                };

                children.push(ino);
            }

            if !self.cache_ttl.is_zero() {
                self.dir_cache.insert(
                    dir_ino,
                    CachedDir {
                        children: children.clone(),
                        expires_at: std::time::Instant::now() + self.cache_ttl,
                    },
                );
            }

            children
        }

        fn find_child(&mut self, parent_ino: u64, name: &str) -> Option<u64> {
            let children = self.populate_dir(parent_ino);
            children
                .into_iter()
                .find(|&ino| self.inodes.get(&ino).is_some_and(|e| e.name == name))
        }

        // ── File download ─────────────────────────────────────────────────────

        fn fetch_file(&mut self, ino: u64) -> Result<(), String> {
            if self.file_cache.contains_key(&ino) {
                return Ok(());
            }
            let entry = self.inodes.get(&ino).ok_or("inode not found")?;
            let file_id = entry.file_id.as_ref().ok_or("no file_id")?.clone();
            let chunk_count = entry.chunk_count;
            let pin = entry.pin.clone()?;

            let encrypted = self.rt.block_on(self.api.download_file(&file_id))?;
            // Use the shared helper that handles both CLI-format (JSON) and
            // web-app-format (raw binary) chunks, and both UUID key derivation
            // methods (binary and string).
            let plaintext =
                crate::crypto::decrypt_file_chunks(&self.master_key, &file_id, pin, &encrypted, chunk_count)?;

            if let Some(e) = self.inodes.get_mut(&ino) {
                e.size = plaintext.len() as u64;
            }
            self.file_cache.insert(ino, plaintext);
            Ok(())
        }

        // ── File upload (called from release()) ───────────────────────────────

        fn upload_file(&mut self, ino: u64, name: &str, plaintext: Vec<u8>, parent_ino: u64) -> Result<(), String> {
            // Resolve parent_id (None = root).
            let parent_file_id: Option<uuid::Uuid> = self
                .inodes
                .get(&parent_ino)
                .and_then(|e| e.file_id.as_deref())
                .and_then(|s| s.parse().ok());

            // L13 (SF-0001): a new file under an id minted here, every echo
            // checked; an error leaves the inode exactly as it was.
            let uploaded = self.rt.block_on(super::upload_new_file(
                &self.api,
                &self.master_key,
                name,
                &plaintext,
                parent_file_id,
            ))?;

            // The server now has this content: a cache entry from before the
            // write describes the previous version and must not survive.
            self.file_cache.remove(&ino);

            // Update inode: the minted id names the file AND derives its key.
            let file_id = uploaded.id.to_string();
            if let Some(entry) = self.inodes.get_mut(&ino) {
                self.id_to_ino.remove(entry.file_id.as_deref().unwrap_or(""));
                entry.file_id = Some(file_id.clone());
                entry.key_uuid = Some(uploaded.id);
                entry.size = plaintext.len() as u64;
                entry.chunk_count = uploaded.chunk_count;
                entry.modified = SystemTime::now();
                // SF-0002 r3 (K4): this is a NEW file, written here under the
                // canonical key: it is not read-only, and its frames are the
                // new-file writer's (Unrecorded pin, task 1943), not the old row's.
                entry.read_only = false;
                entry.pin = Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded);
            }
            self.minted.insert(file_id.clone());
            self.id_to_ino.insert(file_id, ino);

            // Invalidate parent dir cache so readdir shows the new file.
            self.dir_cache.remove(&parent_ino);

            Ok(())
        }

        // ── Root attrs ────────────────────────────────────────────────────────

        fn root_attr() -> FileAttr {
            FileAttr {
                ino: INodeNo(ROOT_INO),
                size: 0,
                blocks: 0,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: FileType::Directory,
                perm: node_perm(true),
                nlink: 2,
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
                rdev: 0,
                blksize: 4096,
                flags: 0,
            }
        }
    }

    // ── FUSE request handlers ─────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    impl BeebeebFsState {
        // ─── Read operations ──────────────────────────────────────────────────

        fn lookup(&mut self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
            let parent = parent.0;
            let name_str = match name.to_str() {
                Some(n) => n.to_string(),
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };
            match self.find_child(parent, &name_str) {
                Some(ino) => {
                    let attr = if ino == ROOT_INO {
                        Self::root_attr()
                    } else {
                        match self.inodes.get(&ino) {
                            Some(e) => e.clone().file_attr(ino),
                            None => {
                                reply.error(Errno::ENOENT);
                                return;
                            }
                        }
                    };
                    reply.entry(&ATTR_TTL, &attr, Generation(0));
                }
                None => reply.error(Errno::ENOENT),
            }
        }

        fn getattr(&mut self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
            let ino = ino.0;
            if ino == ROOT_INO {
                reply.attr(&ATTR_TTL, &Self::root_attr());
                return;
            }
            match self.inodes.get(&ino) {
                Some(e) => {
                    let a = e.clone().file_attr(ino);
                    reply.attr(&ATTR_TTL, &a);
                }
                None => reply.error(Errno::ENOENT),
            }
        }

        fn readdir(&mut self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
            let ino = ino.0;
            let parent_ino = self.inodes.get(&ino).map(|e| e.parent_ino).unwrap_or(ROOT_INO);
            let mut entries: Vec<(u64, FileType, String)> = vec![
                (ino, FileType::Directory, ".".to_string()),
                (parent_ino, FileType::Directory, "..".to_string()),
            ];

            let children = self.populate_dir(ino);
            for &child_ino in &children {
                if let Some(e) = self.inodes.get(&child_ino) {
                    let kind = if e.is_dir {
                        FileType::Directory
                    } else {
                        FileType::RegularFile
                    };
                    entries.push((child_ino, kind, e.name.clone()));
                }
            }

            for (i, (child_ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
                if reply.add(INodeNo(*child_ino), (i + 1) as u64, *kind, name) {
                    break;
                }
            }
            reply.ok();
        }

        fn open(&mut self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
            let ino = ino.0;
            match self.inodes.get(&ino) {
                Some(e) if !e.is_dir => reply.opened(FileHandle(ino), FopenFlags::empty()),
                Some(_) => reply.error(Errno::EISDIR),
                None => reply.error(Errno::ENOENT),
            }
        }

        fn read(
            &mut self,
            _req: &Request,
            ino: INodeNo,
            _fh: FileHandle,
            offset: u64,
            size: u32,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            reply: ReplyData,
        ) {
            let ino = ino.0;
            if !self.file_cache.contains_key(&ino) {
                if let Err(e) = self.fetch_file(ino) {
                    eprintln!("[mount] read ino={ino}: {e}");
                    reply.error(Errno::EIO);
                    return;
                }
            }
            match self.file_cache.get(&ino) {
                Some(data) => {
                    let start = (offset as usize).min(data.len());
                    let end = (start + size as usize).min(data.len());
                    reply.data(&data[start..end]);
                }
                None => reply.error(Errno::EIO),
            }
        }

        // ─── Write operations ─────────────────────────────────────────────────

        /// Create a new file in `parent` with `name`.
        /// Initialises a write buffer; the file is uploaded on `release()`.
        fn create(
            &mut self,
            _req: &Request,
            parent: INodeNo,
            name: &OsStr,
            _mode: u32,
            _umask: u32,
            _flags: i32,
            reply: fuser::ReplyCreate,
        ) {
            let parent = parent.0;
            let name_str = match name.to_str() {
                Some(n) => n.to_string(),
                None => {
                    reply.error(Errno::EINVAL);
                    return;
                }
            };

            // A file with this name already exists in parent: the new file
            // replaces it. SF-0002 r3 (K3): ask the write check now, and trash
            // the old row only once the new one is uploaded (release).
            let replaces = match self.take_replaced_child(parent, &name_str) {
                Ok(replaces) => replaces,
                Err(errno) => {
                    reply.error(errno);
                    return;
                }
            };

            let ino = self.alloc_ino();
            let now = SystemTime::now();
            self.inodes.insert(
                ino,
                InodeEntry {
                    file_id: None,
                    key_uuid: None, // assigned in upload_file()
                    name: name_str.clone(),
                    is_dir: false,
                    size: 0,
                    chunk_count: 0,
                    modified: now,
                    parent_ino: parent,
                    // This mount uploads it as a NEW file, which never records a
                    // pin (task 1943: the new-file writer pin is Unrecorded).
                    pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
                    read_only: false,
                },
            );
            self.pending_creates.insert(
                ino,
                PendingCreate {
                    parent_ino: parent,
                    name: name_str,
                    replaces,
                },
            );
            self.write_buffers.insert(ino, Vec::new());

            let attr = self.inodes[&ino].file_attr(ino);
            // generation=0, fh=ino (we key write_buffers by ino), no open flags
            reply.created(&ATTR_TTL, &attr, Generation(0), FileHandle(ino), FopenFlags::empty());
        }

        /// Apply one write to `ino`'s buffer. Shared by `write()` and the tests,
        /// so the dirty decision below is the one the mount actually makes.
        ///
        /// A write into an existing file marks it dirty: the buffer now holds
        /// content the server does not have, so `release()` must upload it.
        /// Writes into a pending create stay non-dirty (case 1 already uploads).
        fn apply_write(&mut self, ino: u64, offset: u64, data: &[u8]) -> Result<(), Errno> {
            if !self.write_buffers.contains_key(&ino) {
                // A write needs a buffer. An existing empty file legitimately
                // starts from nothing: `echo x > empty` opens it with O_TRUNC,
                // which arrives as `setattr(size = 0)` on a size-0 file and
                // changes nothing, so no buffer is created there. A non-empty
                // stored file without a buffer would lose its content if we
                // started one from zero, so it stays refused.
                let empty_existing = self.inodes.get(&ino).is_some_and(|e| !e.is_dir && e.size == 0);
                if !empty_existing {
                    return Err(Errno::ENOTSUP);
                }
                self.write_buffers.insert(ino, Vec::new());
            }

            let offset = offset as usize;
            let buf = self.write_buffers.get_mut(&ino).expect("buffer inserted above");

            // Extend to cover offset + data.len(), filling gaps with zeros
            // (supports sparse writes from tools like rsync).
            let needed = offset.saturating_add(data.len());
            if needed > buf.len() {
                buf.resize(needed, 0);
            }
            buf[offset..offset + data.len()].copy_from_slice(data);

            // Keep inode size in sync so getattr returns the right value.
            if let Some(entry) = self.inodes.get_mut(&ino) {
                entry.size = buf.len() as u64;
            }

            if !self.pending_creates.contains_key(&ino) {
                self.dirty_inodes.insert(ino);
            }
            Ok(())
        }

        /// Accumulate `data` at `offset` into the in-memory write buffer.
        fn write(
            &mut self,
            _req: &Request,
            ino: INodeNo,
            _fh: FileHandle,
            offset: u64,
            data: &[u8],
            _write_flags: WriteFlags,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            reply: fuser::ReplyWrite,
        ) {
            match self.apply_write(ino.0, offset, data) {
                Ok(()) => reply.written(data.len() as u32),
                Err(e) => reply.error(e),
            }
        }

        /// Apply a `setattr(size = new_size)` to `ino`.
        ///
        /// A stored file's content is loaded first (the same path `read` uses)
        /// and then resized, so `truncate -s N` keeps the file's first N bytes;
        /// a grow zero-pads the tail. Only a resize that changes the length
        /// marks the file dirty, so an unchanged size uploads nothing.
        ///
        /// `Err` means nothing was resized: the inode is unknown (`ENOENT`),
        /// the size is past [`MAX_FILE_SIZE`] (`EFBIG`), or the stored content
        /// could not be read (`EIO`). In every case the write buffer, the inode
        /// size and the dirty set are left untouched, so `release()` cannot
        /// replace the file with zeros.
        fn resize_file(&mut self, ino: u64, new_size: u64) -> Result<TruncateSource, ResizeError> {
            if new_size > MAX_FILE_SIZE {
                // Refuse before touching the buffer: a resize may not be able
                // to allocate what it was asked for.
                return Err(ResizeError::TooLarge);
            }
            let (is_dir, inode_size, has_file_id) = match self.inodes.get(&ino) {
                Some(e) => (e.is_dir, e.size, e.file_id.is_some()),
                None => return Err(ResizeError::NotFound),
            };
            let pending_create = self.pending_creates.contains_key(&ino);
            let has_buffer = self.write_buffers.contains_key(&ino);
            // The read cache is this file's content only when its length is
            // the length the inode says. A cache left by a read of the file
            // before a truncate must not resurrect the removed tail.
            let cache_matches_size = self.file_cache.get(&ino).is_some_and(|c| c.len() as u64 == inode_size);
            let has_stored_content = !is_dir && !pending_create && has_file_id;
            // The length already known without a download: the resident
            // buffer's, else the inode's.
            let known_size = if let Some(buf) = self.write_buffers.get(&ino) {
                buf.len() as u64
            } else {
                inode_size
            };

            // Directories, and a pending create with nothing written yet, have
            // no stored content to load: resize an empty buffer as before.
            let source = if is_dir || (pending_create && !has_buffer) {
                TruncateSource::Fresh
            } else {
                truncate_source(has_buffer, has_stored_content, cache_matches_size, known_size, new_size)
            };

            // Same size: leave the buffer, the cache and the dirty set alone,
            // so release() uploads nothing.
            if source == TruncateSource::Unchanged {
                return Ok(source);
            }

            if source == TruncateSource::Unavailable {
                // Nothing to resize from, and the requested size is above
                // zero: inventing zero bytes would replace the file. Leave
                // everything as it was.
                return Err(ResizeError::Unavailable);
            }

            let existing: Vec<u8> = match source {
                TruncateSource::Buffer => self.write_buffers.remove(&ino).unwrap_or_default(),
                TruncateSource::Cache => self.file_cache.remove(&ino).unwrap_or_default(),
                TruncateSource::Download => {
                    // A cache entry that does not match the inode's size is
                    // not this file's content; drop it so `fetch_file` really
                    // downloads instead of handing the stale copy back. On
                    // failure it returns before anything below, so the buffer,
                    // the inode size and the dirty set are untouched.
                    self.file_cache.remove(&ino);
                    self.fetch_file(ino).map_err(|e| {
                        eprintln!("[mount] resize ino={ino}: content could not be loaded: {e}");
                        ResizeError::Unavailable
                    })?;
                    self.file_cache.remove(&ino).unwrap_or_default()
                }
                TruncateSource::Fresh | TruncateSource::Unchanged | TruncateSource::Unavailable => Vec::new(),
            };

            // A resident buffer is resized in place (no extra copy); the
            // others are copied out of their source.
            let buf = if source == TruncateSource::Buffer {
                resize_owned(existing, new_size)
            } else {
                resized(&existing, new_size)
            };
            self.write_buffers.insert(ino, buf);
            if !pending_create {
                self.dirty_inodes.insert(ino);
            }
            if let Some(entry) = self.inodes.get_mut(&ino) {
                entry.size = new_size;
            }
            Ok(source)
        }

        /// Handle attribute changes:
        /// - chmod / chown → no-op, return current attr
        /// - truncate → load the stored content, then resize it
        fn setattr(
            &mut self,
            _req: &Request,
            ino: INodeNo,
            // Ignored: a requested mode is never applied, so `chmod` cannot widen
            // the owner-only modes from `node_perm()`.
            _mode: Option<u32>,
            _uid: Option<u32>,
            _gid: Option<u32>,
            size: Option<u64>,
            _atime: Option<fuser::TimeOrNow>,
            _mtime: Option<fuser::TimeOrNow>,
            _ctime: Option<SystemTime>,
            _fh: Option<FileHandle>,
            _crtime: Option<SystemTime>,
            _chgtime: Option<SystemTime>,
            _bkuptime: Option<SystemTime>,
            _flags: Option<BsdFileFlags>,
            reply: ReplyAttr,
        ) {
            let ino = ino.0;
            if ino == ROOT_INO {
                reply.attr(&ATTR_TTL, &Self::root_attr());
                return;
            }

            // Handle truncation. This is called when a file is opened with
            // O_TRUNC so that writes can start from a clean slate.
            if let Some(new_size) = size {
                match self.resize_file(ino, new_size) {
                    // Same size: the file was not touched, so release() has
                    // nothing to upload.
                    Ok(_) => {}
                    Err(e) => {
                        // The stored content could not be read, or the inode
                        // is gone / the size is past the cap. Nothing was
                        // changed — in particular the buffer was not filled
                        // with zeros, which would erase the file on release().
                        if e == ResizeError::Unavailable {
                            eprintln!("[mount] setattr size ino={ino}: content could not be loaded");
                        }
                        reply.error(e.errno());
                        return;
                    }
                }
            }

            match self.inodes.get(&ino) {
                Some(e) => reply.attr(&ATTR_TTL, &e.clone().file_attr(ino)),
                None => reply.error(Errno::ENOENT),
            }
        }

        /// Upload the write buffer when the last file descriptor is closed.
        fn release(
            &mut self,
            _req: &Request,
            ino: INodeNo,
            _fh: FileHandle,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            _flush: bool,
            reply: ReplyEmpty,
        ) {
            let ino = ino.0;
            // Take the buffer and the decision together, so the test drives the
            // same code this handler does. Absent buffer → nothing to upload.
            let (action, buf, pending) = self.take_release_upload(ino);

            match action {
                // ── Case 3: clean release (no write occurred) ─────────────────
                ReleaseUpload::Nothing => reply.ok(),

                // ── Case 1: brand-new file ────────────────────────────────────
                ReleaseUpload::NewFile => {
                    let pending = pending.expect("a pending create carries its metadata");
                    match self.commit_new_file(ino, buf, pending) {
                        Ok(()) => reply.ok(),
                        Err(errno) => reply.error(errno),
                    }
                }

                // ── Case 2: existing file re-written (e.g. O_TRUNC overwrite) ──
                ReleaseUpload::Overwrite => match self.commit_overwrite(ino, buf) {
                    Ok(()) => reply.ok(),
                    Err(errno) => reply.error(errno),
                },
            }
        }

        /// `release()` case 2: upload `buf` as the new copy of the stored file at
        /// `ino`, then trash the old one. The old file is trashed only AFTER the
        /// new one exists under the id this client minted (L13, SF-0001): an
        /// aborted upload (a changed echo, a network error) leaves the old file
        /// untouched and the inode still on it, never the old one in the trash
        /// with nothing in its place.
        ///
        /// SF-0002 r3 (K3): the old row is asked the write check first. A
        /// read-only row is never replaced (EPERM) and one the check could not
        /// read is left alone (EAGAIN): nothing is uploaded or trashed.
        fn commit_overwrite(&mut self, ino: u64, buf: Vec<u8>) -> Result<(), Errno> {
            let (name, parent_ino, old_file_id) = match self.inodes.get(&ino) {
                Some(e) => (e.name.clone(), e.parent_ino, e.file_id.clone()),
                None => return Err(Errno::ENOENT),
            };
            self.check_stored_row(ino, "overwrite")?;
            if let Err(e) = self.upload_file(ino, &name, buf, parent_ino) {
                eprintln!("[mount] release upload (overwrite): {e}");
                return Err(Errno::EIO);
            }
            if let Some(old_id) = old_file_id {
                if let Err(e) = self.rt.block_on(self.api.trash_file(&old_id)) {
                    // Non-fatal: the new copy exists; the old one stays listed.
                    eprintln!("[mount] release: trash old file {old_id}: {e}");
                }
            }
            Ok(())
        }

        /// `release()` case 1: upload a pending create, then trash the row it
        /// replaces (if any), only once the new file exists under the id this
        /// client minted (SF-0002 r3 K3, the L5 order).
        fn commit_new_file(&mut self, ino: u64, buf: Vec<u8>, pending: PendingCreate) -> Result<(), Errno> {
            if let Err(e) = self.upload_file(ino, &pending.name, buf, pending.parent_ino) {
                eprintln!("[mount] release upload (new file): {e}");
                return Err(Errno::EIO);
            }
            if let Some(old_id) = pending.replaces {
                if let Err(e) = self.rt.block_on(self.api.trash_file(&old_id)) {
                    // Non-fatal: the new copy exists; the old one stays listed.
                    eprintln!("[mount] release: trash replaced file {old_id}: {e}");
                }
            }
            Ok(())
        }

        /// `create()` over a name that already exists in `parent`: the stored
        /// row there is about to be replaced. Ask the write check (EPERM for a
        /// read-only row, EAGAIN when it cannot be read; the row and its inode
        /// stay as they are), then drop its inode and return its id, for
        /// `release()` to trash after the new upload completed (SF-0002 r3 K3).
        fn take_replaced_child(&mut self, parent: u64, name: &str) -> Result<Option<String>, Errno> {
            let Some(old_ino) = self.find_child(parent, name) else {
                return Ok(None);
            };
            self.check_stored_row(old_ino, "replace")?;
            let replaced = self.inodes.get(&old_ino).and_then(|e| e.file_id.clone());
            if let Some(id) = &replaced {
                self.id_to_ino.remove(id);
            }
            self.inodes.remove(&old_ino);
            self.write_buffers.remove(&old_ino);
            self.pending_creates.remove(&old_ino);
            self.dirty_inodes.remove(&old_ino);
            self.file_cache.remove(&old_ino);
            self.dir_cache.remove(&parent);
            Ok(replaced)
        }

        /// Take the buffer off `ino` and decide what `release()` does with it.
        /// The buffer, the pending create and the dirty flag are consumed here
        /// exactly as the handler consumes them.
        fn take_release_upload(&mut self, ino: u64) -> (ReleaseUpload, Vec<u8>, Option<PendingCreate>) {
            let Some(buf) = self.write_buffers.remove(&ino) else {
                return (ReleaseUpload::Nothing, Vec::new(), None);
            };
            let pending = self.pending_creates.remove(&ino);
            let dirty = self.dirty_inodes.remove(&ino);
            let action = release_upload(true, pending.is_some(), dirty);
            (action, buf, pending)
        }

        // ─── Directory write operations ───────────────────────────────────────

        /// Create an encrypted folder.
        fn mkdir(&mut self, _req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) {
            let parent = parent.0;
            let name_str = match name.to_str() {
                Some(n) => n.to_string(),
                None => {
                    reply.error(Errno::EINVAL);
                    return;
                }
            };

            // Generate a UUID for key derivation and folder identity.
            let folder_uuid = uuid::Uuid::new_v4();
            let folder_key = beebeeb_core::kdf::derive_file_key(&self.master_key, folder_uuid.to_string().as_bytes());

            let name_blob = match beebeeb_core::encrypt::encrypt_metadata(&folder_key, &name_str) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[mount] mkdir encrypt: {e}");
                    reply.error(Errno::EIO);
                    return;
                }
            };
            let name_enc = match serde_json::to_string(&name_blob) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[mount] mkdir serialize: {e}");
                    reply.error(Errno::EIO);
                    return;
                }
            };

            let parent_file_id: Option<uuid::Uuid> = self
                .inodes
                .get(&parent)
                .and_then(|e| e.file_id.as_deref())
                .and_then(|s| s.parse().ok());

            let resp = match self
                .rt
                .block_on(self.api.create_folder(&name_enc, parent_file_id, Some(folder_uuid)))
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[mount] mkdir API: {e}");
                    reply.error(Errno::EIO);
                    return;
                }
            };

            // L13 (SF-0001): only the id this client minted; a different echo
            // aborts before the folder is listed or used as a parent.
            let server_id = match super::folder_create_reply(&resp, folder_uuid) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("[mount] mkdir: {e}");
                    reply.error(Errno::EIO);
                    return;
                }
            };

            let ino = self.alloc_ino();
            let now = SystemTime::now();
            self.id_to_ino.insert(server_id.clone(), ino);
            self.inodes.insert(
                ino,
                InodeEntry {
                    file_id: Some(server_id),
                    key_uuid: Some(folder_uuid),
                    name: name_str,
                    is_dir: true,
                    size: 0,
                    chunk_count: 0,
                    modified: now,
                    parent_ino: parent,
                    pin: no_content(),
                    read_only: false,
                },
            );
            self.dir_cache.remove(&parent);

            let attr = self.inodes[&ino].file_attr(ino);
            reply.entry(&ATTR_TTL, &attr, Generation(0));
        }

        /// Soft-delete (trash) a file.
        fn unlink(&mut self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
            let parent = parent.0;
            let name_str = match name.to_str() {
                Some(n) => n,
                None => {
                    reply.error(Errno::EINVAL);
                    return;
                }
            };

            let ino = match self.find_child(parent, name_str) {
                Some(i) => i,
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };

            let file_id = match self.inodes.get(&ino).and_then(|e| e.file_id.clone()) {
                Some(id) => id,
                None => {
                    // Pending (not yet uploaded) — just remove locally.
                    self.inodes.remove(&ino);
                    self.pending_creates.remove(&ino);
                    self.write_buffers.remove(&ino);
                    self.dir_cache.remove(&parent);
                    reply.ok();
                    return;
                }
            };

            if let Err(e) = self.rt.block_on(self.api.trash_file(&file_id)) {
                eprintln!("[mount] unlink {file_id}: {e}");
                reply.error(Errno::EIO);
                return;
            }

            self.id_to_ino.remove(&file_id);
            self.inodes.remove(&ino);
            self.file_cache.remove(&ino);
            self.write_buffers.remove(&ino);
            self.dir_cache.remove(&parent);
            reply.ok();
        }

        /// Soft-delete (trash) an empty folder.
        fn rmdir(&mut self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
            let parent = parent.0;
            let name_str = match name.to_str() {
                Some(n) => n,
                None => {
                    reply.error(Errno::EINVAL);
                    return;
                }
            };

            let ino = match self.find_child(parent, name_str) {
                Some(i) => i,
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };

            // Must be empty: list children (bypasses cache to be sure).
            let children = self.populate_dir(ino);
            if !children.is_empty() {
                reply.error(Errno::ENOTEMPTY);
                return;
            }

            let file_id = match self.inodes.get(&ino).and_then(|e| e.file_id.clone()) {
                Some(id) => id,
                None => {
                    self.inodes.remove(&ino);
                    self.dir_cache.remove(&parent);
                    reply.ok();
                    return;
                }
            };

            if let Err(e) = self.rt.block_on(self.api.trash_file(&file_id)) {
                eprintln!("[mount] rmdir {file_id}: {e}");
                reply.error(Errno::EIO);
                return;
            }

            self.id_to_ino.remove(&file_id);
            self.inodes.remove(&ino);
            self.dir_cache.remove(&parent);
            self.dir_cache.remove(&ino);
            reply.ok();
        }

        /// SF-0002: may the stored row behind `ino` be renamed, overwritten or
        /// replaced (`what`, for the log)? The one write check: `EPERM` for a
        /// read-only row, `EAGAIN` when the check could not decide (retryable;
        /// the row is not marked read-only).
        fn check_stored_row(&self, ino: u64, what: &str) -> Result<(), Errno> {
            let Some(entry) = self.inodes.get(&ino) else {
                return Err(Errno::ENOENT);
            };
            let Some(file_id) = entry.file_id.as_deref() else {
                return Ok(()); // not uploaded yet: the change is local
            };
            // Round 3 (K5): a file this mount uploaded under a key it minted is
            // writable by construction; no probe.
            if self.minted.contains(file_id) {
                return Ok(());
            }
            let target = crate::owned_keys::WriteTarget {
                file_id,
                is_folder: entry.is_dir,
                name_read_only: entry.read_only,
                // Rows here open under the owner's own conventions (the master key).
                row_key: None,
                size_bytes: Some(entry.size),
                chunk_count: entry.chunk_count,
            };
            match self
                .rt
                .block_on(crate::owned_keys::write_check(&self.api, &self.master_key, &target))
            {
                crate::owned_keys::WriteCheck::Writable => Ok(()),
                crate::owned_keys::WriteCheck::UpperRepair => {
                    eprintln!(
                        "[mount] {what} refused for {file_id}: {}",
                        crate::crypto::OWNER_REPAIR_READ_ONLY
                    );
                    Err(Errno::EPERM)
                }
                crate::owned_keys::WriteCheck::Unknown => {
                    eprintln!(
                        "[mount] {what} refused for {file_id}: {}",
                        crate::owned_keys::WRITE_CHECK_UNKNOWN
                    );
                    Err(Errno::EAGAIN)
                }
            }
        }

        /// Move and/or rename a file or folder.
        fn rename(
            &mut self,
            _req: &Request,
            parent: INodeNo,
            name: &OsStr,
            newparent: INodeNo,
            newname: &OsStr,
            _flags: RenameFlags,
            reply: ReplyEmpty,
        ) {
            let parent = parent.0;
            let newparent = newparent.0;
            let name_str = match name.to_str() {
                Some(n) => n,
                None => {
                    reply.error(Errno::EINVAL);
                    return;
                }
            };
            let newname_str = match newname.to_str() {
                Some(n) => n.to_string(),
                None => {
                    reply.error(Errno::EINVAL);
                    return;
                }
            };

            let ino = match self.find_child(parent, name_str) {
                Some(i) => i,
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };

            let (file_id, key_uuid, old_name) = match self.inodes.get(&ino) {
                Some(e) => (e.file_id.clone(), e.key_uuid, e.name.clone()),
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };

            // Validate before local pending-file changes or conflict deletion.
            let new_parent_file_id = match rename_parent_target(
                parent,
                newparent,
                self.inodes.get(&newparent).map(|e| (e.is_dir, e.file_id.as_deref())),
            ) {
                Ok(target) => target,
                Err(e) => {
                    eprintln!("[mount] rename parent {newparent}: {e}");
                    let errno = match e {
                        crate::commands::move_target::ParentError::NotFound => Errno::ENOENT,
                        _ => Errno::EIO,
                    };
                    reply.error(errno);
                    return;
                }
            };

            let file_id = match file_id {
                Some(id) => id,
                None => {
                    // Pending (not uploaded yet) — just rename locally.
                    if let Some(e) = self.inodes.get_mut(&ino) {
                        e.name = newname_str.clone();
                        e.parent_ino = newparent;
                    }
                    if let Some(p) = self.pending_creates.get_mut(&ino) {
                        p.parent_ino = newparent;
                        p.name = newname_str;
                    }
                    self.dir_cache.remove(&parent);
                    self.dir_cache.remove(&newparent);
                    reply.ok();
                    return;
                }
            };

            match self.rename_stored(
                ino,
                &file_id,
                key_uuid,
                &old_name,
                parent,
                newparent,
                &newname_str,
                new_parent_file_id,
            ) {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(errno),
            }
        }

        /// The stored half of `rename()`: the source row is on the server.
        ///
        /// SF-0002: a rename rewrites the name envelope under the id's
        /// convention, so a read-only source (name or CURRENT version) is
        /// refused, and so is one the check could not read; reparenting stays a
        /// pure move. Round 3 (K3): a rename onto an existing name replaces the
        /// row there: it is asked the write check too, and trashed only AFTER the
        /// move succeeded, so a refused or failed move leaves it untouched.
        #[allow(clippy::too_many_arguments)]
        fn rename_stored(
            &mut self,
            ino: u64,
            file_id: &str,
            key_uuid: Option<uuid::Uuid>,
            old_name: &str,
            parent: u64,
            newparent: u64,
            newname: &str,
            new_parent_file_id: Option<Option<uuid::Uuid>>,
        ) -> Result<(), Errno> {
            let name_changed = old_name != newname;
            let parent_changed = parent != newparent;
            if name_changed {
                self.check_stored_row(ino, "rename")?;
            }

            // Re-encrypt the name if it changed, using the file's existing key.
            let new_name_enc: Option<String> = if name_changed {
                let ku = key_uuid
                    .or_else(|| file_id.parse().ok())
                    .unwrap_or_else(uuid::Uuid::new_v4);
                let fk = beebeeb_core::kdf::derive_file_key(&self.master_key, ku.to_string().as_bytes());
                let blob = beebeeb_core::encrypt::encrypt_metadata(&fk, newname).map_err(|e| {
                    eprintln!("[mount] rename encrypt: {e}");
                    Errno::EIO
                })?;
                Some(serde_json::to_string(&blob).map_err(|e| {
                    eprintln!("[mount] rename serialize: {e}");
                    Errno::EIO
                })?)
            } else {
                None
            };

            // The row a rename onto an existing name replaces.
            let replaced = match self.find_child(newparent, newname) {
                Some(conflict_ino) if conflict_ino != ino => {
                    self.check_stored_row(conflict_ino, "replace")?;
                    Some(conflict_ino)
                }
                _ => None,
            };

            if let Err(e) = self
                .rt
                .block_on(self.api.move_file(file_id, new_name_enc.as_deref(), new_parent_file_id))
            {
                eprintln!("[mount] rename move_file: {e}");
                return Err(Errno::EIO);
            }

            if let Some(conflict_ino) = replaced {
                if let Some(cid) = self.inodes.get(&conflict_ino).and_then(|e| e.file_id.clone()) {
                    if let Err(e) = self.rt.block_on(self.api.trash_file(&cid)) {
                        // Non-fatal: the move happened; the replaced row stays listed.
                        eprintln!("[mount] rename: trash replaced file {cid}: {e}");
                    }
                    self.id_to_ino.remove(&cid);
                }
                self.inodes.remove(&conflict_ino);
                self.file_cache.remove(&conflict_ino);
            }

            // Update local inode.
            if let Some(entry) = self.inodes.get_mut(&ino) {
                if name_changed {
                    entry.name = newname.to_string();
                }
                if parent_changed {
                    entry.parent_ino = newparent;
                }
            }
            self.dir_cache.remove(&parent);
            if parent_changed {
                self.dir_cache.remove(&newparent);
            }
            Ok(())
        }
    }

    // ── FUSE trait implementation (locks the state per request) ───────────────

    impl Filesystem for BeebeebFs {
        fn lookup(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
            self.lock().lookup(req, parent, name, reply);
        }

        fn getattr(&self, req: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
            self.lock().getattr(req, ino, fh, reply);
        }

        fn readdir(&self, req: &Request, ino: INodeNo, fh: FileHandle, offset: u64, reply: ReplyDirectory) {
            self.lock().readdir(req, ino, fh, offset, reply);
        }

        fn open(&self, req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
            self.lock().open(req, ino, flags, reply);
        }

        fn read(
            &self,
            req: &Request,
            ino: INodeNo,
            fh: FileHandle,
            offset: u64,
            size: u32,
            flags: OpenFlags,
            lock_owner: Option<LockOwner>,
            reply: ReplyData,
        ) {
            self.lock().read(req, ino, fh, offset, size, flags, lock_owner, reply);
        }

        fn create(
            &self,
            req: &Request,
            parent: INodeNo,
            name: &OsStr,
            mode: u32,
            umask: u32,
            flags: i32,
            reply: fuser::ReplyCreate,
        ) {
            self.lock().create(req, parent, name, mode, umask, flags, reply);
        }

        fn write(
            &self,
            req: &Request,
            ino: INodeNo,
            fh: FileHandle,
            offset: u64,
            data: &[u8],
            write_flags: WriteFlags,
            flags: OpenFlags,
            lock_owner: Option<LockOwner>,
            reply: fuser::ReplyWrite,
        ) {
            self.lock()
                .write(req, ino, fh, offset, data, write_flags, flags, lock_owner, reply);
        }

        fn setattr(
            &self,
            req: &Request,
            ino: INodeNo,
            mode: Option<u32>,
            uid: Option<u32>,
            gid: Option<u32>,
            size: Option<u64>,
            atime: Option<fuser::TimeOrNow>,
            mtime: Option<fuser::TimeOrNow>,
            ctime: Option<SystemTime>,
            fh: Option<FileHandle>,
            crtime: Option<SystemTime>,
            chgtime: Option<SystemTime>,
            bkuptime: Option<SystemTime>,
            flags: Option<BsdFileFlags>,
            reply: ReplyAttr,
        ) {
            self.lock().setattr(
                req, ino, mode, uid, gid, size, atime, mtime, ctime, fh, crtime, chgtime, bkuptime, flags, reply,
            );
        }

        fn release(
            &self,
            req: &Request,
            ino: INodeNo,
            fh: FileHandle,
            flags: OpenFlags,
            lock_owner: Option<LockOwner>,
            flush: bool,
            reply: ReplyEmpty,
        ) {
            self.lock().release(req, ino, fh, flags, lock_owner, flush, reply);
        }

        fn mkdir(&self, req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, reply: ReplyEntry) {
            self.lock().mkdir(req, parent, name, mode, umask, reply);
        }

        fn unlink(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
            self.lock().unlink(req, parent, name, reply);
        }

        fn rmdir(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
            self.lock().rmdir(req, parent, name, reply);
        }

        fn rename(
            &self,
            req: &Request,
            parent: INodeNo,
            name: &OsStr,
            newparent: INodeNo,
            newname: &OsStr,
            flags: RenameFlags,
            reply: ReplyEmpty,
        ) {
            self.lock().rename(req, parent, name, newparent, newname, flags, reply);
        }
    }

    // ── Crypto helpers ────────────────────────────────────────────────────────

    fn decrypt_name(master_key: &beebeeb_core::kdf::MasterKey, file_id: &str, name_encrypted: &str) -> String {
        crate::crypto::decrypt_name(master_key, file_id, name_encrypted)
            .unwrap_or_else(|| format!("[{}]", &file_id[..8.min(file_id.len())]))
    }

    // ── CLI entry points ──────────────────────────────────────────────────────

    /// The mount options and session ACL `bb mount` hands to fuser.
    ///
    /// Pulled out of [`run`] so a test can assert who is allowed to access the
    /// mount without mounting anything.
    fn mount_config() -> (Vec<MountOption>, SessionACL) {
        let mount_options = vec![
            MountOption::RW, // Day 2: full read-write
            MountOption::FSName("beebeeb".to_string()),
            MountOption::Subtype("beebeeb".to_string()),
            MountOption::AutoUnmount,
            MountOption::DefaultPermissions,
        ];
        // `allow_other` is a session ACL in fuser 0.17+, not a MountOption.
        //
        // `SessionACL::Owner` (fuser's default, no `allow_other`) is what we
        // want, but fuser 0.18 refuses it together with `AutoUnmount`
        // (src/session.rs:165-170):
        //
        //     if options.mount_options.contains(&MountOption::AutoUnmount)
        //         && options.acl == SessionACL::Owner
        //     {
        //         return Err(io::Error::new(
        //             io::ErrorKind::InvalidInput,
        //             format!("auto_unmount requires acl != Owner, got: {:?}", options.acl),
        //
        // because fusermount needs `allow_other` to handle auto_unmount.
        // `RootAndOwner` is the narrowest ACL that keeps `AutoUnmount`. Other
        // local accounts are refused by the kernel: `DefaultPermissions` checks
        // the owner-only modes from `node_perm` (EACCES). fuser also filters
        // most request kinds from other uids (src/request.rs:73-90), but not
        // read, write, readdir, fsync, release or forget. It still mounts with
        // the kernel's allow_other, so root-run jobs can read the mount; the
        // stricter `Owner` needs dropping `AutoUnmount` (follow-up task).
        (mount_options, SessionACL::RootAndOwner)
    }

    pub async fn run(mountpoint: PathBuf, foreground: bool, cache_ttl: u64) -> Result<(), String> {
        let config = load_config();
        if config.session_token.is_none() {
            return Err("Not logged in. Run `bb login` first.".to_string());
        }
        let mk_b64 = config.master_key.ok_or("No master key. Run `bb login`.")?;
        let mk_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(&mk_b64)
                .map_err(|e| format!("invalid master key: {e}"))?,
        );
        if mk_bytes.len() != 32 {
            return Err(format!("master key must be 32 bytes, got {}", mk_bytes.len()));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&mk_bytes);
        let master_key = beebeeb_core::kdf::MasterKey::from_bytes(arr);

        std::fs::create_dir_all(&mountpoint).map_err(|e| format!("create mountpoint: {e}"))?;

        let ttl = if cache_ttl == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs(cache_ttl)
        };
        let fs = BeebeebFs::new(ApiClient::from_config(), master_key, ttl);

        // `Config` is `non_exhaustive`: build it from its default and set fields.
        let mut options = Config::default();
        let (mount_options, acl) = mount_config();
        options.mount_options = mount_options;
        options.acl = acl;

        let mp = mountpoint.clone();
        println!();
        println!(
            "  {} {}",
            "◆".custom_color(crate::colors::AMBER_DARK),
            "Vault mounted (read-write)".custom_color(crate::colors::INK_WARM),
        );
        println!(
            "  {}  {}",
            "mountpoint".custom_color(crate::colors::INK_DIM),
            mp.display().to_string().custom_color(crate::colors::AMBER)
        );
        println!(
            "  {}  {}",
            "cache     ".custom_color(crate::colors::INK_DIM),
            if cache_ttl == 0 {
                "disabled".into()
            } else {
                format!("{cache_ttl}s TTL")
            }
            .custom_color(crate::colors::INK_DIM)
        );

        if foreground {
            println!(
                "  {}",
                "Running in foreground — Ctrl+C to unmount.".custom_color(crate::colors::INK_DIM)
            );
            println!();
            tokio::task::spawn_blocking(move || {
                fuser::mount(fs, &mountpoint, &options).map_err(|e| format!("FUSE mount failed: {e}"))
            })
            .await
            .map_err(|e| format!("task panic: {e}"))??;
        } else {
            println!(
                "  {}",
                format!("Unmount: bb unmount {}", mp.display()).custom_color(crate::colors::INK_DIM),
            );
            println!();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = fuser::mount(fs, &mountpoint, &options) {
                    eprintln!("[mount] FUSE exited: {e}");
                }
            });
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Ok(())
    }

    pub async fn unmount(mountpoint: PathBuf) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        let status = std::process::Command::new("umount").arg(&mountpoint).status();

        #[cfg(not(target_os = "macos"))]
        let status = std::process::Command::new("fusermount3")
            .args(["-u", &mountpoint.to_string_lossy()])
            .status()
            .or_else(|_| {
                std::process::Command::new("fusermount")
                    .args(["-u", &mountpoint.to_string_lossy()])
                    .status()
            });

        match status {
            Ok(s) if s.success() => {
                println!(
                    "  {} {}",
                    "◆".custom_color(crate::colors::AMBER_DARK),
                    format!("Unmounted {}", mountpoint.display()).custom_color(crate::colors::INK_WARM),
                );
                Ok(())
            }
            Ok(s) => Err(format!("umount exit status {s}")),
            Err(e) => Err(format!("umount failed: {e}")),
        }
    }

    // ── Owner-only tests ──────────────────────────────────────────────────────
    //
    // The mount is the decrypted vault, so no other local account may read it:
    // the session ACL must not be `SessionACL::All` (fuser's `allow_other`) and
    // every attribute the filesystem reports must be owner-only. These tests
    // call the real builders; nothing is mounted and no API is touched.

    #[cfg(test)]
    mod owner_only_tests {
        use super::*;

        fn entry(is_dir: bool) -> InodeEntry {
            InodeEntry {
                file_id: None,
                key_uuid: None,
                name: "vault-item".to_string(),
                is_dir,
                size: if is_dir { 0 } else { 11 },
                chunk_count: if is_dir { 0 } else { 1 },
                modified: SystemTime::UNIX_EPOCH,
                parent_ino: ROOT_INO,
                pin: Err("a directory has no content".to_string()),
                read_only: false,
            }
        }

        #[test]
        fn acl_is_not_all() {
            let (options, acl) = mount_config();
            assert_ne!(acl, SessionACL::All, "the mount must not be open to every local user");
            // fuser 0.18 `Session::new` rejects `AutoUnmount` with `SessionACL::Owner`
            // (src/session.rs:165-170), so the ACL is also never `Owner` here.
            if options.contains(&MountOption::AutoUnmount) {
                assert_ne!(acl, SessionACL::Owner, "fuser 0.18: auto_unmount requires acl != Owner");
            }
        }

        #[test]
        fn acl_is_root_and_owner() {
            let (_, acl) = mount_config();
            assert_eq!(acl, SessionACL::RootAndOwner);
        }

        #[test]
        fn default_permissions_is_present() {
            let (options, _) = mount_config();
            assert!(
                options.contains(&MountOption::DefaultPermissions),
                "the kernel must enforce the reported mode bits"
            );
        }

        #[test]
        fn node_perm_is_owner_only() {
            assert_eq!(node_perm(true), 0o700, "a directory must not be listable by others");
            assert_eq!(node_perm(false), 0o600, "a file must not be readable by others");
        }

        #[test]
        fn file_attribute_is_owner_only() {
            let dir = entry(true).file_attr(2);
            assert_eq!(dir.perm, 0o700);
            assert_eq!(dir.uid, unsafe { libc::getuid() });
            assert_eq!(dir.gid, unsafe { libc::getgid() });

            let file = entry(false).file_attr(3);
            assert_eq!(file.perm, 0o600);
            assert_eq!(file.uid, unsafe { libc::getuid() });
            assert_eq!(file.gid, unsafe { libc::getgid() });
        }

        #[test]
        fn root_attribute_is_owner_only() {
            let root = BeebeebFsState::root_attr();
            assert_eq!(root.perm, 0o700);
            assert_eq!(root.uid, unsafe { libc::getuid() });
            assert_eq!(root.gid, unsafe { libc::getgid() });
        }
    }

    // ── Truncate tests ────────────────────────────────────────────────────────
    //
    // A `setattr` size request must keep the file's content: load it (from the
    // read cache or the server, the same path `read` uses), then resize it.
    // Nothing is mounted and no server is reachable from these tests — the
    // content lives in the read cache, as a previous `read` leaves it.

    #[cfg(test)]
    mod truncate_tests {
        use super::*;

        const INO: u64 = 7;
        const FILE_ID: &str = "11111111-1111-4111-8111-111111111111";

        fn master_key() -> beebeeb_core::kdf::MasterKey {
            beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32])
        }

        fn file_entry(size: u64) -> InodeEntry {
            file_entry_with_pin(size, Err("this test never reaches the server".to_string()))
        }

        fn file_entry_with_pin(
            size: u64,
            pin: Result<beebeeb_core::chunk_stream::ExpectedChunkFormat, String>,
        ) -> InodeEntry {
            InodeEntry {
                file_id: Some(FILE_ID.to_string()),
                key_uuid: None,
                name: "report.bin".to_string(),
                is_dir: false,
                size,
                chunk_count: 1,
                modified: SystemTime::UNIX_EPOCH,
                parent_ino: ROOT_INO,
                pin,
                read_only: false,
            }
        }

        /// A filesystem state with one stored file whose decrypted content is
        /// already in the read cache. `ApiClient::new_for_test` points at a
        /// closed local port: nothing here may reach a server.
        fn state_with_cached_file(content: &[u8]) -> BeebeebFsState {
            let mut fs = BeebeebFsState::new(
                ApiClient::new_for_test("http://127.0.0.1:9".to_string()),
                master_key(),
                Duration::from_secs(5),
            );
            fs.inodes.insert(INO, file_entry(content.len() as u64));
            fs.file_cache.insert(INO, content.to_vec());
            fs
        }

        /// A filesystem state with one stored file and no cache; `url` is a
        /// local mock server.
        fn state_with_stored_file(
            url: String,
            size: u64,
            pin: beebeeb_core::chunk_stream::ExpectedChunkFormat,
        ) -> BeebeebFsState {
            let mut fs = BeebeebFsState::new(ApiClient::new_for_test(url), master_key(), Duration::from_secs(5));
            fs.inodes.insert(INO, file_entry_with_pin(size, Ok(pin)));
            fs
        }

        /// What `release()` uploads for `ino`: the production decision and the
        /// buffer it consumes, not a copy of them.
        fn what_release_uploads(fs: &mut BeebeebFsState, ino: u64) -> Option<Vec<u8>> {
            let (action, buf, _pending) = fs.take_release_upload(ino);
            (action != ReleaseUpload::Nothing).then_some(buf)
        }

        /// A real v2 frame (`ChunkEncryptor` binds the file id as AAD) for a
        /// single-chunk file: what a v2 download body contains.
        fn v2_frame(plaintext: &[u8]) -> Vec<u8> {
            let mut encryptor = beebeeb_core::chunk_stream::ChunkEncryptor::for_push(
                &master_key(),
                FILE_ID,
                plaintext.len() as u64,
                beebeeb_types::ChunkProfile::Cli,
            )
            .expect("v2 encryptor")
            .with_recorded_pin(beebeeb_core::chunk_stream::ExpectedChunkFormat::V2)
            .expect("pin the v2 format");
            let chunk = encryptor.push_chunk(plaintext).expect("v2 frame");
            encryptor.finish().expect("complete v2 stream");
            chunk.data
        }

        /// The shared mock (`crate::test_server`) answering `GET
        /// /api/v1/files/FILE_ID/download` with `body`. Runs on its own thread
        /// with its own runtime: `fetch_file` drives the client runtime with
        /// `block_on`, so the server cannot share it.
        fn spawn_download_server(body: Vec<u8>) -> String {
            let mock = crate::test_server::shared(crate::test_server::Mock::default());
            mock.lock().unwrap().downloads.insert(FILE_ID.to_string(), body);
            crate::test_server::spawn_on_own_thread(mock)
        }

        /// The shared mock of the V2 upload routes, which records every chunk
        /// body (so a test can decrypt what the client actually sent) and, like
        /// an honest server, echoes the id each init was sent.
        fn spawn_upload_server() -> (String, crate::test_server::Shared) {
            spawn_upload_server_with(crate::test_server::Mock::default())
        }

        /// The stored FILE_ID's empty version (size 0, one frame, the canonical
        /// key), so its SF-0002 write check proves it writable.
        fn stored_empty_version() -> HashMap<String, Vec<u8>> {
            let key = beebeeb_core::kdf::derive_file_key(&master_key(), FILE_ID.as_bytes());
            HashMap::from([(
                FILE_ID.to_string(),
                beebeeb_core::encrypt::encrypt_chunk_raw(&key, b"").unwrap(),
            )])
        }

        fn spawn_upload_server_with(mock: crate::test_server::Mock) -> (String, crate::test_server::Shared) {
            let mock = crate::test_server::shared(mock);
            (crate::test_server::spawn_on_own_thread(mock.clone()), mock)
        }

        #[test]
        fn grow_keeps_the_prefix_and_zero_pads() {
            assert_eq!(resized(b"hello", 9), b"hello\0\0\0\0".to_vec());
        }

        #[test]
        fn shrink_keeps_the_prefix() {
            assert_eq!(resized(b"hello world", 5), b"hello".to_vec());
        }

        #[test]
        fn same_length_is_the_same_content() {
            assert_eq!(resized(b"hello", 5), b"hello".to_vec());
        }

        #[test]
        fn grow_through_setattr_keeps_the_content() {
            let mut fs = state_with_cached_file(b"hello");

            let source = fs.resize_file(INO, 9).expect("a cached file resizes");
            assert_eq!(source, TruncateSource::Cache);
            assert_eq!(fs.inodes[&INO].size, 9);
            assert!(fs.dirty_inodes.contains(&INO), "a real resize must be re-uploaded");
            assert_eq!(what_release_uploads(&mut fs, INO), Some(b"hello\0\0\0\0".to_vec()));
        }

        #[test]
        fn shrink_through_setattr_keeps_the_prefix() {
            let mut fs = state_with_cached_file(b"hello world");

            let source = fs.resize_file(INO, 5).expect("a cached file resizes");
            assert_eq!(source, TruncateSource::Cache);
            assert_eq!(fs.inodes[&INO].size, 5);
            assert_eq!(what_release_uploads(&mut fs, INO), Some(b"hello".to_vec()));
        }

        #[test]
        fn the_same_size_is_unchanged_and_uploads_nothing() {
            let mut fs = state_with_cached_file(b"hello");

            let source = fs.resize_file(INO, 5).expect("an unchanged size is not an error");
            assert_eq!(source, TruncateSource::Unchanged);
            assert!(
                !fs.dirty_inodes.contains(&INO),
                "an unchanged size must not make the file dirty"
            );
            assert!(!fs.write_buffers.contains_key(&INO), "no write buffer is started");
            assert_eq!(
                what_release_uploads(&mut fs, INO),
                None,
                "release() must upload nothing"
            );
            assert_eq!(fs.inodes[&INO].size, 5);
        }

        #[test]
        fn truncating_an_empty_file_keeps_the_following_write() {
            // `echo hi > empty.txt` opens an existing empty file with O_TRUNC:
            // the size request is 0 == 0, so nothing is resized. The write that
            // follows must still be uploaded by release().
            let mut fs = state_with_cached_file(b"");

            let source = fs.resize_file(INO, 0).expect("zero on zero");
            assert_eq!(source, TruncateSource::Unchanged);
            assert!(!fs.dirty_inodes.contains(&INO), "nothing changed, so nothing is dirty");
            assert!(
                !fs.write_buffers.contains_key(&INO),
                "an unchanged size starts no buffer"
            );

            fs.apply_write(INO, 0, b"hi").expect("the write gets a buffer");
            assert!(fs.dirty_inodes.contains(&INO), "a write into a stored file is dirty");
            assert_eq!(
                what_release_uploads(&mut fs, INO),
                Some(b"hi".to_vec()),
                "release() must upload the written bytes"
            );
        }

        #[test]
        fn an_empty_file_overwritten_with_echo_uploads_the_written_bytes() {
            // The same sequence through the real upload path, against a mock
            // server: the bytes the server receives decrypt to "hi".
            let (url, chunks) = spawn_upload_server();
            let mut fs = state_with_stored_file(url, 0, beebeeb_core::chunk_stream::ExpectedChunkFormat::V2);

            assert_eq!(
                fs.resize_file(INO, 0).expect("O_TRUNC on an empty file"),
                TruncateSource::Unchanged
            );
            fs.apply_write(INO, 0, b"hi").expect("write");
            let (action, buf, _pending) = fs.take_release_upload(INO);
            assert_eq!(action, ReleaseUpload::Overwrite);
            fs.upload_file(INO, "empty.txt", buf, ROOT_INO).expect("upload");

            let key_uuid = fs.inodes[&INO].key_uuid.expect("the upload assigns a key UUID");
            let m = chunks.lock().expect("mock state");
            assert_eq!(m.chunk_bodies.len(), 1);
            let stored = m.chunk_bodies.values().next().expect("one chunk");
            let plain = crate::crypto::decrypt_file_chunks(
                &master_key(),
                &key_uuid.to_string(),
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                stored,
                1,
            )
            .expect("the server's chunk decrypts");
            assert_eq!(plain, b"hi", "the server received the written bytes");
        }

        #[test]
        fn stored_content_is_loaded_before_it_is_resized() {
            // Neither a write buffer nor a cached copy: the stored content must
            // be read first, never replaced by an empty buffer.
            assert_eq!(truncate_source(false, true, false, 5, 9), TruncateSource::Download);
            assert_eq!(truncate_source(false, true, false, 11, 5), TruncateSource::Download);
        }

        #[test]
        fn a_resident_buffer_is_resized_in_place() {
            assert_eq!(truncate_source(true, true, true, 5, 2), TruncateSource::Buffer);
            assert_eq!(truncate_source(true, true, false, 5, 9), TruncateSource::Buffer);
        }

        #[test]
        fn truncating_to_zero_keeps_nothing_so_it_downloads_nothing() {
            assert_eq!(truncate_source(false, true, false, 1000, 0), TruncateSource::Fresh);

            let mut fs = state_with_cached_file(b"hello");
            assert_eq!(fs.resize_file(INO, 0).expect("truncate to zero"), TruncateSource::Fresh);
            assert_eq!(what_release_uploads(&mut fs, INO), Some(Vec::new()));
        }

        #[test]
        fn a_pending_create_starts_from_an_empty_buffer() {
            // The pure decision sees no stored content, so it refuses; the
            // caller knows a pending create may start from nothing.
            assert_eq!(truncate_source(false, false, false, 0, 5), TruncateSource::Unavailable);

            let mut fs = state_with_cached_file(b"");
            fs.pending_creates.remove(&INO);
            fs.pending_creates.insert(
                INO,
                PendingCreate {
                    parent_ino: ROOT_INO,
                    name: "new.bin".to_string(),
                    replaces: None,
                },
            );
            fs.inodes.get_mut(&INO).expect("inode").file_id = None;
            assert_eq!(fs.resize_file(INO, 3).expect("pad a new file"), TruncateSource::Fresh);
            assert_eq!(what_release_uploads(&mut fs, INO), Some(vec![0u8; 3]));
        }

        #[test]
        fn a_write_to_a_pending_create_is_not_marked_dirty() {
            let mut fs = state_with_cached_file(b"");
            fs.inodes.get_mut(&INO).expect("inode").file_id = None;
            fs.write_buffers.insert(INO, Vec::new());
            fs.pending_creates.insert(
                INO,
                PendingCreate {
                    parent_ino: ROOT_INO,
                    name: "new.bin".to_string(),
                    replaces: None,
                },
            );

            fs.apply_write(INO, 0, b"hi").expect("write");
            assert!(
                !fs.dirty_inodes.contains(&INO),
                "release() case 1 already uploads a pending create"
            );
            let (action, buf, _pending) = fs.take_release_upload(INO);
            assert_eq!(action, ReleaseUpload::NewFile);
            assert_eq!(buf, b"hi");
        }

        #[test]
        fn a_directory_is_resized_as_an_empty_buffer_and_downloads_nothing() {
            let mut fs = state_with_cached_file(b"");
            fs.file_cache.remove(&INO);
            let entry = fs.inodes.get_mut(&INO).expect("inode");
            entry.is_dir = true;
            entry.file_id = Some(FILE_ID.to_string());

            assert_eq!(
                fs.resize_file(INO, 5).expect("a directory resize"),
                TruncateSource::Fresh
            );
            assert_eq!(fs.inodes[&INO].size, 5);
            assert_eq!(what_release_uploads(&mut fs, INO), Some(vec![0u8; 5]));
        }

        #[test]
        fn an_unreadable_stored_content_changes_nothing() {
            // No cached copy and no buffer: the resize needs the server, which
            // this state cannot reach. It must report the error and leave the
            // file — and the dirty set — exactly as it was, so nothing is
            // uploaded with zeros.
            let mut fs = state_with_cached_file(b"");
            fs.file_cache.remove(&INO);
            fs.inodes.get_mut(&INO).expect("inode").size = 5;

            assert_eq!(
                fs.resize_file(INO, 9).expect_err("content cannot be loaded"),
                ResizeError::Unavailable
            );
            assert!(!fs.dirty_inodes.contains(&INO));
            assert!(!fs.write_buffers.contains_key(&INO));
            assert_eq!(fs.inodes[&INO].size, 5);
        }

        #[test]
        fn a_file_without_stored_content_refuses_to_grow() {
            // The state after a failed release upload: no file_id, no cache,
            // no buffer. Growing it would have to invent zero bytes.
            let mut fs = state_with_cached_file(b"");
            fs.file_cache.remove(&INO);
            fs.inodes.get_mut(&INO).expect("inode").file_id = None;

            let err = fs.resize_file(INO, 5).expect_err("nothing to grow from");
            assert_eq!(err, ResizeError::Unavailable);
            assert_eq!(err.errno(), Errno::EIO);
            assert!(!fs.dirty_inodes.contains(&INO));
            assert!(!fs.write_buffers.contains_key(&INO));
            assert_eq!(fs.inodes[&INO].size, 0);
        }

        #[test]
        fn an_unknown_inode_is_not_found() {
            let mut fs = state_with_cached_file(b"hello");

            let err = fs.resize_file(99, 3).expect_err("unknown inode");
            assert_eq!(err, ResizeError::NotFound);
            assert_eq!(err.errno(), Errno::ENOENT, "an unknown inode is not an I/O error");
        }

        #[test]
        fn a_resize_above_the_cap_is_refused_and_changes_nothing() {
            let mut fs = state_with_cached_file(b"hello");

            let err = fs.resize_file(INO, MAX_FILE_SIZE + 1).expect_err("above the cap");
            assert_eq!(err, ResizeError::TooLarge);
            assert_eq!(err.errno(), Errno::EFBIG);
            assert!(!fs.dirty_inodes.contains(&INO));
            assert!(!fs.write_buffers.contains_key(&INO));
            assert_eq!(fs.inodes[&INO].size, 5);
            assert_eq!(fs.file_cache[&INO], b"hello");
        }

        #[test]
        fn a_successful_download_is_resized_instead_of_replaced_with_zeros() {
            // The Download path with a server that actually answers: the cache
            // is empty, so `fetch_file` must decrypt the stored frame and that
            // content must be what is resized.
            let plaintext = b"hello";
            let mut fs = state_with_stored_file(
                spawn_download_server(v2_frame(plaintext)),
                plaintext.len() as u64,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::V2,
            );
            assert!(!fs.file_cache.contains_key(&INO));

            assert_eq!(
                fs.resize_file(INO, 8).expect("grow from a successful download"),
                TruncateSource::Download
            );
            assert_eq!(
                what_release_uploads(&mut fs, INO),
                Some(b"hello\0\0\0".to_vec()),
                "the downloaded content must be resized, not replaced by zeros"
            );
        }

        #[test]
        fn a_cache_that_does_not_match_the_size_is_not_used_as_content() {
            // A read that raced a truncate leaves the old, longer copy in the
            // cache while the inode says 2 bytes. Growing to 4 must download
            // the real 2 bytes, never resurrect the removed tail.
            let plaintext = b"ab";
            let mut fs = state_with_stored_file(
                spawn_download_server(v2_frame(plaintext)),
                2,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::V2,
            );
            fs.file_cache.insert(INO, b"abcdef".to_vec());

            assert_eq!(
                truncate_source(false, true, false, 2, 4),
                TruncateSource::Download,
                "a cache of the wrong length is not this file's content"
            );
            assert_eq!(fs.resize_file(INO, 4).expect("grow"), TruncateSource::Download);
            assert_eq!(
                what_release_uploads(&mut fs, INO),
                Some(b"ab\0\0".to_vec()),
                "the removed tail must not come back"
            );
        }

        #[test]
        fn a_successful_upload_clears_the_read_cache() {
            let (url, _chunks) = spawn_upload_server();
            let mut fs = state_with_stored_file(url, 0, beebeeb_core::chunk_stream::ExpectedChunkFormat::V2);
            fs.file_cache.insert(INO, b"stale".to_vec());
            fs.apply_write(INO, 0, b"new").expect("write");
            let (action, buf, _pending) = fs.take_release_upload(INO);
            assert_eq!(action, ReleaseUpload::Overwrite);

            fs.upload_file(INO, "x.txt", buf, ROOT_INO).expect("upload");
            assert!(
                !fs.file_cache.contains_key(&INO),
                "a cache from before the write must not survive the upload"
            );
        }

        /// L1 (SF-0001 L13, mount new file): the init echoes another id. The
        /// release fails (EIO), no chunk is PUT, nothing completes, the session
        /// is ended once by the minted id, and the inode records no server id —
        /// neither the echoed one nor any other.
        #[test]
        fn a_new_file_whose_init_echoes_another_id_records_no_server_id() {
            const NEW: u64 = 8;
            let echoed = uuid::Uuid::new_v4().to_string();
            let (url, mock) = spawn_upload_server_with(crate::test_server::Mock {
                init_echo: Some(echoed.clone()),
                ..Default::default()
            });
            let mut fs = BeebeebFsState::new(ApiClient::new_for_test(url), master_key(), Duration::from_secs(5));
            fs.inodes.insert(
                NEW,
                InodeEntry {
                    file_id: None,
                    key_uuid: None,
                    name: "new.txt".to_string(),
                    is_dir: false,
                    size: 0,
                    chunk_count: 0,
                    modified: SystemTime::UNIX_EPOCH,
                    parent_ino: ROOT_INO,
                    pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
                    read_only: false,
                },
            );

            let err = fs
                .upload_file(NEW, "new.txt", b"hello".to_vec(), ROOT_INO)
                .expect_err("a changed echo aborts the upload");
            assert_eq!(err, crate::item_id::CHANGED_ID_MESSAGE);
            assert_eq!(fs.inodes[&NEW].file_id, None, "the inode keeps no server id");
            assert!(!fs.id_to_ino.contains_key(&echoed), "the echoed id is never recorded");
            let m = mock.lock().unwrap();
            assert!(m.chunk_puts.is_empty(), "zero chunk PUTs: {:?}", m.chunk_puts);
            assert!(m.completed.is_empty(), "no completion");
            assert_eq!(m.abandons, m.inits, "ended once, by the minted id");
        }

        /// L1 (mount overwrite): the new copy's upload is refused (a changed
        /// echo). The old file is NOT trashed and the inode stays on it: the
        /// trash runs only after the new copy exists.
        #[test]
        fn an_overwrite_whose_upload_aborts_keeps_the_old_file() {
            let (url, mock) = spawn_upload_server_with(crate::test_server::Mock {
                init_echo: Some(uuid::Uuid::new_v4().to_string()),
                // SF-0002 r3: the old row's write check reads its version.
                downloads: stored_empty_version(),
                ..Default::default()
            });
            let mut fs = state_with_stored_file(url, 0, beebeeb_core::chunk_stream::ExpectedChunkFormat::V2);
            fs.id_to_ino.insert(FILE_ID.to_string(), INO);

            assert_eq!(fs.commit_overwrite(INO, b"new".to_vec()), Err(Errno::EIO));
            assert_eq!(
                fs.inodes[&INO].file_id.as_deref(),
                Some(FILE_ID),
                "still on the old file"
            );
            assert_eq!(fs.id_to_ino.get(FILE_ID), Some(&INO));
            let m = mock.lock().unwrap();
            assert!(m.trashed.is_empty(), "the old file is not trashed: {:?}", m.trashed);
            assert!(m.chunk_puts.is_empty());
        }

        /// The overwrite that succeeds trashes the old file, after the new
        /// copy completed under the minted id.
        #[test]
        fn an_overwrite_trashes_the_old_file_after_the_new_one_completes() {
            // SF-0002 r3: the old row's write check reads its version.
            let (url, mock) = spawn_upload_server_with(crate::test_server::Mock {
                downloads: stored_empty_version(),
                ..Default::default()
            });
            let mut fs = state_with_stored_file(url, 0, beebeeb_core::chunk_stream::ExpectedChunkFormat::V2);
            fs.id_to_ino.insert(FILE_ID.to_string(), INO);

            fs.commit_overwrite(INO, b"new".to_vec()).expect("overwrite");
            let new_id = fs.inodes[&INO].file_id.clone().expect("the new copy's id");
            assert_ne!(new_id, FILE_ID);
            assert_eq!(fs.inodes[&INO].key_uuid.map(|u| u.to_string()), Some(new_id.clone()));
            assert!(!fs.id_to_ino.contains_key(FILE_ID));
            let m = mock.lock().unwrap();
            let complete = m
                .events
                .iter()
                .position(|e| e.starts_with("complete:"))
                .expect("completed");
            let trash = m
                .events
                .iter()
                .position(|e| *e == format!("trash:{FILE_ID}"))
                .expect("the old file is trashed");
            assert!(
                complete < trash,
                "trashed only after the new copy completed: {:?}",
                m.events
            );
        }
    }

    // ── SF-0002 rename check ──────────────────────────────────────────────────
    //
    // `rename()` asks `check_stored_row` before it re-encrypts a name. The API here
    // is a closed local port: any probe fails, so an answer that needed one is
    // EAGAIN (Unknown), and an `Ok` proves nothing was downloaded.

    #[cfg(test)]
    mod rename_check_tests {
        use super::*;

        const DIR_INO: u64 = 5;
        const FILE_INO: u64 = 6;
        const RO_INO: u64 = 7;

        fn inode(file_id: &str, is_dir: bool, read_only: bool) -> InodeEntry {
            InodeEntry {
                file_id: Some(file_id.to_string()),
                key_uuid: file_id.parse().ok(),
                name: "item".to_string(),
                is_dir,
                size: if is_dir { 0 } else { 11 },
                chunk_count: if is_dir { 0 } else { 1 },
                modified: SystemTime::UNIX_EPOCH,
                parent_ino: ROOT_INO,
                pin: no_content(),
                read_only,
            }
        }

        fn state() -> BeebeebFsState {
            let mut fs = BeebeebFsState::new(
                ApiClient::new_for_test("http://127.0.0.1:9".to_string()),
                beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32]),
                Duration::from_secs(5),
            );
            // A folder whose listing even claims a chunk: it must still not be probed.
            let mut dir = inode("22222222-2222-4222-8222-222222222222", true, false);
            dir.chunk_count = 1;
            fs.inodes.insert(DIR_INO, dir);
            fs.inodes
                .insert(FILE_INO, inode("11111111-1111-4111-8111-111111111111", false, false));
            fs.inodes
                .insert(RO_INO, inode("5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30", false, true));
            fs
        }

        /// C4: renaming a folder downloads nothing and is allowed.
        #[test]
        fn a_folder_rename_is_never_probed() {
            assert_eq!(state().check_stored_row(DIR_INO, "rename"), Ok(()));
        }

        /// C1 on the mount: a file whose version the check cannot read is
        /// refused with EAGAIN (retryable), not allowed and not EPERM.
        #[test]
        fn an_unreadable_check_refuses_with_eagain() {
            assert_eq!(state().check_stored_row(FILE_INO, "rename"), Err(Errno::EAGAIN));
        }

        /// A name sealed read-only is EPERM without asking the server.
        #[test]
        fn a_read_only_name_is_eperm() {
            assert_eq!(state().check_stored_row(RO_INO, "rename"), Err(Errno::EPERM));
        }
    }

    // ── SF-0002 round 3: replacing a stored row (K3, K4, K5) ──────────────────
    //
    // An overwrite on release, a create over an existing name and a rename onto
    // one each replace a stored row. These drive the state methods the FUSE
    // handlers call against the shared mock (its own thread: the state drives
    // its runtime with `block_on`). Rows: OK (version served, writable), RO
    // (name read-only), UNK (version not served: the check is Unknown), SRC (a
    // rename's writable source).

    #[cfg(test)]
    mod replace_tests {
        use super::*;
        use crate::test_server::{Mock, Shared, shared, spawn_on_own_thread};
        use beebeeb_core::chunk_stream::ExpectedChunkFormat;

        const OK_ID: &str = "11111111-1111-4111-8111-111111111111";
        const RO_ID: &str = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";
        const UNK_ID: &str = "33333333-3333-4333-8333-333333333333";
        const SRC_ID: &str = "44444444-4444-4444-8444-444444444444";
        const OK_INO: u64 = 10;
        const RO_INO: u64 = 11;
        const UNK_INO: u64 = 12;
        const SRC_INO: u64 = 13;
        const NEW_INO: u64 = 20;

        fn master() -> beebeeb_core::kdf::MasterKey {
            beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32])
        }

        fn entry(file_id: Option<&str>, name: &str, read_only: bool) -> InodeEntry {
            InodeEntry {
                file_id: file_id.map(str::to_string),
                key_uuid: file_id.and_then(|id| id.parse().ok()),
                name: name.to_string(),
                is_dir: false,
                size: 0,
                chunk_count: if file_id.is_some() { 1 } else { 0 },
                modified: SystemTime::UNIX_EPOCH,
                parent_ino: ROOT_INO,
                pin: Ok(ExpectedChunkFormat::Unrecorded),
                read_only,
            }
        }

        fn setup(mut mock: Mock) -> (BeebeebFsState, Shared) {
            for id in [OK_ID, SRC_ID] {
                let key = beebeeb_core::kdf::derive_file_key(&master(), id.as_bytes());
                mock.downloads.insert(
                    id.to_string(),
                    beebeeb_core::encrypt::encrypt_chunk_raw(&key, b"").unwrap(),
                );
            }
            let mock = shared(mock);
            let url = spawn_on_own_thread(mock.clone());
            let mut fs = BeebeebFsState::new(ApiClient::new_for_test(url), master(), Duration::from_secs(60));
            for (ino, id, name, read_only) in [
                (OK_INO, OK_ID, "ok.txt", false),
                (RO_INO, RO_ID, "ro.pdf", true),
                (UNK_INO, UNK_ID, "unk.txt", false),
                (SRC_INO, SRC_ID, "src.txt", false),
            ] {
                fs.inodes.insert(ino, entry(Some(id), name, read_only));
                fs.id_to_ino.insert(id.to_string(), ino);
            }
            // The root listing as the mount last read it (no listing route here).
            fs.dir_cache.insert(
                ROOT_INO,
                CachedDir {
                    children: vec![OK_INO, RO_INO, UNK_INO, SRC_INO],
                    expires_at: std::time::Instant::now() + Duration::from_secs(60),
                },
            );
            (fs, mock)
        }

        fn position(events: &[String], event: &str) -> usize {
            events
                .iter()
                .position(|e| e.starts_with(event))
                .unwrap_or_else(|| panic!("no {event} in {events:?}"))
        }

        /// K3 (overwrite on release): a read-only row is never replaced (EPERM)
        /// and one the check cannot read is left alone (EAGAIN). Nothing is
        /// uploaded, nothing is trashed, and the inodes stay on their rows.
        #[test]
        fn an_overwrite_of_a_read_only_or_unchecked_row_changes_nothing() {
            let (mut fs, mock) = setup(Mock::default());
            assert_eq!(fs.commit_overwrite(RO_INO, b"new".to_vec()), Err(Errno::EPERM));
            assert_eq!(fs.commit_overwrite(UNK_INO, b"new".to_vec()), Err(Errno::EAGAIN));
            assert_eq!(fs.inodes[&RO_INO].file_id.as_deref(), Some(RO_ID));
            assert_eq!(fs.inodes[&UNK_INO].file_id.as_deref(), Some(UNK_ID));
            let m = mock.lock().unwrap();
            assert!(m.inits.is_empty(), "nothing uploaded: {:?}", m.events);
            assert!(m.trashed.is_empty(), "nothing trashed: {:?}", m.events);
        }

        /// K4: after an overwrite the inode describes the NEW file: the
        /// new-file writer's pin (Unrecorded), not the old row's V2, and not
        /// read-only (upload_file clears a flag left on the inode).
        #[test]
        fn an_overwrite_resets_the_pin_and_the_read_only_flag() {
            let (mut fs, _mock) = setup(Mock::default());
            fs.inodes.get_mut(&OK_INO).unwrap().pin = Ok(ExpectedChunkFormat::V2);
            fs.commit_overwrite(OK_INO, b"new".to_vec()).expect("overwrite");
            assert_eq!(fs.inodes[&OK_INO].pin, Ok(ExpectedChunkFormat::Unrecorded));

            fs.inodes.get_mut(&OK_INO).unwrap().read_only = true;
            fs.upload_file(OK_INO, "ok.txt", b"again".to_vec(), ROOT_INO)
                .expect("upload");
            assert!(!fs.inodes[&OK_INO].read_only);
        }

        /// K3 (create over an existing name): the row there is asked the write
        /// check (EPERM / EAGAIN, and it stays), and a writable one is trashed
        /// only after the new file's upload completed.
        #[test]
        fn a_create_over_an_existing_name_trashes_only_after_the_upload() {
            let (mut fs, mock) = setup(Mock::default());
            assert_eq!(fs.take_replaced_child(ROOT_INO, "ro.pdf"), Err(Errno::EPERM));
            assert_eq!(fs.take_replaced_child(ROOT_INO, "unk.txt"), Err(Errno::EAGAIN));
            assert!(fs.inodes.contains_key(&RO_INO) && fs.inodes.contains_key(&UNK_INO));

            let replaces = fs.take_replaced_child(ROOT_INO, "ok.txt").expect("a writable row");
            assert_eq!(replaces.as_deref(), Some(OK_ID));
            assert!(mock.lock().unwrap().trashed.is_empty(), "nothing trashed at create");
            fs.inodes.insert(NEW_INO, entry(None, "ok.txt", false));
            let pending = PendingCreate {
                parent_ino: ROOT_INO,
                name: "ok.txt".to_string(),
                replaces,
            };
            fs.commit_new_file(NEW_INO, b"new".to_vec(), pending).expect("upload");
            let m = mock.lock().unwrap();
            assert!(
                position(&m.events, "complete:") < position(&m.events, &format!("trash:{OK_ID}")),
                "trashed only after the new file completed: {:?}",
                m.events
            );
        }

        /// K3: a create whose upload is refused (a changed echo) leaves the row
        /// it would have replaced untouched.
        #[test]
        fn a_create_whose_upload_aborts_keeps_the_replaced_row() {
            let (mut fs, mock) = setup(Mock {
                init_echo: Some(uuid::Uuid::new_v4().to_string()),
                ..Default::default()
            });
            let replaces = fs.take_replaced_child(ROOT_INO, "ok.txt").expect("a writable row");
            fs.inodes.insert(NEW_INO, entry(None, "ok.txt", false));
            let pending = PendingCreate {
                parent_ino: ROOT_INO,
                name: "ok.txt".to_string(),
                replaces,
            };
            assert_eq!(fs.commit_new_file(NEW_INO, b"new".to_vec(), pending), Err(Errno::EIO));
            assert!(mock.lock().unwrap().trashed.is_empty(), "the replaced row was trashed");
        }

        /// K3 (rename onto an existing name): the row there is asked the write
        /// check (EPERM / EAGAIN: no move, nothing trashed); a writable one is
        /// trashed only after the move succeeded, and a failed move trashes
        /// nothing.
        #[test]
        fn a_rename_onto_an_existing_name_trashes_only_after_the_move() {
            let (mut fs, mock) = setup(Mock::default());
            let rename = |fs: &mut BeebeebFsState, onto: &str| {
                fs.rename_stored(
                    SRC_INO,
                    SRC_ID,
                    SRC_ID.parse().ok(),
                    "src.txt",
                    ROOT_INO,
                    ROOT_INO,
                    onto,
                    None,
                )
            };
            assert_eq!(rename(&mut fs, "ro.pdf"), Err(Errno::EPERM));
            assert_eq!(rename(&mut fs, "unk.txt"), Err(Errno::EAGAIN));
            assert!(mock.lock().unwrap().events.is_empty(), "a refused rename sent nothing");

            mock.lock().unwrap().move_status = Some(500);
            assert_eq!(rename(&mut fs, "ok.txt"), Err(Errno::EIO));
            assert!(
                mock.lock().unwrap().trashed.is_empty(),
                "a failed move trashed the target"
            );

            mock.lock().unwrap().move_status = None;
            rename(&mut fs, "ok.txt").expect("a rename onto a writable row");
            assert_eq!(
                mock.lock().unwrap().events,
                vec![format!("move:{SRC_ID}"), format!("trash:{OK_ID}")]
            );
        }

        /// K5: a file this mount uploaded (under a key it minted) is decided
        /// without a probe. Its version is not served here, so a probe would
        /// answer EAGAIN.
        #[test]
        fn a_file_this_mount_uploaded_is_renamed_without_a_probe() {
            let (mut fs, _mock) = setup(Mock::default());
            fs.inodes.insert(NEW_INO, entry(None, "draft.txt", false));
            fs.upload_file(NEW_INO, "draft.txt", b"draft".to_vec(), ROOT_INO)
                .expect("upload");
            assert_eq!(fs.check_stored_row(NEW_INO, "rename"), Ok(()));
        }
    }
}

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
