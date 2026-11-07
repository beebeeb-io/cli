//! Stage 2 of a folder push (`--parallel` spec §7): up to N prepared files upload at once, started in file order,
//! sharing the run's one limit: chunks in flight across all files ≤ N (the push's chunk slots), each file's own ≤
//! its `used`, every buffer within the one budget (each attempt admitted, `ChunkLimits::admit`). On admission a
//! file takes its upload lock and re-checks under it what stage 1 saw. One failed file never stops the others; a
//! Ctrl-C starts no new file and ends the ones in flight.
//!
//! The live view keeps one overall line and a line per file in flight (2333's renderer draws every active file),
//! "chunks in flight" counts across the files, and the limit it shows is `min(N, Σ used)` of the files in flight.
//!
//! Owns: `upload_all`, the per-file sink. Must not: prepare a file (`push_file::prepare`) or print the summary.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;

use super::push::{PushCtl, Pushed};
use super::push_file::{self, Ready};
use super::push_report::FilePlan;
use crate::api::ApiClient;
use crate::transfer_stats::{FileEnd, Tally};
use crate::transfer_view::LiveProgress;
use crate::upload::{ChunkProgress, FileProgress, Stream};

/// One prepared file of the folder: its index in the push (input order, and its key in the live view), its path
/// relative to the pushed folder, its plan.
pub(super) struct Admit {
    pub(super) index: u32,
    pub(super) rel: String,
    pub(super) plan: FilePlan,
    pub(super) ready: Box<Ready>,
}

/// How one admitted file ended.
pub(super) struct Ended {
    pub(super) index: u32,
    pub(super) rel: String,
    pub(super) result: Result<Pushed, String>,
}

/// Upload `files` (in input order), at most `ctl.parallel` at once. Returns how each admitted file ended (in finish
/// order) and how many were never started (a Ctrl-C came first).
pub(super) async fn upload_all(
    api: &ApiClient,
    master_key: Arc<beebeeb_core::kdf::MasterKey>,
    files: Vec<Admit>,
    ctl: &PushCtl,
    progress: &LiveProgress,
) -> (Vec<Ended>, usize) {
    let limit = RunLimit::new(ctl.parallel);
    let mut queue = files.into_iter();
    let mut running = FuturesUnordered::new();
    let mut ended = Vec::new();
    loop {
        while running.len() < ctl.parallel as usize && !ctl.interrupt.is_set() {
            let Some(file) = queue.next() else { break };
            running.push(upload_one(api, Arc::clone(&master_key), file, ctl, progress, &limit));
        }
        match running.next().await {
            Some(done) => ended.push(done),
            None => break,
        }
    }
    (ended, queue.len())
}

/// One admitted file: its lock, the re-check under it, the upload, its share of the run settled.
async fn upload_one(
    api: &ApiClient,
    master_key: Arc<beebeeb_core::kdf::MasterKey>,
    file: Admit,
    ctl: &PushCtl,
    progress: &LiveProgress,
    limit: &RunLimit,
) -> Ended {
    let Admit {
        index,
        rel,
        plan,
        ready,
    } = file;
    plan.announce_as(progress, index);
    let sink = FileSink {
        live: progress,
        key: index,
        limit,
    };
    let result = async {
        let _lock = push_file::lock(&ready.path)?;
        push_file::recheck(api, &master_key, &ready).await?;
        push_file::upload(api, master_key, *ready, &sink, ctl).await
    }
    .await;
    if let Err(e) = &result
        && !(super::push::was_interrupted(e) && ctl.interrupt.is_set())
    {
        super::push_report::note_failure(progress, &rel, e);
    }
    limit.ended(index, progress);
    progress.end_file_as(
        index,
        match &result {
            Ok(Pushed::Uploaded(_)) => FileEnd::Uploaded,
            Ok(Pushed::AlreadyUploaded | Pushed::Skipped) => FileEnd::NotSent,
            Err(_) => FileEnd::Failed,
        },
    );
    Ended { index, rel, result }
}

/// The limit the live view shows while several files are in flight: `min(N, Σ used)` over them.
struct RunLimit {
    asked: u32,
    used: Mutex<BTreeMap<u32, u32>>,
}

impl RunLimit {
    fn new(asked: u32) -> Self {
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
    fn ended(&self, key: u32, live: &LiveProgress) {
        let mut all = self.used.lock().unwrap_or_else(|e| e.into_inner());
        all.remove(&key);
        if !all.is_empty() {
            live.set_limit(self.asked.min(all.values().sum()));
        }
    }
}

/// The run's sink as one file sees it: its attempts count against its own share (its key), its `used` feeds the
/// run's limit; everything else is the run's.
struct FileSink<'a> {
    live: &'a LiveProgress,
    key: u32,
    limit: &'a RunLimit,
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
