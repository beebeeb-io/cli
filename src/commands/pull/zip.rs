//! `bb pull --zip`: assemble a downloaded folder into a zip on disk without
//! holding the archive in memory, and keep hostile entry names from escaping
//! the destination.
//!
//! Owns: `run_zip`, the `ZipSink` writer, `ZipWalk`/`TempArchive`, entry-name
//! sanitising (`zip_root_name`, `check_archive_path`, `unique_name`) and the
//! nesting cap.
//! Must not: fetch folder listings (see `pull::folder`).
//! Tests: `tests` at the bottom of this file.

use colored::Colorize;
use std::path::PathBuf;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

use super::{guard_existing_output, report, resolve_request_key, warn_skipped};
use crate::transfer_stats::FileEnd;
use crate::transfer_view::LiveProgress;
use crate::transfer_view::units::duration;
use crate::upload::{ChunkProgress, FileProgress, InFlight, Route, Stream};

/// How deep `--zip` follows folders. A real vault is a few levels deep; a
/// server that nests folders without end (or lists a folder inside itself) must
/// not make the walk recurse until the process dies.
pub(super) const MAX_ZIP_DEPTH: usize = 64;

/// The top-level folder name is both the archive prefix and the default output
/// name, so it needs the same single-safe-component check as every descendant
/// (task 1733 round 2): `..` would make a child the zip entry `../child`.
/// Names that are only unsafe on Windows are rewritten (see
/// [`archive_component`] in `safe_path`).
pub(super) fn zip_root_name(name: &str) -> Result<String, String> {
    crate::safe_path::archive_component(name)
        .map_err(|e| format!("folder name {name:?} is not safe to use as an archive name ({e}); refusing to zip it"))
}

