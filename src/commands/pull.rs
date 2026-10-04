use colored::Colorize;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;

pub async fn run(file_id: String, output: Option<PathBuf>, zip: bool, force: bool) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    // --zip mode: resolve as folder and download as a zip archive
    if zip {
        return run_zip(&api, &file_id, output, force).await;
    }

    let (file_id, resolved_name) = resolve_file_arg(&api, &file_id).await?;

    // If no --output was given and we resolved a path, use the resolved name.
    // That name still goes through the same safety check as any server-provided
    // name (task 1733): an explicit `-o` is the only unchecked path.
    let output = match (output, resolved_name.as_deref()) {
        (Some(o), _) => Some(o),
        (None, Some(n)) => Some(crate::safe_path::default_output_path(n)?),
        (None, None) => None,
    };

    // Step 1: Get file metadata to learn chunk count and encrypted name
    let file_meta = api.get_file(&file_id).await?;

    let chunk_count = file_meta.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;

    let name_encrypted_str = file_meta
        .get("name_encrypted")
        .and_then(|v| v.as_str())
        .ok_or("server response missing name_encrypted")?;

    // Load master key
    let master_key = load_master_key()?;

    // File-request upload? Recover its content key C via the request-key path;
    // it decrypts both the name and the chunks (normal files use the
    // master-derived path below).
    let request_content_key: Option<beebeeb_core::kdf::FileKey> =
        if crate::commands::request::is_request_upload(&file_meta) {
            crate::commands::request::RequestKeyResolver::load(&api)
                .await
                .ok()
                .and_then(|mut rk| rk.content_key(&master_key, &file_meta))
        } else {
            None
        };

    // Try to decrypt the filename for display and default output path.
    // Uses the shared crypto module which handles all server formats
    // (Rust EncryptedBlob, web-app base64 blob, plaintext) and both
    // UUID key derivations (binary and string).
    let decrypted_name = match &request_content_key {
        Some(c) => crate::crypto::decrypt_name_with_key(c, name_encrypted_str),
        None => crate::crypto::decrypt_name(&master_key, &file_id, name_encrypted_str),
    };

    let is_folder = file_meta.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);

    if is_folder {
        let folder_name = decrypted_name.as_deref().unwrap_or(&file_id);
        let out_dir = match output {
            Some(o) => o,
            None => crate::safe_path::default_output_path(folder_name)?,
        };
        return pull_folder(&api, &file_id, &out_dir, force).await;
    }

    let display_name = decrypted_name.as_deref().unwrap_or(&file_id);

    // Output path (needed up front for the streaming write).
    // The default (no `-o`) comes from a server-provided name, so it must be a
    // single safe component; `-o` is the user's own choice and is taken as-is.
    let out_path = match output {
        Some(o) => o,
        None => crate::safe_path::default_output_path(decrypted_name.as_deref().unwrap_or(&file_id))?,
    };

    // Never clobber a local file silently (flow "CLI end to end", issue 5):
    // checked before any bytes are downloaded, for both decrypt paths below.
    guard_existing_output(&out_path, force)?;

    // File-request uploads are sealed under a per-file content key C, not the
    // master-derived key the streaming path assumes. Decrypt them via the
    // buffered request-key path (download whole blob → decrypt with C → write).
    if let Some(c) = &request_content_key {
        let dl_start = std::time::Instant::now();
        let encrypted_bytes = api.download_file(&file_id).await?;
        let dl_elapsed = dl_start.elapsed();

        let dec_start = std::time::Instant::now();
        let plaintext = crate::crypto::try_decrypt_all_chunks(c, &encrypted_bytes, chunk_count)?;
        let dec_elapsed = dec_start.elapsed();

        std::fs::write(&out_path, &plaintext).map_err(|e| format!("failed to write file: {e}"))?;

        let dl_speed = if dl_elapsed.as_secs_f64() > 0.0 {
            encrypted_bytes.len() as f64 / dl_elapsed.as_secs_f64()
        } else {
            0.0
        };

        if crate::ui::is_json() {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "id": file_id,
                    "name": display_name,
                    "size_bytes": plaintext.len(),
                    "download_speed_bps": dl_speed as u64,
                    "download_ms": dl_elapsed.as_millis() as u64,
                    "decrypt_ms": dec_elapsed.as_millis() as u64,
                    "output": out_path.display().to_string(),
                }))
                .unwrap()
            );
        } else if crate::ui::is_quiet() {
            println!("{}", out_path.display());
        } else {
            println!(
                "  {} {}  {}  {}",
                "\u{2713}".custom_color(crate::colors::GREEN_OK),
                display_name.custom_color(crate::colors::INK),
                crate::ui::human_size(plaintext.len() as u64).custom_color(crate::colors::INK_DIM),
                crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK)
            );
            println!(
                "    {}",
                format!(
                    "{}ms download \u{00b7} {}ms decrypt",
                    dl_elapsed.as_millis(),
                    dec_elapsed.as_millis()
                )
                .custom_color(crate::colors::INK_DIM)
            );
        }
        return Ok(());
    }

    // Estimate the encrypted size for the progress bar length.
    let size_bytes = file_meta.get("size_bytes").and_then(|v| v.as_i64()).unwrap_or(0) as u64;
    let est_encrypted = size_bytes + 28 * chunk_count as u64;

    // Stream download + decrypt with constant memory + a live progress bar.
    let show_bar = crate::ui::is_rich() && std::io::stdout().is_terminal();
    let progress: Box<dyn crate::upload::ChunkProgress> = if show_bar {
        Box::new(crate::upload::BarProgress::new(1, est_encrypted))
    } else {
        Box::new(crate::upload::NoopProgress)
    };

    let start = std::time::Instant::now();
    let stats = crate::download::stream_download_decrypt(
        &api,
        &master_key,
        &file_id,
        chunk_count,
        &out_path,
        progress.as_ref(),
        display_name,
    )
    .await?;
    progress.finish_all();
    let elapsed = start.elapsed();

    let dl_speed = if elapsed.as_secs_f64() > 0.0 {
        stats.encrypted_bytes as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    if crate::ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": file_id,
                "name": display_name,
                "size_bytes": stats.plaintext_bytes,
                "download_speed_bps": dl_speed as u64,
                "elapsed_ms": elapsed.as_millis() as u64,
                "output": out_path.display().to_string(),
            }))
            .unwrap()
        );
        return Ok(());
    }

    if crate::ui::is_quiet() {
        println!("{}", out_path.display());
        return Ok(());
    }

    // Rich mode (default)
    println!(
        "  {} {}  {}  {}",
        "\u{2713}".custom_color(crate::colors::GREEN_OK),
        display_name.custom_color(crate::colors::INK),
        crate::ui::human_size(stats.plaintext_bytes).custom_color(crate::colors::INK_DIM),
        crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK)
    );
    println!(
        "    {}",
        format!("{}ms · streamed", elapsed.as_millis()).custom_color(crate::colors::INK_DIM)
    );

    Ok(())
}

