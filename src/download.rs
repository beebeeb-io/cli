//! Streaming, constant-memory download + decrypt — the read-side counterpart to
//! `src/upload.rs`.
//!
//! `GET /api/v1/files/{id}/download` returns the chunks concatenated back to
//! back (`nonce(12) || ciphertext || tag(16)` per chunk) with `X-Chunk-Count`
//! + `X-Original-Size` headers but **no** per-chunk-size header. The old path
//! buffered the entire payload (`resp.bytes().await`) before decrypting — peak
//! RAM ≈ file size. This module reads the body incrementally with
//! `Response::chunk()`, reassembles one logical frame at a time, decrypts it
//! through the shared [`ChunkDecryptor`], writes the plaintext, and drops it.
//!
//! ## Legacy compatibility (user-data regression risk — preserved carefully)
//!
//! Only the **current raw format with the string-UUID key** streams. Two legacy
//! shapes fall back to the buffered [`crate::crypto::decrypt_file_chunks`],
//! which is unchanged:
//!
//! - **JSON-blob chunks** (first byte `{`, old CLI uploads) — detected by
//!   peeking the first byte, then buffered + decrypted as today.
//! - **binary-UUID key derivation** (pre-`bb repair` legacy files) — the
//!   [`ChunkDecryptor`] only derives the string-UUID key, so if the first frame
//!   fails to decrypt we buffer the whole payload and hand it to the buffered
//!   path, which retries with the binary-UUID key. `bb repair` semantics intact.
//!
//! Framing (`total / chunk_count` for the first N-1 frames, remainder for the
//! last) matches the existing buffered splitter exactly, so there is no
//! behavioural change for any file — only the memory profile improves.

use std::io::Write;
use std::path::Path;

use beebeeb_core::chunk_stream::ChunkDecryptor;
use beebeeb_core::kdf::MasterKey;

use crate::api::ApiClient;
use crate::upload::ChunkProgress;

/// Per-chunk AEAD overhead: `nonce(12) + tag(16)`.
const CHUNK_OVERHEAD: u64 = 28;

/// Outcome of a streaming download.
pub struct DownloadStats {
    pub plaintext_bytes: u64,
    pub encrypted_bytes: u64,
}

/// Stream-download `file_id`, decrypt to `out_path` with constant memory, and
/// report progress. Falls back to the buffered legacy decrypt for JSON-blob /
/// binary-UUID files (see module docs).
pub async fn stream_download_decrypt(
    api: &ApiClient,
    master_key: &MasterKey,
    file_id: &str,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
) -> Result<DownloadStats, String> {
    let count = chunk_count.max(1);

    let mut resp = api.download_stream(file_id).await?;
    let content_len = resp.content_length();
    let original_size = header_u64(&resp, "X-Original-Size");

    // Best estimate of the total encrypted length, used only to size the first
    // N-1 frames; the last frame always drains the remainder, so a small
    // mis-estimate self-corrects (and a wrong size fails the GCM tag → error).
    let total: u64 = content_len
        .or_else(|| original_size.map(|os| os + CHUNK_OVERHEAD * count as u64))
        .unwrap_or(0);

    // Peek the first byte to detect the JSON-blob legacy format.
    let mut carry: Vec<u8> = Vec::new();
    while carry.is_empty() {
        match resp.chunk().await.map_err(|e| format!("download read: {e}"))? {
            Some(b) => carry.extend_from_slice(&b),
            None => break,
        }
    }
    if carry.is_empty() {
        return Err("download returned an empty body".to_string());
    }

    let prog = progress.begin_file(display_name, total);

    let result = if carry[0] == b'{' {
        // ── Legacy JSON-blob format → buffered decrypt (unchanged path) ──
        drain_rest(&mut resp, &mut carry).await?;
        let stats = buffered_fallback(master_key, file_id, &carry, count, out_path, prog.as_ref())?;
        Ok(stats)
    } else {
        stream_raw(
            &mut resp,
            master_key,
            file_id,
            count,
            total,
            out_path,
            carry,
            prog.as_ref(),
        )
        .await
    };

    match &result {
        Ok(_) => prog.finish(true),
        Err(_) => prog.finish(false),
    }
    result
}

