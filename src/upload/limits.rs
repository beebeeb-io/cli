//! How many chunks of one file `bb push` sends at once, and the byte budget every chunk-sized buffer of an
//! upload is charged to (spec §4.2): the read buffer for the encryptor's life, each seal at twice the chunk
//! (the frame and the sealing copy) until it returns, then the frame alone until its PUT settles. The shares
//! held equal the chunk-sized bytes held, so "never more than 640 MiB" is a property of the code.
//!
//! Owns: [`UPLOAD_BUFFER_BUDGET`], [`used_in_flight`], [`ChunkLimits`], [`BufferShare`].
//! Must not: schedule chunks (that is `pipeline::run_pipeline`).
//! Tests: `limits/tests.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Today's worst moment at one chunk at a time: 5 × 128 MiB (read buffer, one in flight, one in the channel,
/// one being sealed with its copy).
pub(crate) const UPLOAD_BUFFER_BUDGET: u64 = 640 * 1024 * 1024;

/// Core's smallest chunk: a smaller (or empty) file's chunk counts as this.
const MIN_CHUNK: u64 = 4 * 1024 * 1024;

/// The read buffer and one seal (2 chunks) sit beside the chunks in flight, so the next chunk can be sealed
/// while every worker is busy.
const BESIDE_IN_FLIGHT: u64 = 3;

/// `min(asked, max(1, budget / chunk − 3))`: the chunks of a file with `chunk_size` chunks that fit the budget
/// at once.
pub(crate) fn used_in_flight(asked: u32, chunk_size: u64) -> u32 {
    let fit = (UPLOAD_BUFFER_BUDGET / chunk_size.max(MIN_CHUNK))
        .saturating_sub(BESIDE_IN_FLIGHT)
        .max(1);
    asked.max(1).min(u32::try_from(fit).unwrap_or(u32::MAX))
}

/// One push's limit: how many chunks it was asked to send at once, and the budget its buffers share.
pub(crate) struct ChunkLimits {
    asked: u32,
    budget: Arc<Semaphore>,
    held: AtomicU64,
    peak: AtomicU64,
}

impl ChunkLimits {
    pub(crate) fn new(asked: u32) -> Arc<Self> {
        Arc::new(Self {
            asked: asked.max(1),
            budget: Arc::new(Semaphore::new(UPLOAD_BUFFER_BUDGET as usize)),
            held: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        })
    }

    /// One chunk at a time: what every caller but `bb push --parallel` gets.
    pub(crate) fn single() -> Arc<Self> {
        Self::new(1)
    }

    pub(crate) fn asked(&self) -> u32 {
        self.asked
    }

    /// Wait until `bytes` of the budget are free, then hold them until the share is dropped (or shrunk). A
    /// share is never larger than the budget.
    pub(crate) async fn share(self: &Arc<Self>, bytes: u64) -> BufferShare {
        let bytes = bytes.min(UPLOAD_BUFFER_BUDGET);
        let permit = Arc::clone(&self.budget)
            .acquire_many_owned(bytes as u32)
            .await
            .expect("the budget semaphore is never closed");
        let held = self.held.fetch_add(bytes, Ordering::SeqCst) + bytes;
        self.peak.fetch_max(held, Ordering::SeqCst);
        BufferShare {
            limits: Arc::clone(self),
            permit: Some(permit),
            bytes,
        }
    }

    /// The bytes every live share holds now.
    pub(crate) fn held_bytes(&self) -> u64 {
        self.held.load(Ordering::SeqCst)
    }

    /// The most [`held_bytes`](Self::held_bytes) ever was.
    pub(crate) fn peak_held_bytes(&self) -> u64 {
        self.peak.load(Ordering::SeqCst)
    }
}

/// Budget held for one buffer; returned when dropped.
pub(crate) struct BufferShare {
    limits: Arc<ChunkLimits>,
    permit: Option<OwnedSemaphorePermit>,
    bytes: u64,
}

impl BufferShare {
    /// Keep `bytes` of this share and return the rest at once (a seal's copy is freed, its frame stays).
    pub(crate) fn shrink_to(&mut self, bytes: u64) {
        if bytes >= self.bytes {
            return;
        }
        let rest = self.bytes - bytes;
        // Counted down before the bytes go back, so a share granted from them never shows on top of them.
        self.limits.held.fetch_sub(rest, Ordering::SeqCst);
        self.bytes = bytes;
        if let Some(permit) = self.permit.as_mut() {
            drop(permit.split(rest as usize));
        }
    }
}

impl Drop for BufferShare {
    fn drop(&mut self) {
        self.limits.held.fetch_sub(self.bytes, Ordering::SeqCst);
        self.permit.take();
    }
}

#[cfg(test)]
mod tests;
