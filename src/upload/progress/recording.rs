//! Test-only: a progress sink that records every event the drivers send as one
//! line, so a driver test reads what happened in order (task 2333).
//!
//! Lines: `begin <name> <expected> <chunks> <chunk_size>`, `start <i> <route>`,
//! `retry <i>`, `done <i> <bytes> <held>`, `body <chunks> <bytes> <route>`,
//! `settle <i>`, `pool <id8|-> <country|->`, `finish <ok>`,
//! `rollback <bytes> <chunks> <already_stored>`, `limit <n>`,
//! `note <out|err> <line>`. A legacy call the drivers should no longer make is
//! recorded too (`confirmed <bytes>`, `rollback-bytes <bytes>`,
//! `begin-legacy <name>`), so a test can assert it is absent.

use std::sync::{Arc, Mutex};

use super::{ChunkProgress, FileProgress, Held, PoolLabel, Route, Stream, Tally};

#[derive(Default, Clone)]
pub(crate) struct Recording {
    events: Arc<Mutex<Vec<String>>>,
}

fn route(r: Route) -> &'static str {
    match r {
        Route::Direct => "direct",
        Route::Api => "api",
    }
}

fn held(h: Held) -> &'static str {
    match h {
        Held::Transferred => "transferred",
        Held::AlreadyStored => "already_stored",
    }
}

impl Recording {
    fn push(&self, line: String) {
        self.events.lock().unwrap().push(line);
    }

    pub(crate) fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    /// The events whose first word is `kind`.
    pub(crate) fn of(&self, kind: &str) -> Vec<String> {
        self.events()
            .into_iter()
            .filter(|e| e.split(' ').next() == Some(kind))
            .collect()
    }

    /// The most chunks in flight at once, replayed from the start/settle lines.
    pub(crate) fn peak(&self) -> i64 {
        let (mut now, mut peak) = (0i64, 0i64);
        for e in self.events() {
            match e.split(' ').next() {
                Some("start") => now += 1,
                Some("settle") => now -= 1,
                _ => {}
            }
            peak = peak.max(now);
        }
        peak
    }

    /// Chunks started minus chunks settled: 0 once every transfer has ended.
    pub(crate) fn in_flight(&self) -> i64 {
        self.of("start").len() as i64 - self.of("settle").len() as i64
    }

    /// Chunks counted done, net of every rollback.
    pub(crate) fn net_chunks(&self) -> i64 {
        let rolled_back: i64 = self
            .of("rollback")
            .iter()
            .map(|e| e.split(' ').nth(2).unwrap().parse::<i64>().unwrap())
            .sum();
        self.of("done").len() as i64 - rolled_back
    }
}

impl ChunkProgress for Recording {
    fn begin_file(&self, file_name: &str, _expected_ciphertext: u64) -> Box<dyn FileProgress> {
        self.push(format!("begin-legacy {file_name}"));
        Box::new(self.clone())
    }

    fn begin_file_chunks(
        &self,
        file_name: &str,
        expected_ciphertext: u64,
        chunk_count: u32,
        chunk_size: u64,
    ) -> Box<dyn FileProgress> {
        self.push(format!(
            "begin {file_name} {expected_ciphertext} {chunk_count} {chunk_size}"
        ));
        Box::new(self.clone())
    }

    fn rollback(&self, ciphertext_bytes: u64) {
        self.push(format!("rollback-bytes {ciphertext_bytes}"));
    }

    fn rollback_tally(&self, _file_name: &str, tally: Tally) {
        self.push(format!(
            "rollback {} {} {}",
            tally.bytes, tally.chunks, tally.already_stored
        ));
    }

    fn set_limit(&self, max_in_flight: u32) {
        self.push(format!("limit {max_in_flight}"));
    }

    fn note(&self, line: &str, to: Stream) {
        let to = match to {
            Stream::Stdout => "out",
            Stream::Stderr => "err",
        };
        self.push(format!("note {to} {line}"));
    }
}

impl FileProgress for Recording {
    fn chunk_confirmed(&self, ciphertext_bytes: u64) {
        self.push(format!("confirmed {ciphertext_bytes}"));
    }

    fn finish(self: Box<Self>, success: bool) {
        self.push(format!("finish {success}"));
    }

    fn chunk_started(&self, index: u32, r: Route) {
        self.push(format!("start {index} {}", route(r)));
    }

    fn chunk_retried(&self, index: u32) {
        self.push(format!("retry {index}"));
    }

    fn chunk_done(&self, index: u32, bytes: u64, h: Held) {
        self.push(format!("done {index} {bytes} {}", held(h)));
    }

    fn body_done(&self, chunks: u32, bytes: u64, r: Route) {
        self.push(format!("body {chunks} {bytes} {}", route(r)));
    }

    fn chunk_settled(&self, index: u32) {
        self.push(format!("settle {index}"));
    }

    fn pool(&self, label: PoolLabel) {
        let id = label.id8().unwrap_or_else(|| "-".to_string());
        let country = label.country.unwrap_or_else(|| "-".to_string());
        self.push(format!("pool {id} {country}"));
    }
}
