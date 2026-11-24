// Test support for ns_adapters.rs (run by `cargo test` through Node.js; never loaded by the app).
// A tiny fake page with layout boxes, text ranges, computed styles and shadow roots, just enough
// for the travel-proof mask script (MASK_MEASURE). Fake texts only; no NS content is fetched.
'use strict';

let SX = 0, SY = 0;
class FNode {
  constructor(type) { this.nodeType = type; this.childNodes = []; this.parentNode = null; }
  append(...kids) {
    for (const k of kids) { const n = typeof k === 'string' ? new FText(k) : k; n.parentNode = this; this.childNodes.push(n); }
    return this;
  }
  get textContent() { return this.childNodes.map((c) => c.textContent).join(''); }
  get parentElement() { return this.parentNode && this.parentNode.nodeType === 1 ? this.parentNode : null; }
  get children() { return this.childNodes.filter((c) => c.nodeType === 1); }
  getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; }
  contains(o) { for (let n = o; n; n = n.parentNode) if (n === this) return true; return false; }
  querySelectorAll(sel) {
    const out = [];
    const go = (n) => { for (const c of n.childNodes) if (c.nodeType === 1) { if (matchesSel(c, sel)) out.push(c); go(c); } };
    go(this);
    return out;
  }
  querySelector(sel) { return this.querySelectorAll(sel)[0] || null; }
}
// A text node; `rect` [x, y, w, h] in page coordinates (else its parent's box).
class FText extends FNode { constructor(t, rect) { super(3); this.data = t; this.rect = rect; } get textContent() { return this.data; } }
class FRoot extends FNode { constructor(host) { super(host ? 11 : 9); if (host) this.host = host; } }
class FEl extends FNode {
  constructor(tag, a) {
    super(1);
    a = a || {};
    this.tagName = tag.toUpperCase();
    this.attrs = Object.assign({}, a.attrs || {});
    this.rect = a.rect || [0, 0, 0, 0];
    this.style = Object.assign({}, a.style || {});
    this.shadowRoot = null;
    this.open = !!a.open;
    this.popover = !!a.popover;
    this.selectedIndex = a.selectedIndex || 0;
    this.value = a.value || '';
    this.placeholder = '';
    this.noPopoverSupport = !!a.noPopoverSupport;
  }
  get id() { return this.attrs.id || ''; }
  get type() { return this.tagName === 'INPUT' ? (this.attrs.type || 'text') : undefined; }
  hasAttribute(k) { return Object.prototype.hasOwnProperty.call(this.attrs, k) || (k === 'open' && this.open); }
  getBoundingClientRect() { const [x, y, w, h] = this.rect; return { left: x - SX, top: y - SY, width: w, height: h }; }
  get options() { return this.tagName === 'SELECT' ? this.children.map((o) => ({ text: o.textContent })) : undefined; }
  matches(sel) {
    if (sel !== ':popover-open') throw new Error('unsupported selector ' + sel);
    if (this.noPopoverSupport) throw new SyntaxError("':popover-open' is not a valid selector");
    return this.popover;
  }
  attachShadow(init) { const root = new FRoot(this); if (init.mode === 'open') this.shadowRoot = root; return root; }
}
function matchesSel(el, sel) {
  return sel.split(',').some((p) => {
    const s = p.trim();
    const attr = /^\[([\w-]+)\]$/.exec(s);
    return attr ? Object.prototype.hasOwnProperty.call(el.attrs, attr[1]) : el.tagName === s.toUpperCase();
  });
}
const up = (e) => e.parentElement || (e.parentNode && e.parentNode.host) || null;
function computed(el, pseudo) {
  if (pseudo) return { content: el.style[pseudo] || 'none', display: 'inline' };
  let vis = 'visible';
  for (let e = el; e; e = up(e)) if (e.style.visibility) { vis = e.style.visibility; break; }
  return { visibility: vis, opacity: el.style.opacity || '1', display: el.style.display || 'block' };
}

