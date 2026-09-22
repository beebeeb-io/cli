#!/usr/bin/env bash
# Install the Beebeeb prober on a fresh EU VM (Debian/Ubuntu).
# Usage: sudo bash install.sh [--dry-run]
set -euo pipefail

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

run() {
  if [ "$DRY_RUN" = "1" ]; then echo "  would run: $*"; else "$@"; fi
}

echo "== beebeeb prober install (dry_run=$DRY_RUN)"
echo "-- packages: git curl jq unzip build-essential prometheus-node-exporter"
run apt-get update -qq
run apt-get install -y -qq git curl jq unzip build-essential prometheus-node-exporter

echo "-- user + directories"
run useradd --system --create-home --home-dir /var/lib/beebeeb-prober prober || true
run mkdir -p /opt/beebeeb-prober /etc/beebeeb-prober /var/lib/node_exporter/textfile_collector
run chown -R prober:prober /opt/beebeeb-prober /var/lib/beebeeb-prober /var/lib/node_exporter/textfile_collector

echo "-- rust toolchain (for the prober user)"
run su - prober -c "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.96.0"

echo "-- clone the cli repo"
run su - prober -c "git clone https://github.com/beebeeb-io/cli.git /opt/beebeeb-prober/cli"

echo "-- bun + playwright (the vault suite drives a browser for device-auth)"
run su - prober -c "curl -fsSL https://bun.sh/install | bash"
run su - prober -c "cd /opt/beebeeb-prober/cli/scripts && ~/.bun/bin/bun add playwright @otplib/preset-default && ~/.bun/bin/bunx playwright install --with-deps chromium"

echo "-- node_exporter must read the textfile collector directory"
echo "   ensure ARGS includes: --collector.textfile.directory=/var/lib/node_exporter/textfile_collector"
run sed -i 's#^ARGS=.*#ARGS="--collector.textfile.directory=/var/lib/node_exporter/textfile_collector"#' /etc/default/prometheus-node-exporter
run systemctl restart prometheus-node-exporter

echo "-- systemd units"
run install -m 0644 /opt/beebeeb-prober/cli/prober/beebeeb-prober.service /etc/systemd/system/beebeeb-prober.service
run install -m 0644 /opt/beebeeb-prober/cli/prober/beebeeb-prober.timer   /etc/systemd/system/beebeeb-prober.timer
run systemctl daemon-reload
run systemctl enable --now beebeeb-prober.timer

echo
echo "REMAINING MANUAL STEPS (not automated on purpose — they carry secrets):"
echo "  1. cp /opt/beebeeb-prober/cli/prober/prober.env.example /etc/beebeeb-prober/prober.env"
echo "     then fill in BOT_PROBE_1_*, chmod 600, chown prober:prober."
echo "  2. Join this host to the Beebeeb WireGuard network as a peer so app02's"
echo "     Prometheus can scrape 9100 over the tunnel. Do NOT expose 9100 publicly."
echo "  3. Add this host's WireGuard address to the 'node' job in prometheus.yml."
echo "  4. systemctl start beebeeb-prober.service  # first run, watch it"
echo "     journalctl -u beebeeb-prober -f"
