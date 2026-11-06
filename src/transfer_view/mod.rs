//! The CLI side of the transfer numbers (task 2333, spec §6): the lines a
//! terminal shows, and (Task 4) the sink that draws them.
//!
//! Owns: formatting and drawing. Must not: compute a number (`transfer_stats`).

pub(crate) mod line;
pub(crate) mod units;

#[cfg(test)]
mod tests;
