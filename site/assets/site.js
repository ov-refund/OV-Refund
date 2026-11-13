// OV Refund website: language + theme toggles (remembered), latest version, release list.
import { REPO, ASSET } from './config.js';

const root = document.documentElement;
const store = { get: (k) => { try { return localStorage.getItem(k); } catch { return null; } }, set: (k, v) => { try { localStorage.setItem(k, v); } catch { /* ignore */ } } };

// ---------- language ----------
const lang0 = store.get('lang');
root.dataset.lang = lang0 === 'en' || lang0 === 'nl' ? lang0 : (navigator.language || 'nl').toLowerCase().startsWith('nl') ? 'nl' : 'en';
root.lang = root.dataset.lang;
const FLAG_NL = '<svg class="flag" viewBox="0 0 9 6" aria-hidden="true"><rect width="9" height="2" fill="#AE1C28"/><rect y="2" width="9" height="2" fill="#fff"/><rect y="4" width="9" height="2" fill="#21468B"/></svg>';
const FLAG_UK = '<svg class="flag" viewBox="0 0 60 30" aria-hidden="true"><clipPath id="ukc"><path d="M30,15 h30 v15 z v15 h-30 z h-30 v-15 z v-15 h30 z"/></clipPath><path d="M0,0 v30 h60 v-30 z" fill="#012169"/><path d="M0,0 L60,30 M60,0 L0,30" stroke="#fff" stroke-width="6"/><path d="M0,0 L60,30 M60,0 L0,30" clip-path="url(#ukc)" stroke="#C8102E" stroke-width="4"/><path d="M30,0 v30 M0,15 h60" stroke="#fff" stroke-width="10"/><path d="M30,0 v30 M0,15 h60" stroke="#C8102E" stroke-width="6"/></svg>';

// ---------- theme ----------
// theme-init.js (in <head>) already set data-theme: your choice, else the browser/Windows setting, else light.
if (root.dataset.theme !== 'dark' && root.dataset.theme !== 'light') root.dataset.theme = 'light';
const isDark = () => root.dataset.theme === 'dark';
document.addEventListener('themechange', () => drawToggles());
const SUN = '<svg class="ic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>';
const MOON = '<svg class="ic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></svg>';

function drawToggles() {
  const box = document.getElementById('toggles');
  if (!box) return;
  const nl = root.dataset.lang === 'nl';
  const themeLabel = isDark() ? (nl ? 'Licht thema' : 'Light theme') : (nl ? 'Donker thema' : 'Dark theme');
  box.innerHTML = `<button class="fb" id="tgTheme" title="${themeLabel}" aria-label="${themeLabel}">${isDark() ? SUN : MOON}</button><span class="sep"></span>`
    + `<button class="fb" id="tgLang" title="${nl ? 'Switch to English' : 'Wissel naar Nederlands'}" aria-label="${nl ? 'Taal: Nederlands. Switch to English' : 'Language: English. Wissel naar Nederlands'}">${nl ? FLAG_UK : FLAG_NL}${nl ? 'EN' : 'NL'}</button>`;
  document.getElementById('tgTheme').onclick = () => { root.dataset.theme = isDark() ? 'light' : 'dark'; store.set('theme', root.dataset.theme); drawToggles(); };
  document.getElementById('tgLang').onclick = () => { root.dataset.lang = nl ? 'en' : 'nl'; root.lang = root.dataset.lang; store.set('lang', root.dataset.lang); drawToggles(); syncPageText(); drawReleases(); };
}
drawToggles();

