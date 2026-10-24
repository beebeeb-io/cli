# bb 0.14.3 — updates without the GitHub API rate limit

### Bug Fixes / Hardening

- `bb update` and background update checks use the public release manifest instead of GitHub REST API discovery. Shared-IP API rate limits no longer prevent CLI update checks; no GitHub token is required.
- Download archives from the exact release tag in the manifest, retaining SHA-256 verification and staged binary version checks.
- Retains CLI0.14.2's effective curl updates, Homebrew/Scoop ownership, explicit JSON/quiet updates and Beebeeb web billing navigation.

### Install / Update

If0.14.2 reports403, bootstrap the patch through the public installer:

```sh
curl -fsSL https://get.beebeeb.io | sh
bb update
bb --version
```

Restart a running `bb sync` daemon after updating. Unmanaged Windows binaries still use release-archive reinstall or Scoop.

### Verification

Regression tests cover a rate-limited REST route with an available public manifest and exact request counts. Release gates include the updater suite, full required CLI CI, formatting, Clippy with warnings denied, five platform builds, draft asset readback and native package checks. No dependency versions changed.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.14.2...v0.14.3
