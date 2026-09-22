#!/usr/bin/env bash
# Prober entrypoint for the vault suite (0705 phase 2 / decision 0730 item 10).
#
# GitHub-hosted runners cannot complete the large-body chunk PUT against prod
# (4/4 measured attempts hung ~25 minutes; the same push succeeds from any
# non-GitHub network). The server-side half of that class was fixed by task
# 0732. This wrapper runs the SAME scripts/prod-bots/vault-suite.sh from a
# small EU prober host, under a hard timeout, writing a heartbeat every run.
set -uo pipefail

REPO="${BB_PROBER_REPO:-/opt/beebeeb-prober/cli}"
export PROM_TEXTFILE_DIR="${PROM_TEXTFILE_DIR:-/var/lib/node_exporter/textfile_collector}"
export API_BASE_URL="${API_BASE_URL:-https://api.beebeeb.io}"
export EVIDENCE_DIR="${EVIDENCE_DIR:-/var/lib/beebeeb-prober/evidence}"

mkdir -p "$PROM_TEXTFILE_DIR" "$EVIDENCE_DIR"

cd "$REPO" || { echo "prober: repo not found at $REPO"; exit 2; }
git pull --ff-only origin main >/dev/null 2>&1 || echo "prober: git pull failed, running the checked-out revision"
cargo build --release --quiet || { echo "prober: bb build failed"; exit 3; }
export PATH="$REPO/target/release:$PATH"

# 600s is well above a healthy run (seconds) and well below the timer period,
# so a network stall fails the run instead of overlapping with the next one.
timeout 600 bash scripts/prod-bots/vault-suite.sh
exit $?
