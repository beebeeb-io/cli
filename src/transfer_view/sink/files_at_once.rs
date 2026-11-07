//! The sink's side of several files at once (`--parallel`, spec §7): each file's share is kept under its key
//! (`TransferStats::next_file_as`), so the overall line, the per-file lines and "in flight" stay true across them.
//!
//! Owns: the keyed entry points, the run's limit across the files (`RunLimit`) and one file's view of the sink
//! (`FileSink`), shared by `bb push --parallel` and `bb sync`. Must not: draw (the ticker does).

use std::collections::BTreeMap;

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

/// The limit the live view shows while several files are in flight: `min(N, Σ used)` over them.
pub(crate) struct RunLimit {
    asked: u32,
    used: Mutex<BTreeMap<u32, u32>>,
}

impl RunLimit {
    pub(crate) fn new(asked: u32) -> Self {
        Self {
            asked,
            used: Mutex::new(BTreeMap::new()),
        }
    }

    fn set(&self, key: u32, used: u32, live: &LiveProgress) {
        let mut all = self.used.lock().unwrap_or_else(|e| e.into_inner());
        all.insert(key, used);
        live.set_limit(self.asked.min(all.values().sum()));
    }

    /// A file ended: the others' limit, if any are left (else the next file sets it).
    pub(crate) fn ended(&self, key: u32, live: &LiveProgress) {
        let mut all = self.used.lock().unwrap_or_else(|e| e.into_inner());
        all.remove(&key);
        if !all.is_empty() {
            live.set_limit(self.asked.min(all.values().sum()));
        }
    }
}

/// The run's sink as one file sees it: its attempts count against its own share (its key), its `used` feeds the
/// run's limit; everything else is the run's.
pub(crate) struct FileSink<'a> {
    pub(crate) live: &'a LiveProgress,
    pub(crate) key: u32,
    pub(crate) limit: &'a RunLimit,
}

impl ChunkProgress for FileSink<'_> {
    fn begin_file(&self, file_name: &str, expected_ciphertext: u64) -> Box<dyn FileProgress> {
        self.begin_file_chunks(file_name, expected_ciphertext, 0, 0)
    }

    fn begin_file_chunks(
        &self,
        file_name: &str,
        expected_ciphertext: u64,
        chunk_count: u32,
        chunk_size: u64,
    ) -> Box<dyn FileProgress> {
        self.live
            .begin_file_chunks_as(self.key, file_name, expected_ciphertext, chunk_count, chunk_size)
    }

    fn rollback(&self, ciphertext_bytes: u64) {
        self.live.rollback(ciphertext_bytes);
    }

    fn rollback_tally(&self, file_name: &str, tally: Tally) {
        self.live.rollback_tally(file_name, tally);
    }

    fn before_complete(&self, path: &std::path::Path) -> Result<(), String> {
        self.live.before_complete(path)
    }

    fn set_limit(&self, max_in_flight: u32) {
        self.limit.set(self.key, max_in_flight, self.live);
    }

    fn note(&self, line: &str, to: Stream) {
        self.live.note(line, to);
    }

    fn suspend(&self, f: &mut dyn FnMut()) {
        self.live.suspend(f);
    }
}
