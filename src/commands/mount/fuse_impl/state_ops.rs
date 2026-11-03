//! Operations `BeebeebFsState` performs on the vault (fetch, upload, trash, move).
//!
//! Split out of `commands::mount` (file-size ratchet): the FUSE implementation
//! lives here, one file per concern; the facade keeps the stub, `rename_parent_target`
//! and the cross-crate test mods.

use super::*;

// ── FUSE request handlers ─────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
impl BeebeebFsState {
    // ─── Read operations ──────────────────────────────────────────────────

    pub(super) fn lookup(&mut self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
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

    pub(super) fn getattr(&mut self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
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

    pub(super) fn readdir(
        &mut self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
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

    pub(super) fn open(&mut self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let ino = ino.0;
        match self.inodes.get(&ino) {
            Some(e) if !e.is_dir => reply.opened(FileHandle(ino), FopenFlags::empty()),
            Some(_) => reply.error(Errno::EISDIR),
            None => reply.error(Errno::ENOENT),
        }
    }

    pub(super) fn read(
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
    pub(super) fn create(
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
    pub(super) fn apply_write(&mut self, ino: u64, offset: u64, data: &[u8]) -> Result<(), Errno> {
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
    pub(super) fn write(
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
    pub(super) fn resize_file(&mut self, ino: u64, new_size: u64) -> Result<TruncateSource, ResizeError> {
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
            truncate_source(
                TruncateFacts {
                    has_buffer,
                    has_stored_content,
                    cache_matches_size,
                },
                known_size,
                new_size,
            )
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
    pub(super) fn setattr(
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
    pub(super) fn release(
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
    pub(super) fn commit_overwrite(&mut self, ino: u64, buf: Vec<u8>) -> Result<(), Errno> {
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
    pub(super) fn commit_new_file(&mut self, ino: u64, buf: Vec<u8>, pending: PendingCreate) -> Result<(), Errno> {
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
    pub(super) fn take_replaced_child(&mut self, parent: u64, name: &str) -> Result<Option<String>, Errno> {
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
    pub(super) fn take_release_upload(&mut self, ino: u64) -> (ReleaseUpload, Vec<u8>, Option<PendingCreate>) {
        let Some(buf) = self.write_buffers.remove(&ino) else {
            return (ReleaseUpload::Nothing, Vec::new(), None);
        };
        let pending = self.pending_creates.remove(&ino);
        let dirty = self.dirty_inodes.remove(&ino);
        let action = release_upload(ReleaseFacts {
            has_buffer: true,
            pending_create: pending.is_some(),
            dirty,
        });
        (action, buf, pending)
    }

    // ─── Directory write operations ───────────────────────────────────────

    pub(super) fn mkdir(
        &mut self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name_str = match name.to_str() {
            Some(n) => n.to_string(),
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };
        match self.create_folder_entry(parent.0, name_str) {
            Ok((ino, _resume_tag)) => {
                let attr = self.inodes[&ino].file_attr(ino);
                reply.entry(&ATTR_TTL, &attr, Generation(0));
            }
            Err((errno, message)) => {
                eprintln!("[mount] mkdir: {message}");
                reply.error(errno);
            }
        }
    }

    /// The non-reply half of [`Self::mkdir`], split out so the create path can
    /// be driven in tests without a kernel reply channel. Returns the new inode
    /// and the resume tag core minted with the id. A folder has no upload
    /// session to persist the tag, but returning it is what lets the mkdir test
    /// prove the id came from core's mint and not a local random number.
    pub(super) fn create_folder_entry(
        &mut self,
        parent: u64,
        name_str: String,
    ) -> Result<(u64, [u8; 32]), (Errno, String)> {
        // N4: core mints the owner id (carrying the resume tag) and core seals
        // the folder name, the same new-item pair push uses. The old path took
        // a bare local v4 and sealed raw metadata under the derived key, a
        // shape core's name reader refuses.
        let minted =
            crate::item_id::mint_owner(&self.master_key).map_err(|e| (Errno::EIO, format!("mkdir mint: {e}")))?;
        let resume_tag = *minted.tag();
        let folder_uuid = minted.id();
        let name_enc = minted
            .seal_name(
                &self.master_key,
                &name_str,
                None,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            )
            .map_err(|e| (Errno::EIO, format!("mkdir encrypt: {e}")))?;

        let parent_file_id: Option<uuid::Uuid> = self
            .inodes
            .get(&parent)
            .and_then(|e| e.file_id.as_deref())
            .and_then(|s| s.parse().ok());

        let resp = self
            .rt
            .block_on(self.api.create_folder(&name_enc, parent_file_id, Some(folder_uuid)))
            .map_err(|e| (Errno::EIO, format!("mkdir API: {e}")))?;

        // L15 = A (P2-11): a folder created in a shared folder stays owner-only.
        self.rt.block_on(crate::owned_keys::announce_owner_only(
            &self.api,
            &self.master_key,
            parent_file_id.map(|p| p.to_string()).as_deref(),
        ));

        // L13 (SF-0001): only the id this client minted; a different echo
        // aborts before the folder is listed or used as a parent.
        let server_id =
            super::super::folder_create_reply(&resp, folder_uuid).map_err(|e| (Errno::EIO, format!("mkdir: {e}")))?;

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

        Ok((ino, resume_tag))
    }

    /// Soft-delete (trash) a file.
    pub(super) fn unlink(&mut self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
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
    pub(super) fn rmdir(&mut self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
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
    pub(super) fn check_stored_row(&self, ino: u64, what: &str) -> Result<(), Errno> {
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
    pub(super) fn rename(
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
    pub(super) fn rename_stored(
        &mut self,
        ino: u64,
        file_id: &str,
        _key_uuid: Option<uuid::Uuid>,
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

        // L12/A19: a rename that takes the row out of a shared root adopts its
        // foreign subtree first, confirmed by a readback. An ancestry the server
        // will not answer for refuses the rename (fail closed).
        let row = match self.rt.block_on(self.api.get_file(file_id)) {
            Ok(row) => row,
            Err(e) => {
                eprintln!("[mount] rename: read row {file_id}: {e}");
                return Err(Errno::EIO);
            }
        };
        let mut resolver = crate::owned_keys::OwnedKeyResolver::new();
        if let Err(e) = self.rt.block_on(resolver.adopt_before_move(
            &self.api,
            &self.master_key,
            file_id,
            row["parent_id"].as_str(),
            self.inodes.get(&ino).is_some_and(|e| e.is_dir),
        )) {
            eprintln!("[mount] rename refused for {file_id}: {e}");
            return Err(Errno::EIO);
        }

        // N6: resolve the row exactly as push and every listing do. A foreign
        // row opens through its L8 owner envelope or the child wrap of its
        // proven custody root; the old call passed neither wrap, so every
        // foreign item was refused with EPERM no matter how well it opened.
        // L13: the new name is then sealed through core, under the key and
        // chunk format the row itself authenticated — never a bare derive.
        let new_name_enc: Option<String> = if name_changed {
            let is_dir = self.inodes.get(&ino).is_some_and(|e| e.is_dir);
            let resolved = self
                .rt
                .block_on(resolver.resolve(&self.api, &self.master_key, &row))
                .map_err(|e| {
                    eprintln!("[mount] rename refused for {file_id}: {e}");
                    Errno::EPERM
                })?;
            let target = resolved.write_target.clone().ok_or_else(|| {
                eprintln!("[mount] rename refused for {file_id}: the row is read-only");
                Errno::EPERM
            })?;
            let mime = if is_dir {
                None
            } else {
                beebeeb_core::media::guess_mime_type(newname)
            };
            let sealed =
                beebeeb_core::encrypt::encrypt_name_for(target.key(), newname, mime, target.pin()).map_err(|e| {
                    eprintln!("[mount] rename encrypt: {e}");
                    Errno::EIO
                })?;
            Some(sealed)
        } else {
            None
        };

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

        // L15 = A (P2-11): a rename into a shared folder stays owner-only.
        let dest = new_parent_file_id
            .flatten()
            .map(|p| p.to_string())
            .or_else(|| self.inodes.get(&newparent).and_then(|e| e.file_id.clone()));
        self.rt.block_on(crate::owned_keys::announce_owner_only(
            &self.api,
            &self.master_key,
            dest.as_deref(),
        ));

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
