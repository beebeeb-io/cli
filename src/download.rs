//! Streaming, constant-memory download + decrypt — the read-side counterpart to
//! `src/upload.rs`.
//!
//! `GET /api/v1/files/{id}/download` returns the chunks concatenated back to
//! back (`nonce(12) || ciphertext || tag(16)` per chunk) with `X-Chunk-Count`,
//! `X-Original-Size`, and (for V2 files) `X-Chunk-Size` headers. The old path
//! buffered the entire payload (`resp.bytes().await`) before decrypting — peak
//! RAM ≈ file size. This module reads the body incrementally with
//! `Response::chunk()`, reassembles one logical frame at a time, decrypts it
//! through the shared [`ChunkDecryptor`], writes the plaintext, and drops it.
//!
//! ## Legacy compatibility (user-data regression risk — preserved carefully)
//!
//! Only the **current raw format with the string-UUID key** streams. The
//! format is never guessed from the first byte (a random raw nonce starts with
//! `{` 1 time in 256, task 1759): raw streaming is always tried first, and the
//! frame-0 AEAD tag is the arbiter. If the first frame fails to authenticate,
//! the whole payload is buffered and handed to [`crate::crypto::decrypt_file_chunks_with_chunk_size`],
//! which covers both legacy shapes:
//!
//! - **JSON-blob chunks** (old CLI uploads) — buffered + decrypted as before.
//! - **binary-UUID key derivation** (pre-`bb repair` legacy files) — the
//!   [`ChunkDecryptor`] only derives the string-UUID key, so the buffered path
//!   retries with the binary-UUID key. `bb repair` semantics intact.
//!
//! Framing: the server's `X-Chunk-Size` header (the uniform plaintext chunk
//! size for the file's version) is authoritative — each wire frame is
//! `chunk_size + CHUNK_OVERHEAD` for the first N-1 chunks, remainder for the
//! last. Streaming uploads make every chunk but the last exactly that size, so
//! the old `total / chunk_count` average mis-sized frame 0 whenever the file
//! size was not an exact multiple of the chunk size (e.g. a 150 MiB file with
//! 8 MiB chunks). When the header is absent (legacy V1 responses) the streaming
//! guess `total / chunk_count` is only a first attempt; if it fails, the
//! buffered path recovers the real frame size by AEAD-validated probing.

use std::io::Write;
use std::path::Path;

use beebeeb_core::chunk_stream::{ChunkDecryptor, MAX_CHUNK_SIZE};
use beebeeb_core::kdf::MasterKey;

use crate::api::ApiClient;
use crate::upload::ChunkProgress;

/// Per-chunk AEAD overhead: `nonce(12) + tag(16)`.
const CHUNK_OVERHEAD: u64 = 28;

/// Hard ceiling (4 GiB) on the buffered-fallback payload. The fallback exists
/// for legacy JSON-blob / binary-UUID / unknown-frame-size files, which are
/// small by construction; a body past this is refused whatever the headers say.
const FALLBACK_HARD_CEILING: u64 = 4 * 1024 * 1024 * 1024;

/// Worst-case JSON expansion of a legacy `EncryptedBlob` chunk: `nonce` and
/// `ciphertext` are serialised as arrays of decimal numbers, so every byte costs
/// at most 4 bytes on the wire (`255,`). Verified against real serialisation by
/// the `json_bound_covers_serialised_blobs` test.
const JSON_BYTES_PER_BYTE: u64 = 4;

/// Worst-case per-chunk JSON envelope on top of the 4x-expanded
/// `nonce(12) + ciphertext(plaintext + 16)`: `{"cipher_suite":"...","nonce":[],
/// "ciphertext":[]}` measured at 55 bytes, rounded up to 128 for headroom.
const JSON_CHUNK_ENVELOPE: u64 = 128;

