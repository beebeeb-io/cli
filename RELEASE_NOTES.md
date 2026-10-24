# bb 0.14.2 — reliable updates and web billing

This patch makes updating bb explicit and fixes billing navigation. Both billing commands now take you to Beebeeb's web app to manage your account.

### What's New

- **`bb update`** checks for a newer CLI immediately and reports the result. It bypasses the background-check cooldown; `BB_NO_UPDATE=1` still disables automatic updates.

### Bug Fixes / Hardening

- Re-running `curl -fsSL https://get.beebeeb.io | sh` updates the existing Beebeeb installation your shell selects. Homebrew installations remain managed by Homebrew, including repair of a stale binary shadowing the formula.
- **`bb account billing` and `bb billing portal`** open the Beebeeb web billing page. They no longer initiate a payment-method update or send you directly to Mollie.
- Updates retain SHA-256 verification before replacing the executable. This release adds no new dependency or signing-key scheme.

### Verification

- Billing navigation: **6 passed; 0 failed**. Deliberately restoring Mollie navigation/removing authentication made all six fail; restored tests pass with zero payment API requests.
- Installer: **16 passed; 0 failed**, against both the published cargo-dist template and a freshly generated template. Deliberate preflight/version-check mutations were rejected.
- Focused updater run: **21 passed; 0 failed**, plus **2 public command tests** and **1 package-manager output test**. New behavior was checked against deliberate mutations before restoring the passing implementation.
- The release is gated by the full CLI test suite, Clippy with warnings denied, formatting, five platform builds, generated-installer fixtures, and native archive version checks. Final publication evidence is recorded with the release task.
- Of **443 locked packages**, only the first-party CLI version changed; **0 dependency versions** changed.

### Install / Update

```sh
curl -fsSL https://get.beebeeb.io | sh
# After installing 0.14.2:
bb update
bb --version
```

Homebrew users can also run `brew upgrade beebeeb-io/tap/bb`; Windows users can run `scoop update bb`; `bb update` delegates to Scoop for Scoop-managed installations. Unmanaged Windows binaries require reinstalling the release archive or using Scoop. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.14.1...v0.14.2
