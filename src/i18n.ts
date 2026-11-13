import { invoke } from '@tauri-apps/api/core';
import { escapeHtml as h, isEmail, isUnsupportedMail, mailPreset, placePopover, unsupportedMailText, type Lang } from './format';

let lang: Lang = ((): Lang => {
  const sys: Lang = (navigator.language || 'nl').toLowerCase().startsWith('nl') ? 'nl' : 'en';
  try { const saved = localStorage.getItem('lang'); return saved === 'nl' || saved === 'en' ? saved : sys; } catch { return sys; }
})();

export const getLang = () => lang;
export function setLang(l: Lang) {
  lang = l;
  document.documentElement.lang = l;
  try { localStorage.setItem('lang', l); } catch { /* storage unavailable */ }
  syncLang();
}
/** E-mails and Windows notifications (also from scheduled runs) use the app's language. */
export function syncLang() {
  document.documentElement.lang = lang;
  invoke('set_language', { lang }).catch(() => { /* older backend or not ready yet: harmless */ });
}
/** Inline bilingual string: Dutch first. */
export const L = (nl: string, en: string) => (lang === 'nl' ? nl : en);

// Tiny inline flags (Windows does not render flag emoji).
export const FLAG_NL = '<svg class="flag" viewBox="0 0 9 6" aria-hidden="true"><rect width="9" height="2" fill="#AE1C28"/><rect y="2" width="9" height="2" fill="#fff"/><rect y="4" width="9" height="2" fill="#21468B"/></svg>';
export const FLAG_UK = '<svg class="flag" viewBox="0 0 60 30" aria-hidden="true"><clipPath id="ukc"><path d="M30,15 h30 v15 z v15 h-30 z h-30 v-15 z v-15 h30 z"/></clipPath><path d="M0,0 v30 h60 v-30 z" fill="#012169"/><path d="M0,0 L60,30 M60,0 L0,30" stroke="#fff" stroke-width="6"/><path d="M0,0 L60,30 M60,0 L0,30" clip-path="url(#ukc)" stroke="#C8102E" stroke-width="4"/><path d="M30,0 v30 M0,15 h60" stroke="#fff" stroke-width="10"/><path d="M30,0 v30 M0,15 h60" stroke="#C8102E" stroke-width="6"/></svg>';

/** One language toggle: shows the current language; a click switches to the other. */
export const langSwitch = () => {
  const nl = getLang() === 'nl';
  return `<button class="fb lang" data-setlang="${nl ? 'en' : 'nl'}" title="${nl ? 'Switch to English' : 'Wissel naar Nederlands'}" aria-label="${nl ? 'Taal: Nederlands. Switch to English' : 'Language: English. Wissel naar Nederlands'}">${nl ? FLAG_UK : FLAG_NL}${nl ? 'EN' : 'NL'}</button>`;
};

/** Backend messages are "Nederlands / English"; show the half for the current language. */
export function friendlyError(e: unknown): string {
  const s = String(e);
  if (/^invalid args|invalid type|invalid value|missing required key/.test(s)) return L('Ongeldige invoer. Controleer de velden.', 'Invalid input. Please check the fields.');
  return pickLang(s);
}
/** "Nederlands / English" -> the half for the current language; other text unchanged. */
export function pickLang(s: string): string {
  const m = String(s ?? '').match(/^(.*?) \/ (.*)$/s);
  return m ? (getLang() === 'nl' ? m[1] : m[2]) : String(s ?? '');
}

// ---------- shared UI pieces (main screen and wizard) ----------
export const reducedMotion = () => window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;

/** Simple outline icons (stroke = currentColor). */
export const ICON = {
  close: '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>',
  gear: '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/></svg>',
  warn: '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l9.5 17h-19z"/><path d="M12 10v4.5M12 17.5v.01"/></svg>',
  edit: '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 20h4L19 9l-4-4L4 16z"/></svg>',
  more: '<svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor" aria-hidden="true"><circle cx="5" cy="12" r="1.7"/><circle cx="12" cy="12" r="1.7"/><circle cx="19" cy="12" r="1.7"/></svg>',
  ext: '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/></svg>',
};

/** The header of every dialog: logo mark, title and a close button (data-x="close"). */
export const dialogHead = (title: string, id = '') =>
  `<div class="hd"><img class="hd-logo" src="/logo-mark.png" alt="" width="24" height="24"><span class="hd-t"${id ? ` id="${id}"` : ''}>${title}</span><button type="button" class="x" data-x="close" aria-label="${h(L('Sluiten', 'Close'))}">${ICON.close}</button></div>`;

