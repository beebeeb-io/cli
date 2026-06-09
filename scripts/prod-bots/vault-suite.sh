#!/usr/bin/env bash
# Prod bot — vault suite (032 Pillar 5 / 0705 Phase 1). ONE bb build + ONE
# login, then the vault-needing checks in sequence (CI-minute economy).
#
# Phase A (this file): bb device-auth login (bot-probe-1) + transfer check
# (1MB up → down → sha256 compare → delete). Share + revoke checks layer in next.
#
# Requires on PATH: bb, bun, playwright (chromium). Env: BOT_PROBE_1_EMAIL/
# PASSWORD/RECOVERY_PHRASE, optional BB_BOTS_DISABLED, API_BASE_URL.
set -euo pipefail

if [[ "${BB_BOTS_DISABLED:-}" == "1" || "${BB_BOTS_DISABLED:-}" == "true" ]]; then
  echo "vault-suite: BB_BOTS_DISABLED set — skipping."; exit 0
fi

API_BASE_URL="${API_BASE_URL:-https://api.beebeeb.io}"
EVIDENCE_DIR="${EVIDENCE_DIR:-./vault-bot-evidence}"
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$EVIDENCE_DIR"
: "${BOT_PROBE_1_EMAIL:?need BOT_PROBE_1_EMAIL}"
: "${BOT_PROBE_1_PASSWORD:?need BOT_PROBE_1_PASSWORD}"
: "${BOT_PROBE_1_RECOVERY_PHRASE:?need BOT_PROBE_1_RECOVERY_PHRASE}"

WORK="$(mktemp -d)"
cleanup_ids=()
trap 'rm -rf "$WORK"' EXIT

echo "== vault-suite : login"
echo "   api: $API_BASE_URL  user: $BOT_PROBE_1_EMAIL"

# ── 1. bb device-auth login (browser-driven via bb-login-driver.mjs) ──────────
BB_PIPE="$(mktemp -u)"; mkfifo "$BB_PIPE"
( bb --api "$API_BASE_URL" login --headless 2>"$EVIDENCE_DIR/bb-login.stderr" \
    | tee "$EVIDENCE_DIR/bb-login.stdout" > "$BB_PIPE" ) &
BB_PID=$!

USER_CODE=""
while IFS= read -r line; do
  echo "  bb> $line"
  if [[ "$line" =~ ([A-HJ-NP-Z2-9]{4}-[A-HJ-NP-Z2-9]{4}) ]]; then USER_CODE="${BASH_REMATCH[1]}"; break; fi
done < "$BB_PIPE"
rm -f "$BB_PIPE"
[[ -z "$USER_CODE" ]] && { echo "FAIL: no user code from bb login"; kill "$BB_PID" 2>/dev/null || true; exit 1; }
echo "  user_code: $USER_CODE"

USER_CODE="$USER_CODE" API_BASE_URL="$API_BASE_URL" EVIDENCE_DIR="$EVIDENCE_DIR" \
  bun run "$HERE/bb-login-driver.mjs"

wait "$BB_PID"; BB_EXIT=$?
[[ "$BB_EXIT" -ne 0 ]] && { echo "FAIL: bb login exited $BB_EXIT"; exit "$BB_EXIT"; }

bb --api "$API_BASE_URL" whoami | tee "$EVIDENCE_DIR/bb-whoami.stdout"
grep -q "$BOT_PROBE_1_EMAIL" "$EVIDENCE_DIR/bb-whoami.stdout" || { echo "FAIL: whoami mismatch"; exit 1; }
echo "  login OK"

# ── 2. TRANSFER check: 1MB up → down → sha256 compare → delete ────────────────
echo "== vault-suite : transfer check"
SRC="$WORK/bot-xfer-$RANDOM.bin"
head -c 1048576 /dev/urandom > "$SRC"
SRC_HASH="$(sha256sum "$SRC" | awk '{print $1}')"

PUSH_JSON="$(bb push "$SRC" --json)"
echo "$PUSH_JSON" > "$EVIDENCE_DIR/push.json"
FILE_ID="$(echo "$PUSH_JSON" | jq -r '.files[0].id')"
[[ -z "$FILE_ID" || "$FILE_ID" == "null" ]] && { echo "FAIL: no file id from push"; exit 1; }
cleanup_ids+=("$FILE_ID")
echo "  pushed id=$FILE_ID hash=${SRC_HASH:0:12}…"

DST="$WORK/bot-xfer-down.bin"
bb pull "$FILE_ID" --output "$DST" >/dev/null
DST_HASH="$(sha256sum "$DST" | awk '{print $1}')"
if [[ "$SRC_HASH" != "$DST_HASH" ]]; then
  echo "FAIL: hash mismatch — up=${SRC_HASH:0:12} down=${DST_HASH:0:12}"; exit 1
fi
echo "  round-trip hash MATCH"

# ── 3. SHARE check: push → share(double-enc) → cold-open recipient → hash-match → revoke ──
# The permanent shares-key regression guard (the program's central defect class).
echo "== vault-suite : share check"
SHARE_SRC="$WORK/bot-share-$RANDOM.bin"
head -c 524288 /dev/urandom > "$SHARE_SRC" # 512 KB
SHARE_HASH="$(sha256sum "$SHARE_SRC" | awk '{print $1}')"
SHARE_NAME="$(basename "$SHARE_SRC")"
SPUSH="$(bb push "$SHARE_SRC" --json)"
SHARE_FILE_ID="$(echo "$SPUSH" | jq -r '.files[0].id')"
[[ -z "$SHARE_FILE_ID" || "$SHARE_FILE_ID" == "null" ]] && { echo "FAIL: no file id (share push)"; exit 1; }
cleanup_ids+=("$SHARE_FILE_ID")

# create_share — SWAPPABLE create step (0708 A1 is dual-mode: the legacy
# server-generated-token flow used here stays valid forever; when rust-server's
# A1 contract lands, swap ONLY this function for the client-token + two-wrapped-
# blobs flow. The rest of the share check is create-flow-agnostic.
create_share() { bb share "$1" --double-encrypted --json; }

SHARE_JSON="$(create_share "$SHARE_FILE_ID")"
SHARE_URL="$(echo "$SHARE_JSON" | jq -r '.url')"
SHARE_ID="$(echo "$SHARE_JSON" | jq -r '.share_id')"
[[ -z "$SHARE_URL" || "$SHARE_URL" == "null" ]] && { echo "FAIL: no share url"; exit 1; }
echo "  share created: ${SHARE_URL%%#*}#key=<redacted> share_id=$SHARE_ID"

# cold-open recipient verify in a FRESH unauthenticated browser (the load-bearing
# assertion — the exact recipient journey): filename decrypts AND bytes hash-match.
if ! SHARE_URL="$SHARE_URL" EXPECTED_NAME="$SHARE_NAME" EXPECTED_SHA256="$SHARE_HASH" \
      EVIDENCE_DIR="$EVIDENCE_DIR" MODE=verify \
      bun run "$HERE/share-recipient-driver.mjs"; then
  echo "FAIL: cold-open recipient verify"
  bb unshare "$SHARE_ID" >/dev/null 2>&1 || true
  exit 1
fi

# revoke the share (also exercises the revoke path)
bb unshare "$SHARE_ID" 2>&1 | head -1 || true
echo "  share check OK"

# ── cleanup ──
for id in "${cleanup_ids[@]}"; do bb rm "$id" --force >/dev/null 2>&1 || true; done
echo "  cleaned up ${#cleanup_ids[@]} file(s)"

echo "PASS: vault-suite (login + transfer + share) OK"
