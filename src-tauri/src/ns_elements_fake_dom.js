// Test support for ns_elements.rs (run by `cargo test` through Node.js; never loaded by the app).
// A tiny fake DOM with shadow roots and slots, just enough for the element resolver. Page
// layouts mimic NS's claim form (fake texts only; no NS content is fetched).
'use strict';

class FNode {
  constructor(type) { this.nodeType = type; this.childNodes = []; this.parentNode = null; }
  append(...kids) {
    for (const k of kids) { const n = typeof k === 'string' ? new FText(k) : k; n.parentNode = this; this.childNodes.push(n); }
    return this;
  }
  get textContent() { return this.childNodes.map((c) => c.textContent).join(''); }
  getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; }
  querySelectorAll(sel) {
    const out = [];
    const go = (n) => { for (const c of n.childNodes) if (c.nodeType === 1) { if (matches(c, sel)) out.push(c); go(c); } };
    go(this);
    return out;
  }
}
class FText extends FNode { constructor(t) { super(3); this.data = t; } get textContent() { return this.data; } }
class FRoot extends FNode { constructor(host) { super(host ? 11 : 9); if (host) this.host = host; } }
class FEl extends FNode {
  constructor(tag, attrs) { super(1); this.tagName = tag.toUpperCase(); this.attrs = Object.assign({}, attrs || {}); this.shadowRoot = null; this.checked = false; }
  getAttribute(k) { return Object.prototype.hasOwnProperty.call(this.attrs, k) ? String(this.attrs[k]) : null; }
  hasAttribute(k) { return Object.prototype.hasOwnProperty.call(this.attrs, k); }
  get id() { return this.attrs.id || ''; }
  get type() {
    if (this.tagName === 'INPUT') return (this.attrs.type || 'text').toLowerCase();
    if (this.tagName === 'BUTTON') return (this.attrs.type || 'submit').toLowerCase();
    return undefined;
  }
  get disabled() { return this.hasAttribute('disabled'); }
  getBoundingClientRect() { const z = this.hasAttribute('data-unrendered') ? 0 : 10; return { x: 0, y: 0, width: z, height: z }; }
  get options() { return this.tagName === 'SELECT' ? this.childNodes.filter((c) => c.tagName === 'OPTION').map((o) => ({ text: o.textContent })) : undefined; }
  assignedNodes() { const host = this.getRootNode().host; return host ? host.childNodes.slice() : []; }
  attachShadow() { this.shadowRoot = new FRoot(this); return this.shadowRoot; }
}

