// Capture Mijn NS transaction responses while selecting a period in the official UI.
// Body saved locally to .investigation/bodies/history-*.json (gitignored, personal data).
// Usage: node tools/investigate/capture-history.mjs [periodOptionIndex=3]   (3 = "Afgelopen jaar")
import fs from 'node:fs';
import path from 'node:path';

const PORT = 9333;
const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), '..', '..');
const OUT = path.join(ROOT, '.investigation', 'bodies');
const idx = Number(process.argv[2] ?? 3);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const page = (await (await fetch(`http://127.0.0.1:${PORT}/json`)).json()).find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0; const pend = new Map(); const reqs = new Map(); const done = [];
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id) return pend.get(m.id)?.(m.result ?? { error: m.error });
  if (m.method === 'Network.responseReceived' && /mijnns-transaction-api\/couplings\/.+\/transactions/.test(m.params.response.url)) reqs.set(m.params.requestId, m.params.response.url);
  if (m.method === 'Network.loadingFinished' && reqs.has(m.params.requestId)) done.push(m.params.requestId);
};
const send = (method, params = {}) => new Promise((r) => { pend.set(++id, r); ws.send(JSON.stringify({ id, method, params })); });
const ev = async (expr) => (await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true })).result?.value;

await send('Network.enable');
await ev(`location.hash = '#/reishistorie'`);
await sleep(5000);
done.length = 0;
await ev(`(() => { const s = document.querySelector('#filter-period-control'); s.selectedIndex = ${idx}; s.dispatchEvent(new Event('change', {bubbles: true})); return s.options[${idx}].text })()`);
await sleep(800);
{ const r = await ev(`(() => { const b = [...document.querySelectorAll('mns-transaction-filter nes-button')].find(b => /Tonen/.test(b.innerText)); b.scrollIntoView({block:'center'}); const x = b.getBoundingClientRect(); return {x: x.x + x.width/2, y: x.y + x.height/2} })()`); for (const type of ['mouseMoved','mousePressed','mouseReleased']) await send('Input.dispatchMouseEvent', { type, x: r.x, y: r.y, button: 'left', clickCount: 1 }); }
for (let i = 0; i < 30 && done.length === 0; i++) await sleep(500);
await sleep(2000);
let n = 0;
for (const rid of done) {
  const b = await send("Network.getResponseBody", { requestId: rid }); if (!b || b.error || !b.body) { console.log("skip (no body)", b?.error?.message ?? ""); continue; }
  const body = b.base64Encoded ? Buffer.from(b.body, 'base64').toString() : b.body;
  const coupling = (reqs.get(rid).match(/couplings\/([0-9a-f-]{36})/) || [])[1];
  const f = path.join(OUT, `history-${Date.now()}-${n++}.json`);
  fs.writeFileSync(f, JSON.stringify({ coupling, body }));
  const j = JSON.parse(body);
  console.log(`captured ${j[0]?.transactions?.transactions?.length ?? '?'} transactions (of ${j[0]?.transactions?.numberOfTransactions ?? '?'}) -> ${path.basename(f)}`);
}
if (!done.length) console.log('no transactions response captured; page text:', (await ev('document.querySelector("main")?.innerText.slice(0,300)')) ?? '');
ws.close();
