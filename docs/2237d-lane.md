# 2237d CLI lane (tasks 7, 8, 9)

Owner: Codex d3-2237, 2026-10-28. Sequential, no child agents.
Worktree: /Users/guuslangelaar/code/bb-worktrees/d3-2237-cli
Branch: feat/2237d-cli-shared-folders
CLI baseline: eb97a5c9ed22b3b61cfd2a19f28904630292ebe2
Workspace contract: origin/main c2b3eb9dc537b6b228497e91b712f18ece72cf92,
plan 2026-10-28-shared-folders-p1d-desktop-cli, tasks 7-9 only;
approved spec 2026-10-27-shared-folder-link-uploads-design.
Binding: plan D lead review; plan C L1-L7, L6a/L7a.
Core base: a245a549c12415af6311bf4488deca5def65e983 (core main verified).

## Ownership and verification

Own Task 7 mapped CLI resolver/API/crypto/path/ls/search files, Task 8
transfer/pull files, then Task 9 sync/index files and their integration tests.
Do not edit other repository checkouts or server/core sources.

Verification, before implementation: each task's exact named integration binary
on Linux via remote-run.sh d3-2237; Task 7 6 tests, Task 8 6, Task 9 8.
Behavioral red (not compiler errors), green, counted logs. Run the plan's
share_resolves_like_pull, pull_path_traversal, download/direct/upload selections,
fmt/clippy and sync_never_deletes regressions remotely. No production or real
session/config; commands isolate HOME, pass loopback --api and set
BB_REFUSE_NON_LOOPBACK_API=1. Evidence:
/Users/guuslangelaar/code/bb-gate-logs/fullspeed/d3-2237/.
Commit every green step with counts and Codex trailer. One non-draft PR, never merge.
Handoff: PR/head, per-task red/green and checks, decisions/deviations/residuals.

## Decisions

- User's explicit core base overrides the older repo instruction requiring the
  server CORE_REV change in the same commit. Both CLI Cargo.toml revisions and
  Cargo.lock will pin the merged full core SHA; this lane does not own server.
- L6 custody opens only via core; L7 child envelopes bind shared ROOT/item/kind.
- Read-only resolver never adopts or registers custody. No key minting.

## Task 7 initial green, 2026-10-28

Red t7-red-initial.log: 6 executed, 1 passed / 5 failed.
Green t7-green-regressions.log: resolver 6 passed / 0 failed.
Regression and final task completion details will follow.
Initial green saved as WIP before extending compatibility coverage.

Task 7 complete: resolver 6/0; path containment 22/0; share/path parity 1/0,
t7-verified-green.log. Request-branch mutation 5/1, restored 6/0
(t7-compat-red.log). Negative zero-test count fixture rejected 1/1.
Decision: when L6 custody is absent, read an approved sent invite's legacy
seed through core and GET its existing /shares/invites/:id/folder-keys
inventory (server invites.rs get_folder_keys explicitly authorizes sender).
This preserves the required read-only commands; no custody PUT from reads.
Permanent custody uses L6 exclusively; child reads always L7.
Log capture concatenates remote stderr then stdout after completion to avoid
SSH stream interleaving breaking the plan's row-count guard.

Task 8 first green: 6 executed, 6 passed / 0 failed (t8-green-3.log).
Initial red: 1/5 (t8-red.log). Corrected fixtures now use real POST
/files/check-conflict, parent_id query on GET /files, 409 version_unavailable,
and actual V1 frames for downgrade testing. Repeat baseline red before final.
Decision: Task 8 also wires push.rs as the concrete replacement consumer;
sync.rs receives only the new UploadSpec field until Task 9.
ZIP explicit-key entries use a per-entry authenticated disk staging file and
stream into the existing archive writer, retaining bounded memory and cleanup.

Task 8 corrected-fixture baseline red: 6 executed, 1 passed / 5 failed
(t8-corrected-red.log). Restored green plus integrations: 35 passed / 0 failed;
download/direct unit selection: 23 passed / 0 failed; upload/direct selection:
55 passed / 0 failed, 1 pre-existing ignored (t8-regressions.log).
The direct_upload.rs test fixture also needs the new optional UploadSpec field.

Task 8 complete: final shared_folder_transfers 6 passed / 0 failed;
cargo fmt --all -- --check and cargo clippy --locked --all-targets -- -D warnings
both exit 0 (t8-final-checks.log). The initial Clippy finding was an owned test
MutexGuard scope; fixed with a lexical scope, not suppressed. Formatted only
owned files copied back from the testrig. No write-format rollout.

## Task 9 stopped: false plan premise (2026-10-28)

The task requires persisting adoption envelopes in the "existing account/root-bound
sync state" (plan D Task 9 Interfaces). At CLI baseline eb97a5c and this head:
- src/commands/sync.rs:100-110 SyncState has remote_path, remote_folder_id,
  last_sync, session_id and files, with no account or API-origin binding.
- src/commands/sync.rs:223-225 root_unreconciled checks only the root UUID.
- src/commands/sync.rs:605-611 loads .bb-sync.json directly from the local
  directory, without comparing an account identity or API origin.
The opaque envelope's crypto domain is account-specific, but that does not make
the surrounding manifest account-bound or define its account-switch lifecycle.
The brief explicitly requires stopping a task whose premise is wrong rather than
inventing a contract. Therefore no Task 9 implementation or claimed red/green:
0 of its 8 tests executed. Sync/adoption/tombstones/index repair are unfinished.
Required lead correction: specify account binding and migration/invalidation of
existing unbound manifests, including account changes in both watch loops.
No new persistence or account-switch behavior was invented in this lane.
Task 7's in-memory adoption interfaces remain available, with no sync caller.

Graphify update ran after the code commit: 105/105 AST files, 2,375 nodes,
7,110 edges, exit 0, no LLM calls. Generated output and log retained under the
external evidence directory; the broad pre-existing graph regeneration is
excluded from this mapped-code PR (83,086 added / 26,926 removed JSON lines).
Task 9 premise evidence: task9-stop-evidence.txt compares baseline and head.