function compound(el, c) {
  const m = /^([a-z][a-z0-9-]*|\*)?(?:#([\w-]+))?(?:\[([\w-]+)=([\w-]+)\])?$/i.exec(c);
  if (!m) throw new Error('unsupported selector ' + c);
  if (m[1] && m[1] !== '*' && el.tagName !== m[1].toUpperCase()) return false;
  if (m[2] && el.id !== m[2]) return false;
  if (m[3] && el.getAttribute(m[3]) !== m[4]) return false;
  return true;
}
function matches(el, sel) {
  return sel.split(',').some((part) => {
    const parts = part.trim().split(/\s+/);
    if (!compound(el, parts[parts.length - 1])) return false;
    let n = el.parentNode, i = parts.length - 2;
    while (i >= 0 && n && n.nodeType === 1) { if (compound(n, parts[i])) i--; n = n.parentNode; }
    return i < 0;
  });
}

const h = (tag, attrs, ...kids) => new FEl(tag, attrs).append(...kids);
const shadow = (host, ...kids) => { host.attachShadow().append(...kids); return host; };
// NS-style components: the native control and its label live in the component's shadow root.
const radio = (id, label) => shadow(h('nes-radio', {}), h('input', { type: 'radio', id }), h('label', { for: id }, label));
const wrappedRadio = (label) => h('label', {}, h('input', { type: 'radio' }), label);
const textField = (id, label) => shadow(h('nes-input', {}), h('label', { for: id }, label), h('input', { type: 'text', id }));
const checkbox = (id, label) => shadow(h('nes-checkbox', {}), h('input', { type: 'checkbox', id }), h('label', { for: id }, label));
const nesButton = (label, type) => shadow(h('nes-button', {}, label), h('button', type ? { type } : {}, h('slot', {})));
const select = (id, ...opts) => h('select', { id }, ...opts.map((o) => h('option', {}, o)));

const Q_ARRIVAL = 'Heb je je gewenste NS-eindstation bereikt?';
const YES = 'Ja, ik heb mijn NS-eindstation bereikt';
const NO = 'Nee, ik heb mijn NS-eindstation niet bereikt';

function page(ids, opts) {
  opts = opts || {};
  const doc = new FRoot(null);
  const filter = h(opts.filterTag || 'mns-transaction-filter', {},
    select(ids.period, 'Laatste 7 dagen', 'Afgelopen jaar', opts.periodText || 'Afgelopen 1,5 jaar'), nesButton('Tonen', 'button'));
  const rows = h('mns-travel-history-list', {},
    h('div', { id: 'row0' }, opts.linkByText ? h('nes-link', { id: 'row0-link' }, 'Geld terug bij vertraging') : h('a', { 'data-test': 'gtbv-claim-link', id: 'link-0' }, 'Geld terug bij vertraging')),
    h('div', { id: 'row1' }, opts.linkByText ? h('nes-link', { id: 'row1-link' }, 'Geld terug bij vertraging') : h('a', { 'data-test': 'gtbv-claim-link', id: 'link-1' }, 'Geld terug bij vertraging')));
  const arrival = opts.swapped
    ? h('fieldset', {}, h('legend', {}, Q_ARRIVAL), radio('arrival1', YES), radio('arrival0', NO))
    : h('fieldset', {}, h('legend', {}, Q_ARRIVAL), radio(ids.yes, YES), radio(ids.no, NO));
  const decoy = opts.decoy ? h('fieldset', {}, h('legend', {}, 'Een andere vraag'), wrappedRadio(YES)) : h('div', {});
  const delay = opts.duplicate
    ? h('div', {}, h('fieldset', {}, h('legend', {}, 'Vertraging'), wrappedRadio('30 t/m 59 min')), h('fieldset', {}, h('legend', {}, 'Vertraging'), wrappedRadio('30 t/m 59 min')))
    : shadow(h('nes-radio-group', {}), h('div', {}, 'Vertraging'), radio(ids.d30, '30 t/m 59 min'), radio(ids.d60, '60 minuten of meer'));
  const bank = opts.ariaOnly
    ? h('div', {},
      h('input', { type: 'text', 'aria-label': 'Naam rekeninghouder' }),
      h('input', { type: 'text', 'aria-label': 'Bankrekeningnummer (IBAN)' }),
      shadow(h('nes-checkbox', {}), h('input', { type: 'checkbox', 'aria-labelledby': 'decl-text' }), h('span', { id: 'decl-text' }, 'Ik heb alles naar waarheid ingevuld.')))
    : h('div', {}, textField(ids.name, 'Naam rekeninghouder *'), textField(ids.iban, 'Bankrekeningnummer (IBAN)'), checkbox(ids.decl, 'Ik heb alles naar waarheid ingevuld.'));
  doc.append(h('html', {}, h('body', {},
    opts.noHeader ? h('div', {}) : h('header', {}, h('form', { id: 'search' }, h('button', { type: 'submit', id: 'header-search' }, 'Zoeken'))),
    filter, rows,
    h('form', { id: 'claim' }, select(ids.ci, '08:01'), select('checkOut', '08:59'), arrival, decoy,
      textField(ids.dep, 'Vertrektijd volgens dienstregeling'), delay, bank, nesButton('Verzenden', opts.submitType),
      // NS keeps a hidden, zero-size native twin of the component's button in the light DOM for the form.
      opts.twin ? h('button', opts.twin === 'rendered' ? { type: 'submit' } : { type: 'submit', 'data-unrendered': '' }, 'Verzenden') : h('div', {})))));
  return doc;
}

const CURRENT = { period: 'filter-period-control', yes: 'arrival0', no: 'arrival1', d30: 'delay0', d60: 'delay1', dep: 'departure', ci: 'checkIn', name: 'name', iban: 'iban', decl: 'declaration' };
const RENAMED = { period: 'period-v2', yes: 'ans-yes', no: 'ans-no', d30: 'late-30', d60: 'late-60', dep: 'dep-time', ci: 'ci', name: 'holder', iban: 'acct', decl: 'truth' };
const KEYS = { arrival_yes: 0, arrival_no: 0, delay_30: 0, delay_60: 0, departure: 0, iban: 0, name: 0, declaration: 0, submit: 0, check_in: 0, period: 0, show: 0 };

function report(doc) {
  globalThis.document = doc;
  const out = {};
  for (const key of Object.keys(KEYS)) out[key] = Object.assign(window.__ovr.cands(SPECS[key], doc), { strategies: SPECS[key].strategies.length });
  return out;
}
const firstUnique = (spec, root) => { for (let i = 0; i < spec.strategies.length; i++) { const e = window.__ovr.pick(spec, i, root); if (e) return e; } return null; };
const rowRoot = (doc, id) => doc.querySelectorAll('div').find((d) => d.id === id);

function runScenarios() {
  globalThis.window = globalThis;
  (0, eval)(RESOLVER);
  const result = {};

  const cur = page(CURRENT, { submitType: 'submit' });
  result.current = report(cur);
  const decl = window.__ovr.pick(SPECS.declaration, 0, cur);
  const submit = window.__ovr.pick(SPECS.submit, 1, cur);
  const header = cur.querySelectorAll('button').find((b) => b.id === 'header-search');
  result.current.submit_ok = window.__ovr.submitOk(submit, decl);
  result.current.header_submit_ok = window.__ovr.submitOk(header, decl);
  decl.checked = true;
  result.current.checked_after_click = window.__ovr.checked(window.__ovr.pick(SPECS.declaration, 0, cur));
  result.current.claim_link_row1 = window.__ovr.pick(SPECS.claim_link, 0, rowRoot(cur, 'row1')).id;

  const ren = page(RENAMED, { filterTag: 'mns-filter-v2', periodText: 'Afgelopen 1.5  jaar', linkByText: true, noHeader: true });
  result.renamed = report(ren);
  result.renamed.picked_arrival_no = firstUnique(SPECS.arrival_no, ren).id;
  result.renamed.picked_delay_60 = firstUnique(SPECS.delay_60, ren).id;
  result.renamed.submit_ok = window.__ovr.submitOk(window.__ovr.pick(SPECS.submit, 1, ren), firstUnique(SPECS.declaration, ren));
  result.renamed.claim_link_row1 = window.__ovr.pick(SPECS.claim_link, 1, rowRoot(ren, 'row1')).id;

  // The form's button lost its type: `button[type=submit]` now finds only the header search button.
  result.untyped = report(page(CURRENT, {}));
  result.aria = report(page(RENAMED, { ariaOnly: true }));
  result.swapped = report(page(CURRENT, { swapped: true }));
  result.duplicate = report(page(RENAMED, { duplicate: true }));
  result.decoy = report(page(RENAMED, { decoy: true }));
  const twin = page(CURRENT, { submitType: 'submit', noHeader: true, twin: 'hidden' });
  result.twin = report(twin);
  const twinBtn = window.__ovr.pick(SPECS.submit, 0, twin);
  result.twin.picked_in_shadow = !!twinBtn && twinBtn.getRootNode() !== twin;
  result.twin.submit_ok = window.__ovr.submitOk(twinBtn, window.__ovr.pick(SPECS.declaration, 0, twin));
  result.twin_rendered = report(page(CURRENT, { submitType: 'submit', noHeader: true, twin: 'rendered' }));

  result.norm = {};
  for (const s of ['  Ik heb alles naar waarheid ingevuld. ', 'Naam rekeninghouder *', 'Afgelopen 1.5 jaar', 'Afgelopen 1,5   JAAR', 'Heb je je gewenste NS-eindstation bereikt?', 'Station 1. Utrecht', '30 t/m 59 min'])
    result.norm[s] = window.__ovr.norm(s);
  process.stdout.write(JSON.stringify(result));
}
