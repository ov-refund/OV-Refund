// Local browser investigation tool (developer-only, not shipped).
//
// Launches the locally installed Chrome/Edge with a *dedicated* profile under
// .investigation/ (gitignored) and records Fetch/XHR/Document network traffic
// for official NS / OV-chipkaart domains. The user logs in themselves in the
// visible browser window; this tool never sees typed passwords.
//
// Redaction: request/response header VALUES are never written, only names.
// JSON response bodies from allow-listed hosts are written to
// .investigation/bodies/ for local inspection only (contains personal data,
// never commit). Cookies, Authorization and CSRF values are never persisted.
//
// Usage:
//   node tools/investigate/cdp.mjs launch [url]      start browser + recorder (runs until Ctrl+C)
//   node tools/investigate/cdp.mjs eval "<js>"         evaluate JS in the active NS tab, print result
//   node tools/investigate/cdp.mjs tabs                list open tabs
//   node tools/investigate/cdp.mjs goto <url>          navigate the active tab
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), '..', '..');
const OUT = path.join(ROOT, '.investigation');
const PORT = Number(process.env.CDP_PORT || 9333);
const ALLOW = /(^|\.)(ns\.nl|ov-chipkaart\.nl|translink\.nl|ovpay\.nl)$/i;
const BROWSERS = [
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
];

fs.mkdirSync(path.join(OUT, 'bodies'), { recursive: true });

async function targets() {
  const r = await fetch(`http://127.0.0.1:${PORT}/json`);
  return r.json();
}

function connect(wsUrl) {
  const ws = new WebSocket(wsUrl);
  let id = 0;
  const pending = new Map();
  const listeners = [];
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
    } else for (const l of listeners) l(msg);
  };
  const ready = new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  return {
    ready,
    send(method, params = {}, sessionId) {
      const mid = ++id;
      ws.send(JSON.stringify({ id: mid, method, params, ...(sessionId ? { sessionId } : {}) }));
      return new Promise((resolve, reject) => pending.set(mid, { resolve, reject }));
    },
    on(fn) { listeners.push(fn); },
    close() { ws.close(); },
  };
}

// Never persist bodies of authentication/token endpoints.
const DENY_BODY = /(oauth|token|authoriz|login\.ns\.nl|loginapi|saml|akam|josiad)/i;
// Mask card numbers / long digit runs anywhere in a logged URL or filename.
const maskDigits = (s) => s.replace(/\d{4}(?:[ .-]?\d{4}){2,}|\d{10,}/g, '<NUM>')
  .replace(/\b[A-Z]{2}\d{2}[A-Z]{4}\d{6,10}\b/g, '<IBAN>').replace(/[\w.+-]+@[\w-]+\.[\w.]+/g, '<EMAIL>');
function hostOf(u) { try { return new URL(u).hostname; } catch { return ''; } }
function stripQuery(u) { try { const x = new URL(u); return maskDigits(x.origin + x.pathname) + (x.search ? '?<' + [...x.searchParams.keys()].join(',') + '>' : ''); } catch { return '<bad-url>'; } }