/// Stream the modern raw format. On a first-frame key failure (binary-UUID
/// legacy), buffer the whole payload and hand it to the buffered path.
#[allow(clippy::too_many_arguments)]
async fn stream_raw(
    resp: &mut reqwest::Response,
    master_key: &MasterKey,
    file_id: &str,
    count: u32,
    total: u64,
    out_path: &Path,
    mut carry: Vec<u8>,
    prog: &dyn crate::upload::FileProgress,
) -> Result<DownloadStats, String> {
    let n = count as usize;
    // Frame size for the first n-1 frames; the last frame takes the remainder.
    // Matches crypto::decrypt_raw_chunks (total / count). For a single chunk the
    // whole payload is the frame.
    let frame_size = if n <= 1 {
        usize::MAX // sentinel: the single frame is "everything"
    } else {
        (total / count as u64) as usize
    };

    let mut writer = AtomicFile::create(out_path)?;
    let mut decryptor = ChunkDecryptor::for_push(master_key, file_id);
    let mut plaintext_bytes: u64 = 0;
    let mut encrypted_bytes: u64 = 0;

    for i in 0..n {
        let is_last = i == n - 1;
        let frame: Vec<u8> = if is_last || frame_size == usize::MAX {
            drain_rest(resp, &mut carry).await?;
            std::mem::take(&mut carry)
        } else {
            fill_to(resp, &mut carry, frame_size).await?;
            if carry.len() < frame_size {
                return Err(format!(
                    "download truncated: chunk {i}/{n} wanted {frame_size} bytes, got {}",
                    carry.len()
                ));
            }
            carry.drain(..frame_size).collect()
        };

        match decryptor.push_frame(&frame) {
            Ok(dec) => {
                writer.write_all(&dec.data)?;
                plaintext_bytes += dec.data.len() as u64;
                encrypted_bytes += frame.len() as u64;
                prog.chunk_confirmed(frame.len() as u64);
            }
            Err(_) if i == 0 => {
                // First frame failed with the string-UUID key → likely a
                // pre-`bb repair` binary-UUID legacy file. Reassemble the full
                // payload and let the buffered path retry with the binary key.
                writer.abort();
                let mut full = frame; // the first frame we already read
                full.extend_from_slice(&carry);
                drain_rest(resp, &mut carry).await?;
                full.extend_from_slice(&carry);
                return buffered_fallback(master_key, file_id, &full, count, out_path, prog);
            }
            Err(e) => {
                writer.abort();
                return Err(format!("decrypt chunk {i}: {e}"));
            }
        }
    }

    writer.commit()?;
    Ok(DownloadStats {
        plaintext_bytes,
        encrypted_bytes,
    })
}

/// Buffered legacy decrypt via the unchanged `crypto::decrypt_file_chunks`
/// (handles JSON-blob + binary/string dual-key). Writes atomically.
fn buffered_fallback(
    master_key: &MasterKey,
    file_id: &str,
    encrypted: &[u8],
    count: u32,
    out_path: &Path,
    prog: &dyn crate::upload::FileProgress,
) -> Result<DownloadStats, String> {
    let plaintext = crate::crypto::decrypt_file_chunks(master_key, file_id, encrypted, count)?;
    let mut writer = AtomicFile::create(out_path)?;
    writer.write_all(&plaintext)?;
    writer.commit()?;
    prog.chunk_confirmed(encrypted.len() as u64);
    Ok(DownloadStats {
        plaintext_bytes: plaintext.len() as u64,
        encrypted_bytes: encrypted.len() as u64,
    })
}

/// Read more body chunks until `buf.len() >= want` or EOF.
async fn fill_to(resp: &mut reqwest::Response, buf: &mut Vec<u8>, want: usize) -> Result<(), String> {
    while buf.len() < want {
        match resp.chunk().await.map_err(|e| format!("download read: {e}"))? {
            Some(b) => buf.extend_from_slice(&b),
            None => break,
        }
    }
    Ok(())
}

/// Drain the rest of the response body into `buf`.
async fn drain_rest(resp: &mut reqwest::Response, buf: &mut Vec<u8>) -> Result<(), String> {
    while let Some(b) = resp.chunk().await.map_err(|e| format!("download read: {e}"))? {
        buf.extend_from_slice(&b);
    }
    Ok(())
}

fn header_u64(resp: &reqwest::Response, name: &str) -> Option<u64> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

/// Write-to-`.tmp`-then-atomic-rename helper so an interrupted/failed download
/// never leaves a corrupt file at `out_path`.
struct AtomicFile {
    writer: Option<std::io::BufWriter<std::fs::File>>,
    tmp: std::path::PathBuf,
    final_path: std::path::PathBuf,
    committed: bool,
}

impl AtomicFile {
    fn create(out_path: &Path) -> Result<Self, String> {
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
        }
        let tmp = out_path.with_extension(format!(
            "{}bbtmp",
            out_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("{e}."))
                .unwrap_or_default()
        ));
        let file = std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        Ok(Self {
            writer: Some(std::io::BufWriter::new(file)),
            tmp,
            final_path: out_path.to_path_buf(),
            committed: false,
        })
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        self.writer
            .as_mut()
            .expect("writer present until commit/abort")
            .write_all(data)
            .map_err(|e| format!("write {}: {e}", self.final_path.display()))
    }

    fn commit(mut self) -> Result<(), String> {
        let mut w = self.writer.take().expect("writer present");
        w.flush().map_err(|e| format!("flush: {e}"))?;
        drop(w);
        std::fs::rename(&self.tmp, &self.final_path)
            .map_err(|e| format!("rename {}: {e}", self.final_path.display()))?;
        self.committed = true;
        Ok(())
    }

    /// Explicitly abort before falling back to another writer for the same path.
    fn abort(&mut self) {
        self.writer.take();
        let _ = std::fs::remove_file(&self.tmp);
        self.committed = true; // suppress Drop cleanup (already handled)
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if !self.committed {
            self.writer.take();
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}
