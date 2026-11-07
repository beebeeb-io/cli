//! The streaming download + decrypt path: try the raw format, fall back to the buffered legacy paths. The raw-format items live here too.

use super::*;
use beebeeb_core::download::MAX_FRAME_SIZE;

use crate::upload::{Held, InFlight, Route};

/// The API stream reads one frame at a time, in order: the most chunks it has in flight (spec §1.3, task 2333).
const API_STREAM_IN_FLIGHT: u32 = 1;

/// Download a file's raw encrypted body into memory together with the server's
/// `X-Chunk-Size` (the uniform plaintext chunk size, V2 files), for callers that
/// must decrypt with a key the streaming path does not know (file-request
/// uploads, `bb pull --zip`). The chunk size is what lets the decrypt path split
/// a multi-chunk body exactly: every frame but the last is `chunk_size + 28`
/// bytes and the last one is usually shorter. Like the streaming path it treats
/// the header as untrusted (zero, or past core's `MAX_CHUNK_SIZE`, is "absent").
pub async fn download_buffered(
    api: &ApiClient,
    file_id: impl Into<crate::api::FileId>,
) -> Result<(Vec<u8>, Option<u64>), DownloadError> {
    let resp = api.download_stream(file_id).await?;
    let chunk_size = header_u64(&resp, "X-Chunk-Size").filter(|&v| v > 0 && v <= MAX_CHUNK_SIZE);
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| DownloadError::Other(format!("failed to read response: {e}")))?;
    Ok((bytes.to_vec(), chunk_size))
}

/// Read only as much of a file's current version as the first wire frame needs
/// (never the whole file), for the SF-0002 read-only content check. Returns the
/// prefix, the server's `X-Chunk-Size` when present, and whether the body ended
/// before the requested length.
///
/// `wide` (round 3, K2): a legacy multi-chunk raw file served without
/// `X-Chunk-Size` whose first frame was not in the ordinary prefix. Its frame
/// size is found by probing, and every feasible size is at most
/// `(total - 28) / (count - 1)` (the read path's bound), so that much is read.
pub async fn download_frame_prefix(
    api: &ApiClient,
    file_id: impl Into<crate::api::FileId>,
    size_bytes: Option<u64>,
    chunk_count: u32,
    wide: bool,
) -> Result<(Vec<u8>, Option<u64>, bool), DownloadError> {
    let mut resp = api.download_stream(file_id).await?;
    let chunk_size = header_u64(&resp, "X-Chunk-Size").filter(|&v| v > 0 && v <= MAX_CHUNK_SIZE);
    let count = chunk_count.max(1);
    let mut want = frame_probe_len(size_bytes, count, chunk_size, wide);
    let mut buf = Vec::new();
    fill_to(&mut resp, &mut buf, want).await?;
    // Round 3 (K1): a legacy JSON-blob chunk 0 is about 4x its plaintext. Read
    // until a whole blob 0 can be in hand (a raw frame whose nonce happens to
    // start with '{' reads a little more and is decided the same way).
    if buf.first() == Some(&b'{') {
        let json_want = json_chunk0_probe_len(size_bytes, count);
        if buf.len() < json_want {
            fill_to(&mut resp, &mut buf, json_want).await?;
        }
        want = want.max(json_want);
    }
    let complete = buf.len() < want;
    Ok((buf, chunk_size, complete))
}

/// The longest legacy JSON-blob chunk 0 a version of this geometry can have:
/// [`json_wire_bound`] of chunk 0's largest possible plaintext. With `n > 1`
/// uniform chunks `(n-1)·cs < size`, so chunk 0 holds at most
/// `ceil(size / (n-1))` bytes (and never more than the file).
pub(super) fn json_chunk0_probe_len(size_bytes: Option<u64>, chunk_count: u32) -> usize {
    const FILE_PROVIDER_CHUNK: u64 = 1024 * 1024;
    let plain = match size_bytes {
        Some(size) if chunk_count <= 1 => size,
        Some(size) => size.div_ceil(u64::from(chunk_count - 1)).min(size),
        None => FILE_PROVIDER_CHUNK,
    }
    .min(MAX_CHUNK_SIZE);
    usize::try_from(json_wire_bound(plain, 1)).unwrap_or(usize::MAX)
}