const PAGE_TEXT = {
  'index.html': ['geld terug bij vertraging', 'refunds for delays', 'OV Refund vindt vertraagde NS-reizen en helpt je geld terug te vragen.', 'OV Refund finds delayed NS trips and helps you claim a refund.'],
  'instructions.html': ['instructies', 'instructions', 'OV Refund instellen en een claim indienen, stap voor stap.', 'Set up OV Refund and submit a claim, step by step.'],
  'hulp.html': ['hulp en vragen', 'help and questions', 'Antwoorden over installeren, reizen, instellingen en privacy.', 'Answers about installation, trips, settings and privacy.'],
  'disclaimer.html': ['disclaimer en privacy', 'disclaimer and privacy', 'Je gegevens, bugmeldingen en aansprakelijkheid bij OV Refund.', 'Your data, bug reports and liability when using OV Refund.'],
  'versions.html': ['versies', 'versions', 'Versies en downloads van OV Refund.', 'OV Refund releases and downloads.'],
};
function syncPageText() {
  const page = PAGE_TEXT[window.location.pathname.split('/').pop() || 'index.html'];
  if (!page) return;
  const en = root.dataset.lang === 'en';
  document.title = `OV Refund — ${page[en ? 1 : 0]}`;
  let description = document.querySelector('meta[name="description"]');
  if (!description) { description = document.createElement('meta'); description.name = 'description'; document.head.appendChild(description); }
  description.content = page[en ? 3 : 2];
}
syncPageText();

// ---------- downloads & versions (GitHub Releases) ----------
const latestDownload = `https://github.com/${REPO}/releases/latest/download/${ASSET}`;
document.querySelectorAll('[data-download]').forEach((a) => { a.href = latestDownload; });
document.querySelectorAll('[data-repo-link]').forEach((a) => { a.href = `https://github.com/${REPO}`; });
document.querySelectorAll('[data-issues-link]').forEach((a) => { a.href = `https://github.com/${REPO}/issues`; });

