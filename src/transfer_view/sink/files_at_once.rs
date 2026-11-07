//! The sink's side of several files at once (`--parallel`, spec §7): each file's share is kept under its key
//! (`TransferStats::next_file_as`), so the overall line, the per-file lines and "in flight" stay true across them.
//!
//! Owns: the keyed entry points. Must not: draw (the ticker does).

use super::*;

impl LiveProgress {
    /// [`next_file`](LiveProgress::next_file) for the file under `key`.
    pub(crate) fn next_file_as(&self, key: u32, bytes: u64, chunks: u32) {
        lock(&self.shared.model).next_file_as(key, bytes, chunks);
    }

    /// [`end_file`](LiveProgress::end_file) for the file under `key`.
    pub(crate) fn end_file_as(&self, key: u32, end: FileEnd) {
        lock(&self.shared.model).end_file_as(key, end);
    }

    /// `begin_file_chunks` for the file under `key`: its attempt counts against its own share.
    pub(crate) fn begin_file_chunks_as(
        &self,
        key: u32,
        file_name: &str,
        expected_ciphertext: u64,
        chunk_count: u32,
        chunk_size: u64,
    ) -> Box<dyn FileProgress> {
        let slot = lock(&self.shared.model).begin_file_as(key, file_name, expected_ciphertext, chunk_count, chunk_size);
        Box::new(LiveFile {
            slot,
            shared: Arc::clone(&self.shared),
            next_index: AtomicU32::new(0),
        })
    }
}