// ---------- "?" help popovers: hover, click/tap or Enter; Esc or a click elsewhere closes ----------
let tip: { btn: HTMLElement; pop: HTMLElement; pinned: boolean } | null = null;
let tipTimer = 0;
function hideTip() {
  clearTimeout(tipTimer);
  if (!tip) return;
  const { btn, pop } = tip; tip = null;
  try { if (pop.matches(':popover-open')) pop.hidePopover(); } catch { /* popover unsupported */ }
  pop.classList.remove('open');
  btn.setAttribute('aria-expanded', 'false');
}
function showTip(btn: HTMLElement, pinned: boolean) {
  clearTimeout(tipTimer);
  if (tip && tip.btn === btn && tip.pop.isConnected) { if (pinned) tip.pinned = true; return; }
  hideTip();
  const pop = document.getElementById(btn.getAttribute('aria-controls') ?? '');
  if (!pop) return;
  try { pop.showPopover(); } catch { /* popover unsupported: class fallback */ }
  pop.classList.add('open');
  btn.setAttribute('aria-expanded', 'true');
  const a = btn.getBoundingClientRect(), p = pop.getBoundingClientRect();
  const at = placePopover(a, { width: p.width, height: p.height }, { width: window.innerWidth, height: window.innerHeight });
  pop.style.left = `${at.left}px`; pop.style.top = `${at.top}px`;
  tip = { btn, pop, pinned };
}
const inTip = (n: EventTarget | null) => !!tip && n instanceof Node && (tip.btn.contains(n) || tip.pop.contains(n));
let tipsInstalled = false;
/** Wire up every ".qhelp" button (made by format.helpTip), once, for the whole document. */
export function installHelpTips() {
  if (tipsInstalled) return;
  tipsInstalled = true;
  // Hover opens a tip only on real pointer movement: a "?" that appears under a resting mouse (after a redraw
  // or step change) gets a pointerover without any movement and stays closed.
  let last = { x: NaN, y: NaN }, hoverBtn: HTMLElement | null = null;
  document.addEventListener('pointermove', (e) => {
    if (e.pointerType !== 'mouse') return;
    const moved = e.clientX !== last.x || e.clientY !== last.y;
    last = { x: e.clientX, y: e.clientY };
    if (!moved) return;
    const b = (e.target as Element | null)?.closest?.('.qhelp') as HTMLElement | null;
    if (b) { if (tip?.btn !== b && hoverBtn !== b) { clearTimeout(tipTimer); hoverBtn = b; tipTimer = window.setTimeout(() => { hoverBtn = null; showTip(b, false); }, 120); } return; }
    hoverBtn = null;
    if (inTip(e.target)) clearTimeout(tipTimer);
  });
  document.addEventListener('pointerover', (e) => {
    if (e.pointerType === 'mouse' && inTip(e.target)) clearTimeout(tipTimer);
  });
  document.addEventListener('pointerout', (e) => {
    if (e.pointerType !== 'mouse') return;
    const from = (e.target as Element | null)?.closest?.('.qhelp');
    if (from && tip?.btn !== from) { clearTimeout(tipTimer); hoverBtn = null; }
    if (tip && !tip.pinned && inTip(e.target) && !inTip(e.relatedTarget) && document.activeElement !== tip.btn) tipTimer = window.setTimeout(hideTip, 220);
  });
  document.addEventListener('focusout', (e) => {
    if (tip && !tip.pinned && inTip(e.target) && !inTip(e.relatedTarget)) hideTip();
  });
  document.addEventListener('click', (e) => {
    const t = e.target as Element | null;
    const b = t?.closest?.('.qhelp') as HTMLElement | null;
    if (b) {
      e.preventDefault(); e.stopPropagation(); // never toggles a surrounding label or row
      if (tip?.btn === b && tip.pinned) hideTip(); else showTip(b, true);
      return;
    }
    if (t?.closest?.('.qpop')) { if (!t.closest('a')) e.preventDefault(); return; }
    if (tip) hideTip();
  }, true);
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || !tip) return;
    const back = tip.pop.contains(document.activeElement) ? tip.btn : null;
    e.preventDefault(); e.stopPropagation(); // Esc closes the tip, not the dialog behind it
    hideTip();
    back?.focus({ preventScroll: true });
  }, true);
  window.addEventListener('resize', hideTip);
  document.addEventListener('scroll', (e) => { if (tip && !tip.pop.contains(e.target as Node)) hideTip(); }, { capture: true, passive: true });
  // A redraw or dialog close removes the trigger; its top-layer popover must close too.
  new MutationObserver(() => { if (tip && (!tip.btn.isConnected || !tip.pop.isConnected || tip.btn.closest('dialog:not([open])'))) hideTip(); }).observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['open'] });
}