/// Resolve the argument as a folder and download all its files into a zip archive.
pub(super) async fn run_zip(
    api: &ApiClient,
    path_arg: &str,
    output: Option<PathBuf>,
    force: bool,
    verbose: bool,
) -> Result<(), String> {
    let master_key = load_master_key()?;

    let resolved = crate::path::resolve_path(api, &master_key, path_arg).await?;

    if !resolved.is_folder {
        return Err(format!(
            "'{}' is a file, not a folder. Use `bb pull` without --zip.",
            resolved.name,
        ));
    }

    let folder_id = resolved.file_id.ok_or("cannot zip the vault root")?;
    let folder_name = zip_root_name(&resolved.name)?;

    // Determine the output path up front so an existing archive is refused
    // before any blob is fetched.
    let zip_filename = format!("{}.zip", folder_name);
    let out_path = output.unwrap_or_else(|| PathBuf::from(&zip_filename));
    guard_existing_output(&out_path, force)?;

    // Recursively collect all files in the folder tree. One walker for the whole
    // tree: file-request uploads are sealed to a per-file content key, not the
    // master key (task 1760), and its resolver fetches `GET /file-requests` once
    // and unwraps each R_priv once.
    let mut walk = ZipWalk {
        api,
        master_key: &master_key,
        request_keys: None,
        keys: crate::owned_keys::OwnedKeyResolver::new(),
        skipped: 0,
        ancestors: Vec::new(),
        items: Vec::new(),
    };
    walk.collect(&folder_id, &folder_name).await?;
    let ZipWalk { items, mut skipped, .. } = walk;

    if items.is_empty() {
        return Err(format!("folder '{}' is empty — nothing to zip.", folder_name));
    }

    let total_size: u64 = items.iter().map(|e| e.entry.size_bytes).sum();

    // The run's sink (task 2333): every item planned up front, so the bar, the percent and the ETA hold.
    let progress = report::start(verbose);
    let plans: Vec<report::FilePlan> = items
        .iter()
        .map(|item| report::FilePlan::of_size(item.entry.size_bytes, item.entry.chunk_count))
        .collect();
    for plan in &plans {
        progress.plan_file(plan.bytes, plan.chunks);
    }
    progress.seal_plan();

    // Summary line
    if crate::ui::is_rich() {
        progress.note(
            &format!(
                "  {} {} ({} {} {})",
                "\u{2193}".custom_color(crate::colors::GREEN_OK),
                format!("Downloading {}", folder_name).custom_color(crate::colors::INK),
                report::files(items.len()).custom_color(crate::colors::INK_DIM),
                "\u{00b7}".custom_color(crate::colors::INK_DIM),
                crate::ui::human_size(total_size).custom_color(crate::colors::INK_DIM),
            ),
            Stream::Stdout,
        );
    }

    // The archive is written to a sibling temp file and renamed over `out_path`
    // only when it is complete: a failed run (or `--force` over a good archive)
    // never leaves a truncated zip behind. Dropping the guard removes the temp.
    let mut temp = TempArchive::create(&out_path)?;
    let file = temp.take_file();

    // The archive is written here, not by `beebeeb_core::zip::stream_folder_zip`:
    // that function derives every entry's key from the master key and the file
    // id, so it cannot open a file-request upload (task 1760). Follow-up: let
    // core take a per-entry key, then this can call it again.
    // The live lines show the transfer (task 2333); adding to the archive is local and draws nothing of its own.
    let on_progress = |_: &beebeeb_core::zip::ZipProgress| {};
    let mut sink = ZipSink::new(file, total_size, items.len());

    // One file at a time: fetch, decrypt, write, drop. Peak memory is the
    // largest single file, not the whole folder.
    let mut dl_elapsed = std::time::Duration::ZERO;
    let mut zip_elapsed = std::time::Duration::ZERO;
    for (i, item) in items.iter().enumerate() {
        progress.next_file(plans[i].bytes, plans[i].chunks);
        if let Some(key) = &item.explicit_key {
            let staging_dest = out_path.with_file_name(format!(".bb-entry-{}", uuid::Uuid::new_v4()));
            let mut staged = TempArchive::create(&staging_dest)?;
            drop(staged.take_file());
            let result = crate::download::stream_download_decrypt_with_file_key(
                api,
                key.clone(),
                &item.entry.file_id,
                item.entry.chunk_format,
                item.entry.chunk_count,
                &staged.path,
                &progress,
                &item.entry.path,
                None,
            )
            .await;
            if let Err(e) = result {
                progress.end_file(FileEnd::Failed);
                warn_skipped(&format!("{}: {e}", item.entry.path));
                skipped += 1;
                continue;
            }
            let mut reader = std::fs::File::open(&staged.path).map_err(|e| e.to_string())?;
            sink.add_reader(i, &item.entry.path, &mut reader, item.entry.size_bytes, &on_progress)
                .map_err(|_| "zip write failed")?;
            continue;
        }
        // The body arrives whole and counts once it decrypted (spec §3.1, §4.3).
        progress.set_limit(1);
        let file = begin_member(&progress, item, &plans[i]);
        let flight = InFlight::start(file.as_ref(), 0, Route::Api);
        let t = std::time::Instant::now();
        let (blob, chunk_size) = match crate::download::download_buffered(api, &item.entry.file_id).await {
            Ok(got) => got,
            Err(e) => {
                drop(flight);
                // The file closes as failed: the lines left above the error do not show it as active (spec §6.7).
                file.finish(false);
                return Err(e.into());
            }
        };
        dl_elapsed += t.elapsed();

        let t = std::time::Instant::now();
        let plaintext = match decrypt_zip_item(&master_key, item, &blob, chunk_size) {
            Ok(p) => p,
            Err(e) => {
                drop(flight);
                file.finish(false);
                progress.end_file(FileEnd::Failed);
                warn_skipped(&format!("{:?}: {e}", item.entry.path));
                skipped += 1;
                continue;
            }
        };
        file.body_done(plans[i].chunks, blob.len() as u64, Route::Api);
        drop(flight);
        file.finish(true);
        drop(blob);
        if let Err(e) = sink.add(i, &item.entry.path, &plaintext, &on_progress) {
            // An entry that is not a safe archive path never reaches the writer;
            // anything else is an I/O failure on the archive itself.
            match e {
                ZipAddError::UnsafePath(msg) => {
                    warn_skipped(&format!("{:?}: {msg}", item.entry.path));
                    skipped += 1;
                }
                ZipAddError::Write(msg) => return Err(format!("zip failed: {msg}")),
            }
        }
        zip_elapsed += t.elapsed();
    }

    // The transfer is over: the live lines go before the end lines print.
    let transfer = progress.summary();

    let written = sink.written;
    if written == 0 {
        // Nothing made it into the archive: leave `out_path` exactly as it was.
        return Err(crate::exit::with_code(
            crate::exit::INCOMPLETE,
            format!("no file could be added to the archive ({skipped} item(s) left out, see the warnings above)"),
        ));
    }
    sink.finish().map_err(|e| format!("zip failed: {e}"))?;
    temp.commit(&out_path, force)?;

    let total_elapsed = dl_elapsed + zip_elapsed;

    let zip_size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

    if crate::ui::is_rich() {
        println!(
            "  {} {}  {}  {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            out_path.display().to_string().custom_color(crate::colors::INK),
            crate::ui::human_size(zip_size).custom_color(crate::colors::INK_DIM),
            duration(total_elapsed.as_millis() as u64),
        );
        println!(
            "    {}",
            format!(
                "{} \u{00b7} {} download \u{00b7} {} zip",
                report::files(written),
                duration(dl_elapsed.as_millis() as u64),
                duration(zip_elapsed.as_millis() as u64),
            )
            .custom_color(crate::colors::INK_DIM),
        );
        report::print_detail(&transfer, verbose);
    } else if crate::ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "folder": folder_name,
                "file_count": written,
                "total_size_bytes": total_size,
                "zip_size_bytes": zip_size,
                "download_ms": dl_elapsed.as_millis() as u64,
                "zip_ms": zip_elapsed.as_millis() as u64,
                "output": out_path.display().to_string(),
                "transfer": transfer.to_json(),
            }))
            .unwrap()
        );
    } else {
        // Quiet mode
        println!("{}", out_path.display());
    }

    if skipped > 0 {
        return Err(crate::exit::with_code(
            crate::exit::INCOMPLETE,
            format!(
                "{skipped} item(s) were left out of the archive (unsafe names, folder loops, or files that could not be decrypted; see the warnings above)"
            ),
        ));
    }

    Ok(())
}

