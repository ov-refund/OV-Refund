// GUI smoke test of the real demo app through loopback CDP. See gui-smoke.md (Node 22+).
// Demo mode only: refuses to act without the DEMO banner. Writes no files.

const PORT = Number(process.env.PORT || 9337);
if (!Number.isInteger(PORT) || PORT < 1 || PORT > 65535) throw new Error('PORT must be 1..65535');
const VIEW = { width: 1040, height: 720 };
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const q = JSON.stringify;
let ws;
let sequence = 0;
const pending = new Map();
const consoleErrors = [];
const results = [];

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
const isVisible = (sel) => ev(visible(sel));
const wait = (sel, timeout) => until(visible(sel), `selector ${sel}`, timeout);
async function click(sel) {
  await wait(sel);
  await until(`!document.querySelector(${q(sel)})?.matches(':disabled,[aria-disabled="true"]')`, `enabled selector ${sel}`);
  await ev(`document.querySelector(${q(sel)}).click()`);
}
async function value(sel, text) {
  await wait(sel);
  await ev(`(() => { const e = document.querySelector(${q(sel)}); e.value = ${q(text)}; e.dispatchEvent(new Event('input', { bubbles: true })); e.dispatchEvent(new Event('change', { bubbles: true })); })()`);
}
async function tick(sel) {
  await wait(sel);
  if (!await ev(`document.querySelector(${q(sel)}).checked`)) await click(sel);
}
const frames = () => ev('new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(1))))');
const idle = () => until(`!document.querySelector('.scan-btn.busy')`, 'idle selector .scan-btn', 60000);
const invoke = (cmd) => ev(`window.__TAURI_INTERNALS__.invoke(${q(cmd)})`);
const toastText = () => ev(`(() => { const t = document.querySelector('.toast.show'); return t ? { text: t.textContent, err: t.classList.contains('err') } : null; })()`);
const isBusyToast = (t) => /Er loopt al een controle|A check is already running/.test(t?.text ?? '');

async function step(name, fn) {
  try { await fn(); results.push([name, true]); console.log(`PASS ${name}`); }
  catch (error) { results.push([name, false]); console.log(`FAIL ${name}: ${error.message}`); }
}

