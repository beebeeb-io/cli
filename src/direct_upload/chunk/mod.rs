//! The direct transport for one chunk: ask the API for a grant, `PUT` the
//! ciphertext to the storage link, commit it — following the core policy after
//! every failure.
//!
//! Owns: `upload_chunk` and the shutdown-aware `wait` that makes a long
//! `Retry-After` interruptible.
//! Must not: print a link, retry past what `direct_transfer` allows, or decide
//! to abandon the session's transport (the driver does, on [`DirectUploadError`]).
//! Tests: `tests.rs` in this directory drives the real `stream_encrypt_upload`
//! against a scripted API and a storage server.

use super::*;

/// How often a waiting chunk looks at the shutdown flag.
pub(super) const WAIT_SLICE: Duration = Duration::from_millis(250);

/// Sleep `total`, giving up early (`false`) when the run is being shut down.
pub(super) async fn wait(total: Duration, shutdown: &AtomicBool) -> bool {
    let mut left = total;
    while !left.is_zero() {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        let slice = left.min(WAIT_SLICE);
        tokio::time::sleep(slice).await;
        left -= slice;
    }
    !shutdown.load(Ordering::Relaxed)
}

/// Send one chunk through grant -> PUT -> commit, following the core policy after every failure.
/// `Ok` means the server holds the chunk: [`Held::Transferred`] once a PUT of it was sent (committed now, or
/// `skipped` on a later grant because that PUT landed, even when its answer was lost), [`Held::AlreadyStored`]
/// when the first grant said `skipped` (a resumed session already held it: a grant that gave a link found it
/// missing). Every retry the policy orders is reported to `prog` (task 2333).
pub(crate) async fn upload_chunk(
    api: &ApiClient,
    discovery: &dt::Discovery,
    session: &crate::api::DirectUploadId,
    index: u32,
    data: Bytes,
    shutdown: &Arc<AtomicBool>,
    prog: &dyn FileProgress,
) -> Result<Held, DirectUploadError> {
    let size = data.len() as u64;
    // One random seed per chunk: the equal jitter of its backoff, so chunks that fail together do not retry together.
    let mut state = dt::UploadChunkState::with_seed(dt::UploadChunkPolicy::default(), rand::random());
    let mut call = dt::Call::UploadGrant;
    let mut put_sent = false;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Err(DirectUploadError::Interrupted);
        }
        let failure = match call {
            dt::Call::UploadGrant => match api.upload_grant(session.as_str(), index, size, discovery).await {
                UploadCall::Ok(dt::UploadGrant::Skipped) => {
                    return Ok(if put_sent {
                        Held::Transferred
                    } else {
                        Held::AlreadyStored
                    });
                }
                UploadCall::Ok(dt::UploadGrant::Link(link)) => {
                    state.link_issued();
                    let status = api.put_to_storage(&link, data.clone()).await;
                    put_sent = true;
                    if dt::put_stored(status) {
                        call = dt::Call::UploadCommit;
                        continue;
                    }
                    dt::UploadFailure::Put { status }
                }
                UploadCall::Failed(outcome) => dt::UploadFailure::Grant(outcome),
                UploadCall::SessionGone(message) => return Err(DirectUploadError::SessionGone(message)),
            },
            dt::Call::UploadCommit => match api.upload_commit(session.as_str(), index, size).await {
                UploadCall::Ok(_) => return Ok(Held::Transferred),
                UploadCall::Failed(outcome) => dt::UploadFailure::Commit(outcome),
                UploadCall::SessionGone(message) => return Err(DirectUploadError::SessionGone(message)),
            },
            // The policy only ever names these two calls.
            _ => return Err(DirectUploadError::UseApi),
        };
        match state.failed(&failure) {
            dt::UploadNext::Retry { after, call: next } => {
                prog.chunk_retried(index);
                if !wait(after, shutdown).await {
                    return Err(DirectUploadError::Interrupted);
                }
                call = next;
            }
            dt::UploadNext::UseApi => return Err(DirectUploadError::UseApi),
            dt::UploadNext::Terminal(t) => {
                // A dead session reads as it does on the proxy route and on a download: log in again.
                let message = match (t.status, t.error.as_deref()) {
                    (401, _) => crate::api::SESSION_EXPIRED_MESSAGE.to_string(),
                    (status, Some(code)) => format!("{code} (HTTP {status})"),
                    (status, None) => format!("HTTP {status}"),
                };
                return Err(DirectUploadError::Terminal {
                    status: t.status,
                    message,
                });
            }
        }
    }
}
#[cfg(test)]
mod tests;