/// Decrypt one archive member. The result is wiped when dropped. It is decrypted
/// whole, not chunk by chunk into the archive: the decrypt path falls back
/// between the two on-wire formats, and a fallback after bytes were already
/// written into the archive would corrupt it.
pub(super) fn decrypt_zip_item(
    master_key: &beebeeb_core::kdf::MasterKey,
    item: &ZipItem,
    blob: &[u8],
    chunk_size: Option<u64>,
) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
    let e = &item.entry;
    let plaintext = match &item.content_key {
        // File-request upload: the per-file content key opens it.
        Some(c) => crate::crypto::try_decrypt_all_chunks_with_chunk_size(
            c,
            &e.file_id,
            e.chunk_format,
            blob,
            e.chunk_count,
            chunk_size,
        ),
        // Everything else goes through the master-key path every other `bb pull` uses.
        None => crate::crypto::decrypt_file_chunks_with_chunk_size(
            master_key,
            &e.file_id,
            e.chunk_format,
            blob,
            e.chunk_count,
            chunk_size,
        ),
    }
    .map_err(|err| format!("could not be decrypted ({err})"))?;
    Ok(zeroize::Zeroizing::new(plaintext))
}

/// The member's file in the run's sink (spec §3.2). Only a file-request upload, which `decrypt_zip_item` opens with its
/// per-file key, is counted without a live line: its body arrives whole and a line would jump from 0 to 100 %. Every
/// other member is drawn like any pull.
fn begin_member(progress: &LiveProgress, item: &ZipItem, plan: &report::FilePlan) -> Box<dyn FileProgress> {
    if item.content_key.is_some() {
        progress.begin_buffered(&item.entry.path, plan.bytes, plan.chunks)
    } else {
        progress.begin_file_chunks(&item.entry.path, plan.bytes, plan.chunks, 0)
    }
}

