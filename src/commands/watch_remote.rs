//! Remote-event consumer for `bb watch`.
//!
//! Subscribes to the server's SSE stream at `/api/v1/sync/stream` and applies
//! incoming file events to a local mirror directory. Companion to the local
//! filesystem watcher in `commands/watch.rs`, which handles the local→remote
//! direction. Together they keep a folder in sync with the vault.
//!
//! ## Event handling
//!
//! Two frame shapes flow over the channel:
//!
//! - `SyncEvent` (typed): `{"type":"file.uploaded","data":{...},"timestamp":...}`
//!   — fired by upload/share/notification handlers in the server. The five we
//!   care about:
//!   - `file.uploaded`, `file.created` → download + decrypt + write
//!   - `file.deleted`, `file.trashed` → unlink locally
//!   - `file.restored` → re-download
//!
//! - Raw sync_op frames: `{"seq_id":N,"op_type":"...","payload":{...},...}`
//!   — fired by `record_sync_op` for client-submitted CRDT ops. Carries
//!   `seq_id` for gapless reconnection. Most op_types we currently ignore
//!   (rename/move handled by the server emitting a typed event later);
//!   we use these primarily to track the latest seen `seq_id` for catch-up.
//!
//! ## Reconnect strategy
//!
//! On any stream error: exponential backoff 1s → 2 → 4 → 8 → 16 → 30s cap.
//! Before re-opening SSE, fetch `/api/v1/sync/ops?since={last_seq_id}` to
//! replay any ops that landed during the outage. This keeps watch durable
//! across server restarts and network blips.
//!
//! ## Scope
//!
//! Watch is invoked with an optional `--parent` UUID. Remote events whose
//! `parent_id` doesn't match are ignored — we don't recurse into descendant
//! folders. Day-1 limitation; sufficient for the "sync this folder both
//! ways" use case.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use beebeeb_types::EncryptedBlob;
use colored::Colorize;
use futures_util::StreamExt;
use serde_json::Value;
use uuid::Uuid;

use crate::api::ApiClient;
use crate::config::load_config;

/// Cap on the reconnect backoff window.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Run the remote-event consumer until the parent task drops `cancel`.
///
/// `watched_parent` matches the `--parent` arg from `bb watch`. `None` means
/// the vault root; events outside this folder are ignored. `mirror_root`
/// is the local directory the user invoked `bb watch` on — files arrive
/// there with their decrypted names.
pub async fn run(
    api: ApiClient,
    watched_parent: Option<Uuid>,
    mirror_root: PathBuf,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    // Master key is needed to derive per-file keys for decryption.
    let master_key = load_master_key()?;

    let mut last_seq_id: i64 = 0;
    let mut backoff = Duration::from_secs(1);

    loop {
        // 1. Catch up via the polling endpoint. Cheap if no ops landed.
        if last_seq_id > 0 {
            if let Ok(ops_resp) = api.list_ops_since(last_seq_id).await {
                if let Some(ops) = ops_resp.get("ops").and_then(Value::as_array) {
                    for op in ops {
                        if let Some(seq) = op.get("seq_id").and_then(Value::as_i64) {
                            last_seq_id = last_seq_id.max(seq);
                        }
                        // Raw sync_ops mostly cover folder/rename/move; we
                        // don't act on them in this v1, but we still update
                        // `last_seq_id` so reconnects don't replay forever.
                    }
                }
            }
        }

        // 2. Mint a stream token + open SSE.
        let stream_token = match api.create_stream_token().await {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "  {} {}",
                    "warn:".custom_color(crate::colors::AMBER),
                    format!("could not mint stream token: {e}")
                        .custom_color(crate::colors::INK_DIM),
                );
                if wait_or_cancel(&mut cancel, backoff).await.is_break() {
                    return Ok(());
                }
                backoff = next_backoff(backoff);
                continue;
            }
        };

        let resp = match api.open_sync_stream(&stream_token).await {
            Ok(r) => {
                backoff = Duration::from_secs(1); // reset on successful connect
                r
            }
            Err(e) => {
                eprintln!(
                    "  {} {}",
                    "warn:".custom_color(crate::colors::AMBER),
                    format!("sse connect failed: {e}").custom_color(crate::colors::INK_DIM),
                );
                if wait_or_cancel(&mut cancel, backoff).await.is_break() {
                    return Ok(());
                }
                backoff = next_backoff(backoff);
                continue;
            }
        };

        // 3. Read frames until the stream ends or the user cancels.
        let mut byte_stream = resp.bytes_stream();
        let mut buf = String::new();

        loop {
            tokio::select! {
                biased;

                // User pressed Ctrl+C — propagated by the parent watch task.
                _ = &mut cancel => return Ok(()),

                next = byte_stream.next() => {
                    match next {
                        Some(Ok(chunk)) => {
                            // SSE is text/event-stream; safe to assume UTF-8.
                            buf.push_str(&String::from_utf8_lossy(&chunk));
                            // Frames are separated by a blank line.
                            while let Some(idx) = buf.find("\n\n") {
                                let frame: String = buf.drain(..=idx + 1).collect();
                                if let Some(payload) = extract_data(&frame) {
                                    handle_event(
                                        &api,
                                        &master_key,
                                        watched_parent,
                                        &mirror_root,
                                        &payload,
                                        &mut last_seq_id,
                                    )
                                    .await;
                                }
                            }
                        }
                        Some(Err(e)) => {
                            eprintln!(
                                "  {} {}",
                                "warn:".custom_color(crate::colors::AMBER),
                                format!("sse stream error: {e} — reconnecting")
                                    .custom_color(crate::colors::INK_DIM),
                            );
                            break;
                        }
                        None => {
                            // Server closed the stream — try to reconnect.
                            break;
                        }
                    }
                }
            }
        }

        if wait_or_cancel(&mut cancel, backoff).await.is_break() {
            return Ok(());
        }
        backoff = next_backoff(backoff);
    }
}

