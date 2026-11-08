# bb 0.17.2: the four `zip` advisories are gone, and a file that changes while `bb backup` copies it waits for the next run

This release updates one library (`zip`, which `bb pull --zip` uses to write archives) and fixes how `bb backup` treats a file that changes while it is being backed up. `cargo audit` reports 5 vulnerabilities for 0.17.1 and 1 for this release; the one that remains is listed under "Known limits".

### Security

- **`zip` 9.4.1 (it was 8.6.0).** This clears the four `zip` advisories that 0.17.1 listed as known limits: RUSTSEC-2026-0429, -0367, -0368 and -0369. They are about extracting archives. `bb` only writes zip archives (`bb pull --zip`) and never extracts them, and a search of `bb`'s own code finds none of the library's extraction calls, so nobody was exposed; the advisories are gone from the dependency list now.
- **Beebeeb core v0.2.4 (it was v0.2.2).** `zip` was also a dependency of Beebeeb core, so the update needed a core release: v0.2.4 is v0.2.3 plus the `zip` 9 update. Core v0.2.3 adds two backup helpers that `bb` does not call yet.
- **`bb pull --zip` writes the same archive.** For five test entries (random and repetitive data, an empty file, nested folders), `bb`'s archive writer produced the same 340,348 bytes with `zip` 8.6.0 and with 9.4.1. That is a one-off comparison, not a test; no test in the repository compares archive bytes.
- **`cargo audit`: 5 vulnerabilities before, 1 after.** The 4 that are gone are the `zip` ones above.

### Backups

- **A file that changes while it is being backed up waits for the next run.** If a file grows, shrinks or goes away between `bb backup run` looking at it and uploading it (a log file that is still being written is the usual case), the run now counts it under "waiting for the next run" and exits 0. In 0.17.1 a file that grew or shrank before the upload had read it counted as **failed** and the run exited 3, though nothing was wrong with it; the next run backs up what the file is by then. Nothing half old and half new is ever stored.
- **The half-finished upload is ended.** The upload that was started for such a file is ended on the server at once. Until now only a file that changed after its last chunk was sent was ended that way.
- **A file `bb backup` cannot look at still fails, and says so.** A network drive that went away, or a folder `bb` may no longer open, is not "waiting": the run exits 3 and counts the file as failed. Only a file that is different, gone, or no longer a regular file counts as changed.
- **`bb push` and `bb sync`:** a file that grows or shrinks while it is being uploaded still fails with the same message as before; its upload session is now ended at once instead of being left behind.

### Known limits, not fixed in this release

- **`cargo audit` still reports RUSTSEC-2026-0387.** The older version of the library (`crypto-bigint` 0.5.5) is still pulled in by the password-login library (OPAQUE, `opaque-ke` 4.0.1). Beebeeb's password login uses the Ristretto255 suite, which does not use that code. The core team checked this by reading the code; it is not a runtime test. The fix waits on a release of that library.
- **`bb sync --json`:** a run that creates a folder on the server prints a `+ remote folder <name>` line on stdout before the JSON (`would create remote folder` with `--dry-run`), so a JSON parser reading stdout fails on that run. This is unchanged since 0.16.0.
- **Not covered yet** (unchanged from 0.17.0): `bb sync`, `bb mount`, `bb webdav`, `bb repair` and `bb restore` don't check the Backups rule. Until a later release adds it, don't use them to change files inside Backups.

### Verification

- **Test suite** on Linux in CI, default features, at the head of the `zip` change before it was merged: 46 `test result: ok` lines, 0 failed, 1,285 passed, 5 ignored. On a Mac, a full run has **0 failed** now (1,283 passed, 5 ignored); 0.17.1 had 4 failing tests there, which this release fixes (three read Linux configuration paths; the fourth was the backup bug above). The tests run the real `bb` binary against a local mock of the server; none of them talks to production.
- **The backup fix, measured:** ten full test runs in a row on a Mac, 0 failed each (45 `test result: ok` lines, 1,281 passed, 5 ignored), where the same test had failed in every full run on a Mac and three times in CI before. The new tests fail first on the old code (the grown-file test exited 3) and pass after it. Each of the fix's checks was switched off in turn in a copy of the code and made a test fail in 10 of 10 runs: the check that a sealed chunk fits the plan, ending the upload session, asking about a file that went away or whose size moved, treating a read error, and counting only a missing file (not any unreadable one) as changed.
- **The mock now refuses an oversized chunk** the way the real server does, so the grown-file test no longer passes by accident.
- **Audit:** `cargo audit` 5 vulnerabilities before, 1 after; what remains is RUSTSEC-2026-0387 (1), as listed above. The `zip` advisories are gone from the dependency list, not ignored.
- **Review:** the backup change was reviewed four times before it was merged. The reviews found that the real server would answer 413 for a grown file, which the old mock hid, that a resumed upload needed its session ended too, and that a network drive that went away would have counted as "waiting" and exited 0, which is why only a missing file counts as changed.
- **Not run for this release:**
  - A Windows build before the release (the release build is the first). The `zip` update also moved the Windows-only part of eight other libraries to a newer `windows-sys` (0.61.2), within the versions they accept.
  - `bb backup` against a real server, and `bb push` or `bb sync` against one: the tests use a local mock, so ending the upload session of a changed file is checked against the mock, not the real server.
  - An unreadable file at the very last check before an upload completes, and a file that changes in the few milliseconds between `bb` reading its size and opening the upload: the code handles both, but no test can make those two checks fail by a real change; the second is tested by giving the check an old size.
  - The test suite with `--features fuse`. Release binaries ship without FUSE.
  - The self-update from 0.17.1 can only be tested against the published release, so it is checked after publishing and is not part of the counts above.

### Install / Update

```sh
bb update
bb --version   # bb 0.17.2
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.17.1...v0.17.2
