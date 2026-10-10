#!/usr/bin/env bash
# Does a GitHub release for <tag> already exist WITH assets? (task 1839)
#
# Usage: scripts/release-exists.sh <owner/repo> <tag>
# Prints exactly one line on stdout: `skip=true` or `skip=false` (redirect it to
# $GITHUB_OUTPUT). Diagnostics go to stderr.
#
#   release exists and has >= 1 asset  -> skip=true   (a locally built release was
#                                         published; the tag-push CI run must not
#                                         rebuild or overwrite it)
#   release exists but has 0 assets    -> skip=false  (nothing to protect)
#   HTTP 404 (no release for the tag)  -> skip=false  (normal CI-driven release)
#   anything else (network, 5xx, auth) -> exit 1      (fail closed; never guess)
#
# Needs `gh` authenticated (GH_TOKEN) and `jq`. Drafts are not visible by tag,
# which is fine: a draft has no tag yet, so no tag-push run exists for it.

set -euo pipefail

REPO="${1:?usage: $0 <owner/repo> <tag>}"
TAG="${2:?usage: $0 <owner/repo> <tag>}"

rc=0
out="$(gh api "repos/$REPO/releases/tags/$TAG" 2>&1)" || rc=$?

if [[ $rc -ne 0 ]]; then
  if grep -q "HTTP 404" <<<"$out"; then
    echo "no release for $TAG yet: normal CI-driven release" >&2
    echo "skip=false"
    exit 0
  fi
  echo "FAIL: could not determine whether a release for $TAG exists: $out" >&2
  exit 1
fi

n="$(jq -r '.assets | length' <<<"$out")"
if [[ "$n" =~ ^[0-9]+$ && "$n" -gt 0 ]]; then
  echo "release $TAG exists with $n assets: skip" >&2
  echo "skip=true"
else
  echo "release $TAG exists but has no assets: proceed" >&2
  echo "skip=false"
fi
