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
//! For files pulled from the server the UUID = server-assigned file_id. For
//! files we create via FUSE, we generate a random UUID (`key_uuid`) for key
//! derivation before uploading; the server may assign a *different* ID. We
//! store `key_uuid` alongside `file_id` in the inode so `read` can always
//! derive the right key regardless of what the server assigned.

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
                let read_only = name_encrypted.is_some_and(|enc| {
                    crate::crypto::name_is_upper_repair(&self.master_key, &file_id, enc)
                });
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
            // Generate a stable UUID for this file. Key derivation uses this UUID.
            let key_uuid = uuid::Uuid::new_v4();
            let file_key = beebeeb_core::kdf::derive_file_key(&self.master_key, key_uuid.to_string().as_bytes());

            // Encrypt the filename (MIME type is now part of the encrypted envelope).
            let mime = beebeeb_core::media::guess_mime_type(name);
            let name_enc = beebeeb_core::encrypt::encrypt_name(&self.master_key, &key_uuid.to_string(), name, mime)
                .map_err(|e| format!("encrypt name: {e}"))?;

            // Encrypt content using the adaptive chunk-size ladder.
            let plan = beebeeb_types::plan_chunks(plaintext.len() as u64, beebeeb_types::ChunkProfile::Cli);
            let chunk_size = plan.chunk_size_bytes as usize;

            let mut encrypted_chunks: Vec<(u32, Vec<u8>)> = Vec::new();
            let chunks: Vec<&[u8]> = if plaintext.is_empty() {
                vec![&[]] // at least one (empty) chunk
            } else {
                plaintext.chunks(chunk_size).collect()
            };

            for (i, chunk) in chunks.iter().enumerate() {
                let bytes = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, chunk)
                    .map_err(|e| format!("encrypt chunk {i}: {e}"))?;
                encrypted_chunks.push((i as u32, bytes));
            }

            // Resolve parent_id (None = root).
            let parent_file_id: Option<uuid::Uuid> = self
                .inodes
                .get(&parent_ino)
                .and_then(|e| e.file_id.as_deref())
                .and_then(|s| s.parse().ok());

            let is_media = beebeeb_core::media::is_media(mime);
            let chunk_count = encrypted_chunks.len() as i32;

            // V2 upload: init (open session) → chunks (by session) → complete.
            // Mount always mints a fresh `key_uuid` per flush (new file each
            // write), so there's no version replace → `base_version_number` None.
            // The v2 init takes the PLAINTEXT size; the server recomputes the
            // stored encrypted total from the summed chunks.
            let init = self.rt.block_on(self.api.upload_init(
                Some(key_uuid),
                &name_enc,
                parent_file_id,
                plaintext.len() as i64,
                chunk_size as i64,
                chunk_count,
                is_media,
                None,
            ))?;

            let server_id = init.file_id.clone();
            let session_id = init.upload_session_id.clone();

            for (idx, data) in encrypted_chunks {
                self.rt
                    .block_on(self.api.upload_chunk(&session_id, idx, bytes::Bytes::from(data)))?;
            }

            self.rt.block_on(self.api.upload_complete(&session_id))?;

            // Update inode: server_id for API calls, key_uuid for decryption.
            if let Some(entry) = self.inodes.get_mut(&ino) {
                self.id_to_ino.remove(entry.file_id.as_deref().unwrap_or(""));
                entry.file_id = Some(server_id.clone());
                entry.key_uuid = Some(key_uuid);
                entry.size = plaintext.len() as u64;
                entry.chunk_count = chunk_count as u32;
                entry.modified = SystemTime::now();
            }
            self.id_to_ino.insert(server_id, ino);

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

            // If a file with this name already exists in parent, trash it first
            // so the copy-to-existing-name case works cleanly.
            if let Some(old_ino) = self.find_child(parent, &name_str) {
                if let Some(file_id) = self.inodes.get(&old_ino).and_then(|e| e.file_id.clone()) {
                    let _ = self.rt.block_on(self.api.trash_file(&file_id));
                    self.id_to_ino.remove(&file_id);
                }
                self.inodes.remove(&old_ino);
                self.write_buffers.remove(&old_ino);
                self.file_cache.remove(&old_ino);
                self.dir_cache.remove(&parent);
            }

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
                },
            );
            self.write_buffers.insert(ino, Vec::new());

            let attr = self.inodes[&ino].file_attr(ino);
            // generation=0, fh=ino (we key write_buffers by ino), no open flags
            reply.created(&ATTR_TTL, &attr, Generation(0), FileHandle(ino), FopenFlags::empty());
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
            let ino = ino.0;
            // Only files with an active write buffer accept writes.
            if !self.write_buffers.contains_key(&ino) {
                reply.error(Errno::ENOTSUP);
                return;
            }

            let offset = offset as usize;
            let buf = self.write_buffers.entry(ino).or_default();

            // Extend to cover offset + data.len(), filling gaps with zeros
            // (supports sparse writes from tools like rsync).
            let needed = offset + data.len();
            if needed > buf.len() {
                buf.resize(needed, 0);
            }
            buf[offset..offset + data.len()].copy_from_slice(data);

            // Keep inode size in sync so getattr returns the right value.
            if let Some(entry) = self.inodes.get_mut(&ino) {
                entry.size = buf.len() as u64;
            }

            reply.written(data.len() as u32);
        }

        /// Handle attribute changes:
        /// - chmod / chown → no-op, return current attr
        /// - truncate → resize the write buffer (creates one if needed)
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
                let buf = self.write_buffers.entry(ino).or_default();
                buf.resize(new_size as usize, 0);

                // If this is an existing file (not a pending create) being
                // truncated, mark it dirty so release() re-uploads it.
                if !self.pending_creates.contains_key(&ino) {
                    self.dirty_inodes.insert(ino);
                }

                if let Some(entry) = self.inodes.get_mut(&ino) {
                    entry.size = new_size;
                    // Evict the read cache — content is now different.
                    self.file_cache.remove(&ino);
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
            // Take the write buffer — if absent, nothing to upload.
            let buf = match self.write_buffers.remove(&ino) {
                Some(b) => b,
                None => {
                    reply.ok();
                    return;
                }
            };

            // ── Case 1: brand-new file ────────────────────────────────────────
            if let Some(pending) = self.pending_creates.remove(&ino) {
                let name = pending.name.clone();
                let parent_ino = pending.parent_ino;
                match self.upload_file(ino, &name, buf, parent_ino) {
                    Ok(_) => reply.ok(),
                    Err(e) => {
                        eprintln!("[mount] release upload (new file): {e}");
                        reply.error(Errno::EIO);
                    }
                }
                return;
            }

            // ── Case 2: existing file re-written (e.g. O_TRUNC overwrite) ────
            if self.dirty_inodes.remove(&ino) {
                let (name, parent_ino, old_file_id) = match self.inodes.get(&ino) {
                    Some(e) => (e.name.clone(), e.parent_ino, e.file_id.clone()),
                    None => {
                        reply.error(Errno::ENOENT);
                        return;
                    }
                };

                // Trash the old server-side file before uploading the new one.
                if let Some(old_id) = old_file_id {
                    if let Err(e) = self.rt.block_on(self.api.trash_file(&old_id)) {
                        eprintln!("[mount] release: trash old file {old_id}: {e}");
                        // Non-fatal — continue with upload.
                    }
                    self.id_to_ino.remove(&old_id);
                    if let Some(e) = self.inodes.get_mut(&ino) {
                        e.file_id = None;
                    }
                }

                match self.upload_file(ino, &name, buf, parent_ino) {
                    Ok(_) => reply.ok(),
                    Err(e) => {
                        eprintln!("[mount] release upload (overwrite): {e}");
                        reply.error(Errno::EIO);
                    }
                }
                return;
            }

            // ── Case 3: clean release (no write occurred) ─────────────────────
            reply.ok();
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

            let server_id = resp
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| folder_uuid.to_string());

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

            let (file_id, key_uuid, old_name, entry_size, entry_chunks, entry_read_only) = match self.inodes.get(&ino) {
                Some(e) => (
                    e.file_id.clone(),
                    e.key_uuid,
                    e.name.clone(),
                    e.size,
                    e.chunk_count,
                    e.read_only,
                ),
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

            // Determine what changed.
            let name_changed = old_name != newname_str;
            let parent_changed = parent != newparent;

            // SF-0002: a rename rewrites the name envelope under the id's
            // convention, so a row sealed read-only (name or CURRENT version's
            // content) must be refused. Reparenting stays a pure move.
            if name_changed {
                let content_is_repair = if entry_read_only {
                    true
                } else {
                    match self.rt.block_on(crate::owned_keys::content_is_upper_repair(
                        &self.api,
                        &self.master_key,
                        &file_id,
                        Some(entry_size),
                        entry_chunks,
                    )) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("[mount] rename content check {file_id}: {e}");
                            reply.error(Errno::EIO);
                            return;
                        }
                    }
                };
                if content_is_repair {
                    eprintln!("[mount] rename refused: {file_id} is read-only (SF-0002)");
                    reply.error(Errno::EPERM);
                    return;
                }
            }

            // Re-encrypt the name if it changed, using the file's existing key.
            let new_name_enc: Option<String> = if name_changed {
                let ku = key_uuid
                    .or_else(|| file_id.parse().ok())
                    .unwrap_or_else(uuid::Uuid::new_v4);
                let fk = beebeeb_core::kdf::derive_file_key(&self.master_key, ku.to_string().as_bytes());
                match beebeeb_core::encrypt::encrypt_metadata(&fk, &newname_str) {
                    Ok(blob) => match serde_json::to_string(&blob) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            eprintln!("[mount] rename serialize: {e}");
                            reply.error(Errno::EIO);
                            return;
                        }
                    },
                    Err(e) => {
                        eprintln!("[mount] rename encrypt: {e}");
                        reply.error(Errno::EIO);
                        return;
                    }
                }
            } else {
                None
            };

            // If a file with the target name already exists in newparent, trash it.
            if let Some(conflict_ino) = self.find_child(newparent, &newname_str) {
                if conflict_ino != ino {
                    if let Some(cid) = self.inodes.get(&conflict_ino).and_then(|e| e.file_id.clone()) {
                        let _ = self.rt.block_on(self.api.trash_file(&cid));
                        self.id_to_ino.remove(&cid);
                    }
                    self.inodes.remove(&conflict_ino);
                    self.file_cache.remove(&conflict_ino);
                }
            }

            if let Err(e) = self.rt.block_on(
                self.api
                    .move_file(&file_id, new_name_enc.as_deref(), new_parent_file_id),
            ) {
                eprintln!("[mount] rename move_file: {e}");
                reply.error(Errno::EIO);
                return;
            }

            // Update local inode.
            if let Some(entry) = self.inodes.get_mut(&ino) {
                if name_changed {
                    entry.name = newname_str;
                }
                if parent_changed {
                    entry.parent_ino = newparent;
                }
            }
            self.dir_cache.remove(&parent);
            if parent_changed {
                self.dir_cache.remove(&newparent);
            }
            reply.ok();
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
