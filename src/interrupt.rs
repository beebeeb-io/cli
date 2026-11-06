//! One awaitable interrupt for a whole `bb push` (spec §5.1): the shared flag today's checks read (the
//! producer's loop, the direct route's sliced waits), plus a [`Notify`] every wait can select on, so a
//! Ctrl-C drops the chunks in flight and ends a rate-limit pause at once.
//!
//! Owns: [`Interrupt`]. Must not: install a signal handler (the command does) or decide what stops.
//! Tests: `interrupt/tests.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Set once (Ctrl-C), seen by every clone: [`is_set`](Self::is_set) for a check, [`wait`](Self::wait) for a
/// `select!`.
#[derive(Clone)]
pub(crate) struct Interrupt {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Interrupt {
    pub(crate) fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(crate) fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Returns once the interrupt is set: at once if it already is. Registers before it reads the flag, so a
    /// `set` between the two is never missed.
    pub(crate) async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }

    /// The flag itself: what the upload driver's existing checks read (`UploadSpec::shutdown`).
    pub(crate) fn flag(&self) -> &Arc<AtomicBool> {
        &self.flag
    }
}

#[cfg(test)]
mod tests;
