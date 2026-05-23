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

  await page.getByLabel(/email/i).fill(TEST_EMAIL);
  await page.getByLabel(/password/i).fill(TEST_PASSWORD);
  await page.getByRole('button', { name: /sign in|log in/i }).click();

  // TOTP prompt.
  const otp = authenticator.generate(TEST_TOTP_SECRET);
  await page.getByLabel(/code|two-factor|2fa|totp/i).fill(otp);
  await page.getByRole('button', { name: /verify|continue/i }).click();

  // Back to /cli-auth?code=...
  await page.waitForURL(new RegExp(`/cli-auth\\?code=${USER_CODE}`));

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
