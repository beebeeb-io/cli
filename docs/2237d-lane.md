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

## Resume under merged D-L1 and L7b

Lead authorized D-L1 account binding before Task 9, then its eight cases.
Prior stop is superseded by workspace #223. Verification: four integration
cases in sync_account_binding.rs, red then green; Task 9's exact eight cases,
sync_never_deletes, API guard, resolver/transfers, Linux fmt/Clippy.
The prior CI red was the API spawn safety scan: 9/1. Moved the fully guarded
spawn into each integration test file, leaving fixture/server helpers shared.
Resume safety green: API guard 10/0 plus resolver 6/0 and transfers 6/0.

D-L1: sync_account_binding red 1 passed / 3 failed, green 4 passed / 0 failed
(dl1-red.log, dl1-green.log). Account id comes only from authenticated auth/me.
Mismatch returns the binding refusal before device/listing/transfer work;
legacy root readability uses one canonical metadata call. Dry-run never binds.
Both watch resync paths recheck the session identity before folder work.

L7b correction to the earlier Task 7 note: custody folders use bound L7 child
wraps exclusively. Only absent-custody, approved flag-off invites use the named
legacy_invite_child_key reader, which calls core's legacy raw decryptor. L7b
red 5/1 and green 6/0 (l7b-red.log, l7b-green.log).

## Task 9 completed after D-L1

Task 9 original red: 8 executed, 0 passed / 8 failed (t9-red.log).
A clean replay of the original fixtures (ad7e833) on pre-task baseline 723ec4b
again executes 8, 0 passed / 8 failed (t9-red-replay.log). This capture combines
remote output before returning over SSH, so assert-rust-count.py passes without
an interleaved Cargo header. Detached proof commit d8ba13a; owned proof tree removed.
Initial green: 8/0 (t9-green.log). Review strengthened those same eight tests:
5/3 red, 8/0 green (t9-review-red.log, t9-review-green.log).

The eight cases cover first adoption and authenticated bytes; canonical sparse
metadata; restart after custody revocation and a real move outside that root;
rename/move, rapid guest versions and key-preserving local replacement; dirty
local retention plus recoverable tombstones/restores and index removals; bad
keys and failed unbound passes; zero API mutations in dry-run; adoption offline
retry and lost response after commit; bounded CAS retries, concurrent merge,
corrupt-page rebuild and partial-listing deferral. Three repeat passes assert
zero echo uploads. Unrelated owner-index entries survive deltas.

Existing sync_never_deletes: initial fixture incompatibility 1/13; add session
identity and readable legacy-root metadata, keeping all original assertions.
Green 14/0, including the existing watch/root and mass-delete cases.
D-L1 remains 4/0. A broader binary unit run: 605 passed / 1 failed / 3 existing
ignored; it caught the ordinary walk limit regression. Corrected the bound to
apply only to index fallback; all 28 sync units now pass. Compatibility: 84/0
(API guard 10, direct download 29, paths 22, resolver 6, transfers 6,
non-interactive 6, zero-byte 5). Linux fmt and strict all-target Clippy exit 0.
Logs: t9-delete-regressions.log, t9-final-checks.log, t9-clippy-green.log.

### Decisions and scope

- D-L1 supersedes the recorded stop. Account id is the authenticated session's
  user id, checked before manifest work in the initial pass and both watches.
  Failed unbound passes retain no pending envelopes/canonical metadata.
- Resolved key handles stay in memory; bound manifests keep encrypted metadata,
  owner envelopes and dirty index ids. Root rebind clears those old records.
- Foreign-file versions supplement timestamps, avoiding same-second missed
  overwrites while preserving ordinary-file conflict behavior.
- Index repair enumerates at most 20,000 entries and defers incomplete or
  unreadable rebuilds. Shared writes re-read/merge for at most three CAS rounds;
  owner writes use the existing LWW route, preserving unrelated entries.
- Known stale shared entries are removed only after authenticated canonical
  tombstone/move confirmation. Index/adoption transport failures do not block
  opening readable content; deferred opaque state retries on the next sync.
- No new activity producer, watcher service or owner key-publication protocol.
  Existing watch triggers use the same incoming read/replace reconciliation.
- API safety scan remains strict: binary-spawn guards live in each top-level
  integration file; shared test helpers only host fixtures and loopback APIs.
- Core/types remain pinned in Cargo.toml and Cargo.lock to
  a245a549c12415af6311bf4488deca5def65e983. No new dependencies or core edits.

Deviations: approved D-L1 prerequisite and L7b refinement; additional sync
fixture auth routes and regression-driven fixes. No unmerged interface is used.
The prior Task 9 stop is resolved. Brand-new owner-upload publication to link
guests remains the plan's adjacent server/client work, not claimed here.
All runtime testing uses isolated HOME and explicit loopback --api; no prod,
Postgres, local API service, native release build, deployment or merge.

Graphify refresh after code commit: 108/108 AST files, 2,434 nodes, 7,612 edges,
exit 0, no LLM calls. Generated artifacts retained outside the mapped PR at
 graphify-resume-generated/ beside graphify-resume-update.log; broad graph churn
is excluded as before. Owned replay/validation worktrees are cleaned up.

## Lead full-gate regression correction

The lead's full gate at 8ce17eb was 808 passed / 3 failed. The previously
reported 138/0 was a selection, not the full suite. Reproduced move_parent
without changing tests: 4 passed / 3 failed. Bisect on Linux tested 4d6e6cc,
b033f1e, 5c59c5b and 0c0be07, each 4/3; first bad commit is
0c0be0770bb7de7f5a0391d76fc01e75fbf16a2e (Task 7). Its parent eb97a5c
was verified separately: 7 passed / 0 failed.

Cause: path lookup skipped resolver errors for invalid item identities, hiding
an existing malformed destination. Single mv then interpreted any lookup error
as absence and could rename instead of refusing. Repair: reject invalid UUID
metadata before resolving keys and distinguish an absent destination from a
listing/metadata error through an optional path lookup. mv propagates errors.
The seven existing move_parent tests and every expectation remain unchanged.
Targeted green: 7 passed / 0 failed (regression-move-green.log).
Decision: preserve resolver authentication and L7b rules; no name/key fallback
was added. Full cargo test --locked --no-fail-fast is the required final gate.
