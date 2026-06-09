// Reusable bb device-auth login driver for the vault-needing prod bots.
// Adapted from test-2fa-driver.mjs but TOTP-OPTIONAL (bot-probe-1 has no 2FA).
// Drives a SUPPLIED USER_CODE through the web cli-auth flow so `bb login`
// completes and bb is authenticated for the bot account.
//
// Env: USER_CODE, BOT_PROBE_1_EMAIL, BOT_PROBE_1_PASSWORD,
//      BOT_PROBE_1_RECOVERY_PHRASE, optional BOT_PROBE_1_TOTP_SECRET,
//      API_BASE_URL (default prod), EVIDENCE_DIR.
import { chromium } from 'playwright';
import path from 'node:path';
import { mkdirSync } from 'node:fs';

const USER_CODE = process.env.USER_CODE;
const API_BASE_URL = process.env.API_BASE_URL ?? 'https://api.beebeeb.io';
const WEB_BASE_URL = process.env.WEB_BASE_URL ?? API_BASE_URL.replace('api.', 'app.');
const EVIDENCE_DIR = process.env.EVIDENCE_DIR ?? './vault-bot-evidence';
const EMAIL = process.env.BOT_PROBE_1_EMAIL;
const PASSWORD = process.env.BOT_PROBE_1_PASSWORD;
const RECOVERY = process.env.BOT_PROBE_1_RECOVERY_PHRASE;
const TOTP_SECRET = process.env.BOT_PROBE_1_TOTP_SECRET; // optional

if (!USER_CODE) { console.error('USER_CODE required'); process.exit(2); }
if (!EMAIL || !PASSWORD) { console.error('BOT_PROBE_1_EMAIL/PASSWORD required'); process.exit(2); }
mkdirSync(EVIDENCE_DIR, { recursive: true });

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({ userAgent: 'bb-prodbot/1.0' });
const page = await ctx.newPage();

try {
  console.log(`  driver: GET ${WEB_BASE_URL}/cli-auth?code=${USER_CODE}`);
  await page.goto(`${WEB_BASE_URL}/cli-auth?code=${USER_CODE}`);
  await page.waitForURL(/\/login/, { timeout: 30000 });
  await page.waitForTimeout(700);
  await page.getByRole('button', { name: /essential only/i }).click().catch(() => {});
  await page.getByLabel(/^email/i).fill(EMAIL);
  await page.locator('input[autocomplete="current-password"]').fill(PASSWORD);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();

  // TOTP is optional. Race: TOTP input appears (2FA account) vs recovery
  // textarea (fresh-browser device-provision) vs straight back to /cli-auth.
  const otpInput = page.locator('input[autocomplete="one-time-code"]');
  const recovery = page.locator('#recovery-phrase');
  const cliAuthLanded = page.waitForURL(new RegExp(`/cli-auth\\?code=${USER_CODE}`), { timeout: 60000 });
  await Promise.race([
    otpInput.waitFor({ state: 'visible', timeout: 30000 }).catch(() => {}),
    recovery.waitFor({ state: 'visible', timeout: 30000 }).catch(() => {}),
    cliAuthLanded.catch(() => {}),
  ]);

  if (await otpInput.isVisible().catch(() => false)) {
    if (!TOTP_SECRET) throw new Error('TOTP prompt shown but BOT_PROBE_1_TOTP_SECRET unset');
    const { authenticator } = await import('@otplib/preset-default');
    console.log('  driver: TOTP prompt → entering code');
    await otpInput.fill(authenticator.generate(TOTP_SECRET), { force: true });
    await Promise.race([
      recovery.waitFor({ state: 'visible', timeout: 30000 }).catch(() => {}),
      page.waitForURL(new RegExp(`/cli-auth\\?code=${USER_CODE}`), { timeout: 60000 }).catch(() => {}),
    ]);
  }

  if (await recovery.isVisible().catch(() => false)) {
    if (!RECOVERY) throw new Error('device-provision shown but BOT_PROBE_1_RECOVERY_PHRASE unset');
    console.log('  driver: device-provision → restoring vault from recovery phrase');
    await recovery.fill(RECOVERY);
    await page.getByRole('button', { name: /restore vault/i }).click();
    await page.waitForURL(/\/(?!cli-auth)/, { timeout: 60000 });
    await page.waitForLoadState('networkidle', { timeout: 30000 }).catch(() => {});
    await page.waitForTimeout(3000);
    console.log('  driver: vault restored — navigating back to /cli-auth');
    await page.goto(`${WEB_BASE_URL}/cli-auth?code=${USER_CODE}`);
  }

  await page.waitForURL(new RegExp(`/cli-auth\\?code=${USER_CODE}`), { timeout: 60000 });
  await page.getByRole('button', { name: /authori[sz]e|confirm/i }).click();
  await page.waitForTimeout(2000);
  await page.screenshot({ path: path.join(EVIDENCE_DIR, 'bb-login-authorized.png') }).catch(() => {});
  console.log('  driver: authorization confirmed');
} finally {
  await browser.close();
}