/// Pull the JSON payload out of a single SSE frame. Frames may have multiple
/// fields (id:, event:, data:) but we only consume `data:` because the server
/// puts the entire JSON event there. Frames without a `data:` line — like
/// the `: keepalive` heartbeat — return `None`.
fn extract_data(frame: &str) -> Option<String> {
    // Concatenate every `data:` line in the frame (SSE spec allows multiline
    // data; the server currently writes single-line data: but be lenient).
    let mut out = String::new();
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(rest.trim_start());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Dispatch a single decoded SSE payload.
async fn handle_event(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    watched_parent: Option<Uuid>,
    mirror_root: &Path,
    payload: &str,
    last_seq_id: &mut i64,
) {
    let value: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return, // not JSON — skip silently
    };

    // Track seq_id from raw sync_op frames so we can catch up after a drop.
    if let Some(seq) = value.get("seq_id").and_then(Value::as_i64) {
        *last_seq_id = (*last_seq_id).max(seq);
    }

    // Two shapes:
    //   - typed SyncEvent:  { "type": "file.uploaded", "data": {...}, "timestamp": ... }
    //   - raw sync_op:      { "seq_id": N, "op_type": "...", "payload": {...}, ... }
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| value.get("op_type").and_then(Value::as_str));
    let Some(kind) = kind else {
        return;
    };

    let data = value
        .get("data")
        .or_else(|| value.get("payload"))
        .cloned()
        .unwrap_or(Value::Null);

    let file_id = data.get("id").and_then(Value::as_str);
    let parent_id_in_event = data
        .get("parent_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let is_folder = data
        .get("is_folder")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Filter: only act on events for files in the watched folder.
    // `None` parent in the event means root; matches when watched_parent is None.
    if parent_id_in_event != watched_parent {
        return;
    }

    match kind {
        "file.uploaded" | "file.created" | "file.restored" => {
            if is_folder {
                return; // folder creation handled by file/dir mirroring on demand
            }
            if let Some(id) = file_id {
                if crate::loopback::was_recently_uploaded(id) {
                    return; // echo of our own push — skip
                }
                if let Err(e) = download_to_mirror(api, master_key, id, mirror_root).await {
                    eprintln!(
                        "  {} {} {}",
                        "fail".custom_color(crate::colors::RED_ERR),
                        id.custom_color(crate::colors::INK),
                        e.custom_color(crate::colors::INK_DIM),
                    );
                }
            }
        }
        "file.deleted" | "file.trashed" => {
            if let Some(id) = file_id {
                if crate::loopback::was_recently_uploaded(id) {
                    return;
                }
                if let Err(e) = unlink_in_mirror(api, master_key, id, mirror_root).await {
                    eprintln!(
                        "  {} {} {}",
                        "fail".custom_color(crate::colors::RED_ERR),
                        id.custom_color(crate::colors::INK),
                        e.custom_color(crate::colors::INK_DIM),
                    );
                }
            }
        }
        // Renames, moves, share events, etc. — ignored in v1.
        _ => {}
    }
}

/// Download + decrypt + write a single file into the mirror directory,
/// using the encrypted name from the file's metadata.
async fn download_to_mirror(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id: &str,
    mirror_root: &Path,
) -> Result<(), String> {
    let file_uuid: Uuid = file_id
        .parse()
        .map_err(|e| format!("invalid id: {e}"))?;
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

    let meta = api.get_file(file_id).await?;
    let chunk_count = meta
        .get("chunk_count")
        .and_then(Value::as_i64)
        .unwrap_or(1) as u32;
    let name_encrypted = meta
        .get("name_encrypted")
        .and_then(Value::as_str)
        .ok_or("server did not return name_encrypted")?;

    let blob: EncryptedBlob = serde_json::from_str(name_encrypted)
        .map_err(|e| format!("invalid name_encrypted: {e}"))?;
    let name = beebeeb_core::encrypt::decrypt_metadata(&file_key, &blob)
        .map_err(|e| format!("could not decrypt name: {e}"))?;

    let safe_name = sanitize_filename(&name);
    let out_path = mirror_root.join(&safe_name);

    // Skip clobbering if local content already matches: same byte length on
    // disk usually means we already have this file from a prior push or
    // pull. Defensive — protects against any echo not caught by loopback.
    let plaintext = decrypt_file(api, &file_key, file_id, chunk_count).await?;
    if let Ok(existing) = std::fs::metadata(&out_path) {
        if existing.len() == plaintext.len() as u64 {
            return Ok(()); // already in sync
        }
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create dir: {e}"))?;
    }
    std::fs::write(&out_path, &plaintext)
        .map_err(|e| format!("write: {e}"))?;

    println!(
        "  {} {}",
        "↓ synced:".custom_color(crate::colors::GREEN_OK),
        safe_name.custom_color(crate::colors::INK),
    );
    Ok(())
}