// The open dialog and its header + close button must stay fully inside the viewport, and
// the close button must be the element actually under its own centre (not covered/scrolled).
async function dialogInView(sel) {
  const problem = await ev(`(() => {
    const d = document.querySelector(${q(sel)});
    if (!d || !d.open) return 'dialog not open';
    if (getComputedStyle(d).position !== 'fixed') return 'dialog position is ' + getComputedStyle(d).position;
    const inside = (r) => r.width > 0 && r.height > 0 && r.top >= -0.5 && r.left >= -0.5 && r.bottom <= innerHeight + 0.5 && r.right <= innerWidth + 0.5;
    const r = d.getBoundingClientRect(), fmt = (r) => [r.left, r.top, r.right, r.bottom].map(n => Math.round(n)).join(',');
    if (!inside(r)) return 'dialog rect ' + fmt(r) + ' outside viewport ' + innerWidth + 'x' + innerHeight;
    const hd = d.querySelector('.hd'), x = d.querySelector('[data-x=close]');
    if (!hd || !inside(hd.getBoundingClientRect())) return 'header not fully visible';
    if (!x || !inside(x.getBoundingClientRect())) return 'close button not fully visible';
    const c = x.getBoundingClientRect(), hit = document.elementFromPoint(c.left + c.width / 2, c.top + c.height / 2);
    if (!hit || !x.contains(hit)) return 'close button is covered by ' + (hit ? hit.tagName + '.' + hit.className : 'nothing');
    return '';
  })()`);
  if (problem) throw new Error(`${sel}: ${problem}`);
}
async function wheel(x, y, deltaY) {
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
  await send('Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX: 0, deltaY });
  await pause(120); await frames();
}
// Real wheel input over the backdrop (a viewport corner outside the dialog) and over the dialog body.
async function wheelCheck(sel) {
  await dialogInView(sel);
  const r = await ev(`(() => { const r = document.querySelector(${q(sel)}).getBoundingClientRect(); return { l: r.left, t: r.top, r: r.right, b: r.bottom }; })()`);
  const spots = [];
  if (r.l > 8) spots.push(['backdrop', 4, VIEW.height / 2]);
  else if (r.t > 8) spots.push(['backdrop', VIEW.width / 2, 4]);
  else if (VIEW.height - r.b > 8) spots.push(['backdrop', VIEW.width / 2, VIEW.height - 4]);
  const body = await ev(`(() => { const b = document.querySelector(${q(sel + ' .bd')}) || document.querySelector(${q(sel)}); const r = b.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + Math.min(r.height / 2, 120) }; })()`);
  spots.push(['dialog', body.x, body.y]);
  for (const [where, x, y] of spots) {
    for (const deltaY of [400, 400, 1200, -400, -1600, 1600]) {
      await wheel(x, y, deltaY);
      try { await dialogInView(sel); } catch (error) { throw new Error(`after wheel ${deltaY} over ${where}: ${error.message}`); }
    }
  }
  if (!spots.some(([where]) => where === 'backdrop')) console.log(`  note: ${sel} covers the viewport edges; only the dialog was wheeled`);
}

async function finishWizard() {
  // Adaptive: answer whichever wizard step is shown, with fictional demo data.
  for (let i = 0; i < 40 && await ev(`!!document.querySelector('.wizard')`); i++) {
    if (await isVisible('#wzAccept')) { await tick('#wzAccept'); await click('[data-wz=accept]'); }
    else if (await isVisible('[data-wz=login]')) await click('[data-wz=login]');
    else if (await isVisible('#svcNo')) { await tick('#svcNo'); await click('[data-wz=saveservices]'); }
    else if (await isVisible('#wzIban')) {
      await value('#wzBankName', 'Jan de Vries'); await value('#wzIban', 'NL91ABNA0417164300');
      await click('[data-wz=savebank]');
    } else if (await isVisible('[data-wz=saveauto]')) await click('[data-wz=saveauto]'); // automatic stays off
    else if (await isVisible('[data-wz=saveautologin]')) { if (await isVisible('#alNo')) await tick('#alNo'); await click('[data-wz=saveautologin]'); }
    else if (await isVisible('[data-wz=savenotify]')) { await ev(`(() => { const b = document.querySelector('#wzMailBox'); if (b) b.open = false; const w = document.querySelector('#wzWin'); if (w) { w.checked = false; w.dispatchEvent(new Event('change', { bubbles: true })); } })()`); await click('[data-wz=savenotify]'); }
    else if (await isVisible('[data-wz=finish]')) await click('[data-wz=finish]');
    else if (await isVisible('dialog.ask[open] [data-x=yes]')) await click('dialog.ask[open] [data-x=yes]');
    await pause(250);
  }
  if (await ev(`!!document.querySelector('.wizard')`)) throw new Error('wizard did not finish');
}

async function connect() {
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
  const endpoint = new URL(targets[0].webSocketDebuggerUrl);
  if (!['127.0.0.1', 'localhost', '[::1]'].includes(endpoint.hostname)) throw new Error('CDP WebSocket must use loopback');
  ws = new WebSocket(endpoint);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.close(); reject(new Error('CDP WebSocket connection timed out')); }, 5000);
    ws.onopen = () => { clearTimeout(timer); resolve(); };
    ws.onerror = () => { clearTimeout(timer); reject(new Error('Cannot connect to CDP WebSocket')); };
  });
  ws.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    if (message.method === 'Runtime.exceptionThrown') consoleErrors.push(`exception: ${message.params.exceptionDetails.exception?.description ?? message.params.exceptionDetails.text}`);
    else if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') consoleErrors.push(`console.error: ${message.params.args.map(a => a.value ?? a.description ?? '').join(' ')}`);
    else if (message.method === 'Log.entryAdded' && message.params.entry.level === 'error') consoleErrors.push(`log: ${message.params.entry.text}`);
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id); clearTimeout(request.timer);
    if (message.error) request.reject(new Error(`CDP: ${message.error.message}`));
    else request.resolve(message.result);
  };
  ws.onclose = () => rejectPending('CDP connection closed');
  ws.onerror = () => rejectPending('CDP connection failed');
}

