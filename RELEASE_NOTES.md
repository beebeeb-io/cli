# Beebeeb CLI 0.12.0 — accounts are created on the web; `bb` explains a missing or lapsed plan

Beebeeb no longer has free signups. A new account starts a trial on Starter, Basic or Pro, and
that trial needs a payment method: a card, or iDEAL, which sets up a SEPA direct debit. Only the
web checkout can take that payment. So accounts are now created on the web, and the server
(beebeeb-io/server#125) refuses account creation from the CLI and the mobile apps with
`403 signup_web_only`. `bb` does not create accounts. `bb signup` sends you to the web signup,
and `bb login` works as before.

The same server change adds two account states that `bb` now shows and explains:

- **no plan yet**: an account that never started a trial.
- **trial ended unpaid**: the vault is read-only and is deleted 60 days later unless you
  subscribe.

Existing Free accounts are grandfathered and see no change. This is a minor release (0.11.1 →
0.12.0) because it adds a command and changes what `bb` prints. Two pull requests merged since
v0.11.1; only one of them (beebeeb-io/cli#53) changes the `bb` binary.

### What's New

- **`bb signup` opens the web signup instead of creating an account.** It prints
  "Create your account at https://app.beebeeb.io/signup — then run `bb login`.", opens that page
  in your browser when one is available, and exits 0. It doesn't open a browser under `--json`
  (`{"url", "next", "note"}`), under `--quiet` (bare URL), or over SSH / without a display.
  - The web address follows the API `bb` is pointed at: `APP_URL` if set, otherwise `api.X` →
    `app.X`, and a local API → `localhost:5173`. A `--api` pointed at a local or staging server
    never hands out a production link. `bb passkey add` now uses the same logic.
  - `bb` never had a working signup command on `main`. beebeeb-io/cli#34, which would have added
    an email-code + OPAQUE registration flow, was closed without merging in favour of this.
    (beebeeb-io/cli#53)
- **`bb whoami` / `bb status`, `bb quota` and `bb billing show` show the account state.** The
  state comes from `GET /api/v1/billing/subscription`. An account with no plan yet shows plan
  `none`, state `no plan yet`, upload `blocked` and "Your account has no plan yet — choose one
  at https://app.beebeeb.io/choose-plan". A lapsed account shows `read-only (trial ended)` and
  "Your trial has ended; your vault is read-only and will be deleted on <date>. Subscribe at
  https://app.beebeeb.io/billing?view=change". `--json` on `whoami` and `quota` gains
  `account_state` (`ok` / `needs_plan` / `lapsed`) and `data_deletion_at`. A server that
  predates the change sends neither field, which reads as `ok`, and so does any value `bb`
  doesn't recognise. (beebeeb-io/cli#53)

### Bug Fixes / Hardening

- **Refused uploads and shares say why and where to fix it.** For an account without a plan,
  the server refuses upload init and share creation with `409 plan_required` or
  `409 account_lapsed`. `bb push`, `bb sync`, `bb webdav`, `bb mount`, `bb repair` and
  `bb share` now show the matching message above, with the plan-chooser or billing link,
  instead of the server's generic text. The older `413 quota_exceeded` refusal is explained the
  same way when the account state calls for it; on an account whose state is `ok` it is left as
  it was.
  - Any other `409`, such as an upload already in progress or a stale base version, reaches you
    unchanged.
  - The error code and HTTP status are kept, so retry behaviour is unchanged. (beebeeb-io/cli#53)
- **No extra round-trip on the happy path.** `bb push` checks the account state from the
  subscription it already fetched before uploading. Every other path fetches the subscription
  only after a refusal. It skips the fetch for `plan_required` and fetches at most once per run
  otherwise, so a `bb sync` of many files doesn't refetch per file. If that fetch fails, the
  lapsed message still appears, just without a date. (beebeeb-io/cli#53)
- An unused `ApiClient::signup` (a plain-password `/auth/signup` call with no callers) was
  removed, so the CLI now has no account-creation code at all. (beebeeb-io/cli#53)
- beebeeb-io/cli#52 bumped the Scoop manifest to the already-released v0.11.1. It doesn't touch
  `src/` or change `bb`'s behavior.

### Verification

CI on the code this release ships (`175d8f7`, main — the release commit on top of it only bumps the version to 0.12.0 and updates this file and the changelog; its own CI run on the release-notes PR carries the same checks):
https://github.com/beebeeb-io/cli/actions/runs/36518562618

- `cargo build --verbose` — succeeds.
- `cargo test --verbose` — every test binary green:
  - main suite (`bb`): `test result: ok. 317 passed; 0 failed; 2 ignored; 0 measured; 0 filtered
    out; finished in 76.37s`. That is 51 more than 0.11.1's 266. The new tests cover the web-URL
    derivation, account-state parsing with missing, null and unknown fields, the signup and
    lapsed/no-plan messages, the `bb push` pre-check, and mock-server tests of the `409` / `413`
    mapping. The mock-server tests include no subscription lookup on a successful upload, one
    lookup per client, an unrelated `409` left untouched, and share creation.
  - `auth_errors`: `test result: ok. 6 passed; 0 failed`
  - `ecdh_compat`: `test result: ok. 4 passed; 0 failed`
  - `mount_availability`: `test result: ok. 3 passed; 0 failed`
  - `non_interactive`: `test result: ok. 6 passed; 0 failed`
  - `pull_overwrite`: `test result: ok. 5 passed; 0 failed`
  - `share_resolves_like_pull`: `test result: ok. 1 passed; 0 failed`
  - `zero_byte_files`: `test result: ok. 4 passed; 0 failed`
  - Total: 346 passed, 0 failed, 2 ignored across 8 binaries.
- `cargo test --bin bb help_links_return_200 -- --ignored --nocapture` (network-dependent, run
  separately from the main suite) — `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;
  318 filtered out`.
- `cargo clippy --all-targets -- -D warnings` — clean (CI job "Clippy & Format", same run).
- `cargo fmt -- --check` — clean (same job).
- A debug build of this code was run against a local mock API for each account state (not a
  real server). For no
  plan and for lapsed, `bb push` refused with the messages above and exited 1, and
  `bb whoami` / `bb quota` / `bb billing show` showed the notice. For a pre-0.12 server
  response, `bb whoami` shows state `ok` and the plan as before.
- `dist plan` was not run for this notes PR — cargo-dist isn't installed on the machine that
  prepared it. It still runs as the first, fail-closed step of the tag-triggered release
  workflow, which also re-validates this file names `0.12.0` before anything builds.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh    # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in
OTA updater. Windows installs do not self-update yet — run `scoop update bb` to get 0.12.0.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.11.1...v0.12.0
