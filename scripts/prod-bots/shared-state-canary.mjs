// Prod bot — shared-state sanity canary (032 Pillar 5 / task 0705, Phase 1).
// Catches the 0537B class: the api silently losing cross-node shared state
// (Redis) and falling back to per-node in-memory. Two black-box checks, no creds:
//
//  (1) RATE-LIMIT: N rapid GET /health; X-RateLimit-Remaining must decrement
//      ~1:1 (shared store). A ~1:2 rate (each value twice) = per-node counters
//      round-robined across nodes = lost shared store.
//  (2) CLI-AUTH: open the device-auth WS (pubkey lands in one node's
//      LOCAL_SESSIONS), then GET /cli-pubkey?code= N× through the LB; at least
//      one 200 is required. 0% found = WS-node and GET-node never coincide =
//      lost shared store (the exact 0537B outage signature).
//
// Env: optional API_BASE_URL, BB_BOTS_DISABLED. Exit 0 = healthy, 1 = drift, 2 = error.
const API = process.env.API_BASE_URL ?? 'https://api.beebeeb.io';
const WS = API.replace('https://', 'wss://').replace('http://', 'ws://') + '/api/v1/auth/cli';

if (process.env.BB_BOTS_DISABLED === '1' || process.env.BB_BOTS_DISABLED === 'true') {
  console.log('shared-state-canary: BB_BOTS_DISABLED set — skipping.');
  process.exit(0);
}

async function rateLimitCheck() {
  const N = 20;
  const rem = [];
  for (let i = 0; i < N; i++) {
    const r = await fetch(`${API}/health`, { headers: { 'User-Agent': 'bb-prodbot/1.0' } });
    const v = r.headers.get('x-ratelimit-remaining');
    if (v != null) rem.push(parseInt(v, 10));
    await new Promise((s) => setTimeout(s, 120));
  }
  if (rem.length < 5) return { ok: null, note: 'no rate-limit headers', rem };
  const drop = rem[0] - rem[rem.length - 1];
  const reqs = rem.length - 1;
  // per-node(N): drop ≈ reqs/N (ratio ~1/N). shared OR sticky-single-node: ratio ~1.
  // This check can only PROVE per-node (ratio well below 1) — a ratio ~1 is
  // INCONCLUSIVE because a source-IP-sticky LB routes one client to one node,
  // making per-node counters look shared. So: ratio<0.7 => drift (false);
  // otherwise null (inconclusive). The cli-auth probe is the robust positive signal.
  const ratio = reqs > 0 ? drop / reqs : 1;
  const ok = ratio < 0.7 ? false : null;
  return { ok, ratio: ratio.toFixed(2), drop, reqs, firstLast: [rem[0], rem[rem.length - 1]], note: ok === null ? 'inconclusive (sticky-LB can mask per-node)' : 'per-node split detected' };
}

async function cliAuthCheck() {
  const ws = new WebSocket(WS);
  let code = null;
  await new Promise((res) => {
    ws.addEventListener('open', () => ws.send(JSON.stringify({ ecdh_public_key_b64: 'CANARY' + 'A'.repeat(80) })));
    ws.addEventListener('message', (ev) => { try { const m = JSON.parse(ev.data); if (m.user_code) { code = m.user_code; res(); } } catch {} });
    ws.addEventListener('error', () => res());
    setTimeout(res, 15000);
  });
  if (!code) { try { ws.close(); } catch {} return { ok: null, note: 'no user_code from WS' }; }
  let found = 0;
  const N = 12;
  for (let i = 0; i < N; i++) {
    const r = await fetch(`${API}/api/v1/auth/cli-pubkey?code=${code}`, { headers: { 'User-Agent': 'bb-prodbot/1.0' } });
    if (r.status === 200) found++;
    await new Promise((s) => setTimeout(s, 120));
  }
  try { ws.close(); } catch {}
  // healthy: at least one GET hits the WS's node. 0/N = lost shared store.
  return { ok: found > 0, found, of: N };
}

try {
  const rl = await rateLimitCheck();
  const ca = await cliAuthCheck();
  console.log(`shared-state-canary: rate-limit=${JSON.stringify(rl)}`);
  console.log(`shared-state-canary: cli-auth=${JSON.stringify(ca)}`);
  // Fail only on a definitive drift signal (ok===false). null = inconclusive (don't page).
  const drift = rl.ok === false || ca.ok === false;
  console.log(`shared-state-canary: DRIFT=${drift} (rate-limit ok=${rl.ok}, cli-auth ok=${ca.ok})`);
  process.exit(drift ? 1 : 0);
} catch (e) {
  console.error('shared-state-canary error:', String(e));
  process.exit(2);
}
