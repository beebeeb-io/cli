# Beebeeb prober

The hourly **upload canary**: one `bb` device-auth login as `bot-probe-1`, then
a 1 MB push → pull → sha256 compare → delete against `api.beebeeb.io`. It is
the only continuous check that a real file can be encrypted, uploaded,
downloaded and byte-compared end to end.

## Why it does not run on GitHub Actions

`prod-bot-vault.yml`'s schedule was commented out on 2026-06-09 because the
large-body chunk PUT hangs ~25 minutes from GitHub-hosted runners. That was
measured, not guessed: 4/4 parallel attempts hung, while the same `bb push`
succeeds from any non-GitHub network. The server-side half of the class (an
unbounded S3 PutObject) was fixed separately by task 0732. The remaining cause
is the runner's egress, so the fix is a runner that is not GitHub's — decision
`0730` item 10.

The three no-upload bots (`prod-bot-auth`, `prod-bot-shared-state`,
`prod-bot-wasm`) stay on GitHub Actions; they are proven green there.

## Install

```sh
sudo bash prober/install.sh --dry-run   # review
sudo bash prober/install.sh
```

Then fill `/etc/beebeeb-prober/prober.env` (chmod 600), join the host to
WireGuard, and add it to the `node` scrape job.

## How you know it is alive

Every run writes `/var/lib/node_exporter/textfile_collector/bb_prodbot_vault.prom`:

```
bb_prodbot_vault_last_run_timestamp <unix>
bb_prodbot_vault_last_exit_code <int>
bb_prodbot_vault_last_success_timestamp <unix>
```

Two alerts watch it, both `severity: critical`: `ProdBotVaultFailing` (a run
failed) and `ProdBotVaultStale` (no successful run in three hours, **or the
metric is absent entirely**). The second is the dead-man `0705` requires — if
the prober dies, is unplugged, or was never installed, it fires rather than
going quiet.

## Pausing

Set `BB_BOTS_DISABLED=1` in `/etc/beebeeb-prober/prober.env`. The suite exits 0
at its own kill-switch guard and the heartbeat stays fresh, so no alert fires.
