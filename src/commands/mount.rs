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

use std::path::PathBuf;

// ─── Stub (no FUSE feature) ───────────────────────────────────────────────────

#[cfg(not(feature = "fuse"))]
pub async fn run(_mountpoint: PathBuf, _foreground: bool, _cache_ttl: u64) -> Result<(), String> {
    Err(concat!(
        "bb mount requires the `fuse` feature.\n",
        "\n  macOS:  brew install macfuse  (reboot required, then enable in System Settings)\n",
        "          cargo build --features fuse\n",
        "\n  Linux:  apt install libfuse3-dev\n",
        "          cargo build --features fuse",
    )
    .to_string())
}

#[cfg(not(feature = "fuse"))]
pub async fn unmount(_mountpoint: PathBuf) -> Result<(), String> {
    Err("bb unmount requires the `fuse` feature — see `bb mount --help`.".to_string())
}

// ─── Full implementation (fuse feature enabled) ───────────────────────────────

#[cfg(feature = "fuse")]
mod fuse_impl {
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use base64::Engine as _;
    use beebeeb_types::EncryptedBlob;
    use colored::Colorize;
    use fuser::{
        FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry,
        ReplyOpen, Request,
    };

    use crate::api::ApiClient;
    use crate::config::load_config;

    // ── Constants ─────────────────────────────────────────────────────────────