/// The longest plausible first-frame length for a version of `count` chunks,
/// bounded by core's maximum chunk size. Only a prefix of this length is read.
/// `wide`: see [`download_frame_prefix`].
pub(super) fn frame_probe_len(size_bytes: Option<u64>, chunk_count: u32, chunk_size: Option<u64>, wide: bool) -> usize {
    const NONCE_AND_TAG: u64 = 28;
    const FILE_PROVIDER_CHUNK: u64 = 1024 * 1024;
    let total = size_bytes
        .unwrap_or(0)
        .saturating_add(NONCE_AND_TAG.saturating_mul(chunk_count as u64));
    let want = if chunk_count <= 1 {
        total
    } else {
        let mut m = (FILE_PROVIDER_CHUNK + NONCE_AND_TAG).max(total / chunk_count as u64);
        match (chunk_size, size_bytes) {
            (Some(cs), _) => m = m.max(cs.saturating_add(NONCE_AND_TAG)),
            (None, Some(_)) if wide => m = m.max(total.saturating_sub(NONCE_AND_TAG) / (chunk_count as u64 - 1)),
            _ => {}
        }
        m
    };
    usize::try_from(want.min(MAX_CHUNK_SIZE.saturating_add(NONCE_AND_TAG))).unwrap_or(usize::MAX)
}

