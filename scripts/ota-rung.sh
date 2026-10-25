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
#   1. waits until releases/latest/download/dist-manifest.json (the URL
#      src/update.rs fetches) serves the same bytes as the v<new> release asset,
#      and releases/latest reports <new-version> (the CDN lags after an upload);
#   2. downloads the old release binary for THIS host and extracts two fresh
#      copies, each with its own scratch HOME;
#   3. automatic path: copy A runs `bb completions zsh` (a local subcommand that
#      reaches update::check_and_update; since 0.14.2 `--version`/`--help` exit in
#      clap before it, so they never auto-update, task 2195) and must print
#      "Updating bb v<old> -> v<new>" exactly once, exit 0, and still print the
#      completions (the re-exec'd new binary ran the user's command);
#   4. manual path (old >= 0.14.2, where `bb update` exists): copy B runs
#      `bb update` and must print the Updating and "Updated bb v<old> -> v<new>"
#      lines; for an older <old> it is SKIPPED, loudly;
#   5. for each copy that ran: `--version` must report <new-version>, the binary
#      on disk must be byte-identical to the new release archive's bb, and the
#      scratch HOME must hold last_update_check (proof bb used it) and nothing
#      else (no session, no account).
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
mkdir -p "$SCRATCH/h-auto" "$SCRATCH/h-manual" "$SCRATCH/old" "$SCRATCH/auto" "$SCRATCH/manual" "$SCRATCH/new" "$SCRATCH/dl"
echo "scratch: $SCRATCH (h-auto / h-manual are the HOME of every bb run)"

FAILS=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*" >&2; FAILS=$((FAILS + 1)); }

# Every bb invocation goes through here: its own scratch HOME, scratch XDG, updater on.
# OUT=<file> sends stdout there and returns only stderr (the updater's lines).
run_bb() { # home binary args...
  local home="$1" bin="$2" out="${OUT:-/dev/stdout}"; shift 2
  env -u BB_NO_UPDATE HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" "$bin" "$@" 2>&1 >"$out" | sed $'s/\033\\[[0-9;]*m//g'
}

# --- 1. wait for the published state to be what an installed bb will see -----
gh release download "v$NEW" -R "$REPO" -p dist-manifest.json -D "$SCRATCH/dl" --clobber >/dev/null
want="$(shasum -a 256 "$SCRATCH/dl/dist-manifest.json" | awk '{print $1}')"
url="https://github.com/$REPO/releases/latest/download/dist-manifest.json"
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
if [[ "$served" == "$want" ]]; then pass "latest/download/dist-manifest.json (what bb fetches) serves the v$NEW manifest (after $i check(s))"; else fail "latest/download manifest URL still serves ${served:0:12}.. not ${want:0:12}.. after $i checks (CDN lag or stale asset)"; fi
if [[ "$latest" == "v$NEW" ]]; then pass "releases/latest = v$NEW"; else fail "releases/latest is '${latest:-unreachable}', not v$NEW (draft still unpublished, or marked pre-release?)"; fi
man_sha="$(jq -r --arg a "$ARCHIVE" '.artifacts[$a].checksums.sha256 // empty' "$SCRATCH/dl/dist-manifest.json")"
if [[ -n "$man_sha" ]]; then pass "manifest carries checksums.sha256 for $ARCHIVE"; else fail "manifest has NO checksums.sha256 for $ARCHIVE: src/update.rs will refuse to update, silently"; fi
if [[ $FAILS -gt 0 ]]; then echo "RESULT: $FAILS precondition(s) FAILED -- OTA cannot work yet" >&2; exit 1; fi

# --- 2. the old binary, twice ------------------------------------------------------
gh release download "v$OLD" -R "$REPO" -p "$ARCHIVE" -D "$SCRATCH/old" --clobber >/dev/null
for copy in auto manual; do tar -xJf "$SCRATCH/old/$ARCHIVE" -C "$SCRATCH/$copy" --strip-components=1; done
gh release download "v$NEW" -R "$REPO" -p "$ARCHIVE" -D "$SCRATCH/new" --clobber >/dev/null
tar -xJf "$SCRATCH/new/$ARCHIVE" -C "$SCRATCH/new" --strip-components=1
for copy in auto manual; do
  bin="$SCRATCH/$copy/bb"
  [[ -x "$bin" ]] || { echo "FAIL: no bb in v$OLD archive" >&2; exit 1; }
  before="$(BB_NO_UPDATE=1 HOME="$SCRATCH/h-$copy" "$bin" --version 2>&1)"
  if [[ "$before" == "bb $OLD" ]]; then pass "$copy before: '$before'"; else fail "$copy: old binary reports '$before', expected 'bb $OLD'"; fi
