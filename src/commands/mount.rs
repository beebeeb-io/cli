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
//! - All write ops → `EROFS` (read-only filesystem error)
//!
//! # Day 2+ planned
//! - Write support (create, write, unlink, rename)
//! - Partial-read (avoid full file download for head/tail)
//! - Persistent disk cache

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
    ).to_string())
}

#[cfg(not(feature = "fuse"))]
pub async fn unmount(_mountpoint: PathBuf) -> Result<(), String> {
    Err("bb unmount requires the `fuse` feature — see `bb mount --help`.".to_string())
}

// ─── Full implementation (fuse feature enabled) ───────────────────────────────

#[cfg(feature = "fuse")]
mod fuse_impl {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use base64::Engine as _;
    use beebeeb_types::EncryptedBlob;
    use colored::Colorize;
    use fuser::{
        FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory,
        ReplyEmpty, ReplyEntry, ReplyOpen, Request,
    };

    use crate::api::ApiClient;
    use crate::config::load_config;

    // ── Constants ─────────────────────────────────────────────────────────────

    const ROOT_INO: u64 = 1;
    const ATTR_TTL: Duration = Duration::from_secs(5);
    const EROFS: i32 = libc::EROFS;

    // ── Data structures ───────────────────────────────────────────────────────

    #[derive(Clone)]
    struct InodeEntry {
        file_id: Option<String>,
        name: String,
        is_dir: bool,
        size: u64,
        chunk_count: u32,
        modified: SystemTime,
        parent_ino: u64,
    }

    impl InodeEntry {
        fn file_attr(&self, ino: u64) -> FileAttr {
            let kind = if self.is_dir { FileType::Directory } else { FileType::RegularFile };
            FileAttr {
                ino,
                size: self.size,
                blocks: self.size.div_ceil(512),
                atime: self.modified,
                mtime: self.modified,
                ctime: self.modified,
                crtime: self.modified,
                kind,
                perm: if self.is_dir { 0o555 } else { 0o444 },
                nlink: if self.is_dir { 2 } else { 1 },
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
                rdev: 0,
                blksize: 4096,
                flags: 0,
            }
        }
    }

    struct CachedDir {
        children: Vec<u64>,
        expires_at: std::time::Instant,
    }

    pub struct BeebeebFs {
        rt: tokio::runtime::Runtime,
        api: ApiClient,
        master_key: beebeeb_core::kdf::MasterKey,
        inodes: HashMap<u64, InodeEntry>,
        id_to_ino: HashMap<String, u64>,
        next_ino: u64,
        dir_cache: HashMap<u64, CachedDir>,
        file_cache: HashMap<u64, Vec<u8>>,
        cache_ttl: Duration,
    }

    impl BeebeebFs {
        pub fn new(
            api: ApiClient,
            master_key: beebeeb_core::kdf::MasterKey,
            cache_ttl: Duration,
        ) -> Self {
            let mut fs = Self {
                rt: tokio::runtime::Runtime::new()
                    .expect("failed to create FUSE tokio runtime"),
                api,
                master_key,
                inodes: HashMap::new(),
                id_to_ino: HashMap::new(),
                next_ino: 2,
                dir_cache: HashMap::new(),
                file_cache: HashMap::new(),
                cache_ttl,
            };
            fs.inodes.insert(ROOT_INO, InodeEntry {
                file_id: None,
                name: "/".to_string(),
                is_dir: true,
                size: 0,
                chunk_count: 0,
                modified: SystemTime::now(),
                parent_ino: ROOT_INO,
            });
            fs
        }

        fn alloc_ino(&mut self) -> u64 {
            let ino = self.next_ino;
            self.next_ino += 1;
            ino
        }

        /// Fetch and cache the children of `dir_ino`.
        fn populate_dir(&mut self, dir_ino: u64) -> Vec<u64> {
            if let Some(cached) = self.dir_cache.get(&dir_ino) {
                if cached.expires_at > std::time::Instant::now() {
                    return cached.children.clone();
                }
            }

            let parent_id =
                self.inodes.get(&dir_ino).and_then(|e| e.file_id.clone());

            let resp = match self
                .rt
                .block_on(self.api.list_files(parent_id.as_deref()))
            {
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
                let is_folder =
                    file.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
                let size_bytes =
                    file.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                let chunk_count =
                    file.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
                let modified = file
                    .get("updated_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| {
                        UNIX_EPOCH + Duration::from_secs(dt.timestamp().max(0) as u64)
                    })
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
                    self.id_to_ino.insert(file_id.clone(), ino);
                    self.inodes.insert(ino, InodeEntry {
                        file_id: Some(file_id),
                        name,
                        is_dir: is_folder,
                        size: size_bytes,
                        chunk_count,
                        modified,
                        parent_ino: dir_ino,
                    });
                    ino
                };

