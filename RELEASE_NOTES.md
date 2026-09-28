# Beebeeb CLI 0.11.1 — resumed uploads survive a server-side session sweep

This is a single-fix patch release. Server task 1589 gave every upload session an expiring
lease, and a sweeper on the server now deletes any session whose lease has run out. `bb` saves
`(file_id, upload_session_id)` in `pending-uploads.json` so an unchanged file can resume straight
into its existing session on the next run — but once that session was swept, every later
`bb push`/`bb sync` of the file failed with `chunk 0 upload: not found`, forever, because the
local record was only ever cleared on success. `bb` now notices the session is gone and starts a
fresh one under the same file id. Four pull requests merged since v0.11.0; three of them (a Scoop
manifest version bump and two fixes to internal `prod-bots` test scripts) don't change what the
`bb` binary does, which is why this is a patch release rather than a minor one.

### What's New

- **A swept upload session is re-opened automatically instead of failing forever.** When a
  *resumed* session's chunk upload or complete call comes back `404`, or — on an older server —
  `400 "upload session is not writable: expired"`, `bb` drops the stale `pending-uploads.json`
  record, opens a new session under the same stored file id, and re-uploads from chunk 0. This
  can happen at most once per file per run: only a resumed session can trigger it, and if a
  session the run itself just opened vanishes, `bb` reports that as an error instead of retrying
  in a loop. (beebeeb-io/cli#50, server task 1589 / beebeeb-io/server#120)

### Bug Fixes / Hardening

- **A server error while re-opening a session no longer switches file ids.** If the server answers
  a `5xx` to the init for the stored file id, `bb` now retries that same id (up to 3 times, with a
  growing wait) instead of opening a second session under a fresh id — which could have left a
  half-finished file next to a duplicate. Only a `404`, which proves the stored id itself is
  unusable, falls back to a fresh id. A replace upload gets the same retry. (beebeeb-io/cli#50)
- **Progress stays correct when a resumed upload is re-opened.** The bytes and the files-done
  count from the failed attempt are rolled back before the retry, and the rollback is atomic, so
  a `bb sync` with several uploads in flight no longer over- or under-counts. (beebeeb-io/cli#50)
- beebeeb-io/cli#48 and #49 update the `prod-bots` end-to-end test scripts (a login-flow selector
  and a vault-suite share check) to match server changes; beebeeb-io/cli#47 bumped the Scoop
  manifest to the already-released v0.11.0. None of the three touch `src/` or change `bb`'s
  behavior.

### Verification

CI on the exact commit this release is cut from (`f6e9876`, main):
https://github.com/beebeeb-io/cli/actions/runs/36363362059

- `cargo build --verbose` — succeeds.
- `cargo test --verbose` — every test binary green:
  - main suite (`bb`): `test result: ok. 266 passed; 0 failed; 2 ignored; 0 measured; 0 filtered
    out; finished in 97.75s` — includes the new `swept_session_reinit` test module covering the
    fix above.
  - `auth_errors`: `test result: ok. 6 passed; 0 failed`
  - `ecdh_compat`: `test result: ok. 4 passed; 0 failed`
  - `mount_availability`: `test result: ok. 3 passed; 0 failed`
  - `non_interactive`: `test result: ok. 6 passed; 0 failed`
  - `pull_overwrite`: `test result: ok. 5 passed; 0 failed`
  - `share_resolves_like_pull`: `test result: ok. 1 passed; 0 failed`
  - `zero_byte_files`: `test result: ok. 4 passed; 0 failed`
  - Total: 295 passed, 0 failed, 2 ignored across 8 binaries.
- `cargo test --bin bb help_links_return_200 -- --ignored --nocapture` (network-dependent, run
  separately from the main suite) — `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;
  267 filtered out`.
- `cargo clippy --all-targets -- -D warnings` — clean (CI job "Clippy & Format", same run).
- `cargo fmt -- --check` — clean (same job).
- `dist plan` was not run for this notes PR — cargo-dist isn't installed on the machine that
  prepared it. It still runs as the first, fail-closed step of the tag-triggered release
  workflow, which also re-validates this file names `0.11.1` before anything builds.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh    # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in
OTA updater. Windows installs do not self-update yet — run `scoop update bb` to get 0.11.1.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.11.0...v0.11.1
