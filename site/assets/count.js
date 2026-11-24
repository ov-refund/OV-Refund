// Anonymous visit counter (GoatCounter): one count per page view and per download click.
// No cookies, no local storage of visitor data, no personal data; the referrer is the only context sent.
// Not counted: browsers with "Do Not Track", local copies of the site, and the maintainer's own
// browser after opening any page once with #nocount (stored in this browser only; #count undoes it).
const GC = 'https://ovrefund.goatcounter.com/count';
const store = {
  get: (k) => { try { return localStorage.getItem(k); } catch { return null; } },
  set: (k, v) => { try { if (v === null) localStorage.removeItem(k); else localStorage.setItem(k, v); } catch { /* ignore */ } },
};
if (location.hash === '#nocount') store.set('ovr-nocount', '1');
if (location.hash === '#count') store.set('ovr-nocount', null);
const off = location.protocol === 'file:'
  || /^(localhost|127\.|\[::1\])/.test(location.hostname)
  || navigator.doNotTrack === '1' || window.doNotTrack === '1'
  || store.get('ovr-nocount') === '1';

function send(path, title, event) {
  if (off) return;
  const q = new URLSearchParams({ p: path, t: title, r: event ? '' : document.referrer, e: event ? 'true' : 'false', rnd: Math.random().toString(36).slice(2) });
  const url = `${GC}?${q}`;
  try {
    if (navigator.sendBeacon && navigator.sendBeacon(url)) return;
  } catch { /* fall through */ }
  fetch(url, { mode: 'no-cors', keepalive: true, credentials: 'omit' }).catch(() => {});
}

send(location.pathname, document.title, false);

// Download clicks: any link to a release file ("download-<file>" or "download-<file>-<version>").
document.addEventListener('click', (ev) => {
  const a = ev.target instanceof Element ? ev.target.closest('a[href]') : null;
  if (!a) return;
  const m = a.href.match(/\/releases\/(?:latest\/download|download\/([^/]+))\/([^/?#]+)$/);
  if (m) send(`download-${m[2]}${m[1] ? `-${m[1]}` : ''}`, 'Download', true);
}, true);
