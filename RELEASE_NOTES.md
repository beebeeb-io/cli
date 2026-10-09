# Beebeeb CLI 0.13.2 — `bb sync` stops when the server has ended your session

A patch release that fixes one problem: a background `bb sync` service whose session the server
no longer accepts used to be restarted by launchd or systemd every 10 to 20 seconds, forever,
each time asking the server to register the device and being refused. On production this showed
up as `bb` 0.13.0 clients sending the same refused request around six times a minute. `bb sync`
now stops, says why in one line, and the service managers are told not to bring it back. Three
pull requests landed on `main` since v0.13.1: beebeeb-io/cli#71 (the fix), #73 (follow-up
hardening of the fix) and #70 (vendored contract data, nothing at run time), plus the Scoop
manifest bump for 0.13.1 (#69), which does not touch the binary. This is a patch release: no new commands, flags or JSON keys.

### What's New

- Nothing new to use. The one behaviour change is below, under Bug Fixes.

### Bug Fixes / Hardening

- **`bb sync` stops, once, when the server has ended the session.** If the server answers the
  device registration at startup with 401 (session revoked, expired or signed out elsewhere),
  `bb sync` now prints one line and exits with code 77 (`EX_NOPERM`) instead of carrying on and
  being restarted. Run directly in a terminal the line is "Your session has ended. Run `bb login`
  to sign in again." When `bb sync` runs as a background service it reads "Your session has ended.
  Run `bb login`, then `bb sync --daemon` to restart background sync." (beebeeb-io/cli#71) The line is written and flushed before the launchd service removes
  itself, so it reaches the service's log file (beebeeb-io/cli#73).
- **Background sync services no longer restart blindly.** `bb sync --daemon` now writes:
  - macOS (launchd): `KeepAlive` restarts the job only after a failed exit (`SuccessfulExit` =
    false, it was unconditional before), `ThrottleInterval` 30 seconds, and a
    `BEEBEEB_SYNC_SERVICE=1` marker in the job's environment. launchd cannot decline to restart on
    one specific exit code, so when a launchd-managed `bb sync` exits with 77 it runs
    `launchctl remove` on its own job.
  - Linux (systemd user unit): `RestartPreventExitStatus=77`, `RestartSec=30` (it was 10) and the
    same marker. `Restart=on-failure` is unchanged. When `bb sync` is stopped on purpose, waiting for
  the sync process to exit is now bounded (it is killed and reaped) so a stuck process cannot hold
  up the stop (beebeeb-io/cli#73).
- **Existing sync services are updated in place.** `bb sync` and `bb login` rewrite background
  sync services that an earlier `bb` installed (`~/Library/LaunchAgents/io.beebeeb.sync.*.plist`,
  `~/.config/systemd/user/beebeeb-sync-*.service`) to the same policy. Only the restart policy and the `BEEBEEB_SYNC_SERVICE` marker change: the
  arguments, paths, log files, and the service's other environment variables (for example
  `BB_NO_UPDATE`) and settings are kept. The rewritten file keeps its trailing newline. `bb` prints "Updated the background sync service to stop restarting after
  sign-out." Only files with Beebeeb's own name and label are touched, and a file already in the
  new form is left alone. On Linux `bb` also runs `systemctl --user daemon-reload`; on macOS it
  does not reload the job into launchd. An existing restart loop ends after you upgrade: the next
  restart runs the new `bb`, which gets the same 401, exits with 77 and removes its own launchd
  job, so you only need to run `bb login` and `bb sync --daemon` again.
- **Not covered yet.** A 401 that first appears while `bb sync` is already running (the session
  ends mid-watch) is not turned into exit 77 by this release; only the refusal at startup is.
- Vendored onboarding contract update (#70): the iOS and desktop `pre_account` fixtures now say
  signup is web-only. This is test data copied from the server; `bb` reads the server's live
  answer, so nothing changes at run time.

### Verification

Code under test: `774667e` (cli `main`, after #73). The release commits on top of it only change
the version to 0.13.2, `Cargo.lock` (that one line), this file, and the changelog.

This release is built and published from a maintainer's machine, not by the GitHub Actions
release workflow, whose minutes ran out. Artifacts and checksums use the same names and layout
`dist` produces, built with `dist` 0.31.0, the same way as 0.13.1.

- `cargo test --locked`: 492 passed, 0 failed, 2 ignored across 11 test binaries: main suite
  (`bb`) 424, `auth_errors` 8, `ecdh_compat` 4, `mount_availability` 3, `move_parent` 7,
  `non_interactive` 6, `onboarding_state` 9, `pull_overwrite` 5, `pull_path_traversal` 21,
  `share_resolves_like_pull` 1, `zero_byte_files` 4. 0.13.1 had 474.
- `cargo clippy --locked --all-targets -- -D warnings` clean. `cargo fmt -- --check` clean.
- The restart policy and the in-place migration are covered by unit tests on the generated launchd
  plist and systemd unit and on the rewrite of old ones. The 401-at-startup exit code is covered
  in `tests/auth_errors.rs`.
- Verified on a real launchd (macOS, 2026-10-09): a job with the old `KeepAlive` policy whose
  session the server refused made exactly one request, wrote the session-ended message to its log,
  removed itself with `launchctl remove` and was not restarted.
- Not verified: systemd was checked by unit tests on the generated unit and on the rewrite of old
  ones, and by Linux CI only; no live systemd was run. The Windows `bb.exe` is cross-compiled and
  is **not executed** on a Windows machine by this release process. This release has not been run
  against production. Please report a problem if you see one.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh           # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in
OTA updater. Windows installs do not self-update yet: run `scoop update bb` to get 0.13.2. The
Scoop manifest (`scoop/bb.json`) is bumped by a follow-up pull request; until that pull request is
merged, `scoop update bb` still installs 0.13.1.

If `bb sync` was running as a background service and its session has ended, run `bb login`, then
`bb sync --daemon` once after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.13.1...v0.13.2