/// Resolve a user-supplied file reference to `(full_uuid, resolved_name)`.
///
/// Accepts, in order: a full UUID (used as-is, name `None`), a hex short-ID
/// prefix such as the 8-char IDs `bb ls` prints (falls through to path
/// resolution when no file matches), or a plaintext vault path like
/// `folder1/a.bin`. Folders and the vault root are rejected. Shared by
/// `bb pull` and `bb share` so both accept exactly the same references.
pub(crate) async fn resolve_file_arg(api: &ApiClient, arg: &str) -> Result<(String, Option<String>), String> {
    if uuid::Uuid::parse_str(arg).is_ok() {
        // Already a full UUID — use it directly.
        return Ok((arg.to_string(), None));
    }
    if looks_like_id_prefix(arg) {
        // Looks like a hex prefix (e.g. "3e15382b" from `bb ls` output).
        if let Some(full_id) = api.find_file_by_id_prefix(arg).await? {
            return Ok((full_id, None));
        }
        // No match by prefix — fall through to path resolution.
    }
    resolve_as_path(api, arg).await
}

/// Refuse to overwrite an existing local path unless the user said so.
///
/// Before this guard `bb pull note.txt` replaced a local `note.txt` with the
/// remote content — exit 0, no prompt, no backup — so unsaved local edits were
/// lost. Now:
/// - nothing at `out_path` → proceed;
/// - a directory at `out_path` → always an error (a file cannot replace it);
/// - `--force` → proceed (the atomic `.tmp` + rename replaces the file);
/// - a rich terminal with a real tty on stdin → `y/N` prompt (default no);
/// - otherwise (`--json`, `--quiet`, piped/redirected stdin, scripts) → refuse
///   with a non-zero exit, naming `--force` and `-o <path>`. A piped `y` is not
///   consent (same rule as `bb passkey remove`, Codex PR #24).
fn guard_existing_output(out_path: &std::path::Path, force: bool) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(out_path) {
        Ok(m) => m,
        // Not there (or unreadable metadata): nothing to clobber here; any real
        // I/O problem surfaces when the file is written.
        Err(_) => return Ok(()),
    };
    let shown = out_path.display();
    if meta.is_dir() {
        return Err(format!(
            "{shown} is a directory — pass -o <path> to choose where to save the file"
        ));
    }
    if force {
        return Ok(());
    }
    if crate::ui::is_rich() && std::io::stdin().is_terminal() {
        if confirm_overwrite(out_path)? {
            return Ok(());
        }
        return Err(format!("{shown} left unchanged — nothing downloaded"));
    }
    Err(format!(
        "{shown} already exists — use --force to overwrite it, or -o <path> to save elsewhere"
    ))
}

