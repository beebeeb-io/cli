#!/usr/bin/env bash
# Rewrite scoop/bb.json with the version + SHA-256 from the just-built
# Windows artifact. Called by the release workflow after `dist build`
# completes, and runnable manually from a clean checkout.
#
# Usage:
#   scripts/update-scoop-manifest.sh <version> [<sha256-file>]
#
# - <version>: bare semver, e.g. 0.5.0 (the leading 'v' is stripped if
#   present)
# - <sha256-file>: optional path to a file containing the artifact's
#   SHA-256 hash on its own line. If omitted, the script computes it from
#   target/distrib/beebeeb-cli-x86_64-pc-windows-msvc.zip.

set -euo pipefail

VERSION="${1:?need a version}"
VERSION="${VERSION#v}"

SHA_FILE="${2:-}"

if [[ -n "$SHA_FILE" ]]; then
  SHA="$(tr -d '[:space:]' < "$SHA_FILE")"
else
  ARTIFACT="target/distrib/beebeeb-cli-x86_64-pc-windows-msvc.zip"
  if [[ ! -f "$ARTIFACT" ]]; then
    echo "FAIL: $ARTIFACT not found. Run \`dist build --artifacts=local --target x86_64-pc-windows-msvc\` first." >&2
    exit 1
  fi
  SHA="$(shasum -a 256 "$ARTIFACT" | awk '{print $1}')"
fi

MANIFEST="scoop/bb.json"
TMP="$(mktemp)"
jq \
  --arg version "$VERSION" \
  --arg hash "sha256:$SHA" \
  '.version = $version
   | .architecture["64bit"].url = ("https://github.com/beebeeb-io/cli/releases/download/v" + $version + "/beebeeb-cli-x86_64-pc-windows-msvc.zip")
   | .architecture["64bit"].hash = $hash' \
  "$MANIFEST" > "$TMP"

mv "$TMP" "$MANIFEST"
echo "Updated $MANIFEST → version $VERSION, hash $SHA"