/// One archive member: the core `ZipEntry` plus, for a file-request upload,
/// the per-file content key that opens it (`None` = master-key-derived).
pub(super) struct ZipItem {
    pub(super) entry: beebeeb_core::zip::ZipEntry,
    pub(super) content_key: Option<beebeeb_core::kdf::FileKey>,
    pub(super) explicit_key: Option<std::sync::Arc<beebeeb_core::kdf::FileKey>>,
}

pub(super) enum ZipAddError {
    /// The entry path is not a safe archive path; nothing was written.
    UnsafePath(String),
    /// Writing to the archive failed.
    Write(String),
}

/// Archive writer: deflates plaintext into entries and reports progress in
/// PLAINTEXT bytes (the unit `bytes_total` is summed in).
pub(super) struct ZipSink<W: std::io::Write + std::io::Seek> {
    pub(super) archive: zip::write::ZipWriter<W>,
    pub(super) bytes_done: u64,
    pub(super) bytes_total: u64,
    pub(super) file_count: usize,
    /// Entries actually written.
    pub(super) written: usize,
}

impl<W: std::io::Write + std::io::Seek> ZipSink<W> {
    pub(super) fn new(writer: W, bytes_total: u64, file_count: usize) -> Self {
        Self {
            archive: zip::write::ZipWriter::new(writer),
            bytes_done: 0,
            bytes_total,
            file_count,
            written: 0,
        }
    }

    pub(super) fn add(
        &mut self,
        file_index: usize,
        path: &str,
        plaintext: &[u8],
        on_progress: &dyn Fn(&beebeeb_core::zip::ZipProgress),
    ) -> Result<(), ZipAddError> {
        self.add_reader(
            file_index,
            path,
            &mut std::io::Cursor::new(plaintext),
            plaintext.len() as u64,
            on_progress,
        )
    }
    pub(super) fn add_reader(
        &mut self,
        file_index: usize,
        path: &str,
        reader: &mut dyn std::io::Read,
        size: u64,
        on_progress: &dyn Fn(&beebeeb_core::zip::ZipProgress),
    ) -> Result<(), ZipAddError> {
        // Defence in depth: the walk already vetted every component, but the
        // writer is what puts the path into the archive, so it checks again.
        check_archive_path(path).map_err(ZipAddError::UnsafePath)?;
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6))
            .large_file(size >= u32::MAX as u64);
        self.archive
            .start_file(path, options)
            .map_err(|e| ZipAddError::Write(e.to_string()))?;
        std::io::copy(reader, &mut self.archive).map_err(|e| ZipAddError::Write(e.to_string()))?;
        self.written += 1;
        self.bytes_done += size;
        on_progress(&beebeeb_core::zip::ZipProgress {
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            file_index,
            file_count: self.file_count,
            current_file: path.to_string(),
        });
        Ok(())
    }

    pub(super) fn finish(self) -> Result<W, String> {
        self.archive.finish().map_err(|e| e.to_string())
    }
}

/// Every `/`-separated component of an archive path must already be a safe
/// archive name (unchanged by [`archive_component`] in `safe_path`).
pub(super) fn check_archive_path(path: &str) -> Result<(), String> {
    for seg in path.split('/') {
        match crate::safe_path::archive_component(seg) {
            Ok(ok) if ok == seg => {}
            Ok(_) => return Err(format!("component {seg:?} is not a safe archive name")),
            Err(e) => return Err(format!("unsafe archive path ({e})")),
        }
    }
    Ok(())
}

/// The archive under construction: a sibling temp file that is removed on drop
/// unless [`TempArchive::commit`] renamed it into place.
pub(super) struct TempArchive {
    pub(super) path: PathBuf,
    pub(super) file: Option<std::fs::File>,
    pub(super) done: bool,
}

