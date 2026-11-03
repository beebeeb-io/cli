use super::{ConflictPolicy, Direction, SyncOptions, decide};

/// The conflict table: one side changed moves that way; both changed is a
/// conflict unless the policy forces the local copy.
#[test]
fn the_conflict_table() {
    let skip = ConflictPolicy::Skip;
    let force = ConflictPolicy::Force;
    assert_eq!(decide(false, false, skip), Direction::Skip);
    assert_eq!(decide(true, false, skip), Direction::Upload);
    assert_eq!(decide(false, true, skip), Direction::Download);
    assert_eq!(decide(true, true, skip), Direction::Conflict);
    assert_eq!(decide(true, true, force), Direction::Upload);
    // Force does not change the one-sided cases.
    assert_eq!(decide(true, false, force), Direction::Upload);
    assert_eq!(decide(false, true, force), Direction::Download);
}

#[test]
fn watch_options_are_the_plain_continuous_pass() {
    let o = SyncOptions::watch();
    assert!(!o.dry_run && !o.force && !o.delete_remote && !o.once);
    assert!(!o.rehash && !o.allow_mass_delete && !o.rebind);
    assert_eq!(o.concurrency, 4);
}
