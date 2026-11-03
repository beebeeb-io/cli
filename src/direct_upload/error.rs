//! Why a chunk did not go through the direct route, and what the upload driver
//! should do about it ([`DirectUploadError`]).
//!
//! Owns: the four outcomes the driver matches on — fall back to the API for the
//! whole file, let session recovery handle a gone session, stop on an
//! authorization answer, or stop because the run was interrupted.
//! Must not: classify an HTTP answer itself (that is
//! `beebeeb_core::direct_transfer`).

/// Why a chunk did not go through the direct route.
#[derive(Debug)]
pub(crate) enum DirectUploadError {
    /// This session cannot finish direct: abandon it and init again without the token.
    UseApi,
    /// The upload session is gone; the driver's session recovery owns it.
    SessionGone(String),
    /// An authorization answer: do not fall back (the API would refuse too).
    Terminal { status: u16, message: String },
    /// The run was interrupted while the chunk waited.
    Interrupted,
}