    const ROOT_INO: u64 = 1;
    const ATTR_TTL: Duration = Duration::from_secs(5);
    /// 1 MiB — matches `bb push` chunk size.
    const CHUNK_SIZE: usize = 1024 * 1024;

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
    }

    impl InodeEntry {
        fn file_attr(&self, ino: u64) -> FileAttr {
            let kind = if self.is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            FileAttr {
                ino,
                size: self.size,
                blocks: self.size.div_ceil(512),
                atime: self.modified,
                mtime: self.modified,
                ctime: self.modified,
                crtime: self.modified,
                kind,
                // rw-r--r-- for files, rwxr-xr-x for dirs — writable by owner
                perm: if self.is_dir { 0o755 } else { 0o644 },
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

                let name = match file.get("name_encrypted").and_then(|v| v.as_str()) {
                    Some(enc) => decrypt_name(&self.master_key, &file_id, enc),
                    None => format!("[{}]", &file_id[..8.min(file_id.len())]),
                };

                let ino = if let Some(&existing) = self.id_to_ino.get(&file_id) {
                    if let Some(entry) = self.inodes.get_mut(&existing) {
                        entry.name = name;
                        entry.size = size_bytes;
                        entry.modified = modified;
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
                .find(|&ino| self.inodes.get(&ino).map_or(false, |e| e.name == name))
        }

        // ── File download ─────────────────────────────────────────────────────

        fn fetch_file(&mut self, ino: u64) -> Result<(), String> {
            if self.file_cache.contains_key(&ino) {
                return Ok(());
            }
            let entry = self.inodes.get(&ino).ok_or("inode not found")?;
            let file_id = entry.file_id.as_ref().ok_or("no file_id")?.clone();
            let chunk_count = entry.chunk_count;
            // Use key_uuid for key derivation (may differ from server file_id
            // for FUSE-created files).
            let key_uuid = entry.key_uuid.or_else(|| file_id.parse().ok()).ok_or("no key UUID")?;
            let file_key = beebeeb_core::kdf::derive_file_key(&self.master_key, key_uuid.as_bytes());

            let encrypted = self.rt.block_on(self.api.download_file(&file_id))?;
            let plaintext = decrypt_chunks(&encrypted, &file_key, chunk_count)?;

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
            let file_key = beebeeb_core::kdf::derive_file_key(&self.master_key, key_uuid.as_bytes());

            // Encrypt the filename.
            let name_blob =
                beebeeb_core::encrypt::encrypt_metadata(&file_key, name).map_err(|e| format!("encrypt name: {e}"))?;
            let name_enc = serde_json::to_string(&name_blob).map_err(|e| format!("serialize name: {e}"))?;

            // Encrypt content in 1 MiB chunks.
            let mut encrypted_chunks: Vec<(u32, Vec<u8>)> = Vec::new();
            let chunks: Vec<&[u8]> = if plaintext.is_empty() {
                vec![&[]] // at least one (empty) chunk
            } else {
                plaintext.chunks(CHUNK_SIZE).collect()
            };

            for (i, chunk) in chunks.iter().enumerate() {
                let blob = beebeeb_core::encrypt::encrypt_chunk(&file_key, chunk)
                    .map_err(|e| format!("encrypt chunk {i}: {e}"))?;
                let bytes = serde_json::to_vec(&blob).map_err(|e| format!("serialize chunk {i}: {e}"))?;
                encrypted_chunks.push((i as u32, bytes));
            }

            // Resolve parent_id (None = root).
            let parent_file_id: Option<uuid::Uuid> = self
                .inodes
                .get(&parent_ino)
                .and_then(|e| e.file_id.as_deref())
                .and_then(|s| s.parse().ok());

            // Compute total encrypted size for the metadata field.
            let encrypted_size: usize = encrypted_chunks.iter().map(|(_, b)| b.len()).sum();

            // Build metadata JSON matching the server's UploadMetadata struct.
            let mime = mime_guess::from_path(name)
                .first_raw()
                .unwrap_or("application/octet-stream");
            let meta = serde_json::json!({
                "name_encrypted": name_enc,
                "parent_id": parent_file_id,
                "mime_type": mime,
                "size_bytes": encrypted_size,
            });
            let meta_str = serde_json::to_string(&meta).map_err(|e| format!("meta json: {e}"))?;

            let resp = self
                .rt
                .block_on(self.api.upload_encrypted(&meta_str, &encrypted_chunks))?;

            // The server assigns the final file_id.
            let server_id = resp
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| key_uuid.to_string());

            // Update inode: server_id for API calls, key_uuid for decryption.
            if let Some(entry) = self.inodes.get_mut(&ino) {
                self.id_to_ino.remove(entry.file_id.as_deref().unwrap_or(""));
                entry.file_id = Some(server_id.clone());
                entry.key_uuid = Some(key_uuid);
                entry.size = plaintext.len() as u64;
                entry.chunk_count = encrypted_chunks.len() as u32;
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
                ino: ROOT_INO,
                size: 0,
                blocks: 0,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
                rdev: 0,
                blksize: 4096,
                flags: 0,
            }
        }
    }

    // ── FUSE trait implementation ─────────────────────────────────────────────

    impl Filesystem for BeebeebFs {
        // ─── Read operations ──────────────────────────────────────────────────

        fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
            let name_str = match name.to_str() {
                Some(n) => n.to_string(),
                None => {
                    reply.error(libc::ENOENT);
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
                                reply.error(libc::ENOENT);
                                return;
                            }
                        }
                    };
                    reply.entry(&ATTR_TTL, &attr, 0);
                }
                None => reply.error(libc::ENOENT),
            }
        }

        fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
            if ino == ROOT_INO {
                reply.attr(&ATTR_TTL, &Self::root_attr());
                return;
            }
            match self.inodes.get(&ino) {
                Some(e) => {
                    let a = e.clone().file_attr(ino);
                    reply.attr(&ATTR_TTL, &a);
                }
                None => reply.error(libc::ENOENT),
            }
        }

        fn readdir(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
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
                if reply.add(*child_ino, (i + 1) as i64, *kind, name) {
                    break;
                }
            }
            reply.ok();
        }

        fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
            match self.inodes.get(&ino) {
                Some(e) if !e.is_dir => reply.opened(ino, 0),
                Some(_) => reply.error(libc::EISDIR),
                None => reply.error(libc::ENOENT),
            }
        }

        fn read(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: i64,
            size: u32,
            _flags: i32,
            _lock_owner: Option<u64>,
            reply: ReplyData,
        ) {
            if !self.file_cache.contains_key(&ino) {
                if let Err(e) = self.fetch_file(ino) {
                    eprintln!("[mount] read ino={ino}: {e}");
                    reply.error(libc::EIO);
                    return;
                }
            }
            match self.file_cache.get(&ino) {
                Some(data) => {
                    let start = (offset as usize).min(data.len());
                    let end = (start + size as usize).min(data.len());
                    reply.data(&data[start..end]);
                }
                None => reply.error(libc::EIO),
            }
        }

        // ─── Write operations ─────────────────────────────────────────────────

        /// Create a new file in `parent` with `name`.
        /// Initialises a write buffer; the file is uploaded on `release()`.
        fn create(
            &mut self,
            _req: &Request,
            parent: u64,
            name: &OsStr,
            _mode: u32,
            _umask: u32,
            _flags: i32,
            reply: fuser::ReplyCreate,
        ) {
            let name_str = match name.to_str() {
                Some(n) => n.to_string(),
                None => {
                    reply.error(libc::EINVAL);
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
            // generation=0, fh=ino (we key write_buffers by ino), flags=0
            reply.created(&ATTR_TTL, &attr, 0, ino, 0);
        }

        /// Accumulate `data` at `offset` into the in-memory write buffer.
        fn write(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: i64,
            data: &[u8],
            _write_flags: u32,
            _flags: i32,
            _lock_owner: Option<u64>,
            reply: fuser::ReplyWrite,
        ) {
            // Only files with an active write buffer accept writes.
            if !self.write_buffers.contains_key(&ino) {
                reply.error(libc::ENOTSUP);
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
            ino: u64,
            _mode: Option<u32>,
            _uid: Option<u32>,
            _gid: Option<u32>,
            size: Option<u64>,
            _atime: Option<fuser::TimeOrNow>,
            _mtime: Option<fuser::TimeOrNow>,
            _ctime: Option<SystemTime>,
            _fh: Option<u64>,
            _crtime: Option<SystemTime>,
            _chgtime: Option<SystemTime>,
            _bkuptime: Option<SystemTime>,
            _flags: Option<u32>,
            reply: ReplyAttr,
        ) {
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
                None => reply.error(libc::ENOENT),
            }
        }

        /// Upload the write buffer when the last file descriptor is closed.
        fn release(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            _flags: i32,
            _lock_owner: Option<u64>,
            _flush: bool,
            reply: ReplyEmpty,
        ) {
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
                        reply.error(libc::EIO);
                    }
                }
                return;
            }

            // ── Case 2: existing file re-written (e.g. O_TRUNC overwrite) ────
            if self.dirty_inodes.remove(&ino) {
                let (name, parent_ino, old_file_id) = match self.inodes.get(&ino) {
                    Some(e) => (e.name.clone(), e.parent_ino, e.file_id.clone()),
                    None => {
                        reply.error(libc::ENOENT);
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
                        reply.error(libc::EIO);
                    }
                }
                return;
            }

            // ── Case 3: clean release (no write occurred) ─────────────────────
            reply.ok();
        }

        // ─── Directory write operations ───────────────────────────────────────

        /// Create an encrypted folder.
        fn mkdir(&mut self, _req: &Request, parent: u64, name: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) {
            let name_str = match name.to_str() {
                Some(n) => n.to_string(),
                None => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };

            // Generate a UUID for key derivation and folder identity.
            let folder_uuid = uuid::Uuid::new_v4();
            let folder_key = beebeeb_core::kdf::derive_file_key(&self.master_key, folder_uuid.as_bytes());

            let name_blob = match beebeeb_core::encrypt::encrypt_metadata(&folder_key, &name_str) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[mount] mkdir encrypt: {e}");
                    reply.error(libc::EIO);
                    return;
                }
            };
            let name_enc = match serde_json::to_string(&name_blob) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[mount] mkdir serialize: {e}");
                    reply.error(libc::EIO);
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
                    reply.error(libc::EIO);
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
                },
            );
            self.dir_cache.remove(&parent);

            let attr = self.inodes[&ino].file_attr(ino);
            reply.entry(&ATTR_TTL, &attr, 0);
        }

        /// Soft-delete (trash) a file.
        fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
            let name_str = match name.to_str() {
                Some(n) => n,
                None => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };

            let ino = match self.find_child(parent, name_str) {
                Some(i) => i,
                None => {
                    reply.error(libc::ENOENT);
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
                reply.error(libc::EIO);
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
        fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
            let name_str = match name.to_str() {
                Some(n) => n,
                None => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };

            let ino = match self.find_child(parent, name_str) {
                Some(i) => i,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            // Must be empty: list children (bypasses cache to be sure).
            let children = self.populate_dir(ino);
            if !children.is_empty() {
                reply.error(libc::ENOTEMPTY);
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
                reply.error(libc::EIO);
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
            parent: u64,
            name: &OsStr,
            newparent: u64,
            newname: &OsStr,
            _flags: u32,
            reply: ReplyEmpty,
        ) {
            let name_str = match name.to_str() {
                Some(n) => n,
                None => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };
            let newname_str = match newname.to_str() {
                Some(n) => n.to_string(),
                None => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };

            let ino = match self.find_child(parent, name_str) {
                Some(i) => i,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            let (file_id, key_uuid, old_name) = match self.inodes.get(&ino) {
                Some(e) => (e.file_id.clone(), e.key_uuid, e.name.clone()),
                None => {
                    reply.error(libc::ENOENT);
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

            // Re-encrypt the name if it changed, using the file's existing key.
            let new_name_enc: Option<String> = if name_changed {
                let ku = key_uuid
                    .or_else(|| file_id.parse().ok())
                    .unwrap_or_else(uuid::Uuid::new_v4);
                let fk = beebeeb_core::kdf::derive_file_key(&self.master_key, ku.as_bytes());
                match beebeeb_core::encrypt::encrypt_metadata(&fk, &newname_str) {
                    Ok(blob) => match serde_json::to_string(&blob) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            eprintln!("[mount] rename serialize: {e}");
                            reply.error(libc::EIO);
                            return;
                        }
                    },
                    Err(e) => {
                        eprintln!("[mount] rename encrypt: {e}");
                        reply.error(libc::EIO);
                        return;
                    }
                }
            } else {
                None
            };

            // Resolve new parent file_id (None = root).
            let new_parent_file_id: Option<uuid::Uuid> = if parent_changed {
                self.inodes
                    .get(&newparent)
                    .and_then(|e| e.file_id.as_deref())
                    .and_then(|s| s.parse().ok())
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
                reply.error(libc::EIO);
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

    // ── Crypto helpers ────────────────────────────────────────────────────────

    fn decrypt_name(master_key: &beebeeb_core::kdf::MasterKey, file_id: &str, name_encrypted: &str) -> String {
        (|| -> Option<String> {
            let uuid: uuid::Uuid = file_id.parse().ok()?;
            let fk = beebeeb_core::kdf::derive_file_key(master_key, uuid.as_bytes());
            let blob: EncryptedBlob = serde_json::from_str(name_encrypted).ok()?;
            beebeeb_core::encrypt::decrypt_metadata(&fk, &blob).ok()
        })()
        .unwrap_or_else(|| format!("[{}]", &file_id[..8.min(file_id.len())]))
    }

    fn decrypt_chunks(data: &[u8], file_key: &beebeeb_core::kdf::FileKey, chunk_count: u32) -> Result<Vec<u8>, String> {
        let mut plaintext = Vec::new();
        let mut offset = 0;
        for i in 0..chunk_count {
            if offset >= data.len() {
                return Err(format!("unexpected end at chunk {i}/{chunk_count}"));
            }
            let mut de = serde_json::Deserializer::from_slice(&data[offset..]).into_iter::<EncryptedBlob>();
            let blob = match de.next() {
                Some(Ok(b)) => b,
                Some(Err(e)) => return Err(format!("parse chunk {i}: {e}")),
                None => return Err(format!("no data for chunk {i}")),
            };
            offset += de.byte_offset();
            let dec = beebeeb_core::encrypt::decrypt_chunk(file_key, &blob).map_err(|e| format!("decrypt {i}: {e}"))?;
            plaintext.extend_from_slice(&dec);
        }
        Ok(plaintext)
    }

    // ── CLI entry points ──────────────────────────────────────────────────────

    pub async fn run(mountpoint: PathBuf, foreground: bool, cache_ttl: u64) -> Result<(), String> {
        let config = load_config();
        if config.session_token.is_none() {
            return Err("Not logged in. Run `bb login` first.".to_string());
        }
        let mk_b64 = config.master_key.ok_or("No master key. Run `bb login`.")?;
        let mk_bytes = base64::engine::general_purpose::STANDARD
            .decode(&mk_b64)
            .map_err(|e| format!("invalid master key: {e}"))?;
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

        let options = vec![
            MountOption::RW, // Day 2: full read-write
            MountOption::FSName("beebeeb".to_string()),
            MountOption::Subtype("beebeeb".to_string()),
            MountOption::AutoUnmount,
            MountOption::AllowOther,
            MountOption::DefaultPermissions,
        ];

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
                fuser::mount2(fs, &mountpoint, &options).map_err(|e| format!("FUSE mount failed: {e}"))
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
                if let Err(e) = fuser::mount2(fs, &mountpoint, &options) {
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
}

#[cfg(feature = "fuse")]
pub use fuse_impl::{run, unmount};