/// Upper bound on the legacy JSON-blob wire size for `original_size` plaintext
/// bytes in `count` chunks.
fn json_wire_bound(original_size: u64, count: u32) -> u64 {
    let per_chunk = JSON_BYTES_PER_BYTE
        .saturating_mul(CHUNK_OVERHEAD)
        .saturating_add(JSON_CHUNK_ENVELOPE);
    original_size
        .saturating_mul(JSON_BYTES_PER_BYTE)
        .saturating_add(per_chunk.saturating_mul(count as u64))
}

/// Max bytes the buffered fallback may hold, and whether that bound is a
/// guess (`chunked`) or the server's declared `Content-Length`.
///
/// - With a `Content-Length` the cap IS that length, with no ceiling: memory is
///   bounded by bytes the server actually sends, exactly like any download, so
///   a legacy file larger than the ceiling still downloads (lead ruling, task
///   1759 round 5; `main` buffered unbounded).
/// - Without one (chunked transfer) the format is unknown, so the cap is the
///   worst-case legacy JSON-blob size ([`json_wire_bound`]) clamped to
///   `ceiling`, and an overrun gets the "legacy file / chunked limit" message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FallbackCap {
    cap: u64,
    chunked: bool,
}

fn fallback_cap(ceiling: u64, content_len: Option<u64>, original_size: Option<u64>, count: u32) -> FallbackCap {
    match content_len {
        Some(cl) => FallbackCap {
            cap: cl,
            chunked: false,
        },
        None => FallbackCap {
            cap: original_size
                .map_or(ceiling, |os| json_wire_bound(os, count))
                .min(ceiling),
            chunked: true,
        },
    }
}

/// `Err` when `len` bytes exceed the cap, with a message that says why.
fn check_cap(c: FallbackCap, len: u64) -> Result<(), String> {
    if len <= c.cap {
        return Ok(());
    }
    if c.chunked {
        Err(format!(
            "this is a legacy-format file larger than the chunked-download limit ({} bytes; {len} bytes read); \
             retry the download, or the server must send Content-Length for it",
            c.cap
        ))
    } else {
        Err(format!(
            "download body exceeds its declared Content-Length of {} bytes ({len} bytes read); aborting",
            c.cap
        ))
    }
}

/// Outcome of a streaming download.
pub struct DownloadStats {
    pub plaintext_bytes: u64,
    pub encrypted_bytes: u64,
}

/// Stream-download `file_id`, decrypt to `out_path` with constant memory, and
/// report progress. Falls back to the buffered legacy decrypt for JSON-blob /
/// binary-UUID / unknown-frame-size files (see module docs).
pub async fn stream_download_decrypt(
    api: &ApiClient,
    master_key: &MasterKey,
    file_id: &str,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
) -> Result<DownloadStats, String> {
    stream_download_decrypt_with(
        api,
        master_key,
        file_id,
        chunk_count,
        out_path,
        progress,
        display_name,
        FALLBACK_HARD_CEILING,
    )
    .await
}

