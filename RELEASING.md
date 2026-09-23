# Release checklist

## Prerequisites (one-time setup)

1. **Homebrew tap** — create `github.com/beebeeb-io/homebrew-tap` (empty repo, `main` branch).
2. **HOMEBREW_TAP_TOKEN** — create a GitHub PAT with `contents: write` on `homebrew-tap`.
   Add it to `github.com/beebeeb-io/cli → Settings → Secrets → HOMEBREW_TAP_TOKEN`.
3. **Scoop manifest** — lives in *this* repo at `scoop/bb.json` (no separate bucket repo —
   `beebeeb-io/scoop-bucket` doesn't exist). The release workflow keeps it current automatically
   (see step 5 below); there is no one-time setup for it.
4. **`ci/release-pipeline-fixes` prerequisite (task 1497):** the release workflow's
   `open-scoop-manifest-pr` job needs the repo setting **Settings → Actions → General → Workflow
   permissions → "Allow GitHub Actions to create and approve pull requests"** enabled, or its
   `gh pr create` step fails (GITHUB_TOKEN is blocked from opening PRs otherwise, independent of
   the `pull-requests: write` job permission). Verified via `gh api repos/beebeeb-io/cli
   --jq .permissions` returning `can_approve_pull_request_reviews: false` as of this writing —
   flip it before relying on the automated Scoop PR.

## Cutting a release

```bash
# 1. Bump version in Cargo.toml
#    Change: version = "0.1.0"  →  version = "1.0.0"
vim Cargo.toml

# 2. Update Cargo.lock
cargo check

# 3. Author RELEASE_NOTES.md at the repo root for the exact version being cut
#    (intro → What's New → Bug Fixes/Hardening → Verification → Install/Update →
#    changelog link — see the workspace CLAUDE.md "Release notes" section for the
#    canonical format). scripts/check-release-notes.sh gates this file TWICE:
#    first as the opening step of the `plan` job, on the tag push itself, BEFORE
#    anything is built or the GitHub Release is even created — a missing or
#    stale RELEASE_NOTES.md stops the whole pipeline right there — and again in
#    `set-release-notes` right before it overwrites the release body. There is
#    no silent fallback to the auto-generated changelog body.
vim RELEASE_NOTES.md

# 4. Verify the release plan looks right (no build — dry run)
dist plan

# 5. Commit + tag
git add Cargo.toml Cargo.lock RELEASE_NOTES.md CHANGELOG.md
git commit -m "chore: release v1.0.0"
git tag v1.0.0
git push && git push --tags

# → GitHub Actions fires automatically on the tag push:
#   - `plan` job's FIRST step re-validates RELEASE_NOTES.md against the tag's exact
#     version — fails closed before anything is built or the release is created
#   - Builds binaries for 5 targets (macOS arm64+x86, Linux arm64+x86, Windows x86)
#   - Creates GitHub Release with all archives + SHA-256 checksums
#   - Overwrites the release body with the authored RELEASE_NOTES.md (`set-release-notes` job;
#     re-validates + fails loudly if RELEASE_NOTES.md is missing/stale, but never blocks the
#     two jobs below — by this point the plan-job gate has already proven it's fine)
#   - Opens a PR (branch `release/scoop-v1.0.0`) bumping `scoop/bb.json` to the new
#     version + Windows artifact SHA-256 (`open-scoop-manifest-pr` job) — review and
#     merge it manually; it no longer pushes straight to `main` (that used to fail
#     GH006 on the protected branch and, worse, could take the whole `host` job down
#     with it — see task 1497)
#   - Pushes bb.rb formula to beebeeb-io/homebrew-tap
#   - Publishes beebeeb-cli-installer.sh (curl | sh target)
```

## Installation methods (post-release)

```bash
# macOS / Linux — shell installer
curl -sSf https://get.beebeeb.io | sh

# macOS — Homebrew
brew install beebeeb-io/tap/bb

# Windows — Scoop (bb.json lives in this repo, not a separate bucket)
scoop install https://raw.githubusercontent.com/beebeeb-io/cli/main/scoop/bb.json

# Direct download (all platforms)
# https://github.com/beebeeb-io/cli/releases/latest
```

## Shell completions (add to install docs / post-install note)

After installing `bb`, users can enable tab-completion for their shell:

```bash
# Bash (add to ~/.bashrc or drop in the completions dir)
bb completions bash > ~/.local/share/bash-completion/completions/bb
# or: echo 'eval "$(bb completions bash)"' >> ~/.bashrc

# Zsh (add the directory to fpath first)
mkdir -p ~/.zfunc
bb completions zsh > ~/.zfunc/_bb
# In ~/.zshrc, before compinit: fpath=(~/.zfunc $fpath)

# Fish
bb completions fish > ~/.config/fish/completions/bb.fish

# PowerShell
bb completions powershell > ~/Documents/PowerShell/completions/bb.ps1
# Then source it in $PROFILE: . ~/Documents/PowerShell/completions/bb.ps1

# Homebrew tap users get completions automatically via brew's linkage.
```

Include this block in:
- The docs site CLI quick-start article (after the install step)
- The Homebrew formula's `caveats` string (if added later)

## Rollback

```bash
# Delete the tag locally and remotely (aborts the release)
git tag -d v1.0.0
git push origin :refs/tags/v1.0.0

# If the GitHub Release was already created, delete it via:
gh release delete v1.0.0 --yes
```
