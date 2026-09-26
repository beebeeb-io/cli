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

# Heartbeat for the Prometheus node_exporter textfile collector. Only written
# when PROM_TEXTFILE_DIR is set, so the GitHub `workflow_dispatch` path is
# byte-for-byte unchanged. Mirrors the pg_backup_last_success_timestamp
# pattern that backs the BackupStale alert.
VAULT_STARTED_AT="$(date +%s)"
write_heartbeat() {
  local code="$1"
  [ -n "${PROM_TEXTFILE_DIR:-}" ] || return 0
  local out="$PROM_TEXTFILE_DIR/bb_prodbot_vault.prom"
  local tmp="$out.$$"
  {
    echo "# HELP bb_prodbot_vault_last_run_timestamp Unix time the vault suite last started."
    echo "# TYPE bb_prodbot_vault_last_run_timestamp gauge"
    echo "bb_prodbot_vault_last_run_timestamp $VAULT_STARTED_AT"
    echo "# HELP bb_prodbot_vault_last_exit_code Exit code of the last vault suite run."
    echo "# TYPE bb_prodbot_vault_last_exit_code gauge"
    echo "bb_prodbot_vault_last_exit_code $code"
    if [ "$code" = "0" ]; then
      echo "# HELP bb_prodbot_vault_last_success_timestamp Unix time the vault suite last passed."
      echo "# TYPE bb_prodbot_vault_last_success_timestamp gauge"
      echo "bb_prodbot_vault_last_success_timestamp $(date +%s)"
    elif [ -f "$out" ]; then
      # Prometheus text exposition requires HELP/TYPE metadata BEFORE the
      # first sample of a metric; node_exporter's textfile-collector parser
      # rejects a TYPE line seen after a sample, making the WHOLE file
      # unparsable (so last_exit_code never exports either) instead of just
      # dropping the retained success timestamp (Codex P1, PR #15). Emit
      # metadata first, sample last — same order as every other metric above.
      grep '^# HELP bb_prodbot_vault_last_success_timestamp' "$out" 2>/dev/null || true
      grep '^# TYPE bb_prodbot_vault_last_success_timestamp' "$out" 2>/dev/null || true
      grep '^bb_prodbot_vault_last_success_timestamp' "$out" 2>/dev/null || true
    fi
  } > "$tmp"
  mv "$tmp" "$out"
}
# GNU `timeout` (used by the off-GitHub prober wrapper, prober/run-vault-suite.sh,
# with a 600s guard) sends SIGTERM to this process on expiry. Without an
# explicit trap, bash's EXIT trap below sees $? as whatever the last
# completed foreground command returned — which can be 0 (e.g. a successful
# `bb login` moments before the timeout fires mid-`bb push`) even though the
# run as a whole was killed for hanging. That forges a healthy heartbeat for
# the exact hang this timeout exists to catch (Codex P1, PR #15). Map both
# termination signals to an unambiguous nonzero status BEFORE the EXIT trap
# is registered, so `write_heartbeat` always records the real outcome. This
# keeps vault-suite.sh the ONE writer of the heartbeat on every invocation
# path (GitHub workflow_dispatch, a direct run, and under the prober's
# `timeout 600`) — the wrapper never writes a competing heartbeat itself.
trap 'exit 143' TERM
trap 'exit 130' INT

# Registered here (before the BB_BOTS_DISABLED early-exit below) so the
# disabled/kill-switch path also refreshes the heartbeat — see the deviation
# note in this task's notes: registering it only at the WORK= trap below (as
# the plan's literal text places it) means the BB_BOTS_DISABLED=1 exit at
# line ~43 runs before any EXIT trap exists, so write_heartbeat never fires
# and the heartbeat goes stale during an intentional pause, contradicting
# this same commit's README ("the heartbeat stays fresh, so no alert fires").
# $WORK does not exist yet on this path, so guard it.
trap 'code=$?; [ -n "${WORK:-}" ] && rm -rf "$WORK"; write_heartbeat "$code"' EXIT

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
# Cleanup + heartbeat trap already registered above (before the disabled
# check) so it covers every exit path; WORK now being set is handled by that
# trap's guard.

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

# ── 3. SHARE check: push → share(E2E) → cold-open recipient → hash-match → revoke ──
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

# create_share — SWAPPABLE create step (0708 A1 is dual-mode). The
# client-token + two-wrapped-blobs flow this note used to describe as future
# work landed in cli #44 (v0.11.0, 2026-09-26): `bb share` now ALWAYS mints
# the token client-side and sends owner_wrapped_key/owner_wrapped_token, and
# --double-encrypted was removed (the server refuses anything else — the
# hidden --no-double-encrypt errors on purpose). No flag is needed any more;
# keep this function as the swap point if a future create-flow change needs
# one. The rest of the share check is create-flow-agnostic.
create_share() { bb share "$1" --json; }

SHARE_JSON="$(create_share "$SHARE_FILE_ID")"
SHARE_URL="$(echo "$SHARE_JSON" | jq -r '.url')"
SHARE_ID="$(echo "$SHARE_JSON" | jq -r '.share_id')"
DOUBLE_ENCRYPTED="$(echo "$SHARE_JSON" | jq -r '.double_encrypted')"
[[ -z "$SHARE_URL" || "$SHARE_URL" == "null" ]] && { echo "FAIL: no share url"; exit 1; }

# Pin what v0.11.0 actually guarantees (src/commands/share.rs "Share wire
# format"): every share is end-to-end encrypted and the decryption key
# travels ONLY in the link's #key= fragment, never to the server. A share
# link without that fragment, or a create response not marked
# double_encrypted, would mean the E2E guarantee silently regressed.
[[ "$DOUBLE_ENCRYPTED" == "true" ]] || { echo "FAIL: share not marked double_encrypted (got: $DOUBLE_ENCRYPTED)"; exit 1; }
[[ "$SHARE_URL" == *"#key="* ]] || { echo "FAIL: share url has no #key= fragment — key must never reach the server: $SHARE_URL"; exit 1; }
echo "  share created: ${SHARE_URL%%#*}#key=<redacted> share_id=$SHARE_ID double_encrypted=$DOUBLE_ENCRYPTED"

# cold-open recipient verify in a FRESH unauthenticated browser (the load-bearing
# assertion — the exact recipient journey): filename decrypts AND bytes hash-match,
# i.e. the recipient can derive the file key from #key= alone and it round-trips.
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