impl TempArchive {
    /// Create `.<name>.<pid>.partial` next to `dest` (same directory, so the
    /// final rename cannot cross a filesystem). `create_new`: never reuses or
    /// writes through something that is already there.
    pub(super) fn create(dest: &std::path::Path) -> Result<Self, String> {
        let dir = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("archive.zip");
        let path = dir.join(format!(".{name}.{}.partial", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("failed to create {}: {e}", path.display()))?;
        Ok(Self {
            path,
            file: Some(file),
            done: false,
        })
    }

    pub(super) fn take_file(&mut self) -> std::fs::File {
        self.file.take().expect("temp archive file already taken")
    }

    /// Move the finished archive to `dest`. Without `--force` an archive that
    /// appeared at `dest` while this run was working is not replaced.
    pub(super) fn commit(&mut self, dest: &std::path::Path, force: bool) -> Result<(), String> {
        if !force && std::fs::symlink_metadata(dest).is_ok() {
            return Err(format!(
                "{} appeared while the archive was being written; not replacing it (pass --force to overwrite)",
                dest.display()
            ));
        }
        std::fs::rename(&self.path, dest).map_err(|e| format!("failed to write {}: {e}", dest.display()))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Walks a folder tree and builds the flat list of archive members, with paths
/// relative to the top-level folder name (e.g. "Music/Old/track.flac").
pub(super) struct ZipWalk<'a> {
    pub(super) api: &'a ApiClient,
    pub(super) master_key: &'a beebeeb_core::kdf::MasterKey,
    pub(super) request_keys: Option<crate::commands::request::RequestKeyResolver>,
    pub(super) keys: crate::owned_keys::OwnedKeyResolver,
    /// Items left out (unsafe name, folder loop, too deep).
    pub(super) skipped: usize,
    /// Folder ids from the root down to the folder being listed.
    pub(super) ancestors: Vec<String>,
    pub(super) items: Vec<ZipItem>,
}

impl ZipWalk<'_> {
    pub(super) async fn collect(&mut self, folder_id: &str, prefix: &str) -> Result<(), String> {
        self.ancestors.push(folder_id.to_string());
        let r = self.collect_level(folder_id, prefix).await;
        self.ancestors.pop();
        r
    }

    pub(super) async fn collect_level(&mut self, folder_id: &str, prefix: &str) -> Result<(), String> {
        let listing = self.api.list_files(Some(folder_id)).await?;
        let files = listing
            .get("files")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Names already taken in THIS directory, lowercased: macOS and Windows
        // extract case-insensitively, and an uploader controls the names.
        let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();

        self.keys.seed(&files);
        for item in &files {
            let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name_enc = item.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");
            let is_folder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
            let size_bytes = item.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
            let chunk_count = item.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;

            // Request-uploaded files decrypt their name (and later their chunks)
            // with the per-file content key, like the non-zip folder pull.
            let content_key = resolve_request_key(self.api, self.master_key, item, &mut self.request_keys).await;
            let resolved = match self.keys.resolve(self.api, self.master_key, item).await {
                Ok(r) => r,
                Err(e) => {
                    warn_skipped(&e);
                    self.skipped += 1;
                    continue;
                }
            };
            let decrypted_name = resolved.name;
            let explicit_key = if !crate::owned_keys::is_derived_origin(resolved.origin) && content_key.is_none() {
                Some(resolved.key.clone())
            } else {
                None
            };

            // Zip entry paths are `prefix/name`: a server-provided name with `..`
            // or a separator would be a zip-slip entry for whoever extracts the
            // archive (task 1733), so only single safe components are accepted.
            // Names that are merely unsafe on Windows are rewritten, not dropped.
            let safe_name = match crate::safe_path::archive_component(&decrypted_name) {
                Ok(n) => n,
                Err(e) => {
                    warn_skipped(&format!("{decrypted_name:?} in {prefix}: unsafe name ({e})"));
                    self.skipped += 1;
                    continue;
                }
            };

            if is_folder && (self.ancestors.iter().any(|a| a == item_id) || self.ancestors.len() >= MAX_ZIP_DEPTH) {
                warn_skipped(&format!(
                    "folder {decrypted_name:?} in {prefix}: it contains itself or nests more than {MAX_ZIP_DEPTH} levels deep"
                ));
                self.skipped += 1;
                continue;
            }

            let unique = unique_name(&mut used, &safe_name);
            if unique != decrypted_name {
                eprintln!(
                    "  {} {decrypted_name:?} in {prefix} is stored as {unique:?} (the name is not safe or unique on every system)",
                    "!".custom_color(crate::colors::AMBER)
                );
            }
            let path = format!("{prefix}/{unique}");

            if is_folder {
                // Recurse into subfolders
                Box::pin(self.collect(item_id, &path)).await?;
            } else {
                // Read pin from the file's authenticated metadata (task 1943).
                let pin_key = resolved.name_key.as_deref().unwrap_or(&resolved.key);
                let chunk_format = match crate::crypto::content_pin_with_key(pin_key, item_id, name_enc) {
                    Ok(pin) => pin,
                    Err(e) => {
                        warn_skipped(&format!("{path:?}: {e}"));
                        self.skipped += 1;
                        continue;
                    }
                };
                self.items.push(ZipItem {
                    entry: beebeeb_core::zip::ZipEntry {
                        path,
                        file_id: item_id.to_string(),
                        chunk_count,
                        size_bytes,
                        chunk_format,
                    },
                    content_key,
                    explicit_key,
                });
            }
        }

        Ok(())
    }
}

/// `name`, or `stem (2).ext`, `stem (3).ext`, ... if its lowercase form is
/// already in `used`. The chosen name is recorded in `used`.
pub(super) fn unique_name(used: &mut std::collections::HashSet<String>, name: &str) -> String {
    let mut candidate = name.to_string();
    let mut n = 1u32;
    while used.contains(&candidate.to_lowercase()) {
        n += 1;
        candidate = match name.rfind('.') {
            // A leading dot (".bashrc") is part of the stem, not an extension.
            Some(i) if i > 0 && i + 1 < name.len() => format!("{} ({n}){}", &name[..i], &name[i..]),
            _ => format!("{name} ({n})"),
        };
    }
    used.insert(candidate.to_lowercase());
    candidate
}
#[cfg(test)]
mod tests {
    use super::{ZipItem, ZipSink, begin_member, unique_name, zip_root_name};
    use crate::commands::pull::report::FilePlan;
    use crate::transfer_stats::Direction;
    use crate::transfer_view::LiveProgress;
    use crate::transfer_view::sink::Level;
    use crate::upload::Route;
    use beebeeb_core::chunk_stream::ExpectedChunkFormat;
    use beebeeb_core::kdf::FileKey;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn member(path: &str, content_key: Option<FileKey>) -> ZipItem {
        ZipItem {
            entry: beebeeb_core::zip::ZipEntry {
                path: path.to_string(),
                file_id: "11111111-1111-4111-8111-111111111111".to_string(),
                chunk_count: 1,
                size_bytes: 1000,
                chunk_format: ExpectedChunkFormat::V2,
            },
            content_key,
            explicit_key: None,
        }
    }