/// Stream-download `file_id`, decrypt to `out_path` with constant memory, and
/// report progress. Falls back to the buffered legacy decrypt for JSON-blob /
/// binary-UUID / unknown-frame-size files (see module docs).
///
/// `pin` is the file's read pin from its authenticated metadata
/// ([`content_pin`] in `crypto::pins`, task 1943): core's reader opens a
/// v2-pinned file only as v2, and rejects one that ends before its last chunk.
#[allow(clippy::too_many_arguments)]
pub async fn stream_download_decrypt(
    api: &ApiClient,
    master_key: &MasterKey,
    file_id: &str,
    pin: ExpectedChunkFormat,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
    pool: Option<&PoolLabel>,
) -> Result<DownloadStats, DownloadError> {
    stream_download_decrypt_with(
        api,
        master_key,
        file_id,
        pin,
        chunk_count,
        out_path,
        progress,
        display_name,
        pool,
        FALLBACK_HARD_CEILING,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn stream_download_decrypt_with_file_key(
    api: &ApiClient,
    file_key: std::sync::Arc<beebeeb_core::kdf::FileKey>,
    file_id: &str,
    pin: ExpectedChunkFormat,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
    pool: Option<&PoolLabel>,
) -> Result<DownloadStats, DownloadError> {
    stream_download_key(
        api,
        DownloadKey::Explicit(file_key),
        file_id,
        pin,
        chunk_count,
        out_path,
        progress,
        display_name,
        pool,
        FALLBACK_HARD_CEILING,
    )
    .await
}

pub(super) enum DownloadKey<'a> {
    Legacy(&'a MasterKey),
    Explicit(std::sync::Arc<beebeeb_core::kdf::FileKey>),
}

impl DownloadKey<'_> {
    pub(super) fn decryptor(&self, id: &str, pin: ExpectedChunkFormat) -> ChunkDecryptor {
        match self {
            Self::Legacy(master) => ChunkDecryptor::for_push(master, id, pin),
            // The decryptor owns its copy; `key_copy` wipes the by-value bytes it passes through.
            Self::Explicit(key) => ChunkDecryptor::for_push_with_file_key(crate::crypto::key_copy(key), id, pin),
        }
    }
    /// The owner path's master key, for the buffered fallback. An explicit key
    /// (shared folder, owner wrap, file request) has no fallback: its content
    /// opens under that key or not at all, so a frame-0 failure ends the download
    /// at once, before the rest of the body is read (SF-0002 round 2, C6).
    pub(super) fn legacy(&self) -> Result<&MasterKey, DownloadError> {
        match self {
            Self::Legacy(m) => Ok(m),
            Self::Explicit(_) => Err(DownloadError::Unreadable(
                "explicit-key content failed authentication".into(),
            )),
        }
    }
}

/// [`stream_download_decrypt`] with an explicit chunked-fallback ceiling
/// (production passes [`FALLBACK_HARD_CEILING`]; tests lower it).
#[allow(clippy::too_many_arguments)]
pub(super) async fn stream_download_decrypt_with(
    api: &ApiClient,
    master_key: &MasterKey,
    file_id: &str,
    pin: ExpectedChunkFormat,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
    pool: Option<&PoolLabel>,
    ceiling: u64,
) -> Result<DownloadStats, DownloadError> {
    stream_download_key(
        api,
        DownloadKey::Legacy(master_key),
        file_id,
        pin,
        chunk_count,
        out_path,
        progress,
        display_name,
        pool,
        ceiling,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn stream_download_key(
    api: &ApiClient,
    key: DownloadKey<'_>,
    file_id: &str,
    pin: ExpectedChunkFormat,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
    pool: Option<&PoolLabel>,
    ceiling: u64,
) -> Result<DownloadStats, DownloadError> {
    let count = chunk_count.max(1);

    // Task 2173: straight from object storage when the server offers it and the
    // handshake succeeds. `UseApi` means nothing was published (and nothing is
    // left on disk): this file goes through the API below, as it always did.
    let direct = match &key {
        DownloadKey::Legacy(master) => {
            crate::direct_download::try_download(
                api,
                master,
                file_id,
                pin,
                chunk_count,
                out_path,
                progress,
                display_name,
                pool,
            )
            .await?
        }
        DownloadKey::Explicit(file_key) => {
            crate::direct_download::try_download_with_file_key(
                api,
                file_key.clone(),
                file_id,
                pin,
                chunk_count,
                out_path,
                progress,
                display_name,
                pool,
            )
            .await?
        }
    };
    let moved_direct = match direct {
        crate::direct_download::DirectResult::Done(stats) => return Ok(stats),
        crate::direct_download::DirectResult::UseApi { transferred_bytes } => transferred_bytes,
    };
    progress.set_limit(API_STREAM_IN_FLIGHT);

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
        match resp
            .chunk()
            .await
            .map_err(|e| DownloadError::Other(format!("download read: {e}")))?
        {
            Some(b) => carry.extend_from_slice(&b),
            None => break,
        }
    }
    if carry.is_empty() {
        return Err("download returned an empty body".to_string().into());
    }

    let prog = progress.begin_file_chunks(display_name, total, count, chunk_size.unwrap_or(0));
    if let Some(label) = pool {
        prog.pool(label.clone());
    }

    let result = stream_raw(
        &mut resp,
        &key,
        file_id,
        pin,
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
    // A direct attempt that fell back moved its chunks too: the file's speed counts them (spec §4.3).
    result.map(|stats| DownloadStats {
        transferred_bytes: stats.transferred_bytes + moved_direct,
        ..stats
    })
}

/// Stream the modern raw format. On a first-frame key failure (binary-UUID
/// legacy), buffer the whole payload and hand it to the buffered path.
#[allow(clippy::too_many_arguments)]
pub(super) async fn stream_raw(
    resp: &mut reqwest::Response,
    key: &DownloadKey<'_>,
    file_id: &str,
    pin: ExpectedChunkFormat,
    count: u32,
    total: u64,
    chunk_size: Option<u64>,
    fb_cap: FallbackCap,
    out_path: &Path,
    mut carry: Vec<u8>,
    prog: &dyn crate::upload::FileProgress,
) -> Result<DownloadStats, DownloadError> {
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
        // A legacy response has no `X-Chunk-Size`; the average is the best
        // frame size we can guess. Bound it: the server controls
        // `X-Original-Size`, so an unbounded average would let a hostile
        // response make `fill_to` buffer gigabytes (core's downloader,
        // 2b76195, refuses the same case).
        let average = total / count as u64;
        if average > MAX_FRAME_SIZE as u64 {
            return Err(format!("average frame size {average} exceeds the maximum {MAX_FRAME_SIZE}").into());
        }
        usize::try_from(average).unwrap_or(usize::MAX)
    };

    let mut writer = AtomicFile::create(out_path)?;
    let mut decryptor = key.decryptor(file_id, pin);
    let mut plaintext_bytes: u64 = 0;
    let mut encrypted_bytes: u64 = 0;

    for i in 0..n {
        let is_last = i == n - 1;
        // In flight from the moment its read begins until it is counted, fails or the download ends.
        let _flight = InFlight::start(prog, i as u32, Route::Api);
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
                let master_key = key.legacy()?;
                let mut full = std::mem::take(&mut carry);
                drain_rest(resp, &mut full, fb_cap).await?;
                return buffered_fallback(master_key, file_id, pin, &full, count, chunk_size, out_path, prog);
            }
            if carry.len() < frame_size {
                return Err(format!(
                    "download truncated: chunk {i}/{n} wanted {frame_size} bytes, got {}",
                    carry.len()
                )
                .into());
            }
            carry.drain(..frame_size).collect()
        };

        match decryptor.push_frame(&frame) {
            Ok(dec) => {
                writer.write_all(&dec.data)?;
                plaintext_bytes += dec.data.len() as u64;
                encrypted_bytes += frame.len() as u64;
                prog.chunk_done(i as u32, frame.len() as u64, Held::Transferred);
            }
            Err(_) if i == 0 => {
                // First frame failed to authenticate: a JSON-blob file, a
                // pre-`bb repair` binary-UUID file, or a frame-size guess that
                // was wrong. Reassemble the full payload and let the buffered
                // path try both parsers, both keys and the frame-size probe.
                writer.abort();
                let master_key = key.legacy()?;
                let mut full = frame; // the first frame we already read
                // `carry` holds the bytes read past frame 0; append it once,
                // then drain the remainder under the fallback cap.
                full.append(&mut carry);
                drain_rest(resp, &mut full, fb_cap).await?;
                return buffered_fallback(master_key, file_id, pin, &full, count, chunk_size, out_path, prog);
            }
            Err(e) => {
                writer.abort();
                // Frame i > 0 failed to authenticate: this client cannot read the file now.
                return Err(DownloadError::Unreadable(format!("decrypt chunk {i}: {e}")));
            }
        }
    }

    // v2: the stream must have ended with the frame that authenticated as last
    // (truncation detection, task 1908). v1 has no last marker.
    if let Err(e) = decryptor.finish() {
        writer.abort();
        // The stream ended before the frame marked last: cut short or tampered with.
        return Err(DownloadError::Unreadable(format!("download incomplete: {e}")));
    }
    writer.commit()?;
    Ok(DownloadStats {
        plaintext_bytes,
        encrypted_bytes,
        transport: "api",
        transferred_bytes: encrypted_bytes,
    })
}

