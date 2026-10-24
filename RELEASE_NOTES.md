# Beebeeb CLI 0.14.1 — compatibility and packaging refresh

This patch prepares the CLI for the coordinated server 0.1.1 release. It retains the command
behavior of 0.14.0. Since that release, the only change on main updated the Windows Scoop
manifest to 0.14.0; this release adds version metadata and these authored notes.

### What's New

- **A fresh release package.** Existing upload, download, region and sync commands keep their
  current behavior. No new command or transfer protocol is introduced.

### Bug Fixes / Hardening

- The existing core pin (`337436ae753ba99ac6c4dabd18593543367f1324`) and all dependency versions
  remain unchanged. The server uses a different reviewed core revision; compatibility must be
  checked before publishing this CLI release.
- This metadata update does not claim a new runtime or security fix.

### Verification

- The existing release-notes guard rejected the stale 0.14.0 notes for a 0.14.1 release before
  this update (exit 1); the updated notes passed (exit 0). Locked offline metadata and formatting
  checks also passed (exit 0).
- Of 443 locked package entries, the first-party CLI package is the only changed version.
  No dependency, core pin, source module or release script changes are included.
- Not yet verified for 0.14.1: server 0.1.1 compatibility, platform binaries, installer/updater
  behavior, the five-target release build, or the sixteen packaged assets and checksums. The
  existing release-local procedure must pass those applicable gates before publication.
- Previous 0.14.0 test results remain evidence for that earlier source; no new full test-suite
  or performance result is claimed here.

### Install / Update

0.14.1 is a release candidate until its release gates pass and the release is published.
Once published, use the existing installer or package manager:

```sh
curl -fsSL https://get.beebeeb.io | sh
brew upgrade beebeeb-io/tap/bb
scoop update bb
```

The Scoop manifest is updated through its normal follow-up pull request after the Windows
artifact checksum is available. Until then, Scoop can continue to offer the previous version.
Restart a running `bb sync` daemon after updating. Windows users update through Scoop.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.14.0...v0.14.1
