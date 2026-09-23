# Beebeeb CLI 0.10.0 — account security, billing, and vault organization land in the CLI

This release brings the CLI to parity with the web app for day-to-day account management —
two-factor authentication, session and passkey management, and billing all now work from the
terminal — and adds vault organization commands (`mkdir`, `mv`, `rm`/`restore`/`trash`, `search`)
that were previously web-only. It also closes out the migration of every upload path to the v2
chunked session route and removes a login-time fallback that could silently degrade a failed key
derivation. 114 commits since v0.9.1 (2026-05-31).

### What's New

**Vault organization**
- `bb mkdir <path>` (with `-p` for recursive) creates folders (`fc2fc7f`).
- `bb mv <src> <dst>` renames or moves a file/folder; a bulk form moves several sources into one
  destination folder (`b2e7fdf`, `60604ad`).
- `bb rm`, `bb restore`, and `bb trash list`/`bb trash empty` bring the trash lifecycle to the
  CLI, including multi-target and recursive `rm` (task 0502/0503, `56bb6a6`) and a `--permanent`
  irreversible delete gated behind a mandatory step-up token, restricted to a single target at a
  time since the token is single-use, with an explicit "we cannot recover them" warning (task
  0504/0507, `4af6e0a`, `64f1afe`, `dd99e31`, `63b0696`).
- `bb search <query>` finds files/folders by name, decrypted locally; it now fetches the file
  index once and decrypts in parallel instead of walking every folder (task 0810, `81d3f16`,
  `9e78530`).
- `bb ls` gained `-l`/`-a`/`-R`/`--depth`/`--sort`/`-r` and batches name decryption in parallel
  (task 0810, `b710427`, `ae3706d`).
- `bb ls`, `bb search`, and `bb trash list` no longer silently truncate at 200 entries — listing
  now walks the full server-side cursor (task 0755, `70b3612`).

**Two-factor authentication**
- `bb 2fa status`, `setup`, `enable`, and `disable` manage TOTP from the CLI: `setup` renders the
  otpauth URI as an ASCII QR and prints backup codes with a loss warning; `status` reads the
  account's TOTP flag directly, since no separate status route exists server-side (PR #17/#18/#19,
  tasks 0477/0478/0479).

**Sessions and passkeys**
- `bb sessions list` shows device, kind, country, and last-seen, with the current session marked
  (PR #20, task 0480).
- `bb sessions revoke <id>` and `bb sessions revoke-all-others` end sessions; revoking the current
  session is refused with a pointer to `bb logout` (PR #21, task 0481).
- `bb passkey list`, `bb passkey add` (opens the right web passkey page for whichever environment
  `bb` is configured against — prod vs. local — since adding a passkey is a browser-only WebAuthn
  ceremony), and `bb passkey remove` (PR #22/#23/#24, tasks 0482/0483/0484).

**Billing**
- `bb billing show`, `bb billing usage`, and `bb billing invoices` read live billing data — plan,
  server-computed quota including referral bonus, an explicitly approximate per-region usage
  breakdown, and invoice PDFs downloaded to a private, collision-safe temp file (PR #25/#26/#27,
  tasks 0485/0486/0487).
- `bb account billing` opens the billing portal; `bb account addons` views and purchases storage
  add-ons (task 0489, `bf80927`).
- `bb account update --email <new>` changes the account email, with verification sent to the new
  inbox (`6af780e`).

### Bug Fixes / Hardening

- The upload driver (`bb push`, `bb sync`, `bb mount`, `bb webdav`) is fully migrated to the v2
  `/api/v1/uploads/*` session route (task 0689, `b763a24`); `bb repair` — the last caller of the
  deprecated V1 multipart path — was migrated too (`c9d6145`).
- `bb sync` reconciles remote deletions instead of resurrecting them (task 0806, `0332a49`), and
  stops its session cleanly on exit so a closed sync no longer zombies or spams the server
  (`2292d74`).
- `bb login`'s browser handshake now uses `beebeeb_core::cli_auth` instead of a hand-rolled
  P-256/AES-GCM/HKDF implementation, wire-pinned against a shared ECDH compatibility vector (task
  0861, `5e309f6`); decrypted login credentials are wrapped in `Zeroizing` and wiped from memory
  on drop (task 0862, `7ed6caf`).
- The OPAQUE zero-key fallback was removed from login — a failed key derivation now fails loudly
  instead of silently degrading (task 0473, `43dd6a4`).
- `bb whoami` handles the `Plan::Starter` variant instead of failing to build against newer core
  plan types (PR #12, task 1386).
- Billing prices are now read from the server; the stale local price table was dropped (task 1040,
  `6085c6a`).
- Every API request now sends `X-Beebeeb-Client: cli` and `X-Beebeeb-Client-Version` so the server
  can attribute writes by client (PR #14, task 1392).
- The non-functional `bb account export`/`bb account delete` stub subcommands were removed from
  the command tree — those flows live in the web app (Guus decision 0859, `ebc4981`).

### Verification

- `cargo build --release` — succeeds; `./target/release/bb --version` prints `bb 0.10.0`.
- `cargo test` — two binaries: `test result: ok. 219 passed; 0 failed; 1 ignored` (main suite),
  `test result: ok. 4 passed; 0 failed` (login ECDH handshake suite) — 223 passed, 0 failed total.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo fmt -- --check` — clean.
- `dist plan` — plans `v0.10.0` across all 5 targets (macOS aarch64/x86_64, Linux musl
  aarch64/x86_64, Windows msvc) plus the shell installer and Homebrew formula.
- Not covered here: the actual tagged cargo-dist CI build, the `curl | sh` install smoke test, and
  any live-server command (`login`/`whoami`) — those need the lead's tag push and are out of scope
  for a worktree PR that must not touch production or a real account.

### Install / Update

```
curl -sSf https://beebeeb.io/install.sh | sh    # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) also self-update on next run via the
built-in OTA updater. Windows installs do not self-update yet — run `scoop update bb` to get
0.10.0.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.9.1...v0.10.0
