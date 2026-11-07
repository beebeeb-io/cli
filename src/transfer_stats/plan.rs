//! A file's share of a run's plan, from what its row states (spec §4.2): `bb pull`'s and `bb sync`'s downloads.
//!
//! Owns: the chunk overhead and the row-to-plan rule. Must not: read a file or the network.

/// AES-GCM's nonce and tag: what each chunk adds to the plaintext on the wire.
pub(crate) const CHUNK_OVERHEAD: u64 = 28;

/// A file row's share of the run: its ciphertext and its chunks, as the row states them. The driver's own reading
/// of the file (the manifest, the stream's length, a buffered body) replaces the estimate when the file begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FilePlan {
    pub(crate) bytes: u64,
    pub(crate) chunks: u32,
}

impl FilePlan {
    pub(crate) fn of_row(row: &serde_json::Value) -> Self {
        let size = row.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let chunks = row.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        Self::of_size(size, chunks)
    }

    /// A file of `size` plaintext bytes in `chunks` chunks (at least one: an empty file has one). Saturating: a
    /// hostile `size_bytes` never panics.
    pub(crate) fn of_size(size: u64, chunks: u32) -> Self {
        let chunks = chunks.max(1);
        Self {
            bytes: size.saturating_add(CHUNK_OVERHEAD.saturating_mul(chunks as u64)),
            chunks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review P3 (#142): a row whose size is near `u64::MAX` plans saturated instead of overflowing.
    #[test]
    fn a_huge_row_size_saturates() {
        assert_eq!(FilePlan::of_size(u64::MAX - 1, 2).bytes, u64::MAX);
        assert_eq!(FilePlan::of_size(5, 0), FilePlan { bytes: 33, chunks: 1 });
    }
}