/// Buffered legacy decrypt via `crypto::decrypt_file_chunks_with_chunk_size`
/// (handles JSON-blob, binary/string dual-key and unknown frame sizes). Writes
/// atomically.
#[allow(clippy::too_many_arguments)]
pub(super) fn buffered_fallback(
    master_key: &MasterKey,
    file_id: &str,
    pin: ExpectedChunkFormat,
    encrypted: &[u8],
    count: u32,
    chunk_size: Option<u64>,
    out_path: &Path,
    prog: &dyn crate::upload::FileProgress,
) -> Result<DownloadStats, DownloadError> {
    let plaintext =
        crate::crypto::decrypt_file_chunks_with_chunk_size(master_key, file_id, pin, encrypted, count, chunk_size)?;
    let mut writer = AtomicFile::create(out_path)?;
    writer.write_all(&plaintext)?;
    writer.commit()?;
    prog.body_done(count, encrypted.len() as u64, Route::Api);
    Ok(DownloadStats {
        plaintext_bytes: plaintext.len() as u64,
        encrypted_bytes: encrypted.len() as u64,
        transport: "api",
        transferred_bytes: encrypted.len() as u64,
    })
}

/// Read more body chunks until `buf.len() >= want` or EOF.
pub(super) async fn fill_to(resp: &mut reqwest::Response, buf: &mut Vec<u8>, want: usize) -> Result<(), DownloadError> {
    while buf.len() < want {
        match resp
            .chunk()
            .await
            .map_err(|e| DownloadError::Other(format!("download read: {e}")))?
        {
            Some(b) => buf.extend_from_slice(&b),
            None => break,
        }
    }
    Ok(())
}

/// Drain the rest of the response body into `buf`, refusing to hold more than
/// the cap (see [`fallback_cap`]).
pub(super) async fn drain_rest(
    resp: &mut reqwest::Response,
    buf: &mut Vec<u8>,
    cap: FallbackCap,
) -> Result<(), DownloadError> {
    check_cap(cap, buf.len() as u64)?;
    while let Some(b) = resp
        .chunk()
        .await
        .map_err(|e| DownloadError::Other(format!("download read: {e}")))?
    {
        check_cap(cap, (buf.len() as u64).saturating_add(b.len() as u64))?;
        buf.extend_from_slice(&b);
    }
    Ok(())
}

pub(super) fn header_u64(resp: &reqwest::Response, name: &str) -> Option<u64> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}
