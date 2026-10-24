# bb 0.14.2 — reliable updates and web billing

This patch makes updating bb explicit and fixes billing navigation. Both billing commands now take you to Beebeeb's web app to manage your account.

### What's New

- **`bb update`** checks for a newer CLI immediately and reports the result. It bypasses the background-check cooldown; `BB_NO_UPDATE=1` still disables automatic updates.

### Bug Fixes / Hardening

- Re-running `curl -fsSL https://get.beebeeb.io | sh` updates the existing Beebeeb installation your shell selects. Homebrew installations remain managed by Homebrew, including repair of a stale binary shadowing the formula.
- **`bb account billing` and `bb billing portal`** open the Beebeeb web billing page. They no longer initiate a payment-method update or send you directly to Mollie.
- Updates retain SHA-256 verification before replacing the executable. This release adds no new dependency or signing-key scheme.

### Verification

Release checks are in progress. Publication requires focused regression and mutation checks, the full CLI test suite, formatting, Clippy, five release archives and installer/checksum verification. Final counts will be recorded here before publication.

### Install / Update

```sh
curl -fsSL https://get.beebeeb.io | sh
# After installing 0.14.2:
bb update
bb --version
```

Homebrew users can also run `brew upgrade beebeeb-io/tap/bb`; Windows users can run `scoop update bb`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.14.1...v0.14.2
