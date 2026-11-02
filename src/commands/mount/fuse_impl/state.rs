//! Inode-table and cache bookkeeping for `BeebeebFsState`.
//!
//! Split out of `commands::mount` (file-size ratchet): the FUSE implementation
//! lives here, one file per concern; the facade keeps the stub, `rename_parent_target`
//! and the cross-crate test mods.

use super::*;

impl BeebeebFsState {
    pub(super) fn new(api: ApiClient, master_key: beebeeb_core::kdf::MasterKey, cache_ttl: Duration) -> Self {
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

    pub(super) fn alloc_ino(&mut self) -> u64 {
        let ino = self.next_ino;
        self.next_ino += 1;
        ino
    }

    // ── Directory listing ─────────────────────────────────────────────────

    pub(super) fn populate_dir(&mut self, dir_ino: u64) -> Vec<u64> {
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
            let read_only =
                name_encrypted.is_some_and(|enc| crate::crypto::name_is_upper_repair(&self.master_key, &file_id, enc));
            let pin = match (is_folder, name_encrypted) {
                (true, _) => no_content(),
                (false, Some(enc)) => crate::crypto::content_pin(&self.master_key, &file_id, enc)
                    .map_err(|e| e.to_string()),
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

    pub(super) fn find_child(&mut self, parent_ino: u64, name: &str) -> Option<u64> {
        let children = self.populate_dir(parent_ino);
        children
            .into_iter()
            .find(|&ino| self.inodes.get(&ino).is_some_and(|e| e.name == name))
    }

    // ── File download ─────────────────────────────────────────────────────

    pub(super) fn fetch_file(&mut self, ino: u64) -> Result<(), String> {
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
        let plaintext = crate::crypto::decrypt_file_chunks(&self.master_key, &file_id, pin, &encrypted, chunk_count)?;

        if let Some(e) = self.inodes.get_mut(&ino) {
            e.size = plaintext.len() as u64;
        }
        self.file_cache.insert(ino, plaintext);
        Ok(())
    }

    // ── File upload (called from release()) ───────────────────────────────

    pub(super) fn upload_file(
        &mut self,
        ino: u64,
        name: &str,
        plaintext: Vec<u8>,
        parent_ino: u64,
    ) -> Result<(), String> {
        // Resolve parent_id (None = root).
        let parent_file_id: Option<uuid::Uuid> = self
            .inodes
            .get(&parent_ino)
            .and_then(|e| e.file_id.as_deref())
            .and_then(|s| s.parse().ok());

        // L13 (SF-0001): a new file under an id minted here, every echo
        // checked; an error leaves the inode exactly as it was.
        let uploaded = self.rt.block_on(super::super::upload_new_file(
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

    pub(super) fn root_attr() -> FileAttr {
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
