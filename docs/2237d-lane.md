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