    /// Review P2 (#143), spec §3.2 / §6.2: `bb pull --zip` is live. Only a file-request upload (its body arrives whole,
    /// so a line would jump from 0 to 100 %) is counted without a line; the account's own file is drawn like any pull.
    #[test]
    fn an_owner_member_is_drawn_and_only_a_request_member_is_buffered() {
        let (progress, _record) =
            LiveProgress::for_test(Direction::Download, Level::Default, 80, Arc::new(AtomicU64::new(0)));
        let owner = member("Folder/owner.bin", None);
        let request = member("Folder/request.bin", Some(FileKey::from_bytes([9; 32])));
        let plans = [FilePlan::of_size(1000, 1), FilePlan::of_size(1000, 1)];
        for plan in &plans {
            progress.plan_file(plan.bytes, plan.chunks);
        }
        progress.seal_plan();
        let owner_file = begin_member(&progress, &owner, &plans[0]);
        let request_file = begin_member(&progress, &request, &plans[1]);
        let lines = progress.render_now();
        assert!(
            lines.iter().any(|l| l.contains("owner.bin")),
            "the account's own file is drawn: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("request.bin")),
            "a file-request upload is never drawn: {lines:?}"
        );
        owner_file.body_done(1, 1028, Route::Api);
        owner_file.finish(true);
        request_file.body_done(1, 1028, Route::Api);
        request_file.finish(true);
        let end = progress.summary();
        assert_eq!(
            (end.files_done, end.chunks_done),
            (2, 2),
            "both members are counted: {end:?}"
        );
    }