async function run() {
  await connect();
  await send('Page.enable'); await send('Runtime.enable'); await send('Log.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: VIEW.width, height: VIEW.height, deviceScaleFactor: 1, mobile: false });
  await ev(`localStorage.setItem('lang', 'nl')`);
  await send('Page.reload');
  await pause(500);
  await wait('.demo-banner', 20000); // Guard before any login/check/submit action.

  await step('setup: wizard, demo login and first check', async () => {
    await finishWizard();
    if (!await isVisible('tr.row')) {
      await idle(); await click('.top [data-act=scan]');
    }
    await until(`!!document.querySelector('tr.row') && !document.querySelector('.scan-btn.busy')`, 'trips after Controleer nu', 60000);
    await idle();
    if (!await isVisible('.demo-banner')) throw new Error('DEMO banner disappeared');
  });
  if (!results.at(-1)[1]) return;

  await step('submit dialog stays in view under mouse-wheel', async () => {
    await ev(`(() => { const b = document.querySelector('[data-cat=all]'); if (b) b.click(); })()`);
    const review = 'tr.row.is-claim [data-act=review]';
    if (!await isVisible(review)) {
      await click('tr.row.is-claim .chev-btn');
    }
    await click(review);
    await wait('dialog.review[open]');
    try {
      await wheelCheck('dialog.review');
      await tick('#reachYes');
      await click('dialog.review [data-x=pf]');
      await until(`!document.querySelector('dialog.review [data-x=pf]').disabled && document.querySelector('#pf').textContent.includes('Gelukt')`, 'demo preflight result');
      if (!await ev(`document.activeElement === document.querySelector('dialog.review [data-x=pf]')`)) throw new Error('preflight lost keyboard focus');
      if (!await ev(`(() => { const h = document.querySelector('#subHint').getBoundingClientRect(); const b = document.querySelector('dialog.review [data-x=pf]').getBoundingClientRect(); return h.bottom <= b.top + 1 && h.width > 300; })()`)) throw new Error('submit hint is not on its own row');
    }
    finally { if (await isVisible('dialog.review[open] [data-x=close]')) await click('dialog.review[open] [data-x=close]'); }
    await until(`!document.querySelector('dialog.review[open]')`, 'closed submit dialog');
  });

  await step('Settings (1040x720) stays in view under mouse-wheel', async () => {
    await click('aside [data-act=settings]');
    await wait('dialog.settings[open]');
    try {
      for (const tab of ['account', 'help', 'erase']) {
        if (await isVisible(`dialog.settings [data-tab=${tab}]`)) { await click(`dialog.settings [data-tab=${tab}]`); await frames(); }
        await wheelCheck('dialog.settings');
      }
    } finally { if (await isVisible('dialog.settings[open] [data-x=close]')) await click('dialog.settings[open] [data-x=close]'); }
    await until(`!document.querySelector('dialog.settings[open]')`, 'closed Settings');
  });

  await step('bug dialog stays in view under mouse-wheel', async () => {
    await click('footer [data-act=bugreport]');
    await wait('dialog.bugdlg[open]');
    try { await wheelCheck('dialog.bugdlg'); }
    finally { if (await isVisible('dialog.bugdlg[open] [data-x=close]')) await click('dialog.bugdlg[open] [data-x=close]'); }
    await until(`!document.querySelector('dialog.bugdlg[open]')`, 'closed bug dialog');
  });

  await step('Simuleer automatische controle succeeds and restores the mode', async () => {
    await idle();
    const before = (await invoke('get_settings'))?.mode;
    if (!before) throw new Error('cannot read mode before the run');
    await ev(`(() => { const t = document.querySelector('.toast'); if (t) { t.textContent = ''; t.classList.remove('show', 'err'); } })()`);
    await click('.demo-banner [data-act=demoauto]');
    const deadline = Date.now() + 60000;
    let toast = null;
    while (Date.now() < deadline) {
      toast = await toastText();
      if (toast?.err || isBusyToast(toast) || /Automatische controle:|Automatic check:/.test(toast?.text ?? '')) break;
      await pause(100);
    }
    if (isBusyToast(toast)) throw new Error(`refused: "${toast.text}"`);
    if (!toast || toast.err || !/Automatische controle:|Automatic check:/.test(toast.text)) throw new Error(`no success message (toast: ${q(toast?.text ?? null)})`);
    await idle();
    const after = (await invoke('get_settings'))?.mode;
    if (after !== before) throw new Error(`mode was ${before}, is now ${after}`);
    console.log(`  ${toast.text.trim()} (mode ${after})`);
  });

  await step('⋯ Meer closes on Esc with focus back on its button', async () => {
    // The menu lives inside an opened trip: open the first claimable trip's details first.
    await ev(`(() => { if (!document.querySelector('.more-menu')) document.querySelector('tr.row.is-claim .chev-btn')?.click(); return 1; })()`);
    await wait('.more-menu summary');
    const key = await ev(`(() => { const d = document.querySelector('.more-menu'); d.scrollIntoView({ block: 'center' }); return d.dataset.k; })()`);
    const menu = `.more-menu[data-k=${q(key)}]`;
    await click(`${menu} summary`);
    await until(`!!document.querySelector(${q(menu)})?.open`, 'open ⋯ Meer menu');
    await ev(`(() => { const m = document.querySelector(${q(menu)}); (m.querySelector('.menu button, .menu a, .menu [tabindex]') || m.querySelector('summary')).focus(); })()`);
    await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27, nativeVirtualKeyCode: 27 });
    await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27, nativeVirtualKeyCode: 27 });
    await frames();
    await until(`!document.querySelector(${q(menu)})?.open`, 'closed ⋯ Meer menu after Esc', 3000);
    const focus = await ev(`(() => { const a = document.activeElement; return a?.tagName === 'SUMMARY' && a.closest('.more-menu')?.dataset.k === ${q(key)}; })()`);
    if (!focus) throw new Error(`focus is on ${await ev(`document.activeElement?.outerHTML.slice(0, 80)`)}`);
  });

  await step('language toggle updates <html lang>', async () => {
    const start = await ev('document.documentElement.lang');
    const other = start === 'nl' ? 'en' : 'nl';
    await click('footer [data-setlang]');
    await until(`document.documentElement.lang === ${q(other)}`, `<html lang="${other}">`, 5000);
    await click('footer [data-setlang]');
    await until(`document.documentElement.lang === ${q(start)}`, `<html lang="${start}">`, 5000);
  });

  await step('no console errors or exceptions', async () => {
    await pause(300);
    if (consoleErrors.length) throw new Error(`${consoleErrors.length}:\n  ${consoleErrors.join('\n  ')}`);
  });
}

try { await run(); }
catch (error) { results.push(['run', false]); console.log(`FAIL ${error.message}`); }
finally {
  try { if (ws?.readyState === WebSocket.OPEN) await send('Emulation.clearDeviceMetricsOverride'); } catch { /* disconnected */ }
  ws?.close();
  rejectPending('Smoke session ended');
}
const failed = results.filter(([, ok]) => !ok).length;
console.log(failed ? `gui-smoke: ${failed} of ${results.length} checks FAILED` : `gui-smoke: all ${results.length} checks passed`);
process.exitCode = failed ? 1 : 0;
