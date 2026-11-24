// Session-restore experiment. Cookie VALUES stay in this process's memory only;
// nothing is printed or written except cookie names and booleans.
// 1) read all ns.nl cookies from the running investigation browser
// 2) close the browser (session cookies die)
// 3) relaunch with the same profile, check login state WITHOUT restore
// 4) inject the saved cookies (session ones included), reload, check login state
import { spawn } from 'node:child_process';
import path from 'node:path';

const PORT = 9333;
const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), '..', '..');
const PROFILE = path.join(ROOT, '.investigation', 'browser-profile');
const CHROME = 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function cdp(wsUrl) {
  const ws = new WebSocket(wsUrl);
  await new Promise((r) => (ws.onopen = r));
  let id = 0; const pend = new Map();
  ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id) pend.get(m.id)?.(m.result ?? { error: m.error }); };
  return { send: (method, params = {}) => new Promise((r) => { pend.set(++id, r); ws.send(JSON.stringify({ id, method, params })); }), close: () => ws.close() };
}
const page = async () => (await (await fetch(`http://127.0.0.1:${PORT}/json`)).json()).find((t) => t.type === 'page');
const browser = async () => (await (await fetch(`http://127.0.0.1:${PORT}/json/version`)).json()).webSocketDebuggerUrl;

async function loginState(c) {
  await c.send('Page.navigate', { url: 'https://www.ns.nl/mijnns#/reishistorie' });
  for (let i = 0; i < 20; i++) {
    await sleep(1000);
    const r = await c.send('Runtime.evaluate', { expression: '({u: location.host + location.pathname, ok: /Uitloggen/.test(document.body?.innerText||"")})', returnByValue: true });
    const v = r.result?.value;
    if (v?.ok || /login\.ns\.nl/.test(v?.u || '')) return v;
  }
  return { u: 'timeout', ok: false };
}

let c = await cdp((await page()).webSocketDebuggerUrl);
const { cookies } = await c.send('Network.getAllCookies');
const saved = cookies.filter((k) => /ns\.nl$/.test(k.domain));
console.log(`saved ${saved.length} ns.nl cookies (${saved.filter((k) => k.session).length} session cookies) in memory`);
c.close();

const b = await cdp(await browser());
await b.send('Browser.close');
await sleep(3000);

spawn(CHROME, [`--user-data-dir=${PROFILE}`, `--remote-debugging-port=${PORT}`, '--no-first-run', 'about:blank'], { detached: true, stdio: 'ignore' }).unref();
for (let i = 0; i < 40; i++) { try { await page(); break; } catch { await sleep(250); } }
c = await cdp((await page()).webSocketDebuggerUrl);
console.log('after restart, no restore:', await loginState(c));

await c.send('Network.setCookies', {
  cookies: saved.map((k) => ({ name: k.name, value: k.value, domain: k.domain, path: k.path, secure: k.secure, httpOnly: k.httpOnly,
    sameSite: k.sameSite, ...(k.session ? {} : { expires: k.expires }) })),
});
console.log('after restoring saved cookies:', await loginState(c));
c.close();
