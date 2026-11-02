//! Size caps that bound the buffered fallback and the wire-size guessing.

use super::DownloadError;

/// Per-chunk AEAD overhead: `nonce(12) + tag(16)`.
pub(super) const CHUNK_OVERHEAD: u64 = 28;

/// Hard ceiling (4 GiB) on the buffered-fallback payload. The fallback exists
/// for legacy JSON-blob / binary-UUID / unknown-frame-size files, which are
/// small by construction; a body past this is refused whatever the headers say.
pub(super) const FALLBACK_HARD_CEILING: u64 = 4 * 1024 * 1024 * 1024;

/// Worst-case JSON expansion of a legacy `EncryptedBlob` chunk: `nonce` and
/// `ciphertext` are serialised as arrays of decimal numbers, so every byte costs
/// at most 4 bytes on the wire (`255,`). Verified against real serialisation by
/// the `json_bound_covers_serialised_blobs` test.
pub(super) const JSON_BYTES_PER_BYTE: u64 = 4;

/// Worst-case per-chunk JSON envelope on top of the 4x-expanded
/// `nonce(12) + ciphertext(plaintext + 16)`: `{"cipher_suite":"...","nonce":[],
/// "ciphertext":[]}` measured at 55 bytes, rounded up to 128 for headroom.
pub(super) const JSON_CHUNK_ENVELOPE: u64 = 128;

/// Upper bound on the legacy JSON-blob wire size for `original_size` plaintext
/// bytes in `count` chunks.
pub(super) fn json_wire_bound(original_size: u64, count: u32) -> u64 {
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
pub(super) struct FallbackCap {
    pub(super) cap: u64,
    pub(super) chunked: bool,
}

pub(super) fn fallback_cap(
    ceiling: u64,
    content_len: Option<u64>,
    original_size: Option<u64>,
    count: u32,
) -> FallbackCap {
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
pub(super) fn check_cap(c: FallbackCap, len: u64) -> Result<(), DownloadError> {
    if len <= c.cap {
        return Ok(());
    }
    if c.chunked {
        Err(format!(
            "this is a legacy-format file larger than the chunked-download limit ({} bytes; {len} bytes read); \
             retry the download, or the server must send Content-Length for it",
            c.cap
        )
        .into())
    } else {
        Err(format!(
            "download body exceeds its declared Content-Length of {} bytes ({len} bytes read); aborting",
            c.cap
        )
        .into())
    }
}
