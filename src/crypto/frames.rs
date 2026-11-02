//! Chunk-frame probing (ladder sizes, first-frame opens) split out of `crypto.rs`.

use super::*;

/// Upper bound on the AEAD work spent probing for the frame size when the
/// caller has no chunk-size metadata (bytes of ciphertext authenticated). This
/// bounds the exhaustive scan (step 4 of [`raw_frame_size`]) only: the known
/// size and the power-of-two ladder always run in full.
pub(super) const FRAME_PROBE_BUDGET: u64 = 64 * 1024 * 1024;

/// Plaintext chunk sizes the platform has ever emitted as powers of two: 1 KiB
/// up to 256 MiB (`beebeeb_types` chunk planner is 4 MiB..256 MiB; 1 MiB is the
/// pre-V2 fixed size). Probed first when no chunk size is known.
pub(super) fn ladder_frame_sizes() -> impl Iterator<Item = u64> {
    (10..=28u32).map(|k| (1u64 << k) + FRAME_OVERHEAD)
}

/// Per-frame AEAD overhead: `nonce (12) + tag (16)`.
pub(super) const FRAME_OVERHEAD: u64 = (NONCE_LEN + TAG_LEN) as u64;

/// Does `frame` authenticate as frame 0 (not last) of a multi-chunk file under a
/// format `pin` allows? Core's random-access opener; only the verdict is kept.
/// A v2 frame 0's own nonce is the upload's version id.
pub(super) fn opens_as_first_frame(file_key: &FileKey, file_id_str: &str, pin: ExpectedChunkFormat, frame: &[u8]) -> bool {
    let formats: &[ChunkFormat] = match pin {
        ExpectedChunkFormat::V2 => &[ChunkFormat::V2],
        ExpectedChunkFormat::Unrecorded => &[ChunkFormat::V2, ChunkFormat::V1Legacy],
    };
    let version_id = frame.get(..NONCE_LEN).unwrap_or_default();
    formats.iter().any(|&format| {
        open_chunk(file_key, file_id_str, version_id, 0, false, format, frame)
            .map(zeroize::Zeroizing::new) // wiped on drop; only the verdict is kept
            .is_ok()
    })
}

/// Work out the wire frame size of the first `count - 1` frames of a raw
/// multi-chunk file (every frame but the last is `chunk_size + 28` bytes; the
/// last holds the remainder and is usually shorter).
///
/// The wire carries no per-chunk lengths, so the size comes from, in order:
/// 1. `chunk_size` (the `X-Chunk-Size` header), when the caller has it;
/// 2. the powers of two the planner emits (1 KiB..256 MiB);
/// 3. the uniform split `ceil(total / count)` (every chunk the same size);
/// 4. every other size the byte count allows, until the scan budget
///    [`FRAME_PROBE_BUDGET`] (ciphertext bytes authenticated) is spent.
///
/// A candidate is accepted only if the first frame AUTHENTICATES under the file
/// key (in a format the pin allows), so a wrong guess costs time and can never
/// produce wrong plaintext.
pub(super) fn raw_frame_size(
    file_key: &FileKey,
    file_id_str: &str,
    pin: ExpectedChunkFormat,
    data: &[u8],
    count: usize,
    chunk_size: Option<u64>,
) -> Result<usize, CryptoError> {
    probe_first_frame_len(data, data.len() as u64, count, chunk_size, &mut |frame| {
        opens_as_first_frame(file_key, file_id_str, pin, frame)
    })
}

/// The frame-size search of [`raw_frame_size`] (same order, same bounds, same
/// budget), over `data` that may be only a PREFIX of the version: `total` is
/// the whole encrypted length, which bounds the feasible sizes, and a size past
/// the bytes in hand is skipped. `opens` is asked whether a candidate first
/// frame authenticates; the first size it accepts is returned. Shared by the
/// read path and the SF-0002 write check (round 3, K2), so both find the same
/// frame for a legacy multi-chunk file served without `X-Chunk-Size`.
pub(crate) fn probe_first_frame_len(
    data: &[u8],
    total: u64,
    count: usize,
    chunk_size: Option<u64>,
    opens: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<usize, CryptoError> {
    let n = count as u64;
    // total = (n-1)*F + last, 28 <= last <= F  =>  F in [ceil(total/n), (total-28)/(n-1)].
    let lo = total.div_ceil(n);
    let hi = total.saturating_sub(FRAME_OVERHEAD) / (n - 1);
    if lo > hi {
        return Err(CryptoError::Malformed(format!(
            "raw data of {total} bytes cannot be split into {count} chunks of >= {FRAME_OVERHEAD} bytes"
        )));
    }

    let mut probe = FrameProbe {
        opens,
        data,
        lo,
        hi,
        tried: Vec::new(),
        spent: 0,
    };

    if let Some(cs) = chunk_size.and_then(|cs| cs.checked_add(FRAME_OVERHEAD))
        && probe.accepts(cs)
    {
        return Ok(cs as usize);
    }
    for f in ladder_frame_sizes() {
        if probe.accepts(f) {
            return Ok(f as usize);
        }
    }
    if probe.accepts(lo) {
        return Ok(lo as usize);
    }
    let mut f = lo;
    while f <= hi && probe.spent < FRAME_PROBE_BUDGET {
        if probe.accepts(f) {
            return Ok(f as usize);
        }
        f += 1;
    }
    Err(CryptoError::Malformed(format!(
        "no chunk size splits {total} bytes into {count} authenticating chunks \
         (tried {} candidate frame sizes)",
        probe.tried.len()
    )))
}

/// Candidate-frame-size prober for [`probe_first_frame_len`].
pub(super) struct FrameProbe<'a> {
    pub(super) opens: &'a mut dyn FnMut(&[u8]) -> bool,
    pub(super) data: &'a [u8],
    pub(super) lo: u64,
    pub(super) hi: u64,
    pub(super) tried: Vec<u64>,
    /// Ciphertext bytes authenticated so far (against [`FRAME_PROBE_BUDGET`]).
    pub(super) spent: u64,
}

impl FrameProbe<'_> {
    /// True if `f` is a feasible frame size (the byte count allows it, and the
    /// bytes are in hand) whose first frame authenticates. Each size is tried
    /// at most once.
    pub(super) fn accepts(&mut self, f: u64) -> bool {
        if f < self.lo || f > self.hi || f > self.data.len() as u64 || self.tried.contains(&f) {
            return false;
        }
        self.tried.push(f);
        self.spent = self.spent.saturating_add(f);
        (self.opens)(&self.data[..f as usize])
    }
}
