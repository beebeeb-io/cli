# Beebeeb CLI 0.13.4 — `bb account update --email` no longer sends your password

This is a one-fix release. In 0.13.3 and earlier, running `bb account update --email` on a normal
(OPAQUE) account sent your account password to the server, in the body of a request, even though the
email change then failed. 0.13.4 never does. Changing your email from the CLI still does not work for
these accounts; you do it in the web app. Since v0.13.3 two pull requests landed on `main`:
beebeeb-io/cli#80 (the fix) and #79 (the Scoop manifest bump for 0.13.3, which does not touch the
binary).

### What's New

Nothing. There are no new commands, flags or changed defaults in this release.

### Bug Fixes / Hardening

- **`bb account update --email` no longer sends your password to the server on an OPAQUE account.**
  Most accounts are OPAQUE accounts: the server is built so that it never receives the password.
  `bb account update --email` asked for your password and proved it over OPAQUE (that part has been
  correct since 0.13.3 and sends no password). It then always went on to call the old email-change
  endpoint, `PUT /api/v1/auth/account/email`, with your plaintext password in the request body. For an
  OPAQUE account that endpoint refuses the change, so you saw an error and nothing happened, but the
  server had already been sent the password. This is what 0.13.3 and earlier did: the 0.13.3 release
  notes said `bb account update` "still sends your password to the email-change endpoint itself", and
  that was true for every account, including OPAQUE ones. 0.13.4 stops after the OPAQUE check, which
  happens on your computer, makes no request to the old endpoint, and prints this instead:
  "Changing the email on an OPAQUE account is not yet supported from the CLI (the server does not
  expose the key material the re-registration needs). Use the web app for now. Your email was not
  changed." The command exits 1. (beebeeb-io/cli#80, task 1907)
  - **The email change itself still has to be done in the web app.** The CLI cannot do it for an OPAQUE
    account yet, as before.
  - **Accounts without an OPAQUE credential behave as before.** The server tells `bb` which kind of
    account you have while it checks your password; only an account with no OPAQUE credential still uses
    the old plaintext endpoint, because that is the only way it can change its email.
  - **If you ran `bb account update --email` with 0.13.3 or earlier,** the password you typed was sent
    to the Beebeeb server over TLS in a request body that the server rejected. This release fixes the
    client; it cannot undo a request already made. If you want to be certain, change your password in
    the web app.
  - **This release is the client half.** The server-side change that makes the old endpoint refuse an
    OPAQUE account before it reads a password is a separate change and is not part of this release; the
    fix here does not depend on it.

### Verification

Code under test: `a3bf683` (cli `main`, after #80) plus `92f7f05` (the Scoop manifest, no code). The
release commit on top of them only changes the version to 0.13.4, `Cargo.lock` (that one line), this
file, and the changelog.

This release is built and published from a maintainer's machine, the same way as 0.13.3, not by the
GitHub Actions release workflow. Artifacts and checksums use the same names and layout `dist`
produces, built with `dist` 0.31.0.

- `cargo test --locked`: 532 passed, 0 failed, 2 ignored across 14 test binaries: main suite (`bb`)
  444, `account_email_change` 2, `auth_errors` 8, `ecdh_compat` 4, `mount_availability` 3,
  `move_parent` 7, `non_interactive` 6, `onboarding_state` 9, `pull_overwrite` 5,
  `pull_path_traversal` 21, `share_resolves_like_pull` 1, `step_up_opaque` 4, `webdav_auth` 14,
  `zero_byte_files` 4. 0.13.3 had 530.
- `cargo clippy --locked --all-targets -- -D warnings` clean. `cargo fmt -- --check` clean.
- The new `account_email_change` tests run the real `bb` binary under a pseudo-terminal against a mock
  API that records every request, with a scratch `HOME`. The OPAQUE test was run red against the old
  behaviour first (it saw `PUT /api/v1/auth/account/email` after the OPAQUE check) and the legacy test
  was mutation-checked afterwards.
- Run end to end on macOS (arm64, 2026-10-15) against a local API built from server `main`
  (`e9df0587`) on a scratch database, with a scratch `HOME`, for an OPAQUE account signed in with
  `bb login --headless`: `bb account update --email <new address>` with the correct password prints the
  "Use the web app" message above and exits 1. The requests the API saw were `confirm-opaque-start`
  and `confirm-opaque-finish`, and nothing else: no `PUT /api/v1/auth/account/email`. The same run with
  `bb` 0.13.3 built from its tag showed the extra `PUT /api/v1/auth/account/email` (answered 400),
  so the check can tell the two apart. This was done with both a debug build of this commit and the
  release `aarch64-apple-darwin` archive this release ships, with the same result.
- Not verified: the legacy path (an account with no OPAQUE credential still sends its password to
  the old endpoint, by necessity) is covered by a test and was not run against a live API. Windows:
  the `bb.exe` is cross-compiled and is not executed on a Windows machine by this release process. The
  Linux binaries are cross-compiled and not executed here either. This release has not been run against
  production. Please report a problem if you see one.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh           # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in OTA
updater. Windows installs do not self-update yet: run `scoop update bb` to get 0.13.4. The Scoop
manifest (`scoop/bb.json`) is bumped by a follow-up pull request; until that pull request is merged,
`scoop update bb` still installs 0.13.3.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.13.3...v0.13.4
