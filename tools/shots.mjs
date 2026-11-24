// Capture the real demo UI through loopback CDP. See shots.md (Node 22+).
import fs from 'node:fs';
import path from 'node:path';

const PORT = Number(process.env.PORT || 9336);
const LANG = process.env.LANG_CODE || 'nl';
const THEME = process.env.THEME || 'light';
const HOME_ONLY = process.argv.includes('--home-only');
if (!Number.isInteger(PORT) || PORT < 1 || PORT > 65535) throw new Error('PORT must be 1..65535');
if (!['nl', 'en'].includes(LANG)) throw new Error('LANG_CODE must be nl or en');
if (!['light', 'dark'].includes(THEME)) throw new Error('THEME must be light or dark');
const OUT = path.resolve(process.env.OUTDIR || `site/assets/shots/${LANG}${THEME === 'dark' ? '-dark' : ''}`);
const T = (nl, en) => LANG === 'nl' ? nl : en;
const PERSON = LANG === 'nl'
  ? { name: 'Jan de Vries', email: 'jan.devries@example.com' }
  : { name: 'John Doe', email: 'john.doe@example.com' };
const VIEW = { x: 0, y: 0, width: 1280, height: 820 };
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const q = JSON.stringify;
let ws;
let sequence = 0;
const pending = new Map();

