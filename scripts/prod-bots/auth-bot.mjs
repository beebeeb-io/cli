// Prod bot — auth-bot (032 Pillar 5 / task 0705, Phase 1).
// Verifies a REAL OPAQUE login round-trip against prod via the web app:
//   POST /api/v1/opaque/login-start 200 -> login-finish 200 (handshake + session)
//   -> authenticated GET /api/v1/auth/me 200.
// This is the true client-crypto (WASM OPAQUE) round-trip — bb CLI has no
// scripted OPAQUE login (its only login is device-auth). Standing 0696 + auth
// regression guard.
//
// Env: BOT_PROBE_1_EMAIL, BOT_PROBE_1_PASSWORD. Optional WEB_BASE_URL, RUNS,
//      BB_BOTS_DISABLED (kill switch). Captures STATUSES ONLY — never tokens.
// Exit 0 = all runs pass; 1 = a run failed; 2 = misconfig; 0 (skip) if disabled.
import { chromium } from 'playwright';

if (process.env.BB_BOTS_DISABLED === '1' || process.env.BB_BOTS_DISABLED === 'true') {
  console.log('auth-bot: BB_BOTS_DISABLED set — skipping.');
  process.exit(0);
}

const EMAIL = process.env.BOT_PROBE_1_EMAIL;
const PASSWORD = process.env.BOT_PROBE_1_PASSWORD;
const WEB = process.env.WEB_BASE_URL ?? 'https://app.beebeeb.io';
const RUNS = parseInt(process.env.RUNS ?? '2', 10); // ≥2 for LB spread
if (!EMAIL || !PASSWORD) {
  console.error('auth-bot: BOT_PROBE_1_EMAIL/PASSWORD not set');
  process.exit(2);
}

function authUrl(u) {
  return /\/api\/v1\/(opaque\/login|auth\/me)/.test(u) || /\/me(\?|$)/.test(u);
}

async function oneRun(i) {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    userAgent: 'bb-prodbot/1.0',
  });
  const page = await ctx.newPage();
  const events = [];
  page.on('response', (res) => {
    const u = res.url();
    if (authUrl(u)) {
      const path = u.replace(WEB, '').replace('https://api.beebeeb.io', '').split('?')[0];
      events.push({ path, status: res.status() }); // statuses only; no bodies/tokens
    }
  });

  const out = { run: i };
  try {
    await page.goto(`${WEB}/login`, { waitUntil: 'networkidle', timeout: 60000 });
    await page.waitForTimeout(700);
    await page.getByRole('button', { name: /essential only/i }).click().catch(() => {});
    await page.waitForTimeout(300);
    await page.getByLabel(/^email/i).fill(EMAIL);
    await page.locator('input[autocomplete="current-password"]').fill(PASSWORD);
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await page.waitForResponse((r) => /auth\/me|\/me(\?|$)/.test(r.url()) && r.request().method() === 'GET', { timeout: 45000 }).catch(() => {});
    await page.waitForTimeout(2500);
  } catch (e) {
    out.error = String(e);
  } finally {
    await browser.close();
  }

  const startOk = events.some((e) => /login-start/.test(e.path) && e.status >= 200 && e.status < 300);
  const finishIdx = events.findIndex((e) => /login-finish/.test(e.path) && e.status >= 200 && e.status < 300);
  const me200 = finishIdx >= 0 && events.slice(finishIdx + 1).some((e) => /me/.test(e.path) && e.status === 200);
  out.events = events;
  out.pass = startOk && finishIdx >= 0 && me200;
  return out;
}

const results = [];
for (let i = 1; i <= RUNS; i++) {
  const r = await oneRun(i);
  console.log(`auth-bot run ${i}: pass=${r.pass} events=${JSON.stringify(r.events)}${r.error ? ' error=' + r.error : ''}`);
  results.push(r);
}
const allPass = results.every((r) => r.pass);
console.log(`auth-bot: ALL_PASS=${allPass}`);
process.exit(allPass ? 0 : 1);
