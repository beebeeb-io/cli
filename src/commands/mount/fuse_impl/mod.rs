//! Full implementation of `bb mount` (fuse feature enabled).
//!
//! The module owns the types (`BeebeebFs`, `BeebeebFsState`, the inode table) and
//! the `Filesystem` trait impl; the state operations live in `state` and
//! `state_ops`, and the owner-only/truncate/rename/replace tests in `tests`.

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

// ── Submodules ──────────────────────────────────────────────────────────────

mod state;
mod state_ops;

#[cfg(test)]
mod tests;

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
