//! Several files of one push in flight at once (`--parallel`, spec §7, PR 2): each has its own share of the
//! totals under a key its caller chose (the file's index in the push), where [`TransferStats::next_file`] and
//! [`TransferStats::end_file`] keep one. The rules are theirs: the keyed calls lend the share to them.
//!
//! Owns: the keyed shares. Must not: change how one file at a time is counted.

use super::*;

/// One file's share, and the last attempt begun under it.
#[derive(Clone, Copy)]
pub(super) struct KeyedShare {
    share: Share,
    last: Option<FileSlot>,
}

impl TransferStats {
    /// [`next_file`](Self::next_file) for the file under `key`, while other files are in flight.
    pub(crate) fn next_file_as(&mut self, key: u32, bytes: u64, chunks: u32) {
        let share = Share {
            bytes,
            chunks,
            done_at_start: 0,
        };
        self.at_once.insert(key, KeyedShare { share, last: None });
    }

    /// [`begin_file`](Self::begin_file) for the file under `key`: its own share follows what the driver read.
    pub(crate) fn begin_file_as(
        &mut self,
        key: u32,
        name: &str,
        bytes: u64,
        chunk_count: u32,
        chunk_size: u64,
    ) -> FileSlot {
        let outer = std::mem::replace(&mut self.share, self.at_once.get(&key).map(|k| k.share));
        let slot = self.begin_file(name, bytes, chunk_count, chunk_size);
        if let (Some(keyed), Some(share)) = (self.at_once.get_mut(&key), self.share) {
            keyed.share = share;
            keyed.last = Some(slot);
        }
        self.share = outer;
        slot
    }

    /// [`end_file`](Self::end_file) for the file under `key`. What it sent is its last attempt's (an earlier
    /// attempt's count was rolled back), not the run's progress since it was announced: other files moved too.
    pub(crate) fn end_file_as(&mut self, key: u32, end: FileEnd) {
        let Some(keyed) = self.at_once.remove(&key) else { return };
        let sent = keyed.last.map_or(0, |slot| self.files[slot.0 as usize].bytes_done);
        let share = Share {
            done_at_start: self.bytes_done.saturating_sub(sent),
            ..keyed.share
        };
        let outer = self.share.replace(share);
        self.end_file(end);
        self.share = outer;
    }
}