// ---------- keyboard focus inside dialogs ----------
/** Controls Tab can reach in `box`, in order: enabled, visible, not inside a closed section or popover. */
export function tabbables(box: ParentNode): HTMLElement[] {
  return [...box.querySelectorAll<HTMLElement>('button, input:not([type=hidden]), select, textarea, a[href], summary, [tabindex]:not([tabindex="-1"])')].filter((e) =>
    !(e as HTMLButtonElement).disabled && e.tabIndex >= 0 && !e.closest('[inert], [hidden]') && e.getClientRects().length > 0
    && !(e.parentElement?.closest('details:not([open])') && !(e.tagName === 'SUMMARY' && e.parentElement?.matches('details:not([open])')))
    && !e.closest('.qpop:not(.open)'));
}
/** Tab reaches the end of a radio group's own stop: the same group counts as one control. */
const sameStop = (a: Element | null, b: Element) => a === b || (a instanceof HTMLInputElement && b instanceof HTMLInputElement && a.type === 'radio' && b.type === 'radio' && !!a.name && a.name === b.name);
/** Tab past the last (or Shift+Tab before the first) enabled control wraps around: focus never drops to the page. */
export function trapTab(ev: KeyboardEvent, box: HTMLElement) {
  const all = tabbables(box);
  if (!all.length) return;
  const first = all[0], last = all[all.length - 1], at = document.activeElement;
  if (!at || !box.contains(at)) { ev.preventDefault(); (ev.shiftKey ? last : first).focus(); return; }
  if (ev.shiftKey ? sameStop(at, first) : sameStop(at, last)) { ev.preventDefault(); (ev.shiftKey ? last : first).focus(); }
}
let trapInstalled = false;
/** Every modal dialog keeps Tab inside itself (once, for the whole document). */
export function installDialogTrap() {
  if (trapInstalled) return;
  trapInstalled = true;
  document.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Tab' || ev.defaultPrevented || ev.altKey || ev.ctrlKey || ev.metaKey) return;
    const open = [...document.querySelectorAll<HTMLDialogElement>('dialog[open]')].filter((d) => { try { return d.matches(':modal'); } catch { return true; } });
    const top = open[open.length - 1];
    if (top) trapTab(ev, top);
  });
}

// ---------- in-app confirmation dialogs (instead of the browser's confirm()/prompt()) ----------
export interface AskOptions { title: string; body: string; ok: string; cancel?: string; danger?: boolean; /** Focus starts on Cancel (a choice that is easy to get wrong). */ focusCancel?: boolean }
function openAsk(o: AskOptions, extra: string, wire: (dlg: HTMLDialogElement, ok: HTMLButtonElement) => () => unknown): Promise<unknown> {
  return new Promise((resolve) => {
    const back = document.activeElement as HTMLElement | null;
    const dlg = document.createElement('dialog');
    dlg.className = o.danger ? 'ask danger' : 'ask';
    dlg.setAttribute('aria-labelledby', 'askT');
    dlg.innerHTML = `${dialogHead(h(o.title), 'askT')}<div class="bd"><p>${o.body}</p>${extra}</div><div class="ft"><span class="grow"></span><button class="btn" data-x="no">${h(o.cancel ?? L('Annuleren', 'Cancel'))}</button><button class="btn ${o.danger ? 'danger-solid' : 'primary'}" data-x="yes">${h(o.ok)}</button></div>`;
    document.body.appendChild(dlg);
    const ok = dlg.querySelector('[data-x=yes]') as HTMLButtonElement;
    const value = wire(dlg, ok);
    let answer: unknown = null;
    ok.onclick = () => { if (ok.disabled) return; answer = value(); dlg.close(); };
    dlg.querySelectorAll<HTMLButtonElement>('[data-x=no],[data-x=close]').forEach((b) => { b.onclick = () => dlg.close(); });
    dlg.addEventListener('close', () => { dlg.remove(); if (back?.isConnected) back.focus({ preventScroll: true }); resolve(answer); });
    dlg.showModal();
    ((dlg.querySelector('input') as HTMLElement | null) ?? (o.danger || o.focusCancel ? dlg.querySelector('[data-x=no]') as HTMLElement : ok)).focus();
  });
}
/** Yes/no question in the app's own style. Resolves true only on the confirm button. */
export async function askConfirm(o: AskOptions): Promise<boolean> {
  return (await openAsk(o, '', () => () => true)) === true;
}
/** Risky erase actions: the user types the confirmation phrase (ALLES WISSEN / ERASE ALL for a full erase). Resolves the typed word, or null. */
export async function askWord(o: AskOptions & { all?: boolean }): Promise<string | null> {
  const words = o.all ? ['ALLES WISSEN', 'ERASE ALL'] : ['WISSEN', 'ERASE'];
  const word = words[getLang() === 'nl' ? 0 : 1];
  const r = await openAsk(o, `<label class="field-l">${L(`Typ ${word} om te bevestigen`, `Type ${word} to confirm`)}<input class="in" id="askWord" autocomplete="off" spellcheck="false"></label>`, (dlg, ok) => {
    const inp = dlg.querySelector('#askWord') as HTMLInputElement;
    const valid = () => words.includes(inp.value.trim().toUpperCase());
    ok.disabled = true;
    inp.addEventListener('input', () => { ok.disabled = !valid(); });
    inp.addEventListener('keydown', (e) => { if (e.key === 'Enter' && valid()) { e.preventDefault(); ok.click(); } });
    return () => inp.value.trim().toUpperCase();
  });
  return typeof r === 'string' && r ? r : null;
}

