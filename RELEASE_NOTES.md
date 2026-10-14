# Beebeeb CLI 0.13.3 — `bb webdav` now needs a password and is read-only by default

This release closes a hole in `bb webdav` and changes how `bb` proves your password for destructive
actions. Until now `bb webdav` served your decrypted vault on `127.0.0.1` to anything that could reach
the port: a web page in your browser (through DNS rebinding), any other account on the same machine,
with no password and with writes allowed. It now asks for a password, refuses requests that did not
come from a local program you started, and does not allow writes unless you ask for them. Separately,
`bb trash empty`, `bb rm --permanent` and `bb account update` no longer send your password to the
server when they ask you to confirm it. Three pull requests landed on `main` since v0.13.2:
beebeeb-io/cli#76 (`bb webdav`), #77 (password confirmation) and #75 (release tooling, nothing at run
time), plus the Scoop manifest bump for 0.13.2 (#74), which does not touch the binary.

**This release changes what `bb webdav` does by default. Read "What's New" before you update if you
mount your vault through it.**

### What's New

- **`bb webdav` is read-only by default.** Uploads, deletes, renames, copies and property changes
  (PUT, DELETE, MKCOL, MOVE, COPY, PROPPATCH) are refused with `403` and a message that names the
  flag. To save files through a mount, start it with `bb webdav --read-write`. `--read-only` is still
  accepted and now does nothing (it is the default); passing it together with `--read-write` is an
  error. (beebeeb-io/cli#76)
- **`bb webdav` prints a random password at startup.** Every client must send it (HTTP Basic, any
  user name). `bb webdav` prints the password and a ready-to-use mount URL,
  `http://bb:<password>@localhost:<port>/`, once, when it starts. A new password is generated on every
  run. In Finder (Go, Connect to Server) enter `http://localhost:<port>` and type the password when it
  asks; in rclone or Cyberduck use the printed URL, user and password.
  - **If you mount your vault unattended** (a login item, a script, a service), it will stop
    connecting after you update: the password changes on every start and is only printed on the
    terminal. There is no way to set a fixed password in this release.
- **Windows Explorer is not supported by `bb webdav`.** Explorer ("Map network drive") refuses a
  password over plain HTTP by default, and `bb webdav` serves plain HTTP on loopback only. On Windows,
  use the Beebeeb desktop app, or connect with rclone, Cyberduck or WinSCP. `bb webdav --help`, the
  README, and a one-line hint that `bb webdav` prints when it starts on Windows now say the same.
  Windows Explorer is not something we tested; this is what we know from how it handles Basic
  authentication, not from a run on a Windows machine.

### Bug Fixes / Hardening

- **`bb webdav` refuses requests that are not from a local program.** A request whose `Host` is not
  `127.0.0.1:<port>` or `localhost:<port>` (DNS rebinding) and any request carrying an `Origin` header
  (what a browser sends from a web page) get `403`, and a missing or wrong password gets `401`. The
  gateway listens on `127.0.0.1` only, and refuses to start with a clear message when the port is
  already taken. Before this release, anything that could reach the port could list, read, overwrite
  and delete the decrypted vault. Another account on the same machine is stopped by the password, not
  by the `Host` check. (beebeeb-io/cli#76)
- **Step-up confirmation no longer sends your password.** `bb trash empty`, `bb rm --permanent` and
  `bb account update` ask for your password again before a destructive action. `bb` now proves it
  over OPAQUE (`POST /auth/confirm-opaque-start`, then `/auth/confirm-opaque-finish`), the same way
  the web app does, so the server never receives the password for this step. Only an account that has
  no OPAQUE credential still uses the old plaintext confirm; the server tells `bb` which one applies.
  The password you type, and the OPAQUE state derived from it, are wiped from memory after use.
  (beebeeb-io/cli#77)
  - **`bb account update` still sends your password** to the email-change endpoint itself. That is
    unchanged by this release.
- **The step-up error says what actually happened.** A wrong password prints `incorrect password`.
  If the server answers 401 to the confirm request, your session has ended, and `bb` now prints
  "Your session expired or was revoked. Run `bb login` to sign in again." instead of blaming your
  password. (beebeeb-io/cli#77)
- **Release tooling (#75, nothing at run time).** The local release procedure is a script with
  checksum and self-update assertions (`scripts/release-local.sh`, `scripts/ota-rung.sh`), and the
  release workflow now skips a tag whose release already exists.

### Verification

Code under test: `3853ee3` (cli `main`, after #77). The release commit on top of it only changes the
version to 0.13.3, `Cargo.lock` (that one line), this file, and the changelog.

This release is built and published from a maintainer's machine, the same way as 0.13.2, not by the
GitHub Actions release workflow. Artifacts and checksums use the same names and layout `dist`
produces, built with `dist` 0.31.0.

- `cargo test --locked`: 530 passed, 0 failed, 2 ignored across 13 test binaries: main suite (`bb`)
  444, `auth_errors` 8, `ecdh_compat` 4, `mount_availability` 3, `move_parent` 7, `non_interactive`
  6, `onboarding_state` 9, `pull_overwrite` 5, `pull_path_traversal` 21, `share_resolves_like_pull`
  1, `step_up_opaque` 4, `webdav_auth` 14, `zero_byte_files` 4. 0.13.2 had 492.
- `cargo clippy --locked --all-targets -- -D warnings` clean. `cargo fmt -- --check` clean.
- The new `webdav_auth` and `step_up_opaque` tests run the real `bb` binary. Each was run red against
  the old behaviour first (the webdav tests against the unguarded gateway, the step-up tests against
  the plaintext confirm), and the code was mutated afterwards to make sure the tests fail for the
  right reason.
- Run end to end on macOS (arm64, 2026-10-15) against a local API built from server `main`
  (`e9df0587`) on a scratch database, with a scratch `HOME`: `bb login --headless` approved through the
  device-approval flow, two files uploaded into a folder, then `bb webdav` mounted with rclone and
  checked with curl.
  - Read-only (default): `rclone lsf` lists the folder, `rclone cat` returns the decrypted text; a
    `copyto` is refused with `403 Forbidden` and the message that names `--read-write`; a PUT and a
    DELETE with the right password are `403`; no password and a wrong password are `401`; a foreign
    `Host` and an `Origin` header are `403`; a PROPFIND with the password is `207`. The vault was
    unchanged afterwards.
  - With `--read-write`: the same `copyto` succeeds, a PUT is `201`, a DELETE is `204`, and the file
    written through the gateway decrypts with `bb pull` and with `rclone cat`.
  - Step-up (`bb trash empty` on a real terminal): a wrong password prints `incorrect password` and
    empties nothing; the right password empties the trash; the requests the API saw were
    `confirm-opaque-start`, `confirm-opaque-finish`, then the delete, and `/auth/confirm` was never
    called. With the session ended on the server after the prompt, `bb` printed the "run `bb login`"
    line, not "incorrect password".
- Not verified: Finder was not run against `bb webdav` in this release (rclone and curl were).
  Windows: the `bb.exe` is cross-compiled and is not executed on a Windows machine by this release
  process. The Linux binaries are cross-compiled and not executed here either, and `bb webdav` was not
  run on Linux or Windows. The plaintext fallback of the step-up confirm (an account with no OPAQUE
  credential) is covered by tests and was not run against a live API, and neither was `bb account
  update`. This release has not been run against production. Please report a problem if you see one.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh           # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in OTA
updater, so `bb webdav` will behave as described above the next time you start it after that. Windows
installs do not self-update yet: run `scoop update bb` to get 0.13.3. The Scoop manifest
(`scoop/bb.json`) is bumped by a follow-up pull request; until that pull request is merged,
`scoop update bb` still installs 0.13.2.

If you mount your vault through `bb webdav` and save files to it, restart it with `bb webdav
--read-write` and enter the printed password when your client asks.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.13.2...v0.13.3