function rejectPending(message) {
  for (const request of pending.values()) { clearTimeout(request.timer); request.reject(new Error(message)); }
  pending.clear();
}
function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timed out: ${method}`)); }, 15000);
    pending.set(id, { resolve, reject, timer });
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function ev(expression) {
  const result = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
  return result.result?.value;
}
async function until(expression, description, timeout = 15000, interval = 60) {
  const deadline = Date.now() + timeout;
  do {
    if (await ev(expression)) return;
    await pause(interval);
  } while (Date.now() < deadline);
  throw new Error(`Timed out waiting for ${description}`);
}
const visible = (sel) => `(() => { const e = document.querySelector(${q(sel)}); return !!e && e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden'; })()`;
const wait = (sel) => until(visible(sel), `selector ${sel}`);
async function click(sel) {
  await wait(sel);
  await until(`!document.querySelector(${q(sel)})?.matches(':disabled,[aria-disabled="true"]')`, `enabled selector ${sel}`);
  await ev(`document.querySelector(${q(sel)}).click()`);
}
async function value(sel, value) {
  await wait(sel);
  await ev(`(() => { const e = document.querySelector(${q(sel)}); e.value = ${q(value)}; e.dispatchEvent(new Event('input', { bubbles: true })); e.dispatchEvent(new Event('change', { bubbles: true })); })()`);
}
async function tick(sel) {
  await wait(sel);
  if (!await ev(`document.querySelector(${q(sel)}).checked`)) await click(sel);
}
async function settle() {
  await ev(`(async () => {
    await document.fonts.ready;
    await Promise.all([...document.images].map(i => i.decode().catch(() => {})));
    await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
  })()`);
  await until(`!document.getAnimations().some(a => a.effect?.getTiming().iterations !== Infinity && a.playState === 'running')`, 'finite UI animations');
}
async function rect(sel, pad = 24) {
  await wait(sel);
  return ev(`(() => {
    const r = document.querySelector(${q(sel)}).getBoundingClientRect();
    if (r.top < 0 || r.left < 0 || r.bottom > innerHeight || r.right > innerWidth)
      throw new Error('Capture target outside viewport: ' + ${q(sel)});
    const x = Math.max(0, r.left - ${pad}), y = Math.max(0, r.top - ${pad});
    return { x, y, width: Math.min(innerWidth, r.right + ${pad}) - x, height: Math.min(innerHeight, r.bottom + ${pad}) - y };
  })()`);
}
const clear = () => ev(`document.querySelectorAll('.shot-ring').forEach(e => e.classList.remove('shot-ring')); window.__shotHint = null;`);
async function annotate(sel, text, side = 'l') {
  await wait(sel);
  await clear();
  await ev(`document.querySelector(${q(sel)}).classList.add('shot-ring'); window.__shotHint = ${q({ sel, text, side })};`);
}

// site.js uses HINTS[lang][name], shared by light/dark. Merge siblings into
// their common parent (shots/hints.js), never into nl-dark/hints.js, etc.
function mergeHints(name, hint) {
  const file = path.join(path.dirname(OUT), 'hints.js');
  let all = {};
  let newline = '\r\n';
  if (fs.existsSync(file)) {
    const source = fs.readFileSync(file, 'utf8');
    newline = source.includes('\r\n') ? '\r\n' : '\n';
    const match = source.match(/export\s+default\s+(\{[\s\S]*\})\s*;?\s*$/);
    if (!match) throw new Error(`Cannot parse existing JSON export in ${file}`);
    all = JSON.parse(match[1]);
  }
  all[LANG] ??= {};
  if (hint) all[LANG][name] = hint;
  else delete all[LANG][name];
  const source = '// Generated by tools/shots.mjs; percentage positions, shared by both themes.\n'
    + `export default ${JSON.stringify(all, null, 1)};\n`;
  fs.writeFileSync(file, source.replace(/\n/g, newline));
}
async function scrub() {
  await ev(String.raw`(() => {
    // Replace every displayed mailbox, including demo login and help text.
    const replace = s => s.replace(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi, ${q(PERSON.email)});
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let n; (n = walker.nextNode());) {
      if (!n.parentElement.closest('script,style')) n.nodeValue = replace(n.nodeValue);
    }
    document.querySelectorAll('input:not([type=password])').forEach(e => { e.value = replace(e.value); });
    document.querySelectorAll('a[href^="mailto:"]').forEach(e => { e.href = 'mailto:' + ${q(PERSON.email)}; });
  })()`);
}
async function shot(name, clip, progress = false) {
  if (HOME_ONLY && name !== '00-home') return true;
  if (!progress) { await settle(); await scrub(); }
  const hint = await ev(`(() => {
    const h = window.__shotHint; if (!h) return null;
    const r = document.querySelector(h.sel).getBoundingClientRect();
    return { ax: h.side === 'l' ? r.left - 7 : h.side === 'r' ? r.right + 7 : r.left + r.width / 2,
      ay: h.side === 'u' ? r.top - 7 : r.top + r.height / 2, side: h.side, text: h.text };
  })()`);
  if (progress && !await ev(progressReady)) return false;
  const result = await send('Page.captureScreenshot', {
    format: 'webp', quality: 88, clip: { ...clip, scale: 1.5 }, captureBeyondViewport: false,
  });
  // Discard a frame if the fast demo finished during screenshot capture.
  if (progress && !await ev(progressReady)) return false;
  const file = path.join(OUT, `${name}.webp`);
  fs.writeFileSync(file, Buffer.from(result.data, 'base64'));
  mergeHints(name, hint && {
    x: +((hint.ax - clip.x) / clip.width * 100).toFixed(2),
    y: +((hint.ay - clip.y) / clip.height * 100).toFixed(2), side: hint.side, text: hint.text,
  });
  console.log(`saved ${file}`);
  return true;
}
async function wizardShot(name) {
  await settle();
  await ev(`document.querySelector('.wz-panel').scrollTop = 0`);
  await shot(name, await rect('.wz-panel', 0));
}
// Any visible mid-check state counts (the demo re-check is fast and may skip the month stage).
const progressReady = `!!document.querySelector('.top .scan-btn.busy #progFill') && (document.querySelector('.top #progTxt')?.textContent || '').trim().length > 0`;

async function run() {
  let pages;
  const deadline = Date.now() + 15000;
  do {
    try {
      const response = await fetch(`http://127.0.0.1:${PORT}/json`, { signal: AbortSignal.timeout(1500) });
      if (response.ok) pages = await response.json();
    } catch { /* WebView2 may still be starting. */ }
    if (pages?.some(t => t.type === 'page' && /^https?:\/\/(tauri\.localhost|localhost)([:/]|$)/.test(t.url))) break;
    await pause(100);
  } while (Date.now() < deadline);
  if (!pages) throw new Error(`Cannot discover CDP on loopback PORT=${PORT}; start the demo with --remote-debugging-port=${PORT}`);
  const targets = pages.filter(t => /^https?:\/\/(tauri\.localhost|localhost)([:/]|$)/.test(t.url) && t.type === 'page');
  if (targets.length !== 1) throw new Error(`Expected one app page on PORT=${PORT}; found ${targets.length}. Start only the demo instance on this port.`);
  const target = targets[0];
  const endpoint = new URL(target.webSocketDebuggerUrl);
  if (!['127.0.0.1', 'localhost', '[::1]'].includes(endpoint.hostname)) throw new Error('CDP WebSocket must use loopback');
  ws = new WebSocket(endpoint);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.close(); reject(new Error('CDP WebSocket connection timed out')); }, 5000);
    ws.onopen = () => { clearTimeout(timer); resolve(); };
    ws.onerror = () => { clearTimeout(timer); reject(new Error('Cannot connect to CDP WebSocket')); };
  });
  ws.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id); clearTimeout(request.timer);
    if (message.error) request.reject(new Error(`CDP: ${message.error.message}`));
    else request.resolve(message.result);
  };
  ws.onclose = () => rejectPending('CDP connection closed');
  ws.onerror = () => rejectPending('CDP connection failed');
  await send('Page.enable');
  await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
  await send('Emulation.setDeviceMetricsOverride', { width: VIEW.width, height: 1000, deviceScaleFactor: 1, mobile: false });
  await wait('.demo-banner'); // Wait for the app document before accessing its origin's storage.
  await ev(`localStorage.setItem('lang', ${q(LANG)}); localStorage.setItem('theme', ${q(THEME)});`);
  await send('Page.reload');
  await wait('.demo-banner'); // Guard before any login/check/save action.
  await wait('#wzAccept');
  await until(`document.documentElement.dataset.theme === ${q(THEME)} && document.querySelector('#wzTitle')?.textContent === ${q(T('Welkom bij OV Refund', 'Welcome to OV Refund'))}`, 'requested language/theme / selector #wzTitle');
  fs.mkdirSync(OUT, { recursive: true });
  await ev(`(() => {
    const s = document.createElement('style'); s.id = 'shot-style';
    s.textContent = '.shot-ring{outline:3px solid #ffc917!important;outline-offset:3px;border-radius:8px} .wz-panel{max-height:none!important} .wizard{overflow:auto!important;background:var(--page)!important;backdrop-filter:none!important;-webkit-backdrop-filter:none!important} body:has(.wizard) #app{visibility:hidden!important} .toast{visibility:hidden!important} .wz-panel,dialog[open]{box-shadow:none!important;border-radius:0!important}';
    document.head.appendChild(s);
  })()`);
  await tick('#wzAccept');
  await annotate('#wzAccept', T('Vink aan', 'Tick this'), 'l');
  await wizardShot('01-welcome'); await clear(); await click('[data-wz=accept]');

  await annotate('[data-wz=login]', T('Klik hier', 'Click here'), 'r');
  await wizardShot('02-login'); await clear(); await click('[data-wz=login]');
  await wait('#svcNo');
  await wizardShot('03-services');
  await tick('#svcNo'); await click('[data-wz=saveservices]');

  await value('#wzBankName', PERSON.name); await value('#wzIban', 'NL91ABNA0417164300');
  await until(`!document.querySelector('[data-wz=savebank]').disabled`, 'enabled selector [data-wz=savebank]');
  await annotate('#wzIban', T('Je IBAN', 'Your IBAN'), 'u');
  await wizardShot('04-bank'); await clear(); await click('[data-wz=savebank]');

  await tick('#wzAuto'); await wizardShot('05-auto'); await click('[data-wz=saveauto]');
  await tick('#alYes'); await value('#wzNsEmail', PERSON.email); await value('#wzNsPw', 'Example-password-123');
  await wizardShot('06-autologin'); await click('[data-wz=saveautologin]');

  await wait('#wzMailBox');
  await ev(`document.querySelector('#wzMailBox').open = true`);
  await value('#wzMail', PERSON.email);
  await wizardShot('07-notify');
  // example.com has no supported SMTP preset. Close the optional section;
  // never test/send mail, and do not bypass the app's validation.
  await ev(`document.querySelector('#wzMailBox').open = false`);
  // Captures are quiet: never raise Windows notification toasts while preparing example data.
  await ev(`document.querySelector('#wzWin').checked = false`);
  await click('[data-wz=savenotify]');
  // Let the final step's saved resume marker settle before recording wizard_done.
  await settle(); await pause(250); await click('[data-wz=finish]');
  await until(`!document.querySelector('.wizard')`, 'closed setup wizard');
  if (!await ev(`!!document.querySelector('tr.row')`)) {
    await until(`!document.querySelector('.scan-btn.busy')`, 'idle first check', 60000);
    await click('.top [data-act=scan]');
  }
  await until(`!!document.querySelector('tr.row') && !document.querySelector('.scan-btn.busy')`, 'completed first demo check / selector tr.row', 60000);
  await send('Emulation.setDeviceMetricsOverride', { width: VIEW.width, height: VIEW.height, deviceScaleFactor: 1, mobile: false });
  await click('[data-period=all]');
  await wait('[data-period=all][aria-pressed=true]');
  await settle();
  await click('[data-cat=all]');
  await wait('[data-cat=all][aria-pressed=true]');
  await ev('scrollTo(0, 0)');
  if (HOME_ONLY) {
    // A compact illustrative overview made from real demo rows and totals. Keep one claim
    // expanded, with a few surrounding journeys, rather than shrinking the whole dashboard.
    await clear();
    const selected = await ev(`(() => {
      const row = [...document.querySelectorAll('tr.row.is-claim')].find(r =>
        r.querySelector('.route')?.textContent.includes('Leiden Centraal') &&
        r.querySelector('.route')?.textContent.includes('Utrecht Centraal'));
      if (!row) throw new Error('Expected the supported Leiden-Utrecht demo claim');
      row.querySelector('.chev-btn').click(); return row.dataset.row;
    })()`);
    await wait(`tr[data-detail="${selected}"] .big-amt`);
    await ev(`(() => {
      const row = document.querySelector('tr[data-row="${selected}"]');
      const detail = document.querySelector('tr[data-detail="${selected}"]');
      const frame = document.createElement('section'); frame.id = 'homeCapture';
      const head = document.createElement('header'); head.className = 'capture-head';
      const logo = document.querySelector('.brand .logo-img').cloneNode(true);
      head.append(logo);
      const brand = document.createElement('strong'); brand.textContent = 'OV Refund'; head.append(brand);
      const example = document.createElement('span'); example.textContent = ${q(T('Voorbeeld', 'Example'))}; head.append(example);
      const check = document.querySelector('.top [data-act=scan]').cloneNode(true); check.className = 'btn primary capture-check'; head.append(check);
      const body = document.createElement('div'); body.className = 'capture-body';
      const totals = document.querySelector('.summary').cloneNode(true); totals.className = 'capture-totals';
      totals.querySelectorAll('.qhelp,.qpop,.s').forEach(e => e.remove());
      const tabs = document.createElement('div'); tabs.className = 'capture-tabs';
      for (const [text, active] of [[${q(T('Alle reizen', 'All trips'))}, true], [${q(T('Te claimen', 'Claimable'))}, false], [${q(T('Geclaimd', 'Claimed'))}, false]]) {
        const tab = document.createElement('span'); tab.textContent = text; if (active) tab.className = 'selected'; tabs.append(tab);
      }
      const table = document.createElement('table'); table.className = 'capture-table';
      table.innerHTML = '<colgroup><col style="width:90px"><col><col style="width:78px"><col style="width:90px"><col style="width:112px"></colgroup><thead><tr><th>' + ${q(T('Datum', 'Date'))} + '</th><th>' + ${q(T('Reis', 'Trip'))} + '</th><th class="num">' + ${q(T('Vertraging', 'Delay'))} + '</th><th class="num">' + ${q(T('Vergoeding', 'Refund'))} + '</th><th>Status</th></tr></thead>';
      const tbody = document.createElement('tbody'); table.append(tbody);
      const compact = (source, selected = false) => {
        const tr = source.cloneNode(true); tr.className = selected ? 'capture-selected' : 'capture-trip';
        tr.querySelector('.chev')?.remove();
        tr.querySelectorAll('.date .msg').forEach(e => e.remove());
        tr.querySelector('.date b').textContent = tr.querySelector('.date b').textContent.replace(/\\s+\\d{4}$/, '');
        if (tr.querySelector('.row-act')) { tr.querySelector('.status').textContent = ${q(T('Te claimen', 'Claimable'))}; tr.querySelector('.status').classList.add('capture-ready'); }
        return tr;
      };
      const others = [...document.querySelectorAll('tr.row')].filter(r => r !== row);
      const ontime = others.find(r => !r.classList.contains('is-claim') && /${T('Op tijd', 'On time')}/.test(r.querySelector('.status').textContent));
      const otherClaim = others.find(r => r.classList.contains('is-claim'));
      const paid = others.find(r => /${T('Uitbetaald', 'Paid')}/.test(r.querySelector('.status').textContent));
      if (!ontime || !otherClaim || !paid) throw new Error('Missing overview demo journeys');
      tbody.append(compact(ontime), compact(otherClaim), compact(row, true));
      const expanded = document.createElement('tr'); expanded.className = 'capture-expanded';
      const cell = document.createElement('td'); cell.colSpan = 5; expanded.append(cell);
      const claim = document.createElement('div'); claim.className = 'capture-claim';
      const explanation = document.createElement('div'); explanation.className = 'capture-explanation';
      const amount = detail.querySelector('.big-amt').cloneNode(true);
      const sentence = detail.querySelector('.sentence').cloneNode(true);
      sentence.querySelectorAll('.qhelp,.qpop').forEach(e => e.remove());
      const button = row.querySelector('.row-act').cloneNode(true); button.className = 'btn primary capture-action';
      explanation.append(amount, sentence); claim.append(explanation, button); cell.append(claim);
      tbody.append(expanded, compact(paid));
      body.append(totals, tabs, table); frame.append(head, body); document.body.append(frame);
      const style = document.createElement('style'); style.id = 'home-capture-style';
      style.textContent = \`
        html,body { width:800px!important; height:630px!important; min-width:0!important; overflow:hidden!important; margin:0!important; padding:0!important; }
        body > :not(#homeCapture):not(style):not(script) { display:none!important; }
        #homeCapture { width:800px; height:630px; background:var(--page); color:var(--ink); border:1px solid var(--rule); border-radius:16px; overflow:hidden; box-sizing:border-box; }
        .capture-head { height:58px; display:flex; align-items:center; gap:10px; padding:0 20px; background:var(--panel); border-bottom:1px solid var(--rule); }
        .capture-head img { width:28px; height:28px; border-radius:7px; }
        .capture-head strong { font-size:17px; font-weight:750; }
        .capture-head > span { margin-left:auto; font-size:12px; color:var(--ink-2); }
        .capture-check { min-height:32px; padding:0 14px; font-size:12px; margin-left:6px; }
        .capture-body { padding:16px 18px; }
        .capture-totals { display:grid; grid-template-columns:1fr 1fr; gap:12px; }
        .capture-totals .tile { padding:12px 16px; border-radius:12px; box-shadow:none; border:1px solid var(--rule-2); }
        .capture-totals .l { font-size:12px; color:var(--ink-2); }
        .capture-totals .n { margin-top:3px; font-size:27px; line-height:1.15; }
        .capture-tabs { display:flex; align-items:center; gap:4px; padding:14px 0 9px; }
        .capture-tabs span { padding:7px 14px; border-radius:999px; font-size:12px; color:var(--ink-2); }
        .capture-tabs .selected { background:var(--accent); color:var(--accent-ink); font-weight:700; }
        .capture-table { width:100%; table-layout:fixed; border-spacing:0 7px; border-collapse:separate; font-size:12px; }
        .capture-table th { text-align:left; padding:0 9px 2px; font-size:11px; color:var(--ink-2); font-weight:550; }
        .capture-table td { padding:12px 9px; background:var(--panel); border:0; height:47px; box-sizing:border-box; }
        .capture-table .num { text-align:right; font-variant-numeric:tabular-nums; }
        .capture-table .date b { font-size:12px; font-weight:500; }
        .capture-table .route { font-size:12px; font-weight:650; overflow-wrap:normal; }
        .capture-table .delay { font-size:13px; }
        .capture-table .status { font-size:11px; }
        .capture-table .capture-ready { color:var(--ink-2); }
        .capture-table .refund, .capture-table .st { font-size:12px; }
        .capture-table tr > td:first-child { border-radius:10px 0 0 10px; }
        .capture-table tr > td:last-child { border-radius:0 10px 10px 0; }
        .capture-table .capture-selected td { background:var(--claim-bg); box-shadow:inset 0 1px var(--claim-line); }
        .capture-table .capture-selected > td:first-child { border-radius:12px 0 0; box-shadow:inset 1px 0 var(--claim-line),inset 0 1px var(--claim-line); }
        .capture-table .capture-selected > td:last-child { border-radius:0 12px 0 0; box-shadow:inset -1px 0 var(--claim-line),inset 0 1px var(--claim-line); }
        .capture-table .capture-expanded > td { height:auto; padding:0 16px 16px; position:relative; top:-7px; border-radius:0 0 12px 12px!important; background:var(--claim-bg); box-shadow:inset 1px 0 var(--claim-line),inset -1px 0 var(--claim-line),inset 0 -1px var(--claim-line); }
        .capture-claim { display:flex; align-items:center; gap:20px; border-top:1px solid color-mix(in srgb,var(--claim-line) 35%,transparent); padding-top:14px; }
        .capture-explanation { flex:1; min-width:0; }
        .capture-claim .big-amt { margin:0 0 6px; font-size:33px; line-height:1.1; font-weight:750; letter-spacing:-.025em; }
        .capture-claim .big-amt span { font-size:16px; letter-spacing:0; }
        .capture-claim .sentence { margin:0; font-size:12px; line-height:1.5; }
        .capture-action { flex:none; width:174px; min-height:40px; padding:8px 12px; font-size:12px; white-space:normal; text-align:center; }
      \`;
      document.head.append(style); scrollTo(0,0);
    })()`);
    await shot('00-home', { x: 0, y: 0, width: 800, height: 630 }); return;
  }
  await wait('tr.row.is-claim .delay.big'); await wait('aside [data-act=settings]');
  await annotate('.top [data-act=scan]', T('Controleer nu', 'Check now'), 'l');
  await shot('08-main', VIEW); await clear();

  await click('tr.row.is-claim .chev-btn');
  await wait('tr.detail.is-claim .big-amt');
  const review = 'tr.row.is-claim [data-act=review]';
  // Show only the opened trip (row + details). The app scrolls inside its own container, so use a tall
  // viewport with no scrolling and crop in viewport coordinates.
  await send('Emulation.setDeviceMetricsOverride', { width: VIEW.width, height: 2600, deviceScaleFactor: 1, mobile: false });
  await ev(`(() => { scrollTo(0, 0); document.querySelectorAll('*').forEach(e => { if (e.scrollTop) e.scrollTop = 0 }); return 1 })()`);
  await settle();
  await annotate(review, T('Bekijken en indienen', 'Review and submit'), 'l');
  const tripClip = await ev(`(() => {
    const row = document.querySelector('tr.row.is-claim'), det = document.querySelector('tr.detail.is-claim');
    const a = row.getBoundingClientRect(), b = det.getBoundingClientRect(), pad = 12;
    const x = Math.max(0, Math.min(a.left, b.left) - pad), y = Math.max(0, a.top - pad);
    return { x, y, width: Math.max(a.right, b.right) + pad - x, height: b.bottom + pad - y };
  })()`);
  await shot('09-claimable', tripClip); await clear();
  await send('Emulation.setDeviceMetricsOverride', { width: VIEW.width, height: VIEW.height, deviceScaleFactor: 1, mobile: false });
  await click(review);
  await tick('dialog.review #reachYes'); await tick('dialog.review #decl');
  await annotate('dialog.review [data-x=submit]', T('Indienen bij NS', 'Submit to NS'), 'l');
  await settle(); await shot('10-review', await rect('dialog.review[open]', 0)); await clear();
  await click('dialog.review [data-x=close]');

  await click('aside [data-act=settings]'); await click('dialog.settings [data-tab=account]');
  await settle(); await shot('11-settings', await rect('dialog.settings[open]', 0));
  await click('dialog.settings [data-x=close]');
  await ev('scrollTo(0, 0)'); await clear();
  const top = await rect('.top', 0);
  for (let attempt = 0; attempt < 3; attempt++) {
    await until(`!document.querySelector('.scan-btn.busy')`, 'idle selector .scan-btn', 30000);
    await scrub();
    await click('.top [data-act=scan]');
    // Poll immediately: month text usually arrives within 300-800 ms.
    try { await until(progressReady, 'selector .top #progTxt with month text and #progFill', 3500, 25); }
    catch (error) { if (attempt === 2) throw error; continue; }
    if (await shot('12-progress', top, true)) return;
  }
  throw new Error('Missed mid-check selector .top #progTxt with month text after 3 attempts');
}
try { await run(); }
catch (error) { console.error(`shots: ${error.message}`); process.exitCode = 1; }
finally {
  if (ws?.readyState === WebSocket.OPEN) {
    try { await clear(); await ev(`document.getElementById('shot-style')?.remove()`); } catch { /* disconnected */ }
  }
  ws?.close();
  rejectPending('Screenshot session ended');
}
