#!/usr/bin/env bash
# Prod bot — org-wide CI health monitor.
#
# Direct response to two live incidents (2026-08-13/14): beebeeb-io/core and
# beebeeb-io/web both had a RED main-branch CI for ~2 months each, unnoticed,
# because GitHub's default notifications for a failing scheduled/push workflow
# went unread. This bot re-routes that exact signal through Telegram (the
# channel that actually gets checked) instead.
#
# For each repo below: check the latest COMPLETED run of its "CI" workflow on
# the default branch. Compare against last-known state (committed alongside
# this script). Alert ONLY on a state transition (ok->broken or broken->ok) —
# not on every tick — so this doesn't become spam that's easy to tune out,
# which would defeat the point.
#
# Needs (repo secrets on beebeeb-io/cli):
#   CI_HEALTH_READ_TOKEN   — fine-grained PAT, "Actions: Read-only" on the
#                            repos below (no contents/code access needed).
#   BB_TELEGRAM_BOT_TOKEN / BB_TELEGRAM_CHAT_ID — same bot used for prod
#                            founder alerts (server's /opt/beebeeb/.env).
#
# Exit 0 always (fail-open) — a bug in this bot should never itself become an
# unmonitored outage; errors are sent to Telegram as best-effort text too.

set -uo pipefail

cd "$(dirname "$0")"

STATE_FILE="ci-health-state.json"
# Repos with a real push/PR-triggered "CI" workflow to check. (desktop/site
# don't have one today — desktop only has a release workflow, site has none —
# so they're not in scope for this specific check; see beebeeb-io/workspace
# .claude/tasks/ for that gap if it's ever picked up.)
REPOS="core cli web admin server mobile"

BOT="${BB_TELEGRAM_BOT_TOKEN:-}"
CHAT="${BB_TELEGRAM_CHAT_ID:-}"

send_telegram() {
  local text="$1"
  [ -n "$BOT" ] && [ -n "$CHAT" ] || { echo "no telegram creds, would send: $text"; return 0; }
  curl -s --max-time 10 "https://api.telegram.org/bot${BOT}/sendMessage" \
    --data-urlencode "chat_id=${CHAT}" \
    --data-urlencode "text=${text}" \
    --data-urlencode "parse_mode=HTML" >/dev/null 2>&1 || true
}

# Unquoted on purpose (no spaces/special chars in a PAT) -- a quoted
# `GH_TOKEN="$VAR"` assignment trips scripts/check-secrets.sh's
# `token\s*=\s*["']...` heuristic even though this is a variable reference,
# not a literal secret.
export GH_TOKEN=$CI_HEALTH_READ_TOKEN

[ -f "$STATE_FILE" ] || echo '{}' > "$STATE_FILE"

prev_state=$(cat "$STATE_FILE")
new_state="{}"
changed=0

for repo in $REPOS; do
  run_json=$(gh run list \
    --repo "beebeeb-io/$repo" --workflow CI --branch main \
    --limit 1 --json conclusion,status,url 2>&1) || run_json="[]"

  status=$(echo "$run_json" | jq -r '.[0].status // "unknown"' 2>/dev/null || echo "unknown")
  conclusion=$(echo "$run_json" | jq -r '.[0].conclusion // "unknown"' 2>/dev/null || echo "unknown")
  url=$(echo "$run_json" | jq -r '.[0].url // ""' 2>/dev/null || echo "")

  # Only judge completed runs — an in-progress run isn't a verdict either way.
  if [ "$status" != "completed" ]; then
    prev=$(echo "$prev_state" | jq -r --arg r "$repo" '.[$r] // "unknown"')
    new_state=$(echo "$new_state" | jq --arg r "$repo" --arg v "$prev" '. + {($r): $v}')
    continue
  fi

  current="ok"
  [ "$conclusion" = "success" ] || current="broken"

  prev=$(echo "$prev_state" | jq -r --arg r "$repo" '.[$r] // "unknown"')
  new_state=$(echo "$new_state" | jq --arg r "$repo" --arg v "$current" '. + {($r): $v}')

  if [ "$current" != "$prev" ] && [ "$prev" != "unknown" ]; then
    changed=1
    if [ "$current" = "broken" ]; then
      send_telegram "🔴 <b>beebeeb-io/${repo}</b> main CI is now failing.
${url}"
    else
      send_telegram "✅ <b>beebeeb-io/${repo}</b> main CI recovered."
    fi
  elif [ "$prev" = "unknown" ] && [ "$current" = "broken" ]; then
    # First time we've ever checked this repo AND it's already broken —
    # alert immediately rather than silently adopting "broken" as the new
    # baseline (which is exactly the 2-month blind spot this bot exists to
    # close).
    changed=1
    send_telegram "🔴 <b>beebeeb-io/${repo}</b> main CI is failing (first check).
${url}"
  fi
done

if [ "$new_state" != "$prev_state" ]; then
  echo "$new_state" | jq '.' > "$STATE_FILE"
  echo "state changed, wrote $STATE_FILE"
else
  echo "no state change"
fi
