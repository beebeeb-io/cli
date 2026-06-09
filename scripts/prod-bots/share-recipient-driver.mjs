// Share-recipient cold-open driver (share-bot / revoke-bot). Opens a share URL
// in a FRESH UNAUTHENTICATED browser context — the exact recipient journey —
// and verifies, OR asserts the share is revoked/unavailable.
//
// MODE=verify (default): assert the decrypted filename shows, download, and the
//   downloaded bytes sha256 == EXPECTED_SHA256.
// MODE=revoked: assert the page shows an expired/revoked/not-found state.
//
// Env: SHARE_URL (https://app.beebeeb.io/s/<token>#key=<K_c>), EXPECTED_NAME,
//      EXPECTED_SHA256 (verify mode), MODE, EVIDENCE_DIR.
import { chromium } from 'playwright';
import { createHash } from 'node:crypto';
import { readFileSync, mkdirSync } from 'node:fs';
import path from 'node:path';

const SHARE_URL = process.env.SHARE_URL;
const EXPECTED_NAME = process.env.EXPECTED_NAME ?? '';
const EXPECTED_SHA256 = process.env.EXPECTED_SHA256 ?? '';
const MODE = process.env.MODE ?? 'verify';
const EVIDENCE_DIR = process.env.EVIDENCE_DIR ?? './vault-bot-evidence';
if (!SHARE_URL) { console.error('SHARE_URL required'); process.exit(2); }
mkdirSync(EVIDENCE_DIR, { recursive: true });

const browser = await chromium.launch({ headless: true });
// Fresh, isolated context — no auth state. This is the recipient's perspective.
const ctx = await browser.newContext({ userAgent: 'bb-prodbot/1.0', acceptDownloads: true });
const page = await ctx.newPage();

let pass = false, detail = '';
try {
  await page.goto(SHARE_URL, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await page.waitForTimeout(1500);
  await page.getByRole('button', { name: /essential only/i }).click().catch(() => {});

  if (MODE === 'revoked') {
    const revoked = page.getByText(/this link has expired|no longer accessible|share not found/i).first();
    await revoked.waitFor({ state: 'visible', timeout: 20000 });
    pass = true; detail = `revoked-state shown: "${(await revoked.textContent() || '').trim().slice(0, 60)}"`;
  } else {
    // verify: the "Download and decrypt" button appears only after the share
    // loads + the filename decrypts.
    const dlBtn = page.getByRole('button', { name: /download and decrypt/i });
    await dlBtn.waitFor({ state: 'visible', timeout: 30000 });
    // filename
    const nameEl = page.locator('h2').first();
    const shownName = (await nameEl.textContent().catch(() => '') || '').trim();
    const nameOk = EXPECTED_NAME ? shownName.includes(EXPECTED_NAME) : shownName.length > 0;
    // download + hash
    const dlPromise = page.waitForEvent('download', { timeout: 60000 });
    await dlBtn.click();
    const dl = await dlPromise;
    const outPath = path.join(EVIDENCE_DIR, 'share-download.bin');
    await dl.saveAs(outPath);
    const gotHash = createHash('sha256').update(readFileSync(outPath)).digest('hex');
    const hashOk = EXPECTED_SHA256 ? gotHash === EXPECTED_SHA256 : true;
    pass = nameOk && hashOk;
    detail = `name="${shownName}" nameOk=${nameOk} hashOk=${hashOk} got=${gotHash.slice(0, 12)} exp=${EXPECTED_SHA256.slice(0, 12)}`;
    await page.screenshot({ path: path.join(EVIDENCE_DIR, 'share-recipient.png') }).catch(() => {});
  }
} catch (e) {
  detail = `error: ${String(e).slice(0, 160)}`;
  await page.screenshot({ path: path.join(EVIDENCE_DIR, 'share-recipient-error.png') }).catch(() => {});
} finally {
  await browser.close();
}

console.log(`share-recipient[${MODE}]: pass=${pass} ${detail}`);
process.exit(pass ? 0 : 1);