                children.push(ino);
            }

            if !self.cache_ttl.is_zero() {
                self.dir_cache.insert(dir_ino, CachedDir {
                    children: children.clone(),
                    expires_at: std::time::Instant::now() + self.cache_ttl,
                });
            }

            children
        }

        /// Download and decrypt a file into `file_cache`.
        fn fetch_file(&mut self, ino: u64) -> Result<(), String> {
            if self.file_cache.contains_key(&ino) {
                return Ok(());
            }

            let entry = self.inodes.get(&ino).ok_or("inode not found")?;
            let file_id = entry.file_id.as_ref().ok_or("no file_id")?.clone();
            let chunk_count = entry.chunk_count;
            let file_uuid: uuid::Uuid =
                file_id.parse().map_err(|e| format!("uuid: {e}"))?;
            let file_key =
                beebeeb_core::kdf::derive_file_key(&self.master_key, file_uuid.as_bytes());

            let encrypted =
                self.rt.block_on(self.api.download_file(&file_id))?;

            let plaintext = decrypt_chunks(&encrypted, &file_key, chunk_count)?;

            if let Some(e) = self.inodes.get_mut(&ino) {
                e.size = plaintext.len() as u64;
            }
            self.file_cache.insert(ino, plaintext);
            Ok(())
        }

        fn find_child(&mut self, parent_ino: u64, name: &str) -> Option<u64> {
            let children = self.populate_dir(parent_ino);
            children.into_iter().find(|&ino| {
                self.inodes.get(&ino).map_or(false, |e| e.name == name)
            })
        }

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
                perm: 0o555,
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
        fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
            let name_str = match name.to_str() {
                Some(n) => n.to_string(),
                None => { reply.error(libc::ENOENT); return; }
            };

            match self.find_child(parent, &name_str) {
                Some(ino) => {
                    let attr = if ino == ROOT_INO {
                        Self::root_attr()
                    } else {
                        match self.inodes.get(&ino) {
                            Some(e) => e.clone().file_attr(ino),
                            None => { reply.error(libc::ENOENT); return; }
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
                Some(e) => { let a = e.clone().file_attr(ino); reply.attr(&ATTR_TTL, &a); }
                None => reply.error(libc::ENOENT),
            }
        }

        fn readdir(
            &mut self, _req: &Request, ino: u64, _fh: u64,
            offset: i64, mut reply: ReplyDirectory,
        ) {
            let parent_ino = self.inodes.get(&ino).map(|e| e.parent_ino).unwrap_or(ROOT_INO);
            let mut entries: Vec<(u64, FileType, String)> = vec![
                (ino, FileType::Directory, ".".to_string()),
                (parent_ino, FileType::Directory, "..".to_string()),
            ];

            let children = self.populate_dir(ino);
            for &child_ino in &children {
                if let Some(e) = self.inodes.get(&child_ino) {
                    let kind = if e.is_dir { FileType::Directory } else { FileType::RegularFile };
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
                Some(e) if !e.is_dir => reply.opened(0, fuser::consts::FOPEN_KEEP_CACHE),
                Some(_) => reply.error(libc::EISDIR),
                None => reply.error(libc::ENOENT),
            }
        }

        fn read(
            &mut self, _req: &Request, ino: u64, _fh: u64,
            offset: i64, size: u32, _flags: i32, _lock_owner: Option<u64>,
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

        // ── All write ops → EROFS ─────────────────────────────────────────────

        fn write(&mut self, _req: &Request, _ino: u64, _fh: u64, _offset: i64,
            _data: &[u8], _write_flags: u32, _flags: i32, _lock_owner: Option<u64>,
            reply: fuser::ReplyWrite) { reply.error(EROFS); }

        fn create(&mut self, _req: &Request, _parent: u64, _name: &OsStr,
            _mode: u32, _umask: u32, _flags: i32, reply: fuser::ReplyCreate) { reply.error(EROFS); }

        fn mkdir(&mut self, _req: &Request, _parent: u64, _name: &OsStr,
            _mode: u32, _umask: u32, reply: ReplyEntry) { reply.error(EROFS); }

        fn unlink(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: ReplyEmpty) {
            reply.error(EROFS);
        }

        fn rmdir(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: ReplyEmpty) {
            reply.error(EROFS);
        }

        fn rename(&mut self, _req: &Request, _parent: u64, _name: &OsStr,
            _newparent: u64, _newname: &OsStr, _flags: u32, reply: ReplyEmpty) { reply.error(EROFS); }

        fn setattr(&mut self, _req: &Request, _ino: u64, _mode: Option<u32>,
            _uid: Option<u32>, _gid: Option<u32>, _size: Option<u64>,
            _atime: Option<fuser::TimeOrNow>, _mtime: Option<fuser::TimeOrNow>,
            _ctime: Option<SystemTime>, _fh: Option<u64>, _crtime: Option<SystemTime>,
            _chgtime: Option<SystemTime>, _bkuptime: Option<SystemTime>, _flags: Option<u32>,
            reply: ReplyAttr) { reply.error(EROFS); }
    }

    // ── Crypto helpers ────────────────────────────────────────────────────────

    fn decrypt_name(
        master_key: &beebeeb_core::kdf::MasterKey,
        file_id: &str,
        name_encrypted: &str,
    ) -> String {
        (|| -> Option<String> {
            let uuid: uuid::Uuid = file_id.parse().ok()?;
            let fk = beebeeb_core::kdf::derive_file_key(master_key, uuid.as_bytes());
            let blob: EncryptedBlob = serde_json::from_str(name_encrypted).ok()?;
            beebeeb_core::encrypt::decrypt_metadata(&fk, &blob).ok()
        })()
        .unwrap_or_else(|| format!("[{}]", &file_id[..8.min(file_id.len())]))
    }

    fn decrypt_chunks(
        data: &[u8],
        file_key: &beebeeb_core::kdf::FileKey,
        chunk_count: u32,
    ) -> Result<Vec<u8>, String> {
        let mut plaintext = Vec::new();
        let mut offset = 0;
        for i in 0..chunk_count {
            if offset >= data.len() {
                return Err(format!("unexpected end at chunk {i}/{chunk_count}"));
            }
            let mut de = serde_json::Deserializer::from_slice(&data[offset..])
                .into_iter::<EncryptedBlob>();
            let blob = match de.next() {
                Some(Ok(b)) => b,
                Some(Err(e)) => return Err(format!("parse chunk {i}: {e}")),
                None => return Err(format!("no data for chunk {i}")),
            };
            offset += de.byte_offset();
            let dec = beebeeb_core::encrypt::decrypt_chunk(file_key, &blob)
                .map_err(|e| format!("decrypt {i}: {e}"))?;
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

        std::fs::create_dir_all(&mountpoint)
            .map_err(|e| format!("create mountpoint: {e}"))?;

        let ttl = if cache_ttl == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs(cache_ttl)
        };
        let fs = BeebeebFs::new(ApiClient::from_config(), master_key, ttl);

        let options = vec![
            MountOption::RO,
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
            "◆".truecolor(212, 168, 67),
            "Vault mounted".truecolor(208, 200, 154),
        );
        println!("  {}  {}", "mountpoint".truecolor(106, 101, 91),
            mp.display().to_string().truecolor(245, 184, 0));
        println!("  {}  {}",
            "cache     ".truecolor(106, 101, 91),
            if cache_ttl == 0 { "disabled".into() } else { format!("{cache_ttl}s TTL") }
                .truecolor(106, 101, 91));

        if foreground {
            println!("  {}", "Running in foreground — Ctrl+C to unmount.".truecolor(106, 101, 91));
            println!();
            tokio::task::spawn_blocking(move || {
                fuser::mount2(fs, &mountpoint, &options)
                    .map_err(|e| format!("FUSE mount failed: {e}"))
            })
            .await
            .map_err(|e| format!("task panic: {e}"))??;
        } else {
            println!(
                "  {}",
                format!("Unmount: bb unmount {}", mp.display()).truecolor(106, 101, 91),
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
                    "◆".truecolor(212, 168, 67),
                    format!("Unmounted {}", mountpoint.display()).truecolor(208, 200, 154),
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
