# Manual 2FA smoke test

Use this when `scripts/test-2fa-login.sh` cannot run (e.g. release machine
has no Playwright browsers installed, or the test account is locked).

## Prerequisites

- A test account with TOTP enrolled. Credentials in 1Password under
  "Beebeeb / cli-auth test account (2FA)".
- A built `bb` binary on PATH.
- A browser on the same or a different machine.

## Steps

1. Open a terminal. Make a clean evidence directory:
   ```sh
   mkdir -p test-2fa-evidence-manual && cd test-2fa-evidence-manual
   ```
2. Run `bb login` (do not pass `--headless` unless you actually are on SSH —
   we want to exercise the standard path):
   ```sh
   bb --api https://api.beebeeb.io login | tee bb-login.stdout
   ```
3. Note the printed URL and 8-character code.
4. On the same or a different device, open the URL.
5. Sign in with the test account. Complete the TOTP prompt.
6. On the `/cli-auth?code=...` page, confirm the code matches the terminal.
   Click **Authorize**.
7. Wait for the terminal to print `✓ Logged in as cli-auth-test@beebeeb.io`.
8. Run `bb whoami | tee bb-whoami.stdout`. Confirm the email matches.
9. Take a screenshot of the terminal showing both outputs. Save as
   `bb-whoami.png` next to the stdout files.
10. Capture the run metadata into `run.json`:
   ```sh
   cat > run.json <<EOF
   {
     "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
     "api_base_url": "https://api.beebeeb.io",
     "bb_version": "$(bb --version | head -1)",
     "tester": "<your name>",
     "method": "manual"
   }
   EOF
   ```

## Evidence to attach to the verified task

- `bb-login.stdout`
- `bb-whoami.stdout`
- `bb-whoami.png`
- `run.json`

Move the directory into `.claude/tasks/verified/<task-id>-evidence/`
before transitioning the task.
