// Print cookie METADATA (name, domain, expiry, flags) for NS domains — never values.
// Also optionally tests whether a saved-and-restored cookie jar keeps the session:
//   node tools/investigate/cookies.mjs meta
//   node tools/investigate/cookies.mjs probe     -> is the Mijn NS API session valid right now?
const PORT = Number(process.env.CDP_PORT || 9333);
const [cmd = 'meta'] = process.argv.slice(2);

const pages = (await (await fetch(`http://127.0.0.1:${PORT}/json`)).json()).filter((t) => t.type === 'page');
const ws = new WebSocket(pages[0].webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0;
const pending = new Map();
ws.onmessage = (e) => { const m = JSON.parse(e.data); pending.get(m.id)?.(m.result ?? m.error); };
const send = (method, params = {}) => new Promise((r) => { pending.set(++id, r); ws.send(JSON.stringify({ id, method, params })); });

if (cmd === 'meta') {
  const { cookies } = await send('Network.getAllCookies');
  const now = Date.now() / 1000;
  const rows = cookies
    .filter((c) => /ns\.nl$/.test(c.domain))
    .map((c) => ({
      name: c.name.replace(/\d{6,}/g, '<N>'),
      domain: c.domain,
      lifetime: c.session || c.expires < 0 ? 'SESSION' : `${((c.expires - now) / 3600).toFixed(1)}h`,
      httpOnly: c.httpOnly,
      secure: c.secure,
      sameSite: c.sameSite || '',
    }))
    .sort((a, b) => a.domain.localeCompare(b.domain));
  console.table(rows);
} else if (cmd === 'probe') {
  const r = await send('Runtime.evaluate', { expression: `({url: location.href, loggedIn: /Uitloggen/.test(document.body.innerText), t: new Date().toISOString()})`, returnByValue: true });
  console.log(r.result.value);
}
ws.close();
