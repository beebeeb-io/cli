// repos/cli/scripts/test-2fa-driver.mjs
// Driven by ./test-2fa-login.sh. Reads USER_CODE / API_BASE_URL / EVIDENCE_DIR
// out of the env, runs a Playwright session, and exits 0 on success.

import { chromium } from 'playwright';
import { authenticator } from 'otplib';
import path from 'node:path';
import { mkdirSync } from 'node:fs';

const USER_CODE = process.env.USER_CODE;
const API_BASE_URL = process.env.API_BASE_URL ?? 'https://api.beebeeb.io';
const EVIDENCE_DIR = process.env.EVIDENCE_DIR ?? './test-2fa-evidence';

const TEST_EMAIL = process.env.CLI_AUTH_TEST_EMAIL;
const TEST_PASSWORD = process.env.CLI_AUTH_TEST_PASSWORD;
const TEST_TOTP_SECRET = process.env.CLI_AUTH_TEST_TOTP_SECRET;
// Fresh Playwright sessions have no local vault — recovery phrase is needed
// to unlock so the browser can perform the CLI ECDH handshake.
const TEST_RECOVERY_PHRASE = process.env.CLI_AUTH_TEST_RECOVERY_PHRASE;

if (!USER_CODE) {
  console.error('USER_CODE is required');
  process.exit(2);
}

// Derive the web origin from the API base. In prod, api.beebeeb.io ↔ app.beebeeb.io.
// For local dev, the server emits https://app.beebeeb.io regardless — override here.
const WEB_BASE_URL =
  process.env.WEB_BASE_URL ?? API_BASE_URL.replace('api.', 'app.');

mkdirSync(EVIDENCE_DIR, { recursive: true });

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext();
await context.tracing.start({ screenshots: true, snapshots: true, sources: true });

const page = await context.newPage();

try {
  console.log(`  driver: GET ${WEB_BASE_URL}/cli-auth?code=${USER_CODE}`);
  await page.goto(`${WEB_BASE_URL}/cli-auth?code=${USER_CODE}`);

  // We are unauthenticated → expect to be on /login.
  await page.waitForURL(/\/login/);

  // Email input is rendered via BBInput which properly associates id + label.
  await page.getByLabel(/^email/i).fill(TEST_EMAIL);
  // Password input has a bare <label>Password</label> NOT associated by
  // htmlFor/id, so getByLabel() can't find it. Use the autocomplete hint
  // which is unique on this form.
  await page.locator('input[autocomplete="current-password"]').fill(TEST_PASSWORD);
  // Exact "Sign in" — the alt button is "Sign in with passkey" which also matches /sign in/i.
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();

  // TOTP prompt — the 6-digit form auto-submits on fill (see
  // two-factor-prompt.tsx handleChange). Hidden input uses
  // autocomplete="one-time-code" and aria-label="6-digit verification code".
  // No explicit "Verify" click needed.
  const otp = authenticator.generate(TEST_TOTP_SECRET);
  await page
    .locator('input[autocomplete="one-time-code"]')
    .fill(otp, { force: true });

  // After TOTP, this fresh browser session has no local vault yet —
  // app.tsx will route to /onboarding/setup-device (device-provision
  // component). We need to restore the vault from the recovery phrase
  // before /cli-auth can complete the ECDH handshake.
  const recoveryTextarea = page.locator('#recovery-phrase');
  const cliAuthLanded = page.waitForURL(new RegExp(`/cli-auth\\?code=${USER_CODE}`));

  // Race: either we go straight back to /cli-auth (existing vault) or the
  // device-provision page appears with the recovery-phrase textarea.
  await Promise.race([
    recoveryTextarea.waitFor({ state: 'visible', timeout: 30000 }),
    cliAuthLanded,
  ]);

  if (await recoveryTextarea.isVisible().catch(() => false)) {
    if (!TEST_RECOVERY_PHRASE) {
      throw new Error(
        'device-provision page shown but CLI_AUTH_TEST_RECOVERY_PHRASE is unset',
      );
    }
    console.log('  driver: device-provision → entering recovery phrase');
    await recoveryTextarea.fill(TEST_RECOVERY_PHRASE);
    await page.getByRole('button', { name: /restore vault/i }).click();
    // The device-provision flow drops the /cli-auth?code redirect param
    // and sends the user to /. Wait for the vault to finish restoring +
    // for the auth state to settle in sessionStorage/IndexedDB, then
    // navigate back to /cli-auth manually.
    await page.waitForURL(/\/(?!cli-auth)/, { timeout: 60000 });
    // Wait for the drive page to actually finish loading — confirms
    // useKeys hook has unlocked the vault in-memory and persisted state.
    await page.waitForLoadState('networkidle', { timeout: 30000 });
    await page.waitForTimeout(3000);
    console.log('  driver: vault restored — navigating back to /cli-auth');
    await page.goto(`${WEB_BASE_URL}/cli-auth?code=${USER_CODE}`);
  }

  // Now wait for /cli-auth?code=...
  await page.waitForURL(new RegExp(`/cli-auth\\?code=${USER_CODE}`), {
    timeout: 60000,
  });

  // Click the Authorize button on the confirmation page.
  await page.getByRole('button', { name: /authori[sz]e|confirm/i }).click();

  // The page should show a success state. We don't need to assert it
  // verbatim — bb login exiting 0 is the real proof.
  await page.waitForTimeout(2000);
  await page.screenshot({ path: path.join(EVIDENCE_DIR, 'authorized.png') });

  console.log('  driver: authorization confirmed');
} finally {
  await context.tracing.stop({ path: path.join(EVIDENCE_DIR, 'playwright-trace.zip') });
  await browser.close();
}