const h = (tag, a, ...kids) => new FEl(tag, a).append(...kids);
const t = (s, rect) => new FText(s, rect);
const R = (x, y, w, hh) => [x, y, w, hh];

// Mijn NS's travel history, simplified: a header component with an open shadow root (navigation
// labels and the account e-mail), a card picker, the list with the day heading, the trip and the
// next trip, and extra content per scenario.
function page(opts) {
  opts = opts || {};
  const header = h('mns-header', { rect: R(0, 0, 1440, 80) });
  header.attachShadow({ mode: 'open' }).append(
    h('a', { rect: R(10, 10, 80, 20) }, t('Mijn NS', R(10, 10, 80, 20))),
    h('a', { rect: R(100, 10, 80, 20) }, t('Uitloggen', R(100, 10, 80, 20))),
    h('span', { rect: R(200, 10, 90, 20) }, t('Ingelogd met:', R(200, 10, 90, 20))),
    h('span', { rect: R(300, 10, 160, 20) }, t('jan.jansen@example.nl', R(300, 10, 160, 20))));
  const picker = h('select', { rect: R(10, 100, 200, 30), selectedIndex: 0 }, h('option', {}, 'OV J. Jansen'));
  const trip = h('div', { attrs: { tabindex: '0', id: 'trip' }, rect: R(20, 250, 800, 100) }, t('Utrecht Centraal 08:01', R(30, 260, 200, 20)));
  const next = h('div', { attrs: { tabindex: '0', id: 'next' }, rect: R(20, 360, 800, 100) }, t('Amersfoort Centraal 17:10', R(30, 370, 220, 20)));
  const rows = opts.shared
    ? [h('div', { rect: R(20, 250, 800, 210) }, trip, next)]
    : [h('div', { rect: R(20, 250, 800, 100) }, trip), h('div', { rect: R(20, 360, 800, 100) }, next)];
  const list = h('mns-travel-history-list', { rect: R(0, 200, 1440, 600) },
    h('nes-heading', { rect: R(20, 210, 600, 30) }, t('12 november', R(20, 210, 120, 30))), ...rows);
  const extra = [];
  if (opts.overlap) extra.push(h('span', { rect: R(31, 285, 90, 15) }, t('Private fixture', R(31, 285, 90, 15))));
  if (opts.iframe) extra.push(h('iframe', { rect: R(900, 100, 300, 150) }));
  if (opts.tinyFrame) extra.push(h('iframe', { rect: R(0, 0, 1, 1) }));
  if (opts.dialog) extra.push(h('dialog', { rect: R(900, 500, 300, 200), open: true }, t('Weet je het zeker?', R(910, 510, 150, 20))));
  if (opts.popover) extra.push(h('div', { rect: R(900, 500, 300, 200), popover: true }));
  if (opts.closedShadow) { const w = h('ad-widget', { rect: R(900, 900, 200, 50) }); w.attachShadow({ mode: 'closed' }); extra.push(w); }
  if (opts.extras) {
    extra.push(
      h('span', { rect: R(900, 1000, 100, 20), style: { opacity: '0' } }, t('Jan Jansen', R(900, 1000, 100, 20))),
      h('span', { rect: R(900, 1050, 100, 20), style: { display: 'none' } }, t('geheim', R(900, 1050, 100, 20))),
      h('div', { rect: R(900, 1100, 150, 20), style: { '::before': '"J. Jansen"' } }),
      h('div', { rect: R(900, 1150, 30, 20), style: { '::before': '"\\e900"' } }),
      h('div', { rect: R(900, 1200, 30, 20), noPopoverSupport: true }));
  }
  const body = h('body', { rect: R(0, 0, 1440, 2000), style: opts.bodyPseudo ? { '::after': '"Jan Jansen"' } : {} }, header, picker, list, ...extra);
  const html = h('html', { rect: R(0, 0, 1440, 2000) }, body);
  const doc = new FRoot(null).append(html);
  doc.body = body;
  doc.documentElement = html;
  html.scrollWidth = 1440;
  html.scrollHeight = 2000;
  doc.createRange = () => ({
    selectNodeContents(n) { this.n = n; },
    getClientRects() {
      const r = this.n.rect || (this.n.parentNode && this.n.parentNode.rect);
      return r ? [{ left: r[0] - SX, top: r[1] - SY, width: r[2], height: r[3] }] : [];
    },
  });
  return { doc, trip, targets: {
    email: R(300, 10, 160, 20), nav: [R(10, 10, 80, 20), R(100, 10, 80, 20), R(200, 10, 90, 20)], select: R(10, 100, 200, 30),
    next: R(20, 360, 800, 100), heading: R(20, 210, 600, 30), trip: R(20, 250, 800, 100), closed: R(900, 900, 200, 50),
    faded: R(900, 1000, 100, 20), hidden: R(900, 1050, 100, 20), generated: R(900, 1100, 150, 20), icon: R(900, 1150, 30, 20),
  } };
}

