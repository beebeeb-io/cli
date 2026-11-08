# bb 0.17.3: on Windows, a file name `bb` writes, renames or deletes only ever touches the file with that exact name

Windows also finds a file through its folder's 8.3 short names (for example `REPORT~1.PDF` for `Report for 2026.pdf`). Through 0.17.2, a name `bb` built to write, replace, rename or delete could therefore reach a *different* file that already existed in that folder. This release moves `bb` to Beebeeb core v0.2.7, which refuses such a name, and routes the places in `bb` that wrote files by name on their own through the same check. Linux and macOS have no short names; there `bb` behaves as in 0.17.2, apart from the `.bb-trash` fix and the file-request change below.

### Windows

- **Downloads** (`bb pull`, `bb sync`) write into a temporary file next to the target, created new every time, and publish it only if the target's name still reaches no other file. A file that appears under a short name while the download runs is never replaced.
- **`bb pull --zip`** writes its archive the same way. With `--force` it still replaces the archive you named, never another file a short name reaches; the overwrite check refuses such a name before anything is downloaded, with or without `--force`.
- **`bb sync` moves a removed file into `.bb-trash`** only through folders whose names reach themselves. A folder on the way that a short name reaches is refused, the error names both entries, and the file stays where it is.
- **`bb sync` applying a rename from another device** holds that change as a conflict if the old or the new name reaches another entry, instead of failing the whole pass. It checks again right before it creates any folder.
- **File-request pulls** (`bb pull` of uploads received through a file request, and their folder variant) now use the same writer as every other pull. `-o` was not checked at all before.

### Also changed on every platform

- **`.bb-trash` only takes plain relative names.** A synced path that was not one (`..`, an absolute path) could move a file outside the recovery folder; it is now refused.
- **File-request pulls replace the output file instead of writing into it,** as every other pull already did. With `--force` that means the file gets default permissions, a symbolic link at the output is replaced rather than written through, and `-o /dev/stdout` fails (it already failed for every other pull).
- **A temporary file left by a crash is never reused.** It stays where it is; see "Known limits".

### Known limits, not fixed in this release

- **An interrupted download leaves `<name>.tmp` behind** (for example when `bb sync` is stopped mid-download), and the next `bb sync` pass uploads that partial file as if it were yours. Delete such a `.tmp` file by hand. A later release makes sync skip and clean up its own temporary files.
- **On Windows, `bb pull --zip` refuses** a folder or file name with `:`, a trailing dot, or a reserved name such as `CON` instead of rewriting it to `_CON` and the like, because of how core v0.2.7 checks names. Core v0.3.0 fixes this.
- **`cargo audit` was not re-run for this release.** The dependency list differs from 0.17.2 only in the three Beebeeb core packages, so the one item 0.17.2 listed (RUSTSEC-2026-0387, through the password-login library) is expected to remain.
- **Unchanged from 0.17.2:** `bb sync --json` can print a `+ remote folder` line before the JSON; `bb sync`, `bb mount`, `bb webdav`, `bb repair` and `bb restore` don't check the Backups rule yet.

### Verification

- **Test suite on Linux in CI** at the merged head (`b1a5027`): 1,290 passed, 0 failed, 5 ignored.
- **Native Windows** (a real Windows 11 machine, MSVC, temporary folders), the 16 tests that exercise short names:
  - **Before the fix** (the old code with the new tests): 6 passed, 10 failed. The 10 failures are exactly the ten cases above, which also shows the test folder really had 8.3 names.
  - **With the fix:** 16 of 16 passed.
- **Review:** one independent review of the change found nothing blocking (0 of its findings would let a write reach another file). Its one follow-up is the leftover temporary file under "Known limits".
- **Not run for this release:** the full test suite on Windows (only the 16 short-name tests ran natively); `bb` against a real server (the tests use a local mock); the self-update from 0.17.2, which can only be checked against the published release.

### Install / Update

```sh
bb update
bb --version   # bb 0.17.3
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.17.2...v0.17.3
