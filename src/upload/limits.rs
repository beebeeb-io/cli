//! How many chunks of one file `bb push` sends at once, and the byte budget every chunk-sized buffer of an
//! upload is charged to (spec §4.2): the read buffer for the encryptor's life, each seal at twice the chunk
//! (the frame and the sealing copy) until it returns, then the frame alone until its PUT settles. The shares
//! held equal the chunk-sized bytes held, so "never more than 640 MiB" is a property of the code.
//!
//!
//! Across the files of a folder (spec §7, PR 2) the one budget is shared: an upload attempt is admitted when its
//! read buffer and one seal fit (R + 2 C, [`ChunkLimits::admit`]) and keeps them until it ends; its seals draw
//! from the pool first and from that reserve when the pool is short, so every admitted file can always seal its
//! next chunk and no two files can deadlock on the budget. The chunks in flight across every file of the push
//! hold one of `asked` slots ([`ChunkLimits::chunk_slot`]); thumbnails take one turn at a time.
//!
//! Owns: [`UPLOAD_BUFFER_BUDGET`], [`used_in_flight`], [`ChunkLimits`], [`Admission`], [`BufferShare`].
//! Must not: schedule chunks (that is `pipeline::run_pipeline`).
//! Tests: `limits/tests.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Today's worst moment at one chunk at a time: 5 × 128 MiB (read buffer, one in flight, one in the channel,
/// one being sealed with its copy).
pub(crate) const UPLOAD_BUFFER_BUDGET: u64 = 640 * 1024 * 1024;

/// Core's smallest chunk: a smaller (or empty) file's chunk counts as this.
pub(crate) const MIN_CHUNK: u64 = 4 * 1024 * 1024;

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
    /// One per chunk in flight, across every file sharing these limits (spec §7: ≤ `asked`).
    slots: Arc<Semaphore>,
    /// Thumbnails run one at a time across the files sharing these limits (spec §7).
    thumbnail_turn: tokio::sync::Mutex<()>,
}