done

# --- 3. automatic path: a local subcommand that reaches check_and_update -------------
out1="$(OUT="$SCRATCH/completions.zsh" run_bb "$SCRATCH/h-auto" "$SCRATCH/auto/bb" completions zsh && echo "rc=0" || echo "rc=$?")"
while IFS= read -r line; do printf '  auto| %s\n' "$line"; done <<<"$out1"
n_up="$(grep -cF "Updating bb v$OLD -> v$NEW" <<<"$out1" || true)"
if [[ "$n_up" == 1 ]]; then pass "auto: 'bb completions zsh' printed 'Updating bb v$OLD -> v$NEW' exactly once"; else fail "auto: $n_up Updating line(s), want exactly 1 (0 = the automatic update did not trigger)"; fi
if grep -qx "rc=0" <<<"$out1"; then pass "auto: the command exited 0 after the update"; else fail "auto: the command did not exit 0 ($(grep '^rc=' <<<"$out1"))"; fi
if [[ "$(head -1 "$SCRATCH/completions.zsh" 2>/dev/null)" == "#compdef bb" ]]; then pass "auto: the re-exec'd v$NEW still ran the user's command (completions start with '#compdef bb')"; else fail "auto: no completions output after the update: the user's command never ran"; fi

# --- 4. manual path: bb update (exists from 0.14.2) -----------------------------
if [[ "$(printf '%s\n' 0.14.2 "$OLD" | sort -V | head -1)" == 0.14.2 ]]; then
  MANUAL=1
  out2="$(run_bb "$SCRATCH/h-manual" "$SCRATCH/manual/bb" update || true)"
  while IFS= read -r line; do printf '  manual| %s\n' "$line"; done <<<"$out2"
  if grep -qF "Updating bb v$OLD -> v$NEW" <<<"$out2"; then pass "manual: 'bb update' printed 'Updating bb v$OLD -> v$NEW'"; else fail "manual: no Updating line"; fi
  if grep -qxF "Updated bb v$OLD -> v$NEW" <<<"$out2"; then pass "manual: printed 'Updated bb v$OLD -> v$NEW'"; else fail "manual: no 'Updated bb v$OLD -> v$NEW' line"; fi
else
  MANUAL=0
  echo "SKIP  manual: 'bb update' exists from 0.14.2; v$OLD has only the automatic path (proved above)"
fi

# --- 5. version, bytes on disk, and a scratch HOME that bb used and nothing else ------
copies="auto"; [[ $MANUAL == 1 ]] && copies="auto manual"
for copy in $copies; do
  after="$(run_bb "$SCRATCH/h-$copy" "$SCRATCH/$copy/bb" --version || true)"
  if [[ "$after" == "bb $NEW" ]]; then pass "$copy after: '$after'"; else fail "$copy: after the update the binary reports '$after', expected 'bb $NEW'"; fi
  if cmp -s "$SCRATCH/$copy/bb" "$SCRATCH/new/bb"; then pass "$copy: binary on disk is byte-identical to the v$NEW archive's bb"; else fail "$copy: binary on disk differs from the v$NEW archive's bb"; fi
  # awk reads all of find's output (no SIGPIPE under pipefail), keeping the first 20
  files="$(cd "$SCRATCH/h-$copy" && find . -type f | awk 'NR <= 20')"
  extra="$(grep -v '/last_update_check$' <<<"$files" | awk 'NF' || true)"
  if grep -q '/last_update_check$' <<<"$files"; then pass "$copy: bb wrote last_update_check into the scratch HOME (it used it)"; else fail "$copy: no last_update_check in the scratch HOME: bb did not use it (did it write to the real HOME?)"; fi
  if [[ -z "$extra" ]]; then pass "$copy: scratch HOME holds nothing else (no session, no account)"; else fail "$copy: scratch HOME holds unexpected files: $(tr '\n' ' ' <<<"$extra")"; fi
done

if [[ $FAILS -gt 0 ]]; then echo "RESULT: OTA rung FAILED ($FAILS)" >&2; exit 1; fi
if [[ $MANUAL == 1 ]]; then echo "RESULT: OTA rung passed v$OLD -> v$NEW on $TARGET (automatic + manual paths)"
else echo "RESULT: OTA rung passed v$OLD -> v$NEW on $TARGET (automatic path; manual path n/a before 0.14.2)"; fi
