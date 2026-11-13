// Real demo app + simulated gateway. Node 22.13+; use run-demo-review.ps1 -AutoClaims.
// Fixtures are changed only in the randomly named, isolated demo ledger under TEMP.
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import path from 'node:path';
import os from 'node:os';

const port = Number(process.env.PORT);
const file = path.resolve(process.env.DEMO_REVIEW_DB || '.');
assert(Number.isInteger(port) && port > 0 && port < 65536);
assert.equal(path.dirname(path.dirname(file)).toLowerCase(), path.resolve(os.tmpdir()).toLowerCase());
assert.match(path.basename(path.dirname(file)), /^OVRefund-demo-review[a-f0-9]{8}$/);
assert.equal(path.basename(file), 'ovrefund.sqlite');
const sleep = ms => new Promise(r => setTimeout(r, ms));
let ws, seq = 0, db;
const pending = new Map(), errors = [], requests = [], results = [];
function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++seq;
    const timer = setTimeout(() => { pending.delete(id); reject(Error(`Timed out: ${method}`)); }, 60000);
    pending.set(id, { resolve, reject, timer });
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function ev(expression) {
  const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw Error(r.exceptionDetails.exception?.description || JSON.stringify(r.exceptionDetails.exception?.value) || r.exceptionDetails.text);
  return r.result?.value;
}
const invoke = (cmd, args = {}) => ev(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`);
async function until(check, label, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await check()) return; await sleep(60); }
  throw Error(`Timed out: ${label}`);
}
async function step(label, fn) {
  try { await fn(); results.push(true); console.log(`PASS ${label}`); }
  catch (e) { results.push(false); console.log(`FAIL ${label}: ${e.message}`); }
}
const rows = table => db.prepare(`SELECT * FROM ${table}`).all();
const setting = key => db.prepare('SELECT value FROM settings WHERE key=?').get(key)?.value;
const set = (key, value) => db.prepare('INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value').run(key, value);
const countSent = () => Number(db.prepare("SELECT count(*) AS n FROM claim_events WHERE to_status='SUBMITTING' AND actor='automatic'").get().n);
let snapshot;
async function reset({ unresolved = false } = {}) {
  assert(!(await invoke('overview')).busy, 'never reset a fixture while the app is running a check');
  db.exec('BEGIN IMMEDIATE');
  try {
    db.exec('DROP TRIGGER claim_events_no_delete');
    for (const table of ['claim_events', 'claims', 'candidates', 'settings', 'external_claims']) db.exec(`DELETE FROM ${table}`);
    for (const table of ['settings', 'candidates', 'claims', 'claim_events', 'external_claims']) {
      for (const row of snapshot[table]) {
        const keys = Object.keys(row);
        db.prepare(`INSERT INTO ${table} (${keys.join(',')}) VALUES (${keys.map(() => '?').join(',')})`).run(...keys.map(k => row[k]));
      }
    }
    db.exec("CREATE TRIGGER claim_events_no_delete BEFORE DELETE ON claim_events BEGIN SELECT RAISE(ABORT, 'claim_events is append-only'); END;");
    db.exec('COMMIT');
  } catch (e) { db.exec('ROLLBACK'); throw e; }
  await invoke('set_bank_details', { name: 'Demo User', iban: 'NL91ABNA0417164300' });
  if (!unresolved) {
    for (const c of rows('claims').filter(c => c.status === 'INDETERMINATE')) {
      await invoke('set_claim_status', { claimId: c.id, status: 'FAILED' });
    }
  }
  await invoke('save_settings', { s: {
    ...(await invoke('get_settings')), mode: 'automatic', auto_consent: true,
    schedule_enabled: false, notify_windows: false, notify_email: false,
  } });
}
async function startRun() {
  await until(() => ev(`!!document.querySelector('[data-act=demoauto]:not(:disabled)')`), 'enabled simulation button');
  return ev(`(() => {
    window.__autoReviewResult = null;
    window.__autoReviewStage = '';
    document.querySelector('[data-act=demoauto]').click();
    return true;
  })()`);
}
async function finishRun() {
  await until(() => ev('window.__autoReviewResult !== null'), 'automatic run', 60000);
  const r = await ev('window.__autoReviewResult');
  assert(!r.error, r.error);
  return r.value;
}
async function atPause() {
  await until(() => ev(`window.__autoReviewStage === 'pause'`), 'pause between claims');
}

try {
  let targets;
  await until(async () => {
    try { targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); }
    catch { return false; }
    return targets.some(t => /^https?:\/\/(tauri\.localhost|localhost)([:/]|$)/.test(t.url));
  }, 'demo app CDP');
  const pages = targets.filter(t => t.type === 'page' && /^https?:\/\/(tauri\.localhost|localhost)([:/]|$)/.test(t.url));
  assert.equal(pages.length, 1);
  assert(['127.0.0.1', 'localhost', '[::1]'].includes(new URL(pages[0].webSocketDebuggerUrl).hostname));
  ws = new WebSocket(pages[0].webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  ws.onmessage = ({ data }) => {
    const m = JSON.parse(data), p = pending.get(m.id);
    if (m.method === 'Runtime.exceptionThrown') errors.push(m.params.exceptionDetails.text);
    if (m.method === 'Network.requestWillBeSent') requests.push(m.params.request.url);
    if (!p) return;
    pending.delete(m.id); clearTimeout(p.timer);
    m.error ? p.reject(Error(m.error.message)) : p.resolve(m.result);
  };
  await send('Runtime.enable'); await send('Network.enable');
  await until(() => ev(`!!document.querySelector('.demo-banner')`), 'DEMO banner');
  assert.equal((await invoke('overview')).demo, true);
  await ev(`window.__TAURI_INTERNALS__.invoke('plugin:event|listen', {
    event: 'scan-progress', target: { kind: 'Any' },
    handler: window.__TAURI_INTERNALS__.transformCallback(e => { window.__autoReviewStage = e.payload.stage; })
  })`);
  await ev(`window.__TAURI_INTERNALS__.invoke('plugin:event|listen', {
    event: 'run-finished', target: { kind: 'Any' },
    handler: window.__TAURI_INTERNALS__.transformCallback(e => {
      window.__autoReviewResult = e.payload.Ok ? { value: e.payload.Ok } : { error: e.payload.Err || JSON.stringify(e.payload) };
    })
  })`);
  await invoke('set_flag', { key: 'wizard_done', value: '1' });
  await invoke('set_flag', { key: 'disclaimer_accepted', value: 'demo' });
  await invoke('set_flag', { key: 'other_service', value: 'none' });
  await invoke('save_settings', { s: {
    ...(await invoke('get_settings')), mode: 'manual', auto_consent: false,
    schedule_enabled: false, notify_windows: false, notify_email: false,
  } });
  await ev('document.querySelector(".wizard")?.remove()');
  await invoke('open_login');
  await until(async () => { const o = await invoke('overview'); return o.journeys > 0 && !o.busy; }, 'login-triggered check');
  db = new DatabaseSync(file); db.exec('PRAGMA busy_timeout=5000');
  snapshot = Object.fromEntries(['settings', 'candidates', 'claims', 'claim_events', 'external_claims'].map(t => [t, rows(t)]));
  assert(rows('claims').some(c => c.status === 'INDETERMINATE'), 'fixture must contain an uncertain outcome');
  assert(rows('claims').filter(c => c.status === 'READY').length >= 3, 'fixture must contain several ready claims');

  await step('uncertain prior outcome blocks every automatic submission', async () => {
    await reset({ unresolved: true }); const before = countSent();
    const r = await invoke('demo_auto_run');
    assert.equal(r.submitted, 0); assert.equal(r.auto_paused_code, 'uncertain'); assert.equal(countSent(), before);
  });
  await step('missing bank account blocks submissions', async () => {
    await reset(); await invoke('clear_bank_details'); const before = countSent();
    const r = await invoke('demo_auto_run');
    assert.equal(r.submitted, 0); assert.equal(r.auto_paused_code, 'bank'); assert.equal(countSent(), before);
  });
  await step('claim-service answer yes withdraws consent and blocks submissions', async () => {
    await reset(); await invoke('set_flag', { key: 'other_service', value: 'yes' }); const before = countSent();
    const r = await invoke('demo_auto_run');
    assert.equal(r.submitted, 0); assert.equal(countSent(), before); assert(!setting('auto_opt_in_at'));
  });
  await step('Stop during the pause prevents the next claim', async () => {
    await reset(); const before = countSent(); await startRun(); await atPause();
    await send('Emulation.setDeviceMetricsOverride', { width: 1040, height: 720, deviceScaleFactor: 1, mobile: false });
    assert(await ev(`(() => { const t = document.querySelector('#progTxt'); return !!t && t.scrollWidth <= t.clientWidth + 1 && !document.querySelector('#progLive'); })()`), 'pause count fits and has one live announcement');
    await invoke('stop_auto_run'); const r = await finishRun();
    assert.equal(r.submitted, 1); assert.equal(countSent() - before, 1); assert.equal(r.auto_paused_code, 'stopped');
  });
  await step('removing the bank account during a pause stops further submissions', async () => {
    await reset(); const before = countSent(); await startRun(); await atPause();
    await invoke('clear_bank_details'); const r = await finishRun();
    assert.equal(r.submitted, 1); assert.equal(countSent() - before, 1); assert.match(r.auto_paused_reason, /bank account was removed/);
  });
  await step('withdrawing consent during a pause stays off after the demo run', async () => {
    await reset(); const before = countSent(); await startRun(); await atPause();
    await invoke('set_flag', { key: 'other_service', value: 'yes' }); const r = await finishRun();
    assert.equal(r.submitted, 1); assert.equal(countSent() - before, 1);
    assert.equal(setting('mode'), 'manual'); assert(!setting('auto_opt_in_at'));
  });
  await step('claimed-elsewhere import during a pause protects that exact trip', async () => {
    await reset(); await startRun(); await atPause();
    const next = db.prepare("SELECT j.* FROM claims c JOIN journeys j ON j.id=c.journey_id WHERE c.status='READY' ORDER BY j.check_in_utc LIMIT 1").get();
    assert(next);
    const date = new Intl.DateTimeFormat('en-CA', { timeZone: 'Europe/Amsterdam' }).format(new Date(next.check_in_utc));
    const r = await invoke('add_external_claim', { date, origin: next.origin_name, destination: next.dest_name, source: 'demo' });
    assert.equal(r.matched, 1); assert.equal(r.pending, true); assert.equal(r.trips[0].from, next.origin_name);
    await finishRun();
    assert.equal(db.prepare("SELECT count(*) AS n FROM claim_events e JOIN claims c ON c.id=e.claim_id WHERE c.journey_id=? AND e.to_status='SUBMITTING'").get(next.id).n, 0);
  });
  await step('Check now submits only within 14 days and reports the remainder', async () => {
    await reset(); const before = countSent(); const r = await invoke('scan_now');
    assert.equal(r.submitted, 1); assert(r.auto_later > 0); assert.equal(countSent() - before, 1);
  });
  await step('scheduled demo submits all eligible claims once and leaves review trips untouched', async () => {
    await reset(); const before = countSent();
    const reviewIds = rows('claims').filter(c => c.status === 'NEEDS_REVIEW').map(c => c.id);
    const r = await invoke('demo_auto_run'); assert(r.submitted >= 3);
    assert.equal(countSent() - before, r.submitted);
    for (const record of ['DEMO-003', 'DEMO-004']) {
      const sent = db.prepare("SELECT count(*) AS n FROM claim_events e JOIN claims c ON c.id=e.claim_id JOIN journeys j ON j.id=c.journey_id WHERE j.source_record_id=? AND e.to_status='SUBMITTING' AND e.actor='automatic'").get(record);
      assert.equal(sent.n, 1, `${record}: matched 31-minute delay and cancelled-train replacement use the normal automatic queue`);
    }
    for (const id of reviewIds) assert.equal(db.prepare("SELECT count(*) AS n FROM claim_events WHERE claim_id=? AND to_status='SUBMITTING'").get(id).n, 0);
    assert.equal((await invoke('demo_auto_run')).submitted, 0);
    assert.equal(countSent() - before, r.submitted, 'no duplicate after a second check');
    assert.equal(db.prepare("SELECT count(*) AS n FROM (SELECT claim_id FROM claim_events WHERE to_status='SUBMITTING' AND actor='automatic' GROUP BY claim_id HAVING count(*)>1)").get().n, 0);
  });
  await step('no console exceptions and no external web requests', async () => {
    assert.deepEqual(errors, []);
    assert.deepEqual(requests.filter(u => !/^(https?:\/\/((tauri|ipc)\.localhost|localhost)([:/]|$)|data:|blob:|about:)/.test(u)), []);
  });
} catch (e) { console.log(`FAIL setup: ${e.stack}`); results.push(false); }
finally {
  db?.close(); ws?.close();
  for (const p of pending.values()) { clearTimeout(p.timer); p.reject(Error('Demo test ended')); }
}
console.log(`auto-claim-smoke: ${results.filter(Boolean).length}/${results.length} passed`);
process.exitCode = results.every(Boolean) ? 0 : 1;
