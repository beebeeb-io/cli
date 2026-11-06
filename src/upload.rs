//! Streaming, constant-memory chunk upload — the one upload driver shared by
//! `bb sync` (parallel, file-level) and `bb push` (sequential).
//!
//! ## Why this exists
//!
//! The previous path (`std::fs::read` the whole file → encrypt *every* chunk
//! into a `Vec` → sequential PUT) peaked at ~2× file size in RAM per file, and
//! under `bb sync`'s `buffer_unordered(concurrency)` that became ~`8×` the
//! largest file. It also showed no progress. This module replaces it with a
//! look-ahead pipeline whose peak memory is bounded by the chunk size, not the
//! file size.
//!
//! ## Pipeline
//!
//! ```text
//!   producer (spawn_blocking, owns ChunkEncryptor)
//!     → next_chunk()  [AES off the async reactor]
//!     → bounded mpsc (cap 1) ── backpressure ──>  consumer (async)
//!                                                   → PUT /files/{id}/chunks/{i}
//!                                                   → progress.chunk_confirmed()
//! ```
//!
//! The bounded channel gives natural look-ahead: while the consumer PUTs chunk
//! N, the producer has already encrypted chunk N+1 (buffered) and is encrypting
//! N+2. Peak ≈ `~4 × chunk_size` per file (read buffer + channel-held ciphertext
//! + in-flight blocking chunk + PUT body), **never** file-size-proportional.
//!
//! ## Key handling
//!
//! The master key is used only synchronously on the async side (to encrypt the
//! name, build the encryptor, and derive the thumbnail key). The
//! `FileKey`-owning [`ChunkEncryptor`](beebeeb_core::chunk_stream::ChunkEncryptor)
//! is the only thing moved into the blocking closure — raw `MasterKey` bytes are
//! never copied (it is held behind an `Arc`).
//!
//! ## Cancellation
//!
//! `spawn_blocking` cannot be aborted mid-chunk, so a shared `AtomicBool`
//! shutdown flag is checked at the top of every producer iteration and at the
//! top of every consumer iteration. On any early exit the consumer **always**
//! `rx.close()`s before awaiting the producer; closing the receiver wakes a
//! producer parked on the cap-1 channel, so the pipeline can never deadlock.
//!
//! Owns: the upload contract ([`UploadSpec`], [`UploadOutcome`]), progress
//! sinks, session selection/retry, and the pipeline itself.
//! Must not: decide sync conflicts or trash replaced files (see `commands/sync`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;

use beebeeb_core::chunk_stream::{ChunkEncryptor, EncryptedChunk, ExpectedChunkFormat, SessionResume};
use beebeeb_core::kdf::MasterKey;
use beebeeb_types::ChunkProfile;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use uuid::Uuid;

use crate::api::{ApiClient, DirectOffer};

// ── Public surface ──────────────────────────────────────────────────────────

mod model;
use model::*;
pub use model::{INTERRUPTED, ReadRow, UploadOutcome, UploadSpec};

mod error;
pub use error::UploadError;

// ── No-op progress (used for --json / --quiet / non-TTY and the watch loops) ──
// ── indicatif progress (rich + TTY only) ──────────────────────────────────────

mod progress;
use progress::*;
pub use progress::{BarProgress, ChunkProgress, FileProgress, NoopProgress};

mod session;
use session::*;

// ── The streaming upload driver ───────────────────────────────────────────────

mod pipeline;
pub(crate) use pipeline::row_parent;
pub use pipeline::{expected_ciphertext_for, stream_encrypt_upload};