/// Remove the local file corresponding to a deleted/trashed remote file.
/// Best-effort — silently no-ops if the local copy is already gone.
async fn unlink_in_mirror(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id: &str,
    mirror_root: &Path,
) -> Result<(), String> {
    let file_uuid: Uuid = file_id
        .parse()
        .map_err(|e| format!("invalid id: {e}"))?;
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_uuid.as_bytes());

    // Resolve the encrypted name → plaintext to know what to delete locally.
    // After a hard delete the metadata is gone; treat that as "we already
    // can't recover the name" and silently skip.
    let meta = match api.get_file(file_id).await {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let name_encrypted = match meta.get("name_encrypted").and_then(Value::as_str) {
        Some(n) => n,
        None => return Ok(()),
    };
    let blob: EncryptedBlob = match serde_json::from_str(name_encrypted) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    let name = match beebeeb_core::encrypt::decrypt_metadata(&file_key, &blob) {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };

    let safe_name = sanitize_filename(&name);
    let target = mirror_root.join(&safe_name);
    if target.is_file() {
        std::fs::remove_file(&target).map_err(|e| format!("remove: {e}"))?;
        println!(
            "  {} {}",
            "↓ removed:".custom_color(crate::colors::INK_DIM),
            safe_name.custom_color(crate::colors::INK),
        );
    }
    Ok(())
}

/// Pull encrypted bytes for `file_id` and decrypt all chunks.
/// Logic mirrors `commands::pull::pull_single_file`.
async fn decrypt_file(
    api: &ApiClient,
    file_key: &beebeeb_core::kdf::FileKey,
    file_id: &str,
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    let encrypted = api.download_file(file_id).await?;
    let mut plaintext = Vec::with_capacity(encrypted.len());
    let mut offset = 0;
    for i in 0..chunk_count {
        if offset >= encrypted.len() {
            return Err(format!("unexpected end of data at chunk {i}"));
        }
        let mut de = serde_json::Deserializer::from_slice(&encrypted[offset..])
            .into_iter::<EncryptedBlob>();
        let blob = match de.next() {
            Some(Ok(b)) => b,
            Some(Err(e)) => return Err(format!("parse chunk {i}: {e}")),
            None => return Err(format!("no data for chunk {i}")),
        };
        offset += de.byte_offset();
        let decrypted = beebeeb_core::encrypt::decrypt_chunk(file_key, &blob)
            .map_err(|e| format!("decrypt chunk {i}: {e}"))?;
        plaintext.extend_from_slice(&decrypted);
    }
    Ok(plaintext)
}

fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
    let cfg = load_config();
    let mk_b64 = cfg
        .master_key
        .ok_or("No master key found. Run `bb login` first.")?;
    let mk = base64::engine::general_purpose::STANDARD
        .decode(&mk_b64)
        .map_err(|e| format!("invalid master key in config: {e}"))?;
    if mk.len() != 32 {
        return Err(format!("master key must be 32 bytes, got {}", mk.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&mk);
    Ok(beebeeb_core::kdf::MasterKey::from_bytes(arr))
}

/// Strip path separators from a decrypted filename to keep the mirror flat
/// and resistant to traversal attempts. The server is supposed to validate
/// names but defense-in-depth is cheap.
fn sanitize_filename(name: &str) -> String {
    name.replace(['/', '\\'], "_")
}

/// Compute the next backoff delay (capped at [`MAX_BACKOFF`]).
fn next_backoff(current: Duration) -> Duration {
    let doubled = current.saturating_mul(2);
    if doubled > MAX_BACKOFF {
        MAX_BACKOFF
    } else {
        doubled
    }
}

/// Sleep for `delay`, but bail early if `cancel` resolves. Returns
/// `ControlFlow::Break` if cancelled, `Continue` to keep going.
async fn wait_or_cancel(
    cancel: &mut tokio::sync::oneshot::Receiver<()>,
    delay: Duration,
) -> std::ops::ControlFlow<()> {
    tokio::select! {
        biased;
        _ = cancel => std::ops::ControlFlow::Break(()),
        _ = tokio::time::sleep(delay) => std::ops::ControlFlow::Continue(()),
    }
}
