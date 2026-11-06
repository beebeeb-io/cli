//! `bb backup`: back up folders of a headless machine (a Linux server or NAS)
//! into `Backups/<device>/<folder>`, read in place, with core's rules (LD-0113,
//! spec Amendment 8).
//!
//! The pass is `beebeeb_backup::run_pass`; the CLI is its `BackupHost`
//! ([`host`]): its server calls carry `X-Beebeeb-Origin: backup`, its uploader
//! is the CLI's own streaming driver, its records live in the CLI's state dir
//! ([`state`]). The device is the machine: its id is core's
//! `device_id_from_machine_id` of `/etc/machine-id`, and its folder is named
//! by core from the pretty hostname ([`identity`]).
//!
//! Owns: the machine's identity, which local folders are never backed up (the
//! CLI's own state, sync and mount folders), the store and the host.
//! Must not: decide what a pass uploads, moves or marks (core's), or change a
//! source file in any way.

pub(crate) mod host;
pub(crate) mod identity;
pub(crate) mod remote;
pub(crate) mod state;

/// What one run queues at most per folder; the rest comes in the next run
/// (core's plan is idempotent).
pub(crate) const PASS_BUDGET: beebeeb_core::backup::PassBudget = beebeeb_core::backup::PassBudget {
    max_uploads: 2_000,
    max_bytes: 20 * 1024 * 1024 * 1024,
};

/// Milliseconds since the epoch, now.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
