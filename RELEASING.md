# Release checklist

## Prerequisites (one-time setup)

1. **Homebrew tap** — create `github.com/beebeeb-io/homebrew-tap` (empty repo, `main` branch).
2. **HOMEBREW_TAP_TOKEN** — create a GitHub PAT with `contents: write` on `homebrew-tap`.
   Add it to `github.com/beebeeb-io/cli → Settings → Secrets → HOMEBREW_TAP_TOKEN`.
3. **Scoop bucket** — create `github.com/beebeeb-io/scoop-bucket`, add `bb.json` from `dist/bb.json`.
   Update the hash field on each release (see step 6 below).

## Cutting a release

```bash
# 1. Bump version in Cargo.toml
#    Change: version = "0.1.0"  →  version = "1.0.0"
vim Cargo.toml

# 2. Update Cargo.lock
cargo check

# 3. Verify the release plan looks right (no build — dry run)
dist plan

# 4. Commit + tag
git add Cargo.toml Cargo.lock
git commit -m "chore: release v1.0.0"
git tag v1.0.0
git push && git push --tags

# → GitHub Actions fires automatically on the tag push:
#   - Builds binaries for 5 targets (macOS arm64+x86, Linux arm64+x86, Windows x86)
#   - Creates GitHub Release with all archives + SHA-256 checksums
#   - Pushes bb.rb formula to beebeeb-io/homebrew-tap
#   - Publishes beebeeb-cli-installer.sh (curl | sh target)

# 5. After the release completes, update the Scoop manifest:
HASH=$(curl -sL https://github.com/beebeeb-io/cli/releases/download/v1.0.0/beebeeb-cli-x86_64-pc-windows-msvc.zip.sha256)
# Edit dist/bb.json, update version + hash, commit + push to scoop-bucket repo
```

## Installation methods (post-release)

```bash
# macOS / Linux — shell installer
curl -sSf https://beebeeb.io/install.sh | sh

# macOS — Homebrew
brew install beebeeb-io/tap/bb

# Windows — Scoop
scoop bucket add beebeeb https://github.com/beebeeb-io/scoop-bucket
scoop install bb

# Direct download (all platforms)
# https://github.com/beebeeb-io/cli/releases/latest
```

## Rollback

```bash
# Delete the tag locally and remotely (aborts the release)
git tag -d v1.0.0
git push origin :refs/tags/v1.0.0

# If the GitHub Release was already created, delete it via:
gh release delete v1.0.0 --yes
```
