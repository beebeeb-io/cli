#!/usr/bin/env bash
# OTA self-update rung (task 1839): prove a PUBLISHED release actually updates
# an installed older binary, the way a user's `bb` would.
#
# Usage: scripts/ota-rung.sh <old-version> <new-version>
#   e.g. scripts/ota-rung.sh 0.13.1 0.13.2
#
# Run it AFTER publishing and BEFORE announcing. 0.13.1 shipped a
# dist-manifest.json without checksums.sha256; src/update.rs fails closed and
# silent in that case, so nothing in CI or in `cargo test` noticed and no
# macOS/Linux install updated for three days. Only running the old binary
# against the published release catches it.
#
# What it does (read-only against GitHub; never touches the real HOME or any
# bb account -- every bb invocation gets HOME=<scratch>):
#   1. waits until the plain download URL of dist-manifest.json (the URL
#      src/update.rs fetches) serves the same bytes as the release asset, and
#      releases/latest reports <new-version> (the CDN lags after an upload);
#   2. downloads the old release binary for THIS host into a mktemp dir;
#   3. runs it once: must print "Updating bb v<old> -> v<new>";
#   4. runs it again: `--version` must report <new-version>, and the binary on
#      disk must be byte-identical to the new release archive's bb.
#
# Env: OTA_WAIT_TRIES (default 40), OTA_WAIT_SLEEP seconds (default 15),
#      OTA_REPO (default beebeeb-io/cli).
#
# Bash 3.2 compatible.

set -euo pipefail

OLD="${1:?usage: $0 <old-version> <new-version>}"
NEW="${2:?usage: $0 <old-version> <new-version>}"
OLD="${OLD#v}"
NEW="${NEW#v}"
REPO="${OTA_REPO:-beebeeb-io/cli}"
TRIES="${OTA_WAIT_TRIES:-40}"
SLEEP="${OTA_WAIT_SLEEP:-15}"

for t in gh curl jq shasum tar; do
  command -v "$t" >/dev/null 2>&1 || { echo "FAIL: '$t' not on PATH" >&2; exit 1; }
done

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  Linux-x86_64)  TARGET=x86_64-unknown-linux-musl ;;
  Linux-aarch64|Linux-arm64) TARGET=aarch64-unknown-linux-musl ;;
  *) echo "FAIL: no OTA target for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
ARCHIVE="beebeeb-cli-$TARGET.tar.xz"