let releases = null;
async function loadReleases() {
  try {
    const r = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=30`, { headers: { Accept: 'application/vnd.github+json' } });
    if (!r.ok) throw new Error(String(r.status));
    releases = (await r.json()).filter((x) => !x.draft);
  } catch { releases = []; }
}
const fmtDate = (iso) => new Date(iso).toLocaleDateString(root.dataset.lang === 'nl' ? 'nl-NL' : 'en-GB', { day: 'numeric', month: 'long', year: 'numeric' });
const text = (s) => { const d = document.createElement('div'); d.textContent = s ?? ''; return d.innerHTML; };
/** Release notes are written "Nederlands / English" (as in the app): show the half for the page language.
 *  When every line is split like that, each line is split; otherwise the whole text at the first " / ". */
const pickLang = (s, nl) => {
  const t = String(s ?? '');
  const lines = t.split(/\r?\n/);
  const half = (x) => { const m = x.match(/^(.*?) \/ (.*)$/s); return m ? (nl ? m[1] : m[2]) : x; };
  if (lines.filter((l) => l.trim()).every((l) => l.includes(' / '))) return lines.map(half).join('\n');
  return half(t);
};

const mb = (bytes, nl) => `${(bytes / 1048576).toFixed(1).replace('.', nl ? ',' : '.')} MB`;
function drawReleases() {
  if (!releases) return;
  const nl = root.dataset.lang === 'nl';
  const latest = releases.find((x) => !x.prerelease);
  document.querySelectorAll('[data-latest-version]').forEach((el) => {
    const asset = latest?.assets?.find((a) => a.name === ASSET);
    const size = asset ? ` · ${mb(asset.size, nl)}` : '';
    const win = nl ? 'Windows 10 en 11' : 'Windows 10 and 11';
    if (latest) el.textContent = `${nl ? 'Versie' : 'Version'} ${latest.tag_name.replace(/^v/, '')} · ${win}${size}`;
  });
  const list = document.getElementById('releases');
  if (!list) return;
  if (!releases.length) {
    list.innerHTML = `<div class="note">${nl ? 'De versielijst kon niet worden geladen. Bekijk alle versies op' : 'The version list could not be loaded. See all versions on'} <a data-repo-link href="https://github.com/${text(REPO)}/releases">GitHub</a>.</div>`;
    return;
  }
  list.innerHTML = releases.map((x, i) => {
    const asset = (x.assets || []).find((a) => a.name === ASSET);
    return `<article class="release ${i === 0 && !x.prerelease ? 'latest' : ''}">
      <div class="row"><h3>${text(x.tag_name.replace(/^v/, nl ? 'Versie ' : 'Version '))}</h3>
        ${i === 0 && !x.prerelease ? `<span class="badge">${nl ? 'Nieuwste' : 'Latest'}</span>` : ''}${x.prerelease ? `<span class="badge">${nl ? 'Testversie' : 'Pre-release'}</span>` : ''}
        <span class="meta">${fmtDate(x.published_at)}</span>
        ${asset ? `<a class="btn dl" href="${text(asset.browser_download_url)}">Download (${mb(asset.size, nl)})</a>` : ''}</div>
      ${x.body ? `<div class="notes">${text(pickLang(x.body, nl))}</div>` : ''}
    </article>`;
  }).join('');
}
loadReleases().then(drawReleases);

// ---------- instructions: the bar of the step you are reading turns yellow ----------
const guideSteps = [...document.querySelectorAll('.steps .istep')];
if (guideSteps.length && 'IntersectionObserver' in window) {
  const io = new IntersectionObserver((entries) => {
    entries.forEach((e) => { if (e.isIntersecting) guideSteps.forEach((s) => s.classList.toggle('current', s === e.target)); });
  }, { rootMargin: '-45% 0px -45% 0px' }); // a thin band across the middle of the screen
  guideSteps.forEach((s) => io.observe(s));
}

// ---------- closing line: a random train pun per visit ----------
const PUNS = [
  ['Laat je geld niet op het perron staan.', "Don't leave your money on the platform."],
  ['Mis je trein, niet je geld.', 'Miss the train, not the refund.'],
  ['Volgende halte: je eigen rekening.', 'Next stop: your bank account.'],
  ['Zet je geld weer op het juiste spoor.', 'Get your money back on track.'],
  ['Je geld heeft al genoeg vertraging opgelopen.', 'Your money has been delayed long enough.'],
  ['Laat je geld niet zonder jou vertrekken.', "Don't let your refund depart without you."],
];
{
  const pun = PUNS[Math.floor(Math.random() * PUNS.length)];
  const nlH = document.querySelector('[data-pun=nl]'), enH = document.querySelector('[data-pun=en]');
  if (nlH && enH) { nlH.textContent = pun[0]; enH.textContent = pun[1]; }
}

// ---------- instructions: yellow labels on the screenshots (HTML, so they may stick out past the image) ----------
if (document.querySelector('figure.shot')) {
  import('./shots/hints.js').then(({ default: HINTS }) => {
    const labels = [];
    for (const fig of document.querySelectorAll('figure.shot')) {
      const name = (fig.querySelector('img')?.getAttribute('src') || '').split('/').pop().replace(/\.webp$/, '');
      for (const lang of ['nl', 'en']) {
        const h = HINTS[lang]?.[name]; if (!h) continue;
        const el = document.createElement('span');
        el.className = `ihint ${h.side}`; el.lang = lang; el.setAttribute('aria-hidden', 'true'); el.textContent = h.text;
        el.style.setProperty('left', h.x + '%'); el.style.setProperty('top', h.y + '%');
        el.dataset.side = h.side; el.dataset.text = h.text;
        fig.appendChild(el); labels.push(el);
      }
    }
    // a long label on a small screenshot would hide the content: use a short one there
    const SHORT = { nl: 'Klik hier', en: 'Click here' };
    // a label that would run off the screen (small windows) points down from above instead
    const fit = () => labels.forEach((el) => {
      if (!el.offsetParent) return;
      el.className = `ihint ${el.dataset.side}`; el.style.removeProperty('margin-left'); el.textContent = el.dataset.text;
      const fw = el.parentElement.clientWidth;
      if (el.dataset.side !== 'u' && fw < 800 && el.offsetWidth > fw * 0.15 && el.dataset.text.length > SHORT[el.lang].length) el.textContent = SHORT[el.lang];
      const vw = document.documentElement.clientWidth;
      let r = el.getBoundingClientRect();
      if (r.left < 4 || r.right > vw - 4) { el.className = 'ihint u'; r = el.getBoundingClientRect(); }
      if (r.right > vw - 4) el.style.setProperty('margin-left', `${vw - 4 - r.right}px`);
      else if (r.left < 4) el.style.setProperty('margin-left', `${4 - r.left}px`);
    });
    fit(); addEventListener('resize', fit); document.getElementById('tgLang')?.addEventListener('click', () => setTimeout(fit));
  }).catch(() => {});
}