impl ChunkLimits {
    pub(crate) fn new(asked: u32) -> Arc<Self> {
        let asked = asked.max(1);
        Arc::new(Self {
            asked,
            budget: Arc::new(Semaphore::new(UPLOAD_BUFFER_BUDGET as usize)),
            held: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            slots: Arc::new(Semaphore::new(asked as usize)),
            thumbnail_turn: tokio::sync::Mutex::new(()),
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
        self.charged(permit, bytes)
    }

    /// A share of `bytes` taken from the pool now, or `None` when the pool is short (it never waits).
    fn try_share(self: &Arc<Self>, bytes: u64) -> Option<BufferShare> {
        let permit = Arc::clone(&self.budget)
            .try_acquire_many_owned(bytes.min(UPLOAD_BUFFER_BUDGET) as u32)
            .ok()?;
        Some(self.charged(permit, bytes.min(UPLOAD_BUFFER_BUDGET)))
    }

    /// Wrap `permit` (bytes of the pool) as a share counted in `held`.
    fn charged(self: &Arc<Self>, permit: OwnedSemaphorePermit, bytes: u64) -> BufferShare {
        let held = self.held.fetch_add(bytes, Ordering::SeqCst) + bytes;
        self.peak.fetch_max(held, Ordering::SeqCst);
        BufferShare {
            limits: Arc::clone(self),
            permit: Some(permit),
            bytes,
            lent_by: None,
        }
    }

    /// Spec §7: admit one upload attempt of `chunk_size` chunks: wait (in arrival order) until its read buffer and
    /// one seal fit the pool, then hold them until the attempt ends. A waiting attempt holds no budget, and every
    /// admitted one can always seal its next chunk (from its reserve), so files never deadlock on the budget.
    pub(crate) async fn admit(self: &Arc<Self>, chunk_size: u64) -> Admission {
        let chunk = chunk_size.clamp(1, UPLOAD_BUFFER_BUDGET / 3);
        let mut whole = self.share(3 * chunk).await;
        let read = whole.split_off(chunk);
        Admission {
            _read: read,
            reserve: Arc::new(Reserve {
                lend: Arc::new(Semaphore::new((2 * chunk) as usize)),
                _backing: whole,
            }),
        }
    }

    /// A slot for one chunk in flight, held until its PUT settles (spec §7: at most `asked` across the files).
    pub(crate) async fn chunk_slot(&self) -> OwnedSemaphorePermit {
        Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .expect("the slot semaphore is never closed")
    }

    /// Spec §7: one thumbnail at a time across the files, its source read charged to the budget (its 64 MiB cap).
    /// Hold both until the thumbnail is sent.
    pub(crate) async fn thumbnail_turn(
        self: &Arc<Self>,
        source_cap: u64,
    ) -> (tokio::sync::MutexGuard<'_, ()>, BufferShare) {
        let turn = self.thumbnail_turn.lock().await;
        (turn, self.share(source_cap).await)
    }

    /// The bytes every live share holds now (a test instrument).
    #[cfg(test)]
    pub(crate) fn held_bytes(&self) -> u64 {
        self.held.load(Ordering::SeqCst)
    }

    /// The most [`held_bytes`](Self::held_bytes) ever was (a test instrument).
    #[cfg(test)]
    pub(crate) fn peak_held_bytes(&self) -> u64 {
        self.peak.load(Ordering::SeqCst)
    }
}

/// One upload attempt's place in the budget (spec §7): its read buffer, held for the encryptor's life, and a
/// reserve of one seal that its seals fall back on when the pool is short. Dropped when the attempt ends; the
/// reserve's bytes go back to the pool once its last lent frame has settled too.
pub(crate) struct Admission {
    _read: BufferShare,
    reserve: Arc<Reserve>,
}

/// One seal's worth of the budget (2 C), set aside for one attempt and lent to its seals.
struct Reserve {
    /// The reserve's bytes, lent out by permit.
    lend: Arc<Semaphore>,
    /// Charged to the budget for as long as the reserve or a share it lent lives.
    _backing: BufferShare,
}

impl Admission {
    /// The bytes of one seal (`2 C`, the frame and the sealing copy): from the pool when it has them now, else
    /// whichever frees first, the pool or this attempt's reserve.
    pub(crate) async fn seal_share(&self, limits: &Arc<ChunkLimits>, bytes: u64) -> BufferShare {
        if let Some(share) = limits.try_share(bytes) {
            return share;
        }
        let lend = Arc::clone(&self.reserve.lend);
        tokio::select! {
            biased;
            share = limits.share(bytes) => share,
            permit = lend.acquire_many_owned(bytes as u32) => BufferShare {
                limits: Arc::clone(limits),
                permit: Some(permit.expect("a reserve is never closed")),
                bytes,
                lent_by: Some(Arc::clone(&self.reserve)),
            },
        }
    }
}

/// Budget held for one buffer; returned when dropped.
pub(crate) struct BufferShare {
    limits: Arc<ChunkLimits>,
    permit: Option<OwnedSemaphorePermit>,
    bytes: u64,
    /// `Some`: lent by an attempt's reserve, whose bytes are already charged to the budget (not counted again in
    /// `held`); the reserve lives until this share is dropped.
    lent_by: Option<Arc<Reserve>>,
}

impl BufferShare {
    /// Keep `bytes` of this share and return the rest at once (a seal's copy is freed, its frame stays). A frame
    /// lent by a reserve moves to the pool when the pool has its bytes now, so the whole reserve is free for the
    /// attempt's next seal (the worst moments of spec §4.2 stay what they were).
    pub(crate) fn shrink_to(&mut self, bytes: u64) {
        if bytes >= self.bytes {
            return;
        }
        if self.lent_by.is_some()
            && let Some(pooled) = self.limits.try_share(bytes)
        {
            // Dropping the lent share returns all of it to the reserve.
            *self = pooled;
            return;
        }
        let rest = self.bytes - bytes;
        // Counted down before the bytes go back, so a share granted from them never shows on top of them.
        if self.lent_by.is_none() {
            self.limits.held.fetch_sub(rest, Ordering::SeqCst);
        }
        self.bytes = bytes;
        if let Some(permit) = self.permit.as_mut() {
            drop(permit.split(rest as usize));
        }
    }

    /// Split `bytes` off into a share of their own (both stay charged to the budget).
    fn split_off(&mut self, bytes: u64) -> BufferShare {
        let bytes = bytes.min(self.bytes);
        self.bytes -= bytes;
        BufferShare {
            limits: Arc::clone(&self.limits),
            permit: self.permit.as_mut().and_then(|p| p.split(bytes as usize)),
            bytes,
            lent_by: self.lent_by.clone(),
        }
    }
}

impl Drop for BufferShare {
    fn drop(&mut self) {
        if self.lent_by.is_none() {
            self.limits.held.fetch_sub(self.bytes, Ordering::SeqCst);
        }
        self.permit.take();
    }
}

#[cfg(test)]
mod tests;
