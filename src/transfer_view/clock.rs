//! The sink's clock: milliseconds since the first call, from a monotonic
//! `Instant` (the model takes time as a number, spec §4.1).

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

pub(crate) fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}