/// [`stream_download_decrypt`] with an explicit chunked-fallback ceiling
/// (production passes [`FALLBACK_HARD_CEILING`]; tests lower it).
#[allow(clippy::too_many_arguments)]
async fn stream_download_decrypt_with(
    api: &ApiClient,
    master_key: &MasterKey,
    file_id: &str,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
    ceiling: u64,
) -> Result<DownloadStats, String> {
    let count = chunk_count.max(1);

    let mut resp = api.download_stream(file_id).await?;
    let content_len = resp.content_length();
    let original_size = header_u64(&resp, "X-Original-Size");
    // Uniform plaintext chunk size for this file's version (V2 files only). When
    // present it is authoritative for frame sizing; absent → legacy V1 fallback.
    // The header is untrusted: a value past core's MAX_CHUNK_SIZE (256 MiB, the
    // cap core's own ChunkEncryptor/Decryptor enforce) is treated as absent so a
    // hostile server cannot make us buffer (or overflow on) a huge "frame".
    let chunk_size = header_u64(&resp, "X-Chunk-Size").filter(|&v| v > 0 && v <= MAX_CHUNK_SIZE);
    let fb_cap = fallback_cap(ceiling, content_len, original_size, count);

    // Best estimate of the total encrypted length, used only to size the first
    // N-1 frames; the last frame always drains the remainder, so a small
    // mis-estimate self-corrects (and a wrong size fails the GCM tag → error).
    let total: u64 = content_len
        .or_else(|| original_size.map(|os| os + CHUNK_OVERHEAD * count as u64))
        .unwrap_or(0);

    // Read until the first body bytes arrive (an empty body is an error). The
    // bytes are NOT inspected: the format is decided by frame-0 authentication.
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

    let result = stream_raw(
        &mut resp,
        master_key,
        file_id,
        count,
        total,
        chunk_size,
        fb_cap,
        out_path,
        carry,
        prog.as_ref(),
    )
    .await;

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
    chunk_size: Option<u64>,
    fb_cap: FallbackCap,
    out_path: &Path,
    mut carry: Vec<u8>,
    prog: &dyn crate::upload::FileProgress,
) -> Result<DownloadStats, String> {
    let n = count as usize;
    // Frame size for the first n-1 frames; the last frame takes the remainder.
    // When the server sends `X-Chunk-Size` (V2 files), each wire frame is the
    // uniform plaintext chunk size + CHUNK_OVERHEAD (nonce + tag) — this is
    // exact even when the file size is not a multiple of the chunk size. Only
    // legacy V1 responses (no header) start from the `total / count` average;
    // if that mis-sizes frame 0 the first-frame fallback below hands the payload
    // to the buffered path, which recovers the real frame size by AEAD-validated
    // probing. For a single chunk the whole payload is the frame.
    let frame_size = if n <= 1 {
        usize::MAX // sentinel: the single frame is "everything"
    } else if let Some(cs) = chunk_size {
        cs.checked_add(CHUNK_OVERHEAD)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| format!("chunk size {cs} is not addressable"))?
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
            drain_rest(resp, &mut carry, fb_cap).await?;
            std::mem::take(&mut carry)
        } else {
            fill_to(resp, &mut carry, frame_size).await?;
            if carry.len() < frame_size && i == 0 {
                // Frame 0 is shorter than the header claims: a JSON-blob /
                // legacy file served with a V2 header. Not a truncation —
                // hand everything to the buffered path (capped).
                writer.abort();
                let mut full = std::mem::take(&mut carry);
                drain_rest(resp, &mut full, fb_cap).await?;
                return buffered_fallback(master_key, file_id, &full, count, chunk_size, out_path, prog);
            }
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
                // First frame failed to authenticate: a JSON-blob file, a
                // pre-`bb repair` binary-UUID file, or a frame-size guess that
                // was wrong. Reassemble the full payload and let the buffered
                // path try both parsers, both keys and the frame-size probe.
                writer.abort();
                let mut full = frame; // the first frame we already read
                // `carry` holds the bytes read past frame 0; append it once,
                // then drain the remainder under the fallback cap.
                full.append(&mut carry);
                drain_rest(resp, &mut full, fb_cap).await?;
                return buffered_fallback(master_key, file_id, &full, count, chunk_size, out_path, prog);
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

/// Buffered legacy decrypt via `crypto::decrypt_file_chunks_with_chunk_size`
/// (handles JSON-blob, binary/string dual-key and unknown frame sizes). Writes
/// atomically.
fn buffered_fallback(
    master_key: &MasterKey,
    file_id: &str,
    encrypted: &[u8],
    count: u32,
    chunk_size: Option<u64>,
    out_path: &Path,
    prog: &dyn crate::upload::FileProgress,
) -> Result<DownloadStats, String> {
    let plaintext =
        crate::crypto::decrypt_file_chunks_with_chunk_size(master_key, file_id, encrypted, count, chunk_size)?;
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

/// Drain the rest of the response body into `buf`, refusing to hold more than
/// the cap (see [`fallback_cap`]).
async fn drain_rest(resp: &mut reqwest::Response, buf: &mut Vec<u8>, cap: FallbackCap) -> Result<(), String> {
    check_cap(cap, buf.len() as u64)?;
    while let Some(b) = resp.chunk().await.map_err(|e| format!("download read: {e}"))? {
        check_cap(cap, (buf.len() as u64).saturating_add(b.len() as u64))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::header, response::IntoResponse, routing::get};
    use beebeeb_core::encrypt::{encrypt_chunk, encrypt_chunk_raw};
    use beebeeb_core::kdf::{derive_file_key, derive_master_key};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FID: &str = "3e15382b-1111-2222-3333-444455556666";

    /// Counts server-confirmed frames. Streaming reports one per frame; the
    /// buffered fallback reports the whole payload once.
    struct Counting(Arc<AtomicUsize>);
    struct CountingFile(Arc<AtomicUsize>);
    impl ChunkProgress for Counting {
        fn begin_file(&self, _name: &str, _expected: u64) -> Box<dyn crate::upload::FileProgress> {
            Box::new(CountingFile(self.0.clone()))
        }
    }
    impl crate::upload::FileProgress for CountingFile {
        fn chunk_confirmed(&self, _bytes: u64) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn finish(self: Box<Self>, _success: bool) {}
    }

    #[derive(Clone)]
    struct Served {
        body: Arc<Vec<u8>>,
        chunk_size: Option<u64>,
        chunk_count: u32,
    }

    async fn serve(State(s): State<Served>) -> impl IntoResponse {
        let mut h = axum::http::HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
        h.insert("x-chunk-count", s.chunk_count.to_string().parse().unwrap());
        if let Some(cs) = s.chunk_size {
            h.insert("x-chunk-size", cs.to_string().parse().unwrap());
        }
        (h, s.body.as_ref().clone())
    }

    /// Raw multi-chunk wire bytes; `brace_first` re-rolls frame 0's nonce until
    /// it starts with 0x7b.
    fn raw_wire(m: &MasterKey, pt: &[u8], cs: usize, brace_first: bool) -> Vec<u8> {
        let fk = derive_file_key(m, FID.as_bytes());
        let mut out = Vec::new();
        for (i, chunk) in pt.chunks(cs).enumerate() {
            let frame = if i == 0 && brace_first {
                (0..100_000)
                    .map(|_| encrypt_chunk_raw(&fk, chunk).unwrap())
                    .find(|f| f[0] == b'{')
                    .expect("a 0x7b nonce within 100k tries")
            } else {
                encrypt_chunk_raw(&fk, chunk).unwrap()
            };
            out.extend_from_slice(&frame);
        }
        out
    }

    /// Download `body` through the real streaming path against a local mock;
    /// returns (decrypted file, number of progress confirmations).
    async fn download(body: Vec<u8>, chunk_size: Option<u64>, chunk_count: u32, m: &MasterKey) -> (Vec<u8>, usize) {
        download_with(body, chunk_size, chunk_count, m, FALLBACK_HARD_CEILING).await
    }

    async fn download_with(
        body: Vec<u8>,
        chunk_size: Option<u64>,
        chunk_count: u32,
        m: &MasterKey,
        ceiling: u64,
    ) -> (Vec<u8>, usize) {
        let app = Router::new()
            .route("/api/v1/files/:id/download", get(serve))
            .with_state(Served {
                body: Arc::new(body),
                chunk_size,
                chunk_count,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let api = ApiClient::new_for_test(format!("http://{addr}"));
        let dir = std::env::temp_dir().join(format!("bb-dl-1759-{}-{}", std::process::id(), rand::random::<u32>()));
        let out = dir.join("out.bin");
        let confirmed = Arc::new(AtomicUsize::new(0));
        stream_download_decrypt_with(
            &api,
            m,
            FID,
            chunk_count,
            &out,
            &Counting(confirmed.clone()),
            "out.bin",
            ceiling,
        )
        .await
        .expect("download + decrypt succeeds");
        let got = std::fs::read(&out).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (got, confirmed.load(Ordering::SeqCst))
    }

    fn mk() -> MasterKey {
        derive_master_key("test-password", b"test-salt-16bytes").unwrap()
    }

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Task 1759 round 2: raw multi-chunk file, uneven last chunk, frame 0's
    /// nonce starts with '{', X-Chunk-Size present. Must decrypt AND stream
    /// (one confirmation per frame), not be shunted to the buffered path by a
    /// first-byte sniff.
    #[tokio::test]
    async fn streams_raw_uneven_last_chunk_with_brace_first_byte() {
        let m = mk();
        let pt = payload(3 * 4096 + 77);
        let wire = raw_wire(&m, &pt, 4096, true);
        assert_eq!(wire[0], b'{');
        let (got, confirmed) = download(wire, Some(4096), 4, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 4, "4 frames streamed, not one buffered blob");
    }

    /// Same file with a non-brace nonce streams too (control).
    #[tokio::test]
    async fn streams_raw_uneven_last_chunk() {
        let m = mk();
        let pt = payload(3 * 4096 + 77);
        let wire = raw_wire(&m, &pt, 4096, false);
        let (got, confirmed) = download(wire, Some(4096), 4, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 4);
    }

    /// Legacy V1 response (no X-Chunk-Size): the total/count guess mis-sizes
    /// frame 0, so the buffered path recovers the real frame size by probing.
    #[tokio::test]
    async fn legacy_response_without_chunk_size_header_uneven_last_chunk() {
        let m = mk();
        let pt = payload(3 * 4096 + 77);
        for brace in [false, true] {
            let wire = raw_wire(&m, &pt, 4096, brace);
            let (got, confirmed) = download(wire, None, 4, &m).await;
            assert_eq!(got, pt, "brace_first={brace}");
            assert_eq!(confirmed, 1, "buffered fallback reports the payload once");
        }
    }

    /// Legacy CLI JSON-blob multi-chunk file still downloads (buffered).
    #[tokio::test]
    async fn legacy_json_blob_file_still_downloads() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(2 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        assert_eq!(wire[0], b'{');
        let (got, confirmed) = download(wire, None, 3, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 1);
    }

    /// Single raw chunk whose nonce starts with '{' streams (was sniffed away).
    #[tokio::test]
    async fn single_raw_chunk_with_brace_first_byte() {
        let m = mk();
        let pt = payload(500);
        let wire = raw_wire(&m, &pt, 4096, true);
        let (got, confirmed) = download(wire, Some(4096), 1, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 1);
    }

    /// Serve `body` with arbitrary extra headers and WITHOUT a Content-Length
    /// (chunked transfer), then run the real download. Returns the result.
    async fn try_download_chunked(
        body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
        chunk_count: u32,
        m: &MasterKey,
    ) -> Result<Vec<u8>, String> {
        try_download_chunked_with(body, headers, chunk_count, m, FALLBACK_HARD_CEILING).await
    }

    async fn try_download_chunked_with(
        body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
        chunk_count: u32,
        m: &MasterKey,
        ceiling: u64,
    ) -> Result<Vec<u8>, String> {
        let body = Arc::new(body);
        let headers = Arc::new(headers);
        let app = Router::new().route(
            "/api/v1/files/:id/download",
            get(move || {
                let body = body.clone();
                let headers = headers.clone();
                async move {
                    let mut h = axum::http::HeaderMap::new();
                    for (k, v) in headers.iter() {
                        h.insert(*k, v.parse().unwrap());
                    }
                    let parts: Vec<Result<bytes::Bytes, std::convert::Infallible>> = body
                        .chunks(8192)
                        .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
                        .collect();
                    (h, axum::body::Body::from_stream(futures_util::stream::iter(parts)))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let api = ApiClient::new_for_test(format!("http://{addr}"));
        let dir = std::env::temp_dir().join(format!("bb-dl-1759-{}-{}", std::process::id(), rand::random::<u32>()));
        let out = dir.join("out.bin");
        let confirmed = Arc::new(AtomicUsize::new(0));
        let r = stream_download_decrypt_with(
            &api,
            m,
            FID,
            chunk_count,
            &out,
            &Counting(confirmed),
            "out.bin",
            ceiling,
        )
        .await;
        let got = r.map(|_| std::fs::read(&out).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        got
    }

    /// Round 3 P2-1: a hostile X-Chunk-Size (2^40, u64::MAX) must be treated as
    /// absent — no giant frame buffering, no overflow — and the file still
    /// decrypts through the probing fallback.
    #[tokio::test]
    async fn hostile_chunk_size_header_is_ignored() {
        let m = mk();
        let pt = payload(3 * 4096 + 77);
        for hostile in [1u64 << 40, u64::MAX, u64::MAX - 27, MAX_CHUNK_SIZE + 1] {
            let wire = raw_wire(&m, &pt, 4096, false);
            let (got, _) = download(wire, Some(hostile), 4, &m).await;
            assert_eq!(got, pt, "hostile X-Chunk-Size {hostile}");
        }
    }

    /// Round 3 P2-3: V2 header present but frame 0 is short (a JSON-blob file
    /// served with the header) takes the buffered fallback, not "truncated".
    #[tokio::test]
    async fn short_frame_zero_with_chunk_size_header_falls_back() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(2 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        let (got, confirmed) = download(wire, Some(1 << 20), 3, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 1, "buffered fallback");
    }

    /// Round 3 P2-2: the frame-0-failure fallback buffer is capped at
    /// original_size + 28*count; a body that keeps coming is aborted.
    #[tokio::test]
    async fn fallback_buffer_is_capped() {
        let m = mk();
        // Garbage that fails frame-0 auth, far larger than the declared size.
        let body = vec![0x5au8; 2 * 1024 * 1024];
        let err = try_download_chunked(
            body,
            vec![("x-original-size", "1000".into()), ("x-chunk-size", "4096".into())],
            4,
            &m,
        )
        .await
        .expect_err("oversized fallback body must be refused");
        assert!(err.contains("chunked-download limit"), "clear cap error, got: {err}");
    }

    /// The cap function. Content-Length wins outright (no ceiling); chunked is
    /// the JSON bound clamped to the ceiling.
    #[test]
    fn fallback_cap_bounds() {
        let c = FALLBACK_HARD_CEILING;
        assert_eq!(
            fallback_cap(c, Some(500), Some(100), 3),
            FallbackCap {
                cap: 500,
                chunked: false
            }
        );
        // Round 5: a declared length above the ceiling is NOT clamped.
        assert_eq!(
            fallback_cap(c, Some(c + 1), Some(100), 3),
            FallbackCap {
                cap: c + 1,
                chunked: false
            }
        );
        let ch = fallback_cap(c, None, Some(100), 3);
        assert_eq!(
            ch,
            FallbackCap {
                cap: json_wire_bound(100, 3),
                chunked: true
            }
        );
        assert!(ch.cap >= 100 + 28 * 3);
        // Chunked is clamped to the ceiling.
        assert_eq!(fallback_cap(c, None, Some(u64::MAX), 3).cap, c);
        assert_eq!(fallback_cap(1000, None, Some(10_000), 3).cap, 1000);
        assert_eq!(fallback_cap(c, None, None, 3).cap, c);
    }

    /// A body past its own declared length is refused with the Content-Length
    /// message; chunked overrun gets the legacy-format message.
    #[test]
    fn check_cap_messages() {
        let cl = FallbackCap {
            cap: 10,
            chunked: false,
        };
        assert!(check_cap(cl, 10).is_ok());
        let e = check_cap(cl, 11).unwrap_err();
        assert!(e.contains("declared Content-Length of 10"), "{e}");
        let ch = FallbackCap { cap: 10, chunked: true };
        let e = check_cap(ch, 11).unwrap_err();
        assert!(
            e.contains("legacy-format file larger than the chunked-download limit"),
            "{e}"
        );
        assert!(e.contains("retry") && e.contains("Content-Length"), "{e}");
    }

    /// Round 5 P1: legacy JSON-blob body WITH Content-Length above the (test
    /// lowered) ceiling downloads: memory is bounded by what the server sends.
    #[tokio::test]
    async fn legacy_json_with_content_length_above_ceiling_downloads() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(3 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        let ceiling = 1000u64;
        assert!(wire.len() as u64 > ceiling, "test body must exceed the lowered ceiling");
        let (got, _) = download_with(wire, None, 4, &m, ceiling).await;
        assert_eq!(got, pt);
    }

    /// Round 5 P1: the same body chunked (no Content-Length) over the ceiling
    /// is refused with the legacy-format message.
    #[tokio::test]
    async fn legacy_json_chunked_over_ceiling_is_refused_with_new_message() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(3 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        let ceiling = 1000u64;
        assert!(wire.len() as u64 > ceiling);
        let err = try_download_chunked_with(wire, vec![("x-original-size", pt.len().to_string())], 4, &m, ceiling)
            .await
            .expect_err("chunked over ceiling must be refused");
        assert!(
            err.contains("legacy-format file larger than the chunked-download limit"),
            "got: {err}"
        );
    }

    /// Round 4 P1: chunked transfer (no Content-Length) + X-Original-Size +
    /// legacy JSON-blob chunks. The wire is several times the plaintext, so the
    /// raw-frame cap refused a valid download.
    #[tokio::test]
    async fn chunked_legacy_json_blob_with_original_size_downloads() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(3 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        assert!(
            wire.len() as u64 > pt.len() as u64 + 28 * 4,
            "JSON must exceed the raw cap"
        );
        let got = try_download_chunked(wire, vec![("x-original-size", pt.len().to_string())], 4, &m)
            .await
            .expect("legacy JSON download must not be refused by the cap");
        assert_eq!(got, pt);
    }

    /// A body far past the JSON worst case is still refused.
    #[tokio::test]
    async fn chunked_body_far_beyond_json_bound_is_refused() {
        let m = mk();
        let pt_len = 1000u64;
        let bound = json_wire_bound(pt_len, 4);
        let body = vec![0x5au8; (bound as usize) * 10 + 4096];
        let err = try_download_chunked(body, vec![("x-original-size", pt_len.to_string())], 4, &m)
            .await
            .expect_err("must be refused");
        assert!(err.contains("chunked-download limit"), "got: {err}");
    }

    /// The JSON bound constants dominate real serialisation, worst case
    /// included (all-0xFF bytes serialise to 3 digits + comma). Prints the
    /// measured factors.
    #[test]
    fn json_bound_covers_serialised_blobs() {
        use beebeeb_types::{CipherSuite, EncryptedBlob};
        let mut max_ratio = 0f64;
        let mut envelope = 0usize;
        for n in [0usize, 1, 16, 300, 4096, 65536] {
            // Real random blob.
            let m = mk();
            let fk = derive_file_key(&m, FID.as_bytes());
            let real = serde_json::to_vec(&encrypt_chunk(&fk, &vec![7u8; n]).unwrap()).unwrap();
            // Synthetic worst case: every byte is 0xFF.
            let worst = serde_json::to_vec(&EncryptedBlob {
                cipher_suite: CipherSuite::V1Aes256Gcm,
                nonce: vec![0xff; 12],
                ciphertext: vec![0xff; n + 16],
            })
            .unwrap();
            assert!(real.len() <= worst.len());
            let digits_len = 4 * (12 + n + 16);
            envelope = envelope.max(worst.len().saturating_sub(digits_len));
            max_ratio = max_ratio.max(worst.len() as f64 / (n as f64 + 1.0));
            assert!(
                worst.len() as u64 <= json_wire_bound(n as u64, 1),
                "n={n}: {} > {}",
                worst.len(),
                json_wire_bound(n as u64, 1)
            );
        }
        eprintln!("measured JSON envelope overhead = {envelope} bytes; max wire/plaintext ratio = {max_ratio:.2}");
        assert!(envelope as u64 <= JSON_CHUNK_ENVELOPE);
    }
}