/// Minimal interactive y/N confirmation (no extra deps; default no). Only
/// called on a rich terminal with a real tty on stdin.
fn confirm_overwrite(out_path: &std::path::Path) -> Result<bool, String> {
    use std::io::Write;

    print!(
        "  {} {} already exists. Overwrite? [y/N] ",
        "?".custom_color(crate::colors::AMBER),
        out_path.display().to_string().custom_color(crate::colors::INK)
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

/// Returns `true` if the string looks like a UUID prefix: 8-36 hex chars
/// (with optional hyphens). This covers the 8-char short IDs shown by
/// `bb ls` as well as longer partial UUIDs.
fn looks_like_id_prefix(s: &str) -> bool {
    let len = s.len();
    (8..=36).contains(&len)
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && s.chars().any(|c| c.is_ascii_hexdigit())
}

/// Resolve a plaintext vault path (e.g. "/Documents/report.pdf") into a
/// `(file_id, Some(name))` pair.
async fn resolve_as_path(api: &ApiClient, path: &str) -> Result<(String, Option<String>), String> {
    let master_key = load_master_key()?;
    let resolved = crate::path::resolve_path(api, &master_key, path).await?;

    if resolved.file_id.is_none() {
        return Err("cannot download the vault root".to_string());
    }
    if resolved.is_folder {
        return Err(format!(
            "'{}' is a folder. Use `bb ls` to list its contents.",
            resolved.name,
        ));
    }

    Ok((resolved.file_id.unwrap(), Some(resolved.name)))
}

async fn pull_folder(api: &ApiClient, folder_id: &str, out_dir: &std::path::Path, force: bool) -> Result<(), String> {
    // A single resolver, shared across the whole folder tree, so request-uploaded
    // files only trigger one `GET /file-requests` and each R_priv is unwrapped once.
    let mut request_keys: Option<crate::commands::request::RequestKeyResolver> = None;
    let skipped = pull_folder_inner(api, folder_id, out_dir, force, &mut request_keys).await?;
    if skipped > 0 {
        // Everything safe was pulled; the run is not a clean success, though.
        return Err(crate::exit::with_code(
            crate::exit::INCOMPLETE,
            format!("{skipped} item(s) were skipped (see the warnings above); the rest was pulled"),
        ));
    }
    Ok(())
}

/// Warn (stderr, always) that an item was skipped instead of written.
fn warn_skipped(what: &str) {
    eprintln!("  {} skipped {what}", "!".custom_color(crate::colors::AMBER));
}

/// Pull one folder level into `out_dir`. Returns how many items were skipped
/// (unsafe name, symlink, refused overwrite) across this level and below.
async fn pull_folder_inner(
    api: &ApiClient,
    folder_id: &str,
    out_dir: &std::path::Path,
    force: bool,
    request_keys: &mut Option<crate::commands::request::RequestKeyResolver>,
) -> Result<usize, String> {
    let master_key = load_master_key()?;

    std::fs::create_dir_all(out_dir).map_err(|e| format!("failed to create directory: {e}"))?;

    let listing = api.list_files(Some(folder_id)).await?;
    let files = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("invalid file listing response")?;

    println!(
        "  {} {} ({})",
        "pulling".custom_color(crate::colors::GREEN_OK),
        out_dir.display().to_string().custom_color(crate::colors::INK),
        format!("{} items", files.len()).custom_color(crate::colors::INK_DIM),
    );

    let mut skipped = 0usize;
    for item in files {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let is_subfolder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let name_enc = item.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");

        // Request-uploaded files decrypt their name via the request-key path.
        let content_key = resolve_request_key(api, &master_key, item, request_keys).await;
        let decrypted_name = content_key
            .as_ref()
            .and_then(|c| crate::crypto::decrypt_name_with_key(c, name_enc))
            .or_else(|| crate::crypto::decrypt_name(&master_key, item_id, name_enc))
            .unwrap_or_else(|| item_id.to_string());

        // The name is server-provided (for file-request uploads: chosen by an
        // anonymous stranger). It becomes exactly ONE path component under
        // `out_dir`, never a path, and the result must resolve inside `out_dir`
        // (task 1733). An unsafe item is skipped, not fatal: the rest still pulls.
        let target = match crate::safe_path::safe_child(out_dir, &decrypted_name, crate::safe_path::Rules::Portable) {
            Ok(p) => p,
            Err(e) => {
                warn_skipped(&format!("{decrypted_name:?} in {}: {e}", out_dir.display()));
                skipped += 1;
                continue;
            }
        };

        if is_subfolder {
            skipped += Box::pin(pull_folder_inner(api, item_id, &target, force, request_keys)).await?;
        } else {
            // A request upload's name is attacker-chosen, so it must not silently
            // replace a local file of the same name (same rule as a single-file
            // `bb pull`): refuse without --force.
            if content_key.is_some() {
                if let Err(e) = guard_existing_output(&target, force) {
                    warn_skipped(&format!("{}: {e}", target.display()));
                    skipped += 1;
                    continue;
                }
            }
            pull_single_file(api, item_id, &target, content_key).await?;
        }
    }

    Ok(skipped)
}

/// Resolve a row's request content key, lazily loading the shared resolver the
/// first time a request-uploaded file is seen in the tree.
async fn resolve_request_key(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file: &serde_json::Value,
    request_keys: &mut Option<crate::commands::request::RequestKeyResolver>,
) -> Option<beebeeb_core::kdf::FileKey> {
    if !crate::commands::request::is_request_upload(file) {
        return None;
    }
    if request_keys.is_none() {
        *request_keys = Some(match crate::commands::request::RequestKeyResolver::load(api).await {
            Ok(rk) => rk,
            Err(e) => {
                // Say so once; an empty resolver stops every later request
                // upload from retrying (and failing silently) on its own.
                eprintln!(
                    "  {} could not load your file-request keys ({e}); files received through a file request cannot be decrypted in this run",
                    "!".custom_color(crate::colors::AMBER)
                );
                crate::commands::request::RequestKeyResolver::empty()
            }
        });
    }
    request_keys.as_mut().and_then(|rk| rk.content_key(master_key, file))
}

async fn pull_single_file(
    api: &ApiClient,
    file_id: &str,
    out_path: &std::path::Path,
    request_content_key: Option<beebeeb_core::kdf::FileKey>,
) -> Result<(), String> {
    let master_key = load_master_key()?;

    let file_meta = api.get_file(file_id).await?;
    let chunk_count = file_meta.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;

    let name = out_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    // File-request uploads are sealed under a per-file content key C, which the
    // streaming path (master-derived key) can't decrypt — use the buffered
    // request-key path and return early.
    if let Some(c) = &request_content_key {
        let dl_start = std::time::Instant::now();
        let encrypted_bytes = api.download_file(file_id).await?;
        let dl_elapsed = dl_start.elapsed();
        let plaintext = crate::crypto::try_decrypt_all_chunks(c, &encrypted_bytes, chunk_count)?;

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("failed to create dir: {e}"))?;
        }
        std::fs::write(out_path, &plaintext).map_err(|e| format!("failed to write: {e}"))?;

        if crate::ui::is_quiet() {
            println!("{}", out_path.display());
        } else if crate::ui::is_rich() {
            let dl_speed = if dl_elapsed.as_secs_f64() > 0.0 {
                encrypted_bytes.len() as f64 / dl_elapsed.as_secs_f64()
            } else {
                0.0
            };
            println!(
                "    {} {}  {}  {}",
                "\u{2713}".custom_color(crate::colors::GREEN_OK),
                name.custom_color(crate::colors::INK),
                crate::ui::human_size(plaintext.len() as u64).custom_color(crate::colors::INK_DIM),
                crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK)
            );
            println!(
                "      {}",
                format!("{}ms · request", dl_elapsed.as_millis()).custom_color(crate::colors::INK_DIM)
            );
        }
        return Ok(());
    }

    // Streaming, constant-memory decrypt. Folder pulls print a per-file line, so
    // use a no-op progress sink (no nested bars).
    let start = std::time::Instant::now();
    let stats = crate::download::stream_download_decrypt(
        api,
        &master_key,
        file_id,
        chunk_count,
        out_path,
        &crate::upload::NoopProgress,
        &name,
    )
    .await?;
    let elapsed = start.elapsed();

    if crate::ui::is_quiet() {
        println!("{}", out_path.display());
    } else if crate::ui::is_rich() {
        let dl_speed = if elapsed.as_secs_f64() > 0.0 {
            stats.encrypted_bytes as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        println!(
            "    {} {}  {}  {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            name.custom_color(crate::colors::INK),
            crate::ui::human_size(stats.plaintext_bytes).custom_color(crate::colors::INK_DIM),
            crate::ui::human_speed(dl_speed).custom_color(crate::colors::GREEN_OK)
        );
        println!(
            "      {}",
            format!("{}ms · streamed", elapsed.as_millis()).custom_color(crate::colors::INK_DIM)
        );
    }
    // In JSON mode, the folder pull prints nothing per-file — the caller handles output.

    Ok(())
}

