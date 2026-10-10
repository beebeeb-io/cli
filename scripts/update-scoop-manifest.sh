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
# - <sha256-file>: optional path to a file holding the artifact's SHA-256.
#   Accepted shapes (first line is used):
#     <hash>                       bare hash
#     <hash>  <file>               coreutils / `shasum` format
#     <hash> *<file>               dist's own `<artifact>.sha256` format
#   If omitted, the script computes it from
#   target/distrib/beebeeb-cli-x86_64-pc-windows-msvc.zip.
#
# Env: SCOOP_MANIFEST overrides the manifest path (default scoop/bb.json).

set -euo pipefail

VERSION="${1:?need a version}"
VERSION="${VERSION#v}"

SHA_FILE="${2:-}"

if [[ -n "$SHA_FILE" ]]; then
  # First whitespace-delimited token of the first line: handles a bare hash and
  # dist's "<hash> *<file>" form alike (task 1839: the old `tr -d` glued hash and
  # filename together into one garbage string).
  SHA="$(head -n 1 "$SHA_FILE" | awk '{print $1}')"
  if [[ ! "$SHA" =~ ^[0-9a-fA-F]{64}$ ]]; then
    echo "FAIL: first token of $SHA_FILE is not a 64-hex SHA-256: '$SHA'" >&2
    exit 1
  fi
  SHA="$(printf '%s' "$SHA" | tr 'A-F' 'a-f')"
else
  ARTIFACT="target/distrib/beebeeb-cli-x86_64-pc-windows-msvc.zip"
  if [[ ! -f "$ARTIFACT" ]]; then
    echo "FAIL: $ARTIFACT not found. Run \`dist build --artifacts=local --target x86_64-pc-windows-msvc\` first." >&2
    exit 1
  fi
  SHA="$(shasum -a 256 "$ARTIFACT" | awk '{print $1}')"
fi

MANIFEST="${SCOOP_MANIFEST:-scoop/bb.json}"
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