// ---------- e-mail set-up (sidebar form and wizard) ----------
/** Label for the password field: an app password, or the normal mailbox password for providers without app passwords. */
export function mailPwLabel(email: string): string {
  const p = mailPreset(email);
  return p && !p.passwordHelpUrl && !p.unsupported ? L('Wachtwoord van je mailbox', 'Mailbox password') : L('App-wachtwoord', 'App password');
}
/** Numbered, provider-specific steps for the password. `openAttr` makes the "Open the page" button work in its container. */
export function mailSteps(email: string, openAttr: string): string {
  if (!email.includes('@')) return '';
  if (!isEmail(email)) return `<p class="msg bad">${L('Dit e-mailadres klopt niet. Controleer het adres.', 'This e-mail address is not valid. Check the address.')}</p>`;
  if (isUnsupportedMail(email)) return `<p class="msg bad">${unsupportedMailText(getLang())}</p>`;
  const p = mailPreset(email);
  if (!p) return `<p class="msg">${L('Deze mailprovider herkennen we niet. Sla e-mail hier over en stel de server later in bij Jouw gegevens → E-mail voor meldingen → Geavanceerd.', 'We don’t recognise this mail provider. Skip e-mail here and set up the server later under Your details → E-mail for notifications → Advanced.')}</p>`;
  const link = p.passwordHelpUrl ? `<button type="button" class="btn small" ${openAttr} data-url="${h(p.passwordHelpUrl)}">${L('Open de pagina', 'Open the page')} ${ICON.ext}</button>` : '';
  let steps: string[];
  if (p.name === 'Gmail') steps = [
    L(`Klik op ${link}. Je Google-account opent in je browser.`, `Click ${link}. Your Google account opens in your browser.`),
    L('Log in. Vraagt Google om <b>tweestapsverificatie</b>? Zet die dan aan. Zonder kan Google geen app-wachtwoord maken.', 'Sign in. If Google asks for <b>2-Step Verification</b>, turn it on. Without it, Google can’t create an app password.'),
    L('Typ als naam <b>OV Refund</b> en klik op <b>Maken</b>.', 'Type <b>OV Refund</b> as the name and click <b>Create</b>.'),
    L('Kopieer de code van 16 letters en plak hem hieronder.', 'Copy the 16-letter code and paste it below.'),
  ];
  else if (p.passwordHelpUrl) steps = [
    L(`Klik op ${link} en log in bij ${h(p.name)}.`, `Click ${link} and sign in to ${h(p.name)}.`),
    L('Maak een app-wachtwoord met de naam <b>OV Refund</b>.', 'Create an app password named <b>OV Refund</b>.'),
    L('Kopieer het en plak het hieronder.', 'Copy it and paste it below.'),
  ];
  else steps = [L(`Bij ${h(p.name)} gebruik je het gewone wachtwoord van je mailbox. Vul het hieronder in.`, `With ${h(p.name)} you use your normal mailbox password. Enter it below.`)];
  return `<div class="steps"><ol>${steps.map((x) => `<li>${x}</li>`).join('')}</ol>${p.note && !p.passwordHelpUrl ? `<p class="msg">${h(L(...p.note))}</p>` : ''}</div>`;
}
