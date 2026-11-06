//! The CLI side of the transfer numbers (task 2333, spec §6): the lines a
//! terminal shows, and the sink that draws them.
//!
//! Owns: formatting and drawing. Must not: compute a number (`transfer_stats`).

pub(crate) mod clock;
pub(crate) mod line;
pub(crate) mod sink;
pub(crate) mod units;

pub(crate) use sink::{LiveProgress, ViewOptions};

#[cfg(test)]
mod sink_tests;
#[cfg(test)]
mod tests;
