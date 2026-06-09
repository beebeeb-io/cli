// Prod bot — wasm-bot (032 Pillar 5 / 0705 Phase 1).
// Catches the white-screen / CSP / WASM-load failure class: loads
// app.beebeeb.io/login and asserts WasmGuard reaches ready (the login form
// renders — which requires the crypto engine) within a budget, with no
// uncaught page errors. No creds.
//
// WasmGuard (web/src/components/wasm-guard.tsx) shows "Initializing encryption
// engine..." while loading, an error page on failure, and renders children
// (the login form) only when cryptoReady. So time-to-login-form == time-to-WASM-ready.
//
// Env: optional WEB_BASE_URL, WASM_BUDGET_MS (default 10000), BB_BOTS_DISABLED.
// Exit 0 = healthy, 1 = WASM not ready in budget / page error, 2 = error.
import { chromium } from 'playwright';

if (process.env.BB_BOTS_DISABLED === '1' || process.env.BB_BOTS_DISABLED === 'true') {
  console.log('wasm-bot: BB_BOTS_DISABLED set — skipping.');
  process.exit(0);
}

const WEB = process.env.WEB_BASE_URL ?? 'https://app.beebeeb.io';
const BUDGET = parseInt(process.env.WASM_BUDGET_MS ?? '10000', 10);

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, userAgent: 'bb-prodbot/1.0' });
const page = await ctx.newPage();
const pageErrors = [];
const consoleErrors = [];
page.on('pageerror', (e) => pageErrors.push(String(e).slice(0, 200)));
page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 160)); });

let pass = false, elapsedMs = null, note = '';
try {
  await page.goto(`${WEB}/login`, { waitUntil: 'domcontentloaded', timeout: 30000 });
  const t0 = await page.evaluate(() => performance.now());
  // Wait for the login form (renders only after WasmGuard -> cryptoReady).
  await page.getByRole('button', { name: 'Sign in', exact: true }).waitFor({ state: 'visible', timeout: BUDGET });
  await page.getByLabel(/^email/i).waitFor({ state: 'visible', timeout: 2000 });
  const t1 = await page.evaluate(() => performance.now());
  elapsedMs = Math.round(t1 - t0);
  // Confirm the loading state is gone.
  const stillLoading = await page.getByText(/Initializing encryption engine/i).isVisible().catch(() => false);
  pass = !stillLoading && pageErrors.length === 0;
  if (stillLoading) note = 'WasmGuard still showing "Initializing encryption engine"';
} catch (e) {
  // Form never rendered in budget -> white-screen / WASM stuck / error page / CSP break.
  note = `login form not ready within ${BUDGET}ms: ${String(e).slice(0, 120)}`;
  const errVisible = await page.getByText(/encryption engine|could not|failed/i).first().isVisible().catch(() => false);
  if (errVisible) note += ' (WasmGuard error/loading state visible)';
} finally {
  await browser.close();
}

console.log(`wasm-bot: pass=${pass} wasm_ready_ms=${elapsedMs ?? 'n/a'} budget=${BUDGET} pageErrors=${pageErrors.length} consoleErrors=${consoleErrors.length}${note ? ' note="' + note + '"' : ''}`);
if (pageErrors.length) console.log('  pageErrors:', JSON.stringify(pageErrors));
if (consoleErrors.length) console.log('  consoleErrors:', JSON.stringify(consoleErrors.slice(0, 5)));
process.exit(pass ? 0 : 1);
