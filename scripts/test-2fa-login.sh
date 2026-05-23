#!/usr/bin/env bash
# End-to-end smoke test: bb login against a 2FA-enabled account.
#
# Requires:
#   - bb on PATH (cargo install --path ../ or a packaged build)
#   - bun (for the Node runner under scripts/test-2fa-driver.mjs)
#   - playwright already installed in repos/web (we reuse its browsers)
#   - CLI_AUTH_TEST_EMAIL, CLI_AUTH_TEST_PASSWORD, CLI_AUTH_TEST_TOTP_SECRET in env
#   - API_BASE_URL (defaults to https://api.beebeeb.io)
#
# What it does:
#   1. Spawn `bb login` in a background process with a captured stdout pipe.
#   2. Scrape the user code from stdout (matches /[A-HJ-NP-Z2-9]{4}-[A-HJ-NP-Z2-9]{4}/).
#   3. Hand the code to a Playwright driver that signs in + completes TOTP +
#      lands on /cli-auth?code=XXXX-XXXX and clicks "Authorize".
#   4. Wait for bb to exit. Assert exit code 0.
#   5. Run `bb whoami` and assert it prints CLI_AUTH_TEST_EMAIL.
#
# Evidence captured into ./test-2fa-evidence/:
#   - bb-login.stdout, bb-login.stderr
#   - playwright-trace.zip
#   - bb-whoami.stdout
#   - run.json (metadata: timestamp, server URL, exit codes)

set -euo pipefail

API_BASE_URL="${API_BASE_URL:-https://api.beebeeb.io}"
EVIDENCE_DIR="${EVIDENCE_DIR:-./test-2fa-evidence}"
mkdir -p "$EVIDENCE_DIR"

: "${CLI_AUTH_TEST_EMAIL:?need CLI_AUTH_TEST_EMAIL in env}"
: "${CLI_AUTH_TEST_PASSWORD:?need CLI_AUTH_TEST_PASSWORD in env}"
: "${CLI_AUTH_TEST_TOTP_SECRET:?need CLI_AUTH_TEST_TOTP_SECRET in env}"

echo "== bb login 2FA smoke test"
echo "   API:   $API_BASE_URL"
echo "   user:  $CLI_AUTH_TEST_EMAIL"
echo "   out:   $EVIDENCE_DIR"

# Start bb login in the background, capture its stdout.
BB_PIPE="$(mktemp -u)"
mkfifo "$BB_PIPE"

(
  bb --api "$API_BASE_URL" login --headless 2>"$EVIDENCE_DIR/bb-login.stderr" \
    | tee "$EVIDENCE_DIR/bb-login.stdout" > "$BB_PIPE"
) &
BB_PID=$!

# Read the pipe until we see the XXXX-XXXX code.
USER_CODE=""
while IFS= read -r line; do
  echo "  bb> $line"
  if [[ "$line" =~ ([A-HJ-NP-Z2-9]{4}-[A-HJ-NP-Z2-9]{4}) ]]; then
    USER_CODE="${BASH_REMATCH[1]}"
    break
  fi
done < "$BB_PIPE"
rm -f "$BB_PIPE"

if [[ -z "$USER_CODE" ]]; then
  echo "FAIL: never saw a user code in bb stdout"
  kill "$BB_PID" 2>/dev/null || true
  exit 1
fi

echo "  user_code: $USER_CODE"

# Drive the browser. Trace goes into $EVIDENCE_DIR.
USER_CODE="$USER_CODE" \
API_BASE_URL="$API_BASE_URL" \
EVIDENCE_DIR="$EVIDENCE_DIR" \
bun run "$(dirname "$0")/test-2fa-driver.mjs"

# bb should exit 0 once the browser confirms.
wait "$BB_PID"
BB_EXIT=$?
echo "  bb exit: $BB_EXIT"

if [[ "$BB_EXIT" -ne 0 ]]; then
  echo "FAIL: bb login exited $BB_EXIT"
  exit "$BB_EXIT"
fi

# Confirm the session works.
bb --api "$API_BASE_URL" whoami | tee "$EVIDENCE_DIR/bb-whoami.stdout"

if ! grep -q "$CLI_AUTH_TEST_EMAIL" "$EVIDENCE_DIR/bb-whoami.stdout"; then
  echo "FAIL: bb whoami did not return the expected email"
  exit 1
fi

# Record metadata.
cat > "$EVIDENCE_DIR/run.json" <<JSON
{
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "api_base_url": "$API_BASE_URL",
  "user_code": "$USER_CODE",
  "bb_exit": $BB_EXIT,
  "bb_version": "$(bb --version | head -1)"
}
JSON

echo "PASS: bb login → 2FA → bb whoami round-trip succeeded"