function measure(opts) {
  const p = page(opts);
  globalThis.document = p.doc;
  const out = new Function('row', MEASURE)(p.trip);
  const inside = (r, b) => r[0] >= b.x - 0.01 && r[1] >= b.y - 0.01 && r[0] + r[2] <= b.x + b.w + 0.01 && r[1] + r[3] <= b.y + b.h + 0.01;
  const touches = (r, b) => r[0] < b.x + b.w && b.x < r[0] + r[2] && r[1] < b.y + b.h && b.y < r[1] + r[3];
  const covered = (r) => out.hide.some((b) => inside(r, b));
  const touched = (r) => out.hide.some((b) => touches(r, b));
  const tg = p.targets;
  return {
    fail: out.fail, shared: out.shared, heading: out.heading, keep: out.keep, w: out.w, h: out.h,
    email: covered(tg.email), nav_touched: tg.nav.some(touched), select: covered(tg.select), next: covered(tg.next),
    trip_touched: touched(tg.trip) || touched(tg.heading), closed: covered(tg.closed), faded: covered(tg.faded),
    hidden: touched(tg.hidden), generated: covered(tg.generated), icon: touched(tg.icon),
  };
}

function verifyOverlay(mode) {
  const r = { x: 300, y: 10, w: 160, h: 20 };
  const block = { style: { opacity: mode === 'transparent' ? '0' : '1' },
    getBoundingClientRect: () => ({ left: r.x - SX, top: r.y - SY, width: r.w, height: r.h }) };
  const root = { children: [block], style: {} };
  globalThis.document = { getElementById: () => mode === 'missing' ? null : root };
  globalThis.getComputedStyle = e => ({ display: 'block', visibility: 'visible', opacity: e.style.opacity || '1',
    backgroundColor: 'rgb(214, 214, 214)' });
  return new Function('R', VERIFY)([r]);
}

function runScenarios() {
  SX = 0; SY = 50;
  Object.assign(globalThis, { scrollX: SX, scrollY: SY, innerWidth: 1440, innerHeight: 1000, getComputedStyle: computed });
  const result = {
    base: measure({ extras: true }),
    shared: measure({ shared: true }),
    overlap: measure({ overlap: true }),
    iframe: measure({ iframe: true }),
    tiny_frame: measure({ tinyFrame: true }),
    dialog: measure({ dialog: true }),
    popover: measure({ popover: true }),
    closed_shadow: measure({ closedShadow: true }),
    pseudo_over_trip: measure({ bodyPseudo: true }),
    drawn: verifyOverlay('visible'),
    missing_overlay: verifyOverlay('missing'),
    transparent_overlay: verifyOverlay('transparent'),
  };
  process.stdout.write(JSON.stringify(result));
}