# Scratch dir, never $HOME. Removed only if it is the mktemp dir we made.
SCRATCH="$(mktemp -d)"
cleanup() { case "${SCRATCH:-}" in */tmp.*|/tmp/*) rm -rf -- "$SCRATCH" ;; esac; }
trap cleanup EXIT
mkdir -p "$SCRATCH/home" "$SCRATCH/old" "$SCRATCH/new" "$SCRATCH/dl"
echo "scratch: $SCRATCH (HOME for every bb run)"

FAILS=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*" >&2; FAILS=$((FAILS + 1)); }

# Every bb invocation goes through here: scratch HOME, scratch XDG, updater on.
run_bb() { # binary args...
  local bin="$1"; shift
  env -u BB_NO_UPDATE HOME="$SCRATCH/home" XDG_CONFIG_HOME="$SCRATCH/home/.config" XDG_DATA_HOME="$SCRATCH/home/.local/share" "$bin" "$@" 2>&1 | sed $'s/\033\\[[0-9;]*m//g'
}

# --- 1. wait for the published state to be what an installed bb will see -----
gh release download "v$NEW" -R "$REPO" -p dist-manifest.json -D "$SCRATCH/dl" --clobber >/dev/null
want="$(shasum -a 256 "$SCRATCH/dl/dist-manifest.json" | awk '{print $1}')"
url="https://github.com/$REPO/releases/download/v$NEW/dist-manifest.json"
i=0
latest=""
served=""
while :; do
  i=$((i + 1))
  served="$(curl -sfL "$url" | shasum -a 256 | awk '{print $1}' || true)"
  latest="$(gh api "repos/$REPO/releases/latest" --jq .tag_name 2>/dev/null || true)"
  if [[ "$served" == "$want" && "$latest" == "v$NEW" ]]; then break; fi
  if [[ $i -ge $TRIES ]]; then break; fi
  echo "waiting ($i/$TRIES): plain manifest URL serves ${served:0:12}.. (want ${want:0:12}..), releases/latest = ${latest:-?}"
  sleep "$SLEEP"
done
if [[ "$served" == "$want" ]]; then pass "plain dist-manifest.json URL serves the published manifest (after $i check(s))"; else fail "plain manifest URL still serves ${served:0:12}.. not ${want:0:12}.. after $i checks (CDN lag or stale asset)"; fi
if [[ "$latest" == "v$NEW" ]]; then pass "releases/latest = v$NEW"; else fail "releases/latest is '${latest:-unreachable}', not v$NEW (draft still unpublished, or marked pre-release?)"; fi
man_sha="$(jq -r --arg a "$ARCHIVE" '.artifacts[$a].checksums.sha256 // empty' "$SCRATCH/dl/dist-manifest.json")"
if [[ -n "$man_sha" ]]; then pass "manifest carries checksums.sha256 for $ARCHIVE"; else fail "manifest has NO checksums.sha256 for $ARCHIVE: src/update.rs will refuse to update, silently"; fi
if [[ $FAILS -gt 0 ]]; then echo "RESULT: $FAILS precondition(s) FAILED -- OTA cannot work yet" >&2; exit 1; fi

# --- 2. the old binary ----------------------------------------------------------
gh release download "v$OLD" -R "$REPO" -p "$ARCHIVE" -D "$SCRATCH/old" --clobber >/dev/null
tar -xJf "$SCRATCH/old/$ARCHIVE" -C "$SCRATCH/old" --strip-components=1
OLDBIN="$SCRATCH/old/bb"
[[ -x "$OLDBIN" ]] || { echo "FAIL: no bb in v$OLD archive" >&2; exit 1; }
before="$(BB_NO_UPDATE=1 HOME="$SCRATCH/home" "$OLDBIN" --version 2>&1)"
if [[ "$before" == "bb $OLD" ]]; then pass "before: '$before'"; else fail "old binary reports '$before', expected 'bb $OLD'"; fi

# --- 3. first run: must self-update ---------------------------------------------
out1="$(run_bb "$OLDBIN" --version || true)"
while IFS= read -r line; do printf '  run1| %s\n' "$line"; done <<<"$out1"
if grep -Eq "Updating .*v$OLD -> v$NEW" <<<"$out1"; then pass "run1 printed 'Updating ... v$OLD -> v$NEW'"; else fail "run1 did not print the Updating line (OTA did not trigger)"; fi
if grep -q "Updated" <<<"$out1"; then pass "run1 printed the Updated confirmation"; else fail "run1 did not print Updated"; fi

# --- 4. second run + the bytes on disk ------------------------------------------
after="$(run_bb "$OLDBIN" --version || true)"
if [[ "$after" == "bb $NEW" ]]; then pass "after: '$after'"; else fail "after update the binary reports '$after', expected 'bb $NEW'"; fi

gh release download "v$NEW" -R "$REPO" -p "$ARCHIVE" -D "$SCRATCH/new" --clobber >/dev/null
tar -xJf "$SCRATCH/new/$ARCHIVE" -C "$SCRATCH/new" --strip-components=1
if cmp -s "$OLDBIN" "$SCRATCH/new/bb"; then pass "binary on disk is byte-identical to the v$NEW release archive's bb"; else fail "binary on disk differs from the v$NEW archive's bb"; fi

if [[ $FAILS -gt 0 ]]; then echo "RESULT: OTA rung FAILED ($FAILS)" >&2; exit 1; fi
echo "RESULT: OTA rung passed v$OLD -> v$NEW on $TARGET"
