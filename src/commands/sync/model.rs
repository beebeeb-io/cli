
/// What `bb sync` should do this invocation. The control modes return before
/// any tree work; `Run` is the one-shot or watch sync.
#[derive(Clone, Debug)]
pub enum SyncCommand {
    Run,
    Status,
    StopSession(String),
    StopAll,
    StopLegacy,
    Daemon,
}

/// The knobs that shape a sync pass, grouped so a call site names the policy
/// instead of a row of bare booleans.
#[derive(Clone, Debug)]
pub struct SyncOptions {
    pub dry_run: bool,
    pub force: bool,
    pub delete_remote: bool,
    pub once: bool,
    pub rehash: bool,
    pub allow_mass_delete: bool,
    pub rebind: bool,
    pub concurrency: usize,
}

impl SyncOptions {
    /// The `bb watch` redirect: a continuous pass with no flags.
    pub fn watch() -> Self {
        Self {
            dry_run: false,
            force: false,
            delete_remote: false,
            once: false,
            rehash: false,
            allow_mass_delete: false,
            rebind: false,
            concurrency: 4,
        }
    }
}

/// Which side a changed file moves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Skip,
    Upload,
    Download,
    Conflict,
}

/// What to do when both sides changed since the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConflictPolicy {
    Skip,
    Force,
}

/// The one conflict rule: a file changed on one side moves that way; changed
/// on both is a conflict unless the policy forces the local copy.
pub(super) fn decide(local_changed: bool, remote_changed: bool, policy: ConflictPolicy) -> Direction {
    match (local_changed, remote_changed) {
        (false, false) => Direction::Skip,
        (true, false) => Direction::Upload,
        (false, true) => Direction::Download,
        (true, true) => match policy {
            ConflictPolicy::Force => Direction::Upload,
            ConflictPolicy::Skip => Direction::Conflict,
        },
    }
}
