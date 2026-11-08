# bb 0.17.1: security updates for the login key exchange and TLS, and a re-upload that adds a version after `bb backup` loses its records

This is a security release. The one-time key exchange used when a browser approves `bb login` now runs on a fixed version of its elliptic-curve library, and the TLS library, its certificate checks and two supporting libraries are updated. `cargo audit` reports 17 vulnerabilities for the code of 0.17.0 and 5 for this release; the 5 that remain are listed under "Known limits" with why they stay. It also changes one thing in `bb backup`: after a reinstall, a file whose server copy was made within 10 minutes of its last change is uploaded again, as a new version of that same server file.

### Security

- **The `bb login` key exchange runs on a fixed library.** The one-time key exchange used when a browser approves `bb login` now runs on `p256` 0.14 (it was 0.13.2). That clears the timing issues in advisory RUSTSEC-2026-0387 for that code. The login's wire format is unchanged: the tests that check it byte for byte against the browser's own vectors pass.
- **TLS and certificate checks are updated.** `rustls` 0.23.46 (was 0.23.40), `rustls-webpki` 0.103.16 (was 0.103.13) and `webpki-roots` 1.0.11 (was 1.0.7). The advisories they cover are about TLS 1.3 handshake handling, `Debug` output that could show TLS secrets, the size a decompressed certificate may reach, and trust in distrusted intermediate certificates (RUSTSEC-2026-0346, -0285, -0348, -0349, -0382 and -0381).
- **Also updated:** `quinn-proto` 0.11.19 (was 0.11.14; five advisories, RUSTSEC-2026-0342, -0339, -0338, -0340 and -0185) and `crossbeam-epoch` 0.9.21 (was 0.9.18; RUSTSEC-2026-0204).
- **`cargo audit`: 17 vulnerabilities before, 5 after.** The 12 that are gone are the ones above. The 8 warnings (unsound or unmaintained libraries) are the same before and after.
- **Dependency changes:** the three Beebeeb core crates moved from v0.2.1 to v0.2.2 (`2d89adc`); no other line of `Cargo.toml` changed. Other libraries moved along with the ones above in the lockfile.

### Backups after a reinstall

- **A copy made within 10 minutes of the file's last change is uploaded again.** When `bb backup` has lost its records (a reinstall does this), it rebuilds them from the server. Core v0.2.2 only counts a server copy as backed up if it was made more than 10 minutes after the file last changed, and `bb backup` cannot yet compare the clock of your computer with the server's, so it does not trust a copy that close to the change. 0.17.0 counted such a copy as backed up. So after a reinstall, each file backed up within 10 minutes of its last change gets one more version on the server.
- **The re-upload is a new version of that same server file, never a second copy.** The file keeps its id and its place, and its version number goes up by one.
- **Two or more files with that exact name in the server folder: `bb backup` does not guess.** It uploads a new file, as it did before, and prints one note (without a file name).

### Known limits, not fixed in this release

- **`cargo audit` still reports RUSTSEC-2026-0387.** The older version of the library (`crypto-bigint` 0.5.5) is still pulled in by the password-login library (OPAQUE, `opaque-ke` 4.0.1). Beebeeb's password login uses the Ristretto255 suite, which does not use that code. The core team checked this by reading the code; it is not a runtime test. The fix waits on a release of that library.
- **Four advisories in the `zip` library remain** (RUSTSEC-2026-0429, -0367, -0368 and -0369). They are about extracting archives. `bb` only writes zip archives (`bb pull --zip`) and never extracts them, so they do not apply; a search of `bb`'s own code finds none of the library's extraction calls. The fix is a major update of that library (`zip` 9), which is planned.
- **`bb sync --json`:** a run that creates a folder on the server prints a `+ remote folder <name>` line on stdout before the JSON (`would create remote folder` with `--dry-run`), so a JSON parser reading stdout fails on that run. This is unchanged since 0.16.0.
- **Not covered yet** (unchanged from 0.17.0): `bb sync`, `bb mount`, `bb webdav`, `bb repair` and `bb restore` don't check the Backups rule. Until a later release adds it, don't use them to change files inside Backups.

### Verification

- **Test suite** on Linux in CI, default features, at the head of the change before it was merged: 46 `test result: ok` lines, 0 failed, 1,271 passed, 5 ignored. That commit's tree is identical to the merge commit this release is cut from, so the release differs from the tested code only in the version line (`Cargo.toml` and `Cargo.lock`), the release notes and the changelog. The build and release configuration (`dist-workspace.toml` and the workflows) is unchanged since 0.17.0. The tests run the real `bb` binary against a local mock of the server; none of them talks to production.
- **Lint:** the CI jobs for Clippy with warnings denied and rustfmt, for Clippy with the `fuse` feature, and for the installer update regression passed.
- **Login:** the test that checks the key exchange byte for byte against the browser's vectors passed (4 passed, 0 failed), and so did the `bb login` unit tests (13 passed, 0 failed), on the new library.
- **Audit:** `cargo audit` 17 vulnerabilities and 8 warnings before, 5 vulnerabilities and 8 warnings after. What remains: RUSTSEC-2026-0387 (1) and the four `zip` advisories (4), as listed above.
- **Re-upload after a reinstall:** the new tests failed first on the code before the fix (2 of 3 failed: two rows with the same name, and the missing note) and pass after it (3 of 3). Each re-uploaded file keeps its id and has version 2 as one row; a copy made 11 minutes after the change is left alone at version 1; with two same-name files in the folder, a third one is added, the two existing ones are left alone and the note is printed.
- **Review:** the change had one independent code review before it was merged. It found that the first version of the re-upload left a second row with the same name and orphaned the old copy; that is fixed, with the tests above.
- **Seen on a Mac, not on Linux:** a full test run on macOS fails 4 tests (2 in `backup_command`, 2 in `push_rerun_resumes`). They fail the same way on the previous release's dependencies and pass in CI. Three read Linux configuration paths; the cause of the fourth, `a_file_changed_while_it_is_read_is_abandoned_and_waits_for_the_next_run`, was not investigated.
- **Not run for this release:**
  - A real `bb login` against a server or the web approval page, and `bb backup` against a server. The tests use a local mock.
  - A Windows build before the release; the release build is the first.
  - The test suite with `--features fuse`. Release binaries ship without FUSE.
  - A test for two same-named sibling folders (the check that a re-upload only matches the exact folder).
  - The self-update from 0.17.0 can only be tested against the published release, so it is checked after publishing and is not part of the counts above.

### Install / Update

```sh
bb update
bb --version   # bb 0.17.1
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.17.0...v0.17.1
