#!/usr/bin/env bash
# Validates that RELEASE_NOTES.md exists at the repo root and mentions the
# exact release version being cut. Fails closed — exit 1 with a GitHub
# Actions ::error:: annotation — so a missing or stale RELEASE_NOTES.md can
# never ship a release with cargo-dist's auto-generated changelog body
# instead of the authored one (workspace CLAUDE.md "Release notes" rule).
#
# Usage: scripts/check-release-notes.sh <version>
#   <version>: bare semver, e.g. 0.10.0 (no leading 'v')
#
# Called from .github/workflows/release.yml:
#   - EARLY, as the first step of the `plan` job, on tag pushes only — this
#     runs before `dist host --steps=create` (which is what actually creates
#     the GitHub Release), so a bad RELEASE_NOTES.md stops the pipeline
#     before anything is built or published (task 1497 follow-up: the
#     original guard only ran in `set-release-notes`, AFTER `host` had
#     already created the release and cargo-dist had already uploaded
#     artifacts to it with its own generated changelog body).
#   - Again in `set-release-notes`, right before `gh release edit`, as the
#     final overwrite of the release body — kept so that job stays
#     self-contained and re-validates against the exact commit it's about to
#     edit.
#
# Mirrors repos/desktop's `validate-release-notes` job (same regex, same
# annotation shape).

set -euo pipefail

VERSION="${1:?usage: check-release-notes.sh <version>}"

notes_path="RELEASE_NOTES.md"

if [[ ! -f "$notes_path" ]]; then
  echo "::error file=$notes_path::Missing RELEASE_NOTES.md for cli $VERSION. Author it at the repo root (see RELEASING.md) and commit it before tagging — the release body will NOT fall back to the auto-generated changelog."
  exit 1
fi

python3 - "$VERSION" <<'PY'
import pathlib
import re
import sys

version = sys.argv[1]
notes_path = pathlib.Path("RELEASE_NOTES.md")
notes = notes_path.read_text(encoding="utf-8")
exact_version = re.compile(rf"(?<![0-9A-Za-z.-]){re.escape(version)}(?![0-9A-Za-z-]|\.[0-9A-Za-z])")

if not exact_version.search(notes):
    print(
        f"::error file=RELEASE_NOTES.md::RELEASE_NOTES.md must mention the exact release version {version}. "
        "Update the title/body for this release and commit it before tagging. See RELEASING.md.",
        file=sys.stderr,
    )
    sys.exit(1)
PY
