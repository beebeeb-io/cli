//! Owner-only, truncate, rename and replace tests for the FUSE module.
//!
//! Split out of `commands::mount` (file-size ratchet): the FUSE implementation
//! lives here, one file per concern; the facade keeps the stub, `rename_parent_target`
//! and the cross-crate test mods.


// ── Owner-only tests ──────────────────────────────────────────────────────
//
// The mount is the decrypted vault, so no other local account may read it:
// the session ACL must not be `SessionACL::All` (fuser's `allow_other`) and
// every attribute the filesystem reports must be owner-only. These tests
// call the real builders; nothing is mounted and no API is touched.

#[cfg(test)]
mod owner_only_tests {
    use super::super::*;

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
    use super::super::*;

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
    use super::super::*;

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
    use super::super::*;
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