// ---------------------------------------------------------------------------
// --zip: download folder as a zip archive
// ---------------------------------------------------------------------------

/// How deep `--zip` follows folders. A real vault is a few levels deep; a
/// server that nests folders without end (or lists a folder inside itself) must
/// not make the walk recurse until the process dies.
const MAX_ZIP_DEPTH: usize = 64;

/// The top-level folder name is both the archive prefix and the default output
/// name, so it needs the same single-safe-component check as every descendant
/// (task 1733 round 2): `..` would make a child the zip entry `../child`.
/// Names that are only unsafe on Windows are rewritten (see
/// [`crate::safe_path::archive_component`]).
fn zip_root_name(name: &str) -> Result<String, String> {
    crate::safe_path::archive_component(name)
        .map_err(|e| format!("folder name {name:?} is not safe to use as an archive name ({e}); refusing to zip it"))
}

/// Resolve the argument as a folder and download all its files into a zip archive.
async fn run_zip(api: &ApiClient, path_arg: &str, output: Option<PathBuf>, force: bool) -> Result<(), String> {
    let master_key = load_master_key()?;

    // Resolve the path argument to a folder.
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

    // Summary line
    if crate::ui::is_rich() {
        println!(
            "  {} {} ({} {} {})",
            "\u{2193}".custom_color(crate::colors::GREEN_OK),
            format!("Downloading {}", folder_name).custom_color(crate::colors::INK),
            format!("{} files", items.len()).custom_color(crate::colors::INK_DIM),
            "\u{00b7}".custom_color(crate::colors::INK_DIM),
            crate::ui::human_size(total_size).custom_color(crate::colors::INK_DIM),
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
    let on_progress = |progress: &beebeeb_core::zip::ZipProgress| {
        if crate::ui::is_rich() {
            let pct = if progress.bytes_total > 0 {
                ((progress.bytes_done as f64 / progress.bytes_total as f64 * 100.0) as u64).min(100)
            } else {
                0
            };
            eprint!(
                "\r    {} {}%  {}/{}  {}",
                "zipping".custom_color(crate::colors::INK_DIM),
                pct.to_string().custom_color(crate::colors::AMBER),
                (progress.file_index + 1).to_string().custom_color(crate::colors::INK),
                progress.file_count.to_string().custom_color(crate::colors::INK_DIM),
                progress.current_file.custom_color(crate::colors::INK_DIM),
            );
        }
    };
    let mut sink = ZipSink::new(file, total_size, items.len());

    // One file at a time: fetch, decrypt, write, drop. Peak memory is the
    // largest single file, not the whole folder.
    let mut dl_elapsed = std::time::Duration::ZERO;
    let mut zip_elapsed = std::time::Duration::ZERO;
    for (i, item) in items.iter().enumerate() {
        if crate::ui::is_rich() {
            eprint!(
                "\r    {} {}/{}  {}",
                "fetching".custom_color(crate::colors::INK_DIM),
                (i + 1).to_string().custom_color(crate::colors::INK),
                items.len().to_string().custom_color(crate::colors::INK_DIM),
                item.entry.path.custom_color(crate::colors::INK_DIM),
            );
        }
        let t = std::time::Instant::now();
        let (blob, chunk_size) = crate::download::download_buffered(api, &item.entry.file_id).await?;
        dl_elapsed += t.elapsed();

        let t = std::time::Instant::now();
        let plaintext = match decrypt_zip_item(&master_key, item, &blob, chunk_size) {
            Ok(p) => p,
            Err(e) => {
                warn_skipped(&format!("{:?}: {e}", item.entry.path));
                skipped += 1;
                continue;
            }
        };
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

    if crate::ui::is_rich() {
        // Clear the fetching/zipping line
        eprint!("\r{}\r", " ".repeat(80));
    }

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

    // Get final file size
    let zip_size = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

    if crate::ui::is_rich() {
        println!(
            "  {} {}  {}  {:.1}s",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            out_path.display().to_string().custom_color(crate::colors::INK),
            crate::ui::human_size(zip_size).custom_color(crate::colors::INK_DIM),
            total_elapsed.as_secs_f64(),
        );
        println!(
            "    {}",
            format!(
                "{} files \u{00b7} {:.1}s download \u{00b7} {:.1}s zip",
                written,
                dl_elapsed.as_secs_f64(),
                zip_elapsed.as_secs_f64(),
            )
            .custom_color(crate::colors::INK_DIM),
        );
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
fn decrypt_zip_item(
    master_key: &beebeeb_core::kdf::MasterKey,
    item: &ZipItem,
    blob: &[u8],
    chunk_size: Option<u64>,
) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
    let e = &item.entry;
    let plaintext = match &item.content_key {
        // File-request upload: the per-file content key opens it.
        Some(c) => crate::crypto::try_decrypt_all_chunks_with_chunk_size(c, blob, e.chunk_count, chunk_size),
        // Everything else goes through the master-key path every other `bb pull` uses.
        None => {
            crate::crypto::decrypt_file_chunks_with_chunk_size(master_key, &e.file_id, blob, e.chunk_count, chunk_size)
        }
    }
    .map_err(|err| format!("could not be decrypted ({err})"))?;
    Ok(zeroize::Zeroizing::new(plaintext))
}

/// One archive member: the core `ZipEntry` plus, for a file-request upload,
/// the per-file content key that opens it (`None` = master-key-derived).
struct ZipItem {
    entry: beebeeb_core::zip::ZipEntry,
    content_key: Option<beebeeb_core::kdf::FileKey>,
}

enum ZipAddError {
    /// The entry path is not a safe archive path; nothing was written.
    UnsafePath(String),
    /// Writing to the archive failed.
    Write(String),
}

/// Archive writer: deflates plaintext into entries and reports progress in
/// PLAINTEXT bytes (the unit `bytes_total` is summed in).
struct ZipSink<W: std::io::Write + std::io::Seek> {
    archive: zip::write::ZipWriter<W>,
    bytes_done: u64,
    bytes_total: u64,
    file_count: usize,
    /// Entries actually written.
    written: usize,
}

impl<W: std::io::Write + std::io::Seek> ZipSink<W> {
    fn new(writer: W, bytes_total: u64, file_count: usize) -> Self {
        Self {
            archive: zip::write::ZipWriter::new(writer),
            bytes_done: 0,
            bytes_total,
            file_count,
            written: 0,
        }
    }

    fn add(
        &mut self,
        file_index: usize,
        path: &str,
        plaintext: &[u8],
        on_progress: &dyn Fn(&beebeeb_core::zip::ZipProgress),
    ) -> Result<(), ZipAddError> {
        use std::io::Write as _;
        // Defence in depth: the walk already vetted every component, but the
        // writer is what puts the path into the archive, so it checks again.
        check_archive_path(path).map_err(ZipAddError::UnsafePath)?;
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6))
            .large_file(plaintext.len() as u64 >= u32::MAX as u64);
        self.archive
            .start_file(path, options)
            .map_err(|e| ZipAddError::Write(e.to_string()))?;
        self.archive
            .write_all(plaintext)
            .map_err(|e| ZipAddError::Write(e.to_string()))?;
        self.written += 1;
        self.bytes_done += plaintext.len() as u64;
        on_progress(&beebeeb_core::zip::ZipProgress {
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            file_index,
            file_count: self.file_count,
            current_file: path.to_string(),
        });
        Ok(())
    }

    fn finish(self) -> Result<W, String> {
        self.archive.finish().map_err(|e| e.to_string())
    }
}

/// Every `/`-separated component of an archive path must already be a safe
/// archive name (unchanged by [`crate::safe_path::archive_component`]).
fn check_archive_path(path: &str) -> Result<(), String> {
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
struct TempArchive {
    path: PathBuf,
    file: Option<std::fs::File>,
    done: bool,
}

impl TempArchive {
    /// Create `.<name>.<pid>.partial` next to `dest` (same directory, so the
    /// final rename cannot cross a filesystem). `create_new`: never reuses or
    /// writes through something that is already there.
    fn create(dest: &std::path::Path) -> Result<Self, String> {
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

    fn take_file(&mut self) -> std::fs::File {
        self.file.take().expect("temp archive file already taken")
    }

    /// Move the finished archive to `dest`. Without `--force` an archive that
    /// appeared at `dest` while this run was working is not replaced.
    fn commit(&mut self, dest: &std::path::Path, force: bool) -> Result<(), String> {
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
struct ZipWalk<'a> {
    api: &'a ApiClient,
    master_key: &'a beebeeb_core::kdf::MasterKey,
    request_keys: Option<crate::commands::request::RequestKeyResolver>,
    /// Items left out (unsafe name, folder loop, too deep).
    skipped: usize,
    /// Folder ids from the root down to the folder being listed.
    ancestors: Vec<String>,
    items: Vec<ZipItem>,
}

impl ZipWalk<'_> {
    async fn collect(&mut self, folder_id: &str, prefix: &str) -> Result<(), String> {
        self.ancestors.push(folder_id.to_string());
        let r = self.collect_level(folder_id, prefix).await;
        self.ancestors.pop();
        r
    }

    async fn collect_level(&mut self, folder_id: &str, prefix: &str) -> Result<(), String> {
        let listing = self.api.list_files(Some(folder_id)).await?;
        let files = listing
            .get("files")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Names already taken in THIS directory, lowercased: macOS and Windows
        // extract case-insensitively, and an uploader controls the names.
        let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();

        for item in &files {
            let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name_enc = item.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");
            let is_folder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
            let size_bytes = item.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
            let chunk_count = item.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;

            // Request-uploaded files decrypt their name (and later their chunks)
            // with the per-file content key, like the non-zip folder pull.
            let content_key = resolve_request_key(self.api, self.master_key, item, &mut self.request_keys).await;
            let decrypted_name = content_key
                .as_ref()
                .and_then(|c| crate::crypto::decrypt_name_with_key(c, name_enc))
                .or_else(|| crate::crypto::decrypt_name(self.master_key, item_id, name_enc))
                .unwrap_or_else(|| item_id.to_string());

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
                self.items.push(ZipItem {
                    entry: beebeeb_core::zip::ZipEntry {
                        path,
                        file_id: item_id.to_string(),
                        chunk_count,
                        size_bytes,
                    },
                    content_key,
                });
            }
        }

        Ok(())
    }
}

/// `name`, or `stem (2).ext`, `stem (3).ext`, ... if its lowercase form is
/// already in `used`. The chosen name is recorded in `used`.
fn unique_name(used: &mut std::collections::HashSet<String>, name: &str) -> String {
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
    use super::{ZipSink, unique_name, zip_root_name};
    use std::collections::HashSet;

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