async function launch(url) {
  const exe = BROWSERS.find((b) => fs.existsSync(b));
  if (!exe) throw new Error('No Chrome/Edge found');
  const profile = path.join(OUT, 'browser-profile');
  spawn(exe, [`--user-data-dir=${profile}`, `--remote-debugging-port=${PORT}`, '--no-first-run', '--no-default-browser-check', url], { detached: true, stdio: 'ignore' }).unref();
  for (let i = 0; i < 50; i++) { try { await targets(); break; } catch { await new Promise((r) => setTimeout(r, 200)); } }
  const ver = await (await fetch(`http://127.0.0.1:${PORT}/json/version`)).json();
  const b = connect(ver.webSocketDebuggerUrl);
  await b.ready;
  const log = fs.createWriteStream(path.join(OUT, 'network.jsonl'), { flags: 'a' });
  const reqs = new Map();
  let n = 0;
  b.on(async (msg) => {
    const s = msg.sessionId;
    if (msg.method === 'Target.attachedToTarget') {
      const sid = msg.params.sessionId;
      await b.send('Network.enable', {}, sid).catch(() => {});
      await b.send('Runtime.runIfWaitingForDebugger', {}, sid).catch(() => {});
    }
    if (msg.method === 'Network.requestWillBeSent') {
      const { requestId, request, type } = msg.params;
      reqs.set(requestId, { method: request.method, url: request.url, type, hasPost: !!request.hasPostData,
        postKeys: request.postData ? safeKeys(request.postData) : undefined, reqHeaderNames: Object.keys(request.headers || {}) });
    }
    if (msg.method === 'Network.responseReceived') {
      const { requestId, response, type } = msg.params;
      const r = reqs.get(requestId) || {};
      Object.assign(r, { status: response.status, mime: response.mimeType, type, respHeaderNames: Object.keys(response.headers || {}) });
      reqs.set(requestId, r);
    }
    if (msg.method === 'Network.loadingFinished') {
      const r = reqs.get(msg.params.requestId);
      if (!r) return;
      reqs.delete(msg.params.requestId);
      const host = hostOf(r.url);
      if (!ALLOW.test(host)) return;
      if (!['XHR', 'Fetch', 'Document'].includes(r.type)) return;
      const entry = { t: new Date().toISOString(), ...r, url: stripQuery(r.url), fullPathOnly: undefined };
      if (/json/.test(r.mime || '') && !DENY_BODY.test(r.url)) {
        try {
          const body = await b.send('Network.getResponseBody', { requestId: msg.params.requestId }, s);
          const file = `${String(++n).padStart(4, '0')}-${host}-${maskDigits(new URL(r.url).pathname).replace(/[^a-z0-9]+/gi, '_').slice(0, 80)}.json`;
          fs.writeFileSync(path.join(OUT, 'bodies', file), body.base64Encoded ? Buffer.from(body.body, 'base64') : body.body);
          entry.bodyFile = file;
        } catch { /* body evicted */ }
      }
      log.write(JSON.stringify(entry) + '\n');
      console.log(`${r.status} ${r.method} ${r.type} ${entry.url}${entry.bodyFile ? '  -> ' + entry.bodyFile : ''}`);
    }
  });
  await b.send('Target.setDiscoverTargets', { discover: true });
  await b.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true });
  // attach to already-open pages
  for (const t of await targets()) if (t.type === 'page') await b.send('Target.attachToTarget', { targetId: t.id, flatten: true }).catch(() => {});
  b.on(async (msg) => {
    if (msg.method === 'Target.targetCreated' && msg.params.targetInfo.type === 'page')
      await b.send('Target.attachToTarget', { targetId: msg.params.targetInfo.targetId, flatten: true }).catch(() => {});
  });
  console.log('Recorder running. Log in yourself in the browser window. Ctrl+C to stop.');
}

function safeKeys(postData) {
  try { return Object.keys(JSON.parse(postData)); } catch {}
  try { return [...new URLSearchParams(postData).keys()]; } catch {}
  return ['<unparsed>'];
}

async function activePage() {
  const ts = (await targets()).filter((t) => t.type === 'page');
  return ts.find((t) => ALLOW.test(hostOf(t.url))) || ts[0];
}

async function evalJs(expr) {
  const t = await activePage();
  const c = connect(t.webSocketDebuggerUrl);
  await c.ready;
  const r = await c.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true });
  c.close();
  return r.result?.value ?? r;
}

// Trusted mouse click at the centre of the element matched by a JS expression.
async function trustedClick(elementExpr) {
  const t = await activePage();
  const c = connect(t.webSocketDebuggerUrl);
  await c.ready;
  const r = await c.send('Runtime.evaluate', { expression: `(()=>{const e=${elementExpr}; e.scrollIntoView({block:'center'}); const b=e.getBoundingClientRect(); return {x:b.x+b.width/2,y:b.y+b.height/2}})()`, returnByValue: true });
  const { x, y } = r.result.value;
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased'])
    await c.send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
  c.close();
  return { x, y };
}

const [cmd, ...args] = process.argv.slice(2);
if (cmd === 'click') { console.log(await trustedClick(args.join(' '))); process.exit(0); }
if (cmd === 'launch') await launch(args[0] || 'https://www.ns.nl/mijnns');
else if (cmd === 'tabs') console.log((await targets()).filter((t) => t.type === 'page').map((t) => t.url).join('\n'));
else if (cmd === 'eval') { const v = await evalJs(args.join(' ')); console.log(maskDigits(typeof v === 'string' ? v : JSON.stringify(v, null, 2))); }
else if (cmd === 'goto') { console.log(await evalJs(`location.href=${JSON.stringify(args[0])}; 'ok'`)); }
else console.log('usage: launch [url] | tabs | eval <js> | goto <url>');