    #[test]
    fn zip_root_name_rejects_hostile_names() {
        for bad in ["../evil", "/abs", "a/b", "..", ".", "", "C:\\escape", "a\\b", "x\0y"] {
            assert!(zip_root_name(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn zip_root_name_accepts_plain_names() {
        assert_eq!(zip_root_name("Photos 2026").unwrap(), "Photos 2026");
    }

    #[test]
    fn zip_root_name_rewrites_windows_unsafe_names() {
        assert_eq!(zip_root_name("CON").unwrap(), "_CON");
        assert_eq!(zip_root_name("Meeting: notes?").unwrap(), "Meeting_ notes_");
    }

    #[test]
    fn unique_name_suffixes_collisions_case_insensitively() {
        let mut used = HashSet::new();
        assert_eq!(unique_name(&mut used, "scan.pdf"), "scan.pdf");
        assert_eq!(unique_name(&mut used, "scan.pdf"), "scan (2).pdf");
        assert_eq!(unique_name(&mut used, "scan.pdf"), "scan (3).pdf");
        assert_eq!(unique_name(&mut used, "SCAN.PDF"), "SCAN (4).PDF");
        assert_eq!(unique_name(&mut used, "A.txt"), "A.txt");
        assert_eq!(unique_name(&mut used, "a.txt"), "a (2).txt");
        assert_eq!(unique_name(&mut used, "Makefile"), "Makefile");
        assert_eq!(unique_name(&mut used, "makefile"), "makefile (2)");
        assert_eq!(unique_name(&mut used, ".env"), ".env");
        assert_eq!(unique_name(&mut used, ".ENV"), ".ENV (2)");
        // A generated name can itself collide with a real one that comes later.
        assert_eq!(unique_name(&mut used, "scan (2).pdf"), "scan (2) (2).pdf");
    }

    /// Progress is counted in PLAINTEXT bytes, the unit `bytes_total` is summed
    /// in; counting the (larger) ciphertext pushed the percentage past 100.
    #[test]
    fn zip_progress_counts_plaintext_bytes() {
        let seen = std::cell::RefCell::new(Vec::new());
        let on_progress = |p: &beebeeb_core::zip::ZipProgress| seen.borrow_mut().push((p.bytes_done, p.bytes_total));
        let mut sink = ZipSink::new(std::io::Cursor::new(Vec::new()), 30, 2);
        assert!(sink.add(0, "d/a.txt", &[1u8; 10], &on_progress).is_ok());
        assert!(sink.add(1, "d/b.txt", &[2u8; 20], &on_progress).is_ok());
        assert_eq!(*seen.borrow(), vec![(10, 30), (30, 30)]);
        assert_eq!(sink.written, 2);
    }

    #[test]
    fn zip_sink_refuses_unsafe_entry_paths() {
        let on_progress = |_: &beebeeb_core::zip::ZipProgress| {};
        let mut sink = ZipSink::new(std::io::Cursor::new(Vec::new()), 0, 1);
        for bad in ["../x", "/abs", "d/../x", "d/CON", "d/a:b", "d/trail.", "d//x", ""] {
            assert!(sink.add(0, bad, b"x", &on_progress).is_err(), "{bad:?} must be refused");
        }
        assert_eq!(sink.written, 0);
        assert!(sink.add(0, "d/ok.txt", b"x", &on_progress).is_ok());
    }
}
