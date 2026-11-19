// First-start wizard: asks for one thing at a time. The "how to submit" walkthrough is shown on its own (Settings → Help).
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { applyIbanFormat, BANK_NAME_MAX, backupTooLarge, backupTooLargeText, bankNameHint, checkIban, escapeHtml as h, helpTip, IBAN_MAXLENGTH, isEmail, isUnsupportedMail, mailPreset, restoreSummary, smtpLoginChanged, smtpPasswordAgainText, unsupportedMailText as unsupportedMailIn, validHolderName } from './format';
import { askConfirm, dialogHead, friendlyError, getLang, ICON, L, langSwitch, mailPwLabel, mailSteps, reducedMotion, setLang, trapTab } from './i18n';
import { themeButton, toggleTheme } from './theme';
import { automaticSubmitAllowed, wizardStepIds, validAutoLogin, validBank, validWizardMail } from './wizard-validation';

export interface WizardDeps {
  call: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  toast: (msg: string, kind?: '' | 'err') => void;
  done: () => Promise<void>; // closes the wizard and starts the first check
  onClose?: () => void; // restores the walkthrough’s invoking view
  hideToast?: () => void;
}
export interface WizardStart { loggedIn: boolean; nsEmail: string | null; step?: string | null; disclaimerAccepted?: boolean; otherService?: string | null; bankLast4?: string | null; bankName?: string | null }

type Step = 'welcome' | 'login' | 'services' | 'bank' | 'auto' | 'autologin' | 'notify' | 'finish' | 'howto';
/** The steps of this run: "automatic login" only matters when automatic checks are on. */
const steps = (): Step[] => wizardStepIds(st.autoChecks) as Step[];

/** Mijn NS (log in, create an account, link a card). www.ns.nl is on the open_external allowlist. */
export const NS_MIJNNS_URL = 'https://www.ns.nl/mijnns';
/** The public website (GitHub Pages of the public repository). */
export const WEBSITE_URL = 'https://ov-refund.github.io/OV-Refund/';

const st = {
  step: 'welcome' as Step,
  importDone: false,
  loggedIn: false,
  nsEmail: '',
  mailTo: '',
  accepted: false,
  autoLogin: null as boolean | null,
  autoChecks: true,
  interval: 7,
  autoSubmit: false,
  windowsNotify: true,
  otherService: null as boolean | null,
  howto: 0,
  /** Typed Mijn NS password, kept in memory only while the wizard is open (survives Yes -> No -> Yes). */
  nsPw: '',
  /** A Mijn NS password is already stored (for this e-mail address): Next may keep it. */
  nsPwSaved: false,
  nsSavedEmail: '',
  /** A bank account that is already stored (shown when the bank step is opened again). */
  bank: null as null | { last4: string; name: string },
};

let root: HTMLDivElement | null = null;
let deps: WizardDeps;
let helpSeq = 0;
const help = (body: string, topic = '') => helpTip(`wz${++helpSeq}`, body, topic ? `${L('Uitleg', 'Help')}: ${topic}` : L('Uitleg', 'Help'));

let unlistenLogin: (() => void) | null = null;
// While a wizard/how-to overlay is open, the page behind it is inert (no Tab, no clicks).
const setInert = (on: boolean) => { const a = document.getElementById('app'); if (a) a.inert = on; };
/** Step (and how-to page) last drawn: the panel only animates when this changes. */
let drawnKey = '';
/** A wizard action is running (guards against double clicks running it twice). */
let busy = false;
let savedMail: { smtp_password_set?: boolean; smtp?: any } = {};
/** Element that had focus before the how-to overlay opened; it gets focus back on close. */
let opener: HTMLElement | null = null;
/** First step shown in this session: the back-up restore offer is shown there. */
let firstStep: Step = 'welcome';
const closeRoot = () => {
  const el = root;
  root = null; drawnKey = ''; busy = false; unlistenLogin?.(); unlistenLogin = null; setInert(false);
  st.nsPw = '';
  document.documentElement.classList.remove('wz-open');
  if (el) {
    if (reducedMotion()) el.remove();
    else { el.classList.add('closing'); el.addEventListener('animationend', () => el.remove(), { once: true }); setTimeout(() => el.remove(), 400); }
  }
  if (opener?.isConnected) opener.focus({ preventScroll: true });
  deps.onClose?.();
  opener = null;
};

/** A flag the backend may not know yet (older build): never block the wizard on it. */
const quietFlag = (key: string, value: string) => invoke('set_flag', { key, value }).catch(() => { /* older backend: harmless */ });

export async function openWizard(d: WizardDeps, start: WizardStart) {
  deps = d;
  st.loggedIn = start.loggedIn;
  st.nsEmail = start.nsEmail ?? '';
  st.accepted = !!start.disclaimerAccepted;
  st.importDone = start.otherService === 'import_done';
  st.otherService = start.otherService === 'yes' || st.importDone ? true : start.otherService === 'none' ? false : null;
  st.bank = start.bankLast4 ? { last4: start.bankLast4, name: start.bankName ?? '' } : null;
  try { savedMail = await d.call<any>('get_settings') ?? {}; st.mailTo = savedMail.smtp?.to ?? ''; } catch { /* ignore */ }
  // A stored NS password (automatic login): the step shows it as saved and Next keeps it.
  try {
    const o = await invoke<any>('overview');
    st.nsPwSaved = !!o?.auto_login; st.nsSavedEmail = o?.account?.ns_email ?? st.nsEmail;
    if (st.nsPwSaved && st.autoLogin === null) st.autoLogin = true;
  } catch { /* older backend: ask as usual */ }
  // Resume where the user left off; the welcome page is skipped once its notice was accepted.
  const resume = wizardStepIds(true).includes(start.step as Step) ? (start.step as Step) : null;
  st.step = resume && (resume !== 'welcome' || !st.accepted) ? resume : st.accepted ? 'login' : 'welcome';
  if (st.step === 'autologin' && !st.autoChecks) st.step = 'notify';
  firstStep = st.step;
  root = document.createElement('div');
  root.className = 'wizard';
  document.body.appendChild(root);
  document.documentElement.classList.add('wz-open');
  setInert(true);
  root.addEventListener('click', onClick);
  root.addEventListener('input', onInput);
  root.addEventListener('change', onChange);
  root.addEventListener('toggle', updateGate, true);
  root.addEventListener('keydown', onKey);
  unlistenLogin?.();
  unlistenLogin = await listen('login-complete', async () => {
    st.loggedIn = false;
    deps.hideToast?.(); // the "log in using the NS window" hint is done
    try { const o = await deps.call<any>('overview'); st.loggedIn = o?.auth_state === 'ok'; st.nsEmail = o?.account?.ns_email ?? st.nsEmail; } catch { /* ignore */ }
    if (st.step === 'login' && st.loggedIn) go('services'); else draw();
  });
  draw();
}

function go(s: Step) {
  st.step = s;
  if (s !== 'howto') quietFlag('wizard_step', s);
  draw();
  // Focus can still drop to the page after the redraw (e.g. a late event or the clicked button going away):
  // check again once the step is on screen, so it lands on the new step's field or heading.
  setTimeout(focusFirst, 0);
}
const next = () => { const all = steps(); go(all[Math.min(all.indexOf(st.step) + 1, all.length - 1)]); };
const back = () => { const all = steps(); go(all[Math.max(all.indexOf(st.step) - 1, 0)]); };
const val = (id: string) => (root!.querySelector('#' + id) as HTMLInputElement | null)?.value ?? '';
const chk = (id: string) => (root!.querySelector('#' + id) as HTMLInputElement | null)?.checked ?? false;

function frame(title: string, body: string, actions: string, opts: { back?: boolean } = {}) {
  // Progress follows only the steps shown in this setup.
  const activeSteps = steps();
  const i = activeSteps.indexOf(st.step);
  const first = st.step === 'howto' || i === 0 || (st.step === 'login' && st.accepted && firstStep === 'login');
  return `<div class="wz-panel" role="dialog" aria-modal="true" aria-labelledby="wzTitle">
    <div class="wz-head"><img class="logo-img" src="/logo-mark.png" alt="OV Refund" width="40" height="40">
      ${st.step === 'howto' ? '<span class="grow"></span>' : `<div class="wz-progress" aria-hidden="true">${activeSteps.map((_, k) => `<span class="${k < i ? 'done' : k === i ? 'cur' : ''}"></span>`).join('')}</div><span class="sr-only">${L(`Stap ${i + 1} van ${activeSteps.length}`, `Step ${i + 1} of ${activeSteps.length}`)}</span>`}
      <div class="footbox wz-corner">${themeButton()}<span class="fb-sep"></span>${langSwitch()}</div></div>
    <h2 id="wzTitle" tabindex="-1">${title}</h2>
    <div class="wz-body">${body}</div>
    <p class="msg wz-gate" id="wzGate" role="status"></p><div class="wz-actions">${opts.back !== false && !first ? `<button class="btn ghost" data-wz="back">${L('Terug', 'Back')}</button>` : ''}<span class="grow"></span>${actions}</div>
    ${i > 0 && st.step !== 'finish' && st.step !== 'howto' ? `<div class="wz-skip"><button class="btn ghost small" data-wz="skipall">${L('Instellen overslaan', 'Skip setup')}</button></div>` : ''}
  </div>`;
}

/** Offer at the start of the wizard: restore a back-up made on another computer. */
const restoreOffer = () => `<div class="wz-restore"><span>${L('Al eerder gebruikt op een andere computer?', 'Used it on another computer before?')}</span>
  <button class="btn small" data-wz="restore">${L('Back-up terugzetten', 'Restore backup')}</button><input type="file" id="wzRestoreFile" accept=".ovrbackup" hidden></div>`;

/** A look-alike button in the example (not interactive). */
const fakeBtn = (text: string, primary = false) => `<span class="btn fake${primary ? ' primary' : ''}" aria-hidden="true">${text}</span>`;
function howtoSample(step: number): string {
  // A static example (clearly labelled) that mirrors the real list.
  const row = `<div class="wz-sample${step >= 1 ? ' hl' : ''}"><span class="d">${L('12 okt', '12 Oct')}</span><span class="r">Utrecht C. → Amsterdam C.</span><span class="late">+42 min</span><span class="amt">${L('€ 4,15', '€4.15')}</span>${step === 0 ? fakeBtn(L('Bekijken en indienen', 'Review and submit'), true) : ''}</div>`;
  const detail = step >= 1 ? `<div class="wz-detail">${L('Je kwam ongeveer 42 minuten later aan. Bij 30–59 minuten krijg je de helft van de ritprijs terug. NS beslist.', 'You arrived about 42 minutes late. For 30–59 minutes you get half the fare back. NS decides.')}</div>` : '';
  const dialog = step >= 2 ? `<div class="wz-mini"><b>${L('Claim indienen bij NS', 'Submit claim to NS')}</b>
      <span class="fake-check" aria-hidden="true"><span class="box${step >= 3 ? ' on' : ''}"></span>${L('Ik verklaar dat deze gegevens kloppen…', 'I declare that these details are correct…')}</span>
      <div class="wz-btns">${fakeBtn(L('Eerst proberen (verstuurt niets)', 'Try first (sends nothing)'))}${fakeBtn(L('Indienen bij NS', 'Submit to NS'), true)}</div></div>` : '';
  const after = step >= 3 ? `<div class="wz-after"><span class="st info">${L('Ingediend', 'Submitted')}</span> → <span class="st ok">${L('Toegekend', 'Approved')}</span> → <span class="st ok">${L('Uitbetaald', 'Paid')}</span></div>` : '';
  return `<div class="wz-example"><div class="wz-tag">${L('Voorbeeld', 'Example')}</div>${row}${detail}${dialog}${after}</div>`;
}

const HOWTO: [string, string][] = [
  ['Open “Te claimen”. Daar staan je reizen met 30 minuten of meer vertraging.', 'Open “Claimable”. It lists your trips delayed by 30 minutes or more.'],
  ['Klik op een reis om te zien hoe laat je was en wat je terugkrijgt.', 'Click a trip to see how late you were and what you get back.'],
  ['Klik op “Bekijken en indienen”, beantwoord de vraag over je bestemming, vink de verklaring aan en klik op “Indienen bij NS”.', 'Click “Review and submit”, answer the destination question, tick the declaration and click “Submit to NS”.'],
  ['Daarna volg je de status tot NS heeft betaald.', 'Then follow the status until NS has paid.'],
];

/** A new step: focus its first empty text field, else its heading (never the theme button in the corner). */
function focusFirst() {
  if (!root || root.contains(document.activeElement)) return;
  // A field inside a closed section (the e-mail box of "How should we let you know?") still has a box but
  // can't take focus: skipped, or focus would stay on the page. Each candidate is checked after focus().
  const fields = [...root.querySelectorAll<HTMLInputElement>('.wz-body input:not([type=file]):not([type=radio]):not([type=checkbox])')].filter((e) => !e.disabled && e.getClientRects().length > 0 && !e.closest('details:not([open]), [hidden]'));
  for (const f of [fields.find((e) => !e.value), fields[0], root.querySelector<HTMLElement>('#wzTitle')]) {
    if (!f) continue;
    f.focus({ preventScroll: true });
    if (document.activeElement === f) return;
  }
}
/** The stored NS password stays: Yes chosen, nothing typed, same e-mail address. */
const keepNsPw = () => st.autoLogin === true && st.nsPwSaved && !val('wzNsPw') && !!st.nsSavedEmail && val('wzNsEmail').trim().toLowerCase() === st.nsSavedEmail.trim().toLowerCase();

/** Turning on automatic submission: a short, clear warning that must be ticked (wizard and Settings). */
export function confirmAutoSubmit(unverified: boolean): Promise<boolean> {
  return new Promise((resolve) => {
    const back = document.activeElement as HTMLElement | null;
    const dlg = document.createElement('dialog');
    dlg.className = 'auto-warn';
    dlg.setAttribute('aria-labelledby', 'awT');
    dlg.innerHTML = `${dialogHead(L('Claims automatisch indienen?', 'Submit claims automatically?'), 'awT')}<div class="bd">
      <p>${L('OV Refund dient dan echte claims in op jouw naam, zonder dat je ze eerst ziet. Daarbij verklaart OV Refund namens jou dat de gegevens kloppen en dat je de reis nog niet eerder claimde.', 'OV Refund then submits real claims in your name, without you seeing them first. In doing so, OV Refund declares on your behalf that the details are correct and that you have not claimed the trip before.')}</p>
      <ul class="plain">
        <li>${L('De vertraging is een schatting. Een claim kan dus onterecht zijn.', 'The delay is an estimate, so a claim can be wrong.')}</li>
        <li>${L('Een reis die je elders claimde en die niet in OV Refund staat, kan dubbel worden ingediend.', 'A trip you claimed elsewhere that isn’t in OV Refund may be submitted twice.')}</li>
        <li>${L('Bij een storing pauzeert automatisch indienen tot jij het hebt nagekeken.', 'If something fails, automatic submission pauses until you have checked it.')}</li>
      </ul>
      ${unverified ? `<p class="note warn">${ICON.warn}<span>${L('Automatisch indienen is nog niet bevestigd door NS. Kijk na de eerste automatische claim even in Mijn teruggaven.', 'Automatic submission hasn’t been confirmed with NS yet. After the first automatic claim, have a look in Mijn teruggaven.')}</span></p>` : ''}
      <p>${L('Jij bent verantwoordelijk voor claims op jouw naam. NS beslist.', 'You are responsible for claims in your name. NS decides.')} ${help(L('Uitzetten kan altijd: Instellingen → Automatisch.', 'You can turn it off at any time: Settings → Automatic.'))}</p>
      <label class="check boxed"><input type="checkbox" id="awOk"><span>${L('Ik begrijp dit en ben zelf verantwoordelijk', 'I understand this and take responsibility')}</span></label>
    </div><div class="ft"><span class="grow"></span><button class="btn" data-x="no">${L('Niet aanzetten', 'Don’t turn on')}</button><button class="btn primary" data-x="yes" disabled>${L('Aanzetten', 'Turn on')}</button></div>`;
    document.body.appendChild(dlg);
    const ok = dlg.querySelector('#awOk') as HTMLInputElement, yes = dlg.querySelector('[data-x=yes]') as HTMLButtonElement;
    let answer = false;
    ok.addEventListener('change', () => { yes.disabled = !ok.checked; });
    yes.onclick = () => { answer = ok.checked; dlg.close(); };
    dlg.querySelectorAll<HTMLButtonElement>('[data-x=no],[data-x=close]').forEach((b) => { b.onclick = () => dlg.close(); });
    dlg.addEventListener('close', () => { dlg.remove(); if (back?.isConnected) setTimeout(() => back.focus({ preventScroll: true }), 0); resolve(answer); });
    dlg.showModal();
    (dlg.querySelector('[data-x=no]') as HTMLButtonElement).focus();
  });
}

function draw() {
  if (!root) return;
  const typed = new Map<string, string>();
  root.querySelectorAll<HTMLInputElement>('input[id]').forEach((i) => { if (i.type !== 'checkbox' && i.type !== 'radio' && i.type !== 'file') typed.set(i.id, i.value); });
  // Same step redrawn (language/theme switch): ticked boxes and open sections stay as the user left them.
  const sameStep = drawnKey === `${st.step}:${st.step === 'howto' ? st.howto : ''}`;
  const ticks = new Map<string, boolean>();
  if (sameStep) root.querySelectorAll<HTMLInputElement>('input[type=checkbox][id]').forEach((i) => ticks.set(i.id, i.checked));
  const openDet = new Map<string, boolean>();
  if (sameStep) root.querySelectorAll<HTMLDetailsElement>('details[id]').forEach((d) => openDet.set(d.id, d.open));
  let html = '';
  switch (st.step) {
    case 'welcome':
      html = frame(L('Welkom bij OV Refund', 'Welcome to OV Refund'), `
        <p class="lead">${L('OV Refund zoekt je vertraagde treinreizen en helpt je geld terug te vragen bij NS. Gratis, zonder commissie.', 'OV Refund finds your delayed train trips and helps you get your money back from NS. Free, without commission.')} ${help(L('Je gegevens blijven op deze computer: geen account bij ons, geen tracking. Wachtwoorden en je IBAN worden versleuteld bewaard in Windows. Het NS-inlogvenster laadt NS-pagina’s, die inhoud van derden kunnen laden. Windows/WebView2 SmartScreen kan URL’s controleren. Het installatieprogramma kan Microsoft WebView2 downloaden. Een optioneel e-mailoverzicht met routes en datums gaat via je eigen mailprovider.', 'Your data stays on this computer: no account with us, no tracking. Passwords and your IBAN are stored encrypted in Windows. The NS login window loads NS pages, which may load third-party content. Windows/WebView2 SmartScreen may check URLs. The installer may download Microsoft WebView2. An opt-in e-mail summary with routes and dates goes through your own mail provider.'), L('privacy', 'privacy'))}</p>
        <div class="wz-disclaimer">
          <p>${L('OV Refund is niet verbonden aan NS. Je gebruikt de app op eigen risico. De makers zijn niet aansprakelijk voor gemiste claims of andere schade.', 'OV Refund is not affiliated with NS. You use it at your own risk. The authors are not liable for missed claims or any other damage.')} <a href="${WEBSITE_URL}disclaimer.html" target="_blank" rel="noopener">${L('Volledige disclaimer', 'Full disclaimer')}</a></p>
          <label class="check"><input type="checkbox" id="wzAccept" ${st.accepted ? 'checked' : ''}><span>${L('Ik begrijp dit en gebruik OV Refund op eigen risico', 'I understand and use OV Refund at my own risk')}</span></label>
        </div>
        ${firstStep === 'welcome' ? restoreOffer() : ''}`,
        `<button class="btn primary" data-wz="accept" ${st.accepted ? '' : 'disabled'}>${L('Beginnen', 'Get started')}</button>`, { back: false });
      break;
    case 'login':
      html = frame(L('Inloggen bij Mijn NS', 'Log in to Mijn NS'), `${st.loggedIn
        ? `<p class="wz-ok">${L('Verbonden met Mijn NS', 'Connected to Mijn NS')}${st.nsEmail ? ` (${h(st.nsEmail)})` : ''}</p>`
        : `<p class="lead">${L('Log in op de echte NS-pagina. Het venster sluit vanzelf.', 'Log in on the real NS page. The window closes by itself.')} ${help(L('Het venster heeft geen adresbalk, maar het is echt ns.nl. Je wachtwoordmanager werkt hier niet. Kopieer en plak je wachtwoord. Je hebt een Mijn NS-account nodig met je persoonlijke OV-chipkaart eraan gekoppeld.', 'The window has no address bar, but it really is ns.nl. Your password manager doesn’t work here. Copy and paste your password. You need a Mijn NS account with your personal OV-chipkaart linked to it.'))}</p>
           <p><button class="btn primary" data-wz="login">${L('Inloggen bij Mijn NS', 'Log in to Mijn NS')}</button></p>`}
        ${firstStep === 'login' ? restoreOffer() : ''}`,
        `${!st.loggedIn ? `<button class="btn" data-wz="later">${L('Later', 'Later')}</button>` : ''}<button class="btn primary" data-wz="next">${L('Volgende', 'Next')}</button>`);
      break;
    case 'services':
      html = frame(L('Gebruik je een claimdienst?', 'Do you use a claim service?'), `
        <p class="lead">${L('Bijvoorbeeld Trein-vertraging.nl, nu of eerder.', 'For example Trein-vertraging.nl, now or before.')}</p>
        <div class="choice"><label class="check"><input type="radio" name="svc" id="svcYes" ${st.otherService === true ? 'checked' : ''}><span>${L('Ja', 'Yes')}</span></label>
        <label class="check"><input type="radio" name="svc" id="svcNo" ${st.otherService === false ? 'checked' : ''}><span>${L('Nee', 'No')}</span></label></div>
        ${`<p id="wzServiceWarning" class="note warn" ${st.otherService ? '' : 'hidden'}>${ICON.warn}<span>${L('Zeg die dienst eerst op, anders wordt dezelfde reis twee keer geclaimd.', 'Cancel that service first, or the same trip gets claimed twice.')} ${help(L('Voeg daarna de reizen die de dienst al claimde toe bij Instellingen → Gegevens & back-up → Elders geclaimd. Klik daar ook op Import klaar. Tot dan dient OV Refund niets automatisch in.', 'Then add the trips that service already claimed under Settings → Data & backup → Claimed elsewhere. Click Import done there too. Until then OV Refund submits nothing automatically.'))}</span></p>`}`,
        `<button class="btn primary" data-wz="saveservices">${L('Volgende', 'Next')}</button>`);
      break;
    case 'bank':
      html = frame(L('Waar mag NS je geld op storten?', 'Where should NS pay you?'), `
        <p class="lead">${L('NS vraagt bij elke claim om je rekening.', 'NS asks for your bank account with every claim.')} ${help(L('Zonder rekening kun je niet indienen. NS betaalt rechtstreeks aan jou. Je IBAN wordt versleuteld bewaard in Windows en alleen in het NS-formulier ingevuld.', 'Without a bank account you can’t submit. NS pays the refund straight to you. Your IBAN is stored encrypted in Windows and only entered in the NS form.'))}</p>
        ${st.bank ? `<p class="wz-ok">${L('Opgeslagen', 'Saved')}: •••• ${h(st.bank.last4)}${st.bank.name ? `, ${h(st.bank.name)}` : ''}</p><p class="msg">${L('Wil je een andere rekening? Vul die hieronder in.', 'Want a different account? Enter it below.')}</p>` : ''}
        <div class="wz-form"><label>${L('Naam rekeninghouder', 'Account holder')}<input class="in" id="wzBankName" autocomplete="off" maxlength="${BANK_NAME_MAX}" aria-describedby="wzNameMsg" value="${h(st.bank?.name ?? '')}"><span class="msg namemsg" id="wzNameMsg"></span></label>
        <label>IBAN<input class="in mono" id="wzIban" autocomplete="off" spellcheck="false" maxlength="${IBAN_MAXLENGTH}" placeholder="NL00 BANK 0123 4567 89"><span class="ibanmsg msg" id="wzIbanMsg"></span></label></div>`,
        `${st.bank ? '' : `<button class="btn" data-wz="next">${L('Later', 'Later')}</button>`}<button class="btn primary" data-wz="savebank" id="wzSaveBank" disabled>${L('Volgende', 'Next')}</button>`);
      break;
    case 'auto':
      html = frame(L('Automatisch laten controleren?', 'Check automatically?'), `
        <label class="check"><input type="checkbox" class="switch" id="wzAuto" ${st.autoChecks ? 'checked' : ''}><span>${L('Elke week automatisch controleren', 'Check automatically every week')} ${help(L('Dit gebeurt elke 7 dagen op de achtergrond. Stond je computer uit? Dan gebeurt het de volgende keer dat je inlogt op Windows. Tussen de controles draait er niets.', 'This runs in the background every 7 days. Was your computer off? Then it runs the next time you sign in to Windows. Nothing runs between checks.'))}</span></label>
        <label class="check"><input type="checkbox" class="switch" id="wzSubmit" ${!automaticSubmitAllowed(st.autoChecks, st.otherService, st.importDone) ? 'disabled' : ''} ${st.autoSubmit && automaticSubmitAllowed(st.autoChecks, st.otherService, st.importDone) ? 'checked' : ''}><span>${L('Claims ook automatisch indienen', 'Also submit claims automatically')} ${help(L('OV Refund dient alleen reizen met duidelijk bewijs in. Twijfelgevallen wachten op jou.', 'OV Refund only submits trips with clear evidence. Unclear cases wait for you.') + (st.otherService ? `<br>${L('Je gebruikte een claimdienst: dit kan pas als je de reizen van die dienst hebt toegevoegd.', 'You used a claim service: this can only start once you’ve added the trips it claimed.')}` : ''))}</span></label>`,
        `<button class="btn primary" data-wz="saveauto">${L('Volgende', 'Next')}</button>`);
      if (st.otherService && !st.importDone) html = html.replace('<p class="msg wz-gate"', `<p class="msg">${L('Automatisch indienen kan pas als je de reizen van je claimdienst hebt toegevoegd. Dat doe je later bij Instellingen → Gegevens & back-up.', 'Automatic submission can only start once you’ve added the trips your claim service claimed. You do that later under Settings → Data & backup.')}</p><p class="msg wz-gate"`);
      break;
    case 'autologin':
      html = frame(L('Mag OV Refund zelf inloggen?', 'May OV Refund log in by itself?'), `
        <p class="lead">${L('Dan kan de automatische controle zonder jou inloggen bij NS.', 'Then the automatic check can log in to NS without you.')} ${help(L('OV Refund bewaart je e-mailadres en wachtwoord voor Mijn NS versleuteld in Windows en vult ze alleen in op de inlogpagina van NS.', 'OV Refund stores your Mijn NS e-mail address and password encrypted in Windows and only enters them on the NS login page.'))}</p>
        <div class="choice"><label class="check"><input type="radio" name="al" id="alYes" ${st.autoLogin === true ? 'checked' : ''}><span>${L('Ja, automatisch inloggen', 'Yes, log in automatically')}</span></label>
        <label class="check"><input type="radio" name="al" id="alNo" ${st.autoLogin === false ? 'checked' : ''}><span>${L('Nee, ik log zelf in', 'No, I’ll log in myself')}</span></label></div>
        ${`<div id="wzLoginFields" class="wz-form" ${st.autoLogin ? '' : 'hidden'}><label>${L('E-mailadres voor Mijn NS', 'Mijn NS e-mail address')}<input class="in" id="wzNsEmail" type="email" autocomplete="off" value="${h(st.nsEmail)}"></label>
        <label>${L('NS-wachtwoord', 'NS password')}<input class="in" id="wzNsPw" type="password" autocomplete="off"${st.nsPwSaved ? ` placeholder="${L('Opgeslagen', 'Saved')}" aria-describedby="wzNsPwSaved"` : ''}>${st.nsPwSaved ? `<span class="msg" id="wzNsPwSaved">${L('Opgeslagen. Laat leeg om het te houden.', 'Saved. Leave it empty to keep it.')}</span>` : ''}</label></div>`}
        ${`<p id="wzLoginReminder" class="msg" ${st.autoLogin === false && st.autoChecks ? '' : 'hidden'}>${L('Je krijgt dan een herinnering om OV Refund te openen.', 'You’ll then get a reminder to open OV Refund.')}</p>`}`,
        `<button class="btn primary" data-wz="saveautologin">${L('Volgende', 'Next')}</button>`);
      break;
    case 'notify':
      html = frame(L('Hoe wil je het horen?', 'How should we let you know?'), `
        <label class="check"><input type="checkbox" class="switch" id="wzWin" ${st.windowsNotify ? 'checked' : ''}><span>${L('Windows-meldingen', 'Windows notifications')} ${help(L('Een melding rechtsonder als er iets te claimen is of als je opnieuw moet inloggen. Je hoeft niets in te stellen.', 'A notification in the bottom-right corner when there is something to claim or you need to log in again. Nothing to set up.'))}</span></label>
        <details class="wz-mailbox" id="wzMailBox" ${st.mailTo ? 'open' : ''}>
          <summary>${L('Ook een overzicht per e-mail (optioneel)', 'Also a summary by e-mail (optional)')}</summary>
          <p class="msg">${L('Na een automatische controle waarin iets veranderde, krijg je een kort overzicht in je mailbox.', 'After an automatic check in which something changed, you get a short summary in your mailbox.')} ${help(L('Je gebruikt een apart wachtwoord alleen voor OV Refund (een app-wachtwoord), dus je echte wachtwoord blijft geheim. Het wordt versleuteld bewaard in Windows.', 'You use a separate password just for OV Refund (an app password), so your real password stays private. It is stored encrypted in Windows.'))}</p>
          <div class="wz-form"><label>${L('Jouw e-mailadres', 'Your e-mail address')}<input class="in" id="wzMail" autocomplete="email" placeholder="${L('naam@example.com', 'name@example.com')}" value="${h(st.mailTo)}"></label></div>
          <div id="wzMailSteps"></div>
          <div class="wz-form"><label><span id="wzPwLabel">${mailPwLabel(st.mailTo)}</span><input class="in" id="wzMailPw" type="password" autocomplete="off"></label></div>
          <div class="wz-btns"><button class="btn" data-wz="testmail">${L('Testmail sturen', 'Send test e-mail')}</button><span class="msg" id="wzMailResult" role="status"></span></div>
        </details>`,
        `<button class="btn primary" data-wz="savenotify">${L('Volgende', 'Next')}</button>`);
      break;
    case 'howto': {
      const n = HOWTO.length, k = st.howto;
      html = frame(L('Zo dien je een claim in', 'How to submit a claim'), `
        <p class="wz-howto">${L(HOWTO[k][0], HOWTO[k][1])}</p>
        ${howtoSample(k)}`,
        `<button class="btn" data-wz="howprev" aria-label="${L('Vorige stap', 'Previous step')}" ${k === 0 ? 'disabled' : ''}>‹</button><span class="msg" aria-live="polite">${k + 1} / ${n}</span>${k < n - 1 ? `<button class="btn primary" data-wz="hownext">${L('Volgende stap', 'Next step')}</button>` : `<button class="btn primary" data-wz="next">${L('Begrepen', 'Got it')}</button>`}`);
      break;
    }
    case 'finish':
      html = frame(L('Klaar!', 'All set!'), `
        <p class="lead">${st.loggedIn ? L('OV Refund haalt nu je reizen op en controleert ze.', 'OV Refund now fetches your trips and checks them.') : L('Log straks in bij Mijn NS om je reizen op te halen.', 'Log in to Mijn NS next to fetch your trips.')} ${help(L('De eerste controle haalt de treintijden per maand op (zo’n 35 MB per maand) en duurt een paar minuten. Je ziet de resultaten maand voor maand verschijnen. Reizen van deze maand controleert OV Refund begin volgende maand. Je hebt 3 maanden om te claimen.', 'The first check fetches the train times month by month (about 35 MB per month) and takes a few minutes. You’ll see the results appear month by month. OV Refund checks this month’s trips early next month. You have 3 months to claim.'))}</p>`,
        `<button class="btn primary" data-wz="finish">${st.loggedIn ? L('Start eerste controle', 'Start first check') : L('Naar OV Refund', 'Go to OV Refund')}</button>`);
      break;
  }
  root.innerHTML = `<div class="wz-backdrop"></div>${html}`;
  const key = `${st.step}:${st.step === 'howto' ? st.howto : ''}`;
  if (key !== drawnKey) { root.querySelector('.wz-panel')?.classList.add('wz-enter'); drawnKey = key; }
  typed.forEach((v, id) => { const i = root!.querySelector('#' + id) as HTMLInputElement | null; if (i) i.value = v; });
  ticks.forEach((v, id) => { const i = root!.querySelector('#' + id) as HTMLInputElement | null; if (i) i.checked = v; });
  openDet.forEach((open, id) => { const d = root!.querySelector('#' + id) as HTMLDetailsElement | null; if (d) d.open = open; });
  const pw = root.querySelector('#wzNsPw') as HTMLInputElement | null;
  if (pw && !pw.value && st.nsPw) pw.value = st.nsPw;
  revalidate();
  focusFirst();
}

/** Re-run the live checks after a redraw (IBAN message + Next button, name hint, mail steps). */
function revalidate() {
  if (!root) return;
  for (const id of ['wzIban', 'wzBankName', 'wzMail']) root.querySelector('#' + id)?.dispatchEvent(new Event('input', { bubbles: true }));
  updateGate();
}

/** Update the existing button only: input events must keep focus and selection. */
function updateGate() {
  if (!root) return;
  let valid = true, hint = '', quiet = false;
  switch (st.step) {
    case 'welcome': valid = st.accepted; hint = L('Vink de verklaring aan.', 'Tick the declaration.'); break;
    case 'login': valid = st.loggedIn; hint = L('Log eerst in, of kies Later.', 'Log in first, or choose Later.'); break;
    case 'services': valid = st.otherService !== null; hint = L('Kies Ja of Nee.', 'Choose Yes or No.'); break;
    case 'bank': {
      // A stored account may stay as it is (IBAN left empty).
      valid = validBank(val('wzBankName'), val('wzIban')) || (!!st.bank && !val('wzIban').trim());
      // A filled-in but wrong name says what is wrong with it, not "enter the name".
      hint = !validHolderName(val('wzBankName')) ? (val('wzBankName').trim() ? bankNameHint(val('wzBankName'), getLang()) || L('Controleer de naam van de rekeninghouder.', 'Check the account holder’s name.') : L('Vul de naam van de rekeninghouder in.', 'Enter the account holder’s name.')) : L('Vul een geldig IBAN in.', 'Enter a valid IBAN.');
      // The IBAN field already says what is wrong: don't repeat it below.
      quiet = validHolderName(val('wzBankName')) && checkIban(val('wzIban')).state !== 'empty';
      break;
    }
    case 'autologin':
      valid = validAutoLogin(st.autoLogin, val('wzNsEmail'), val('wzNsPw')) || keepNsPw();
      hint = st.autoLogin === null ? L('Kies Ja of Nee.', 'Choose Yes or No.') : !isEmail(val('wzNsEmail').trim()) ? L('Vul een geldig e-mailadres voor Mijn NS in.', 'Enter a valid Mijn NS e-mail address.') : L('Vul je NS-wachtwoord in.', 'Enter your NS password.');
      break;
    case 'notify': {
      valid = validWizardMail(!!root.querySelector<HTMLDetailsElement>('#wzMailBox')?.open, val('wzMail'), val('wzMailPw'), savedMail);
      const to = val('wzMail').trim();
      hint = !isEmail(to) ? L('Vul een geldig e-mailadres in, of sluit het e-mailvak.', 'Enter a valid e-mail address, or close the e-mail section.')
        : isUnsupportedMail(to) || !mailPreset(to) ? L('Gebruik een ander e-mailadres, of sluit het e-mailvak.', 'Use a different e-mail address, or close the e-mail section.')
        : L(`Vul het veld ${mailPwLabel(to)} in, of sluit het e-mailvak.`, `Fill in ${mailPwLabel(to)}, or close the e-mail section.`);
      break;
    }
  }
  const gate = root.querySelector('#wzGate'); if (gate) gate.textContent = valid || quiet ? '' : hint;
  const button = root.querySelector<HTMLButtonElement>('.wz-actions .btn.primary');
  if (button) {
    button.disabled = busy || !valid;
    button.setAttribute('aria-disabled', String(button.disabled));
    button.title = valid ? '' : hint;
  }
}

/** Enter in a field of the current step = the step's primary button (when it is enabled). */
function onKey(ev: KeyboardEvent) {
  if (ev.key === 'Tab' && root) { trapTab(ev, root); return; }
  if (ev.key !== 'Enter' || ev.isComposing) return;
  const t = ev.target as HTMLElement;
  if (!t.matches('.wz-panel input:not([type=checkbox])')) return;
  const primary = root?.querySelector<HTMLButtonElement>('.wz-actions .btn.primary');
  if (primary && !primary.disabled) { ev.preventDefault(); primary.click(); }
}

/** After a theme/language switch the toggle that was used keeps focus, without scrolling. */
function refocus(sel: string) { (root?.querySelector(sel) as HTMLElement | null)?.focus({ preventScroll: true }); }

const unsupportedMailText = () => unsupportedMailIn(getLang());

function onInput(ev: Event) {
  const el = ev.target as HTMLInputElement;
  if (el.id === 'wzAccept') {
    st.accepted = el.checked;
    updateGate();
    return;
  }
  if (el.id === 'wzIban') {
    applyIbanFormat(el); // keeps the caret in place when editing in the middle
    const c = checkIban(el.value, getLang());
    const m = root!.querySelector('#wzIbanMsg')!;
    m.textContent = c.message; m.className = `ibanmsg msg iban-${c.state}`;
  } else if (el.id === 'wzBankName') {
    const m = root!.querySelector('#wzNameMsg');
    // Wrong characters are an error (red, aria-invalid); the length countdown is just a hint.
    const bad = !!el.value && !validHolderName(el.value);
    if (m) { m.textContent = bankNameHint(el.value, getLang()); m.className = `msg namemsg${bad ? ' bad' : ''}`; }
    el.setAttribute('aria-invalid', String(bad));
  } else if (el.id === 'wzMail') {
    root!.querySelector('#wzMailSteps')!.innerHTML = el.value && !isEmail(el.value) ? `<p class="msg bad">${L('Dit e-mailadres klopt niet. Controleer het adres.', 'This e-mail address is not valid. Check the address.')}</p>` : mailSteps(el.value, 'data-wz="openlink"');
    const lbl = root!.querySelector('#wzPwLabel'); if (lbl) lbl.textContent = mailPwLabel(el.value);
  } else if (el.id === 'wzNsPw') {
    st.nsPw = el.value;
  } else if (el.id === 'wzNsEmail') {
    st.nsEmail = el.value;
  } else if (el.name === 'al') {
    st.autoLogin = el.id === 'alYes';
    const fields = root?.querySelector<HTMLElement>('#wzLoginFields'); if (fields) fields.hidden = !st.autoLogin;
    const reminder = root?.querySelector<HTMLElement>('#wzLoginReminder'); if (reminder) reminder.hidden = st.autoLogin !== false || !st.autoChecks;
  } else if (el.id === 'wzAuto') {
    st.autoChecks = el.checked;
    const submit = root?.querySelector<HTMLInputElement>('#wzSubmit');
    if (submit) { submit.disabled = !automaticSubmitAllowed(el.checked, st.otherService, st.importDone); if (submit.disabled) { submit.checked = false; st.autoSubmit = false; } }
  } else if (el.name === 'svc') {
    st.otherService = el.id === 'svcYes';
    const warning = root?.querySelector<HTMLElement>('#wzServiceWarning'); if (warning) warning.hidden = !st.otherService;
  }
  updateGate();
}

/** Read the chosen back-up file and hand it to the backend (as base64). Files over the size limit are refused before
 *  they are read. Returns the backend's summary ({kept, replaced, auto_backup_path} or older {journeys, claims}),
 *  or null when the file was refused. `force` (replace existing data) only after the caller's specific confirmation. */
async function restoreFrom(file: File, force: boolean, callFn: WizardDeps['call'] = deps.call, toastFn: WizardDeps['toast'] = deps.toast): Promise<any | null> {
  if (backupTooLarge(file.size)) { toastFn(backupTooLargeText(getLang()), 'err'); return null; }
  const buf = new Uint8Array(await file.arrayBuffer());
  let bin = '';
  for (let i = 0; i < buf.length; i += 0x8000) bin += String.fromCharCode(...buf.subarray(i, i + 0x8000));
  const data = btoa(bin);
  // Tauri maps a Rust `data_base64` argument to `dataBase64`; a request struct would read `data_base64`: send both.
  const r = await callFn<any>('import_backup', { dataBase64: data, data_base64: data, ...(force ? { force: true } : {}) });
  return r ?? {};
}
export { restoreFrom as restoreBackupFile };

async function onChange(ev: Event) {
  const el = ev.target as HTMLInputElement;
  if (el.id !== 'wzRestoreFile') { onInput(ev); return; }
  if (!el.files?.[0]) return;
  const f = el.files[0];
  el.value = '';
  try {
    const r = await restoreFrom(f, false);
    if (!r) return;
    deps.toast([L('Back-up teruggezet.', 'Backup restored.'), ...restoreSummary(r, getLang(), { recheck: typeof r?.unassessed === 'number', loggedIn: st.loggedIn }), L('Wachtwoorden en je IBAN zitten niet in een back-up. Log daarom opnieuw in bij NS en stel je rekening opnieuw in.', 'Passwords and your IBAN are not part of a backup. So log in to NS and set up your bank account again.')].join(' '));
    // The main window behind the wizard shows the restored trips straight away.
    document.dispatchEvent(new Event('datachange'));
  } catch { /* shown by call() */ }
}

async function onClick(ev: Event) {
  const t = ev.target as HTMLElement;
  const link = t.closest('a[target=_blank]') as HTMLAnchorElement | null;
  if (link) { ev.preventDefault(); await deps.call('open_external', { url: link.href }).catch(() => {}); return; }
  if (t.closest('[data-theme-toggle]')) { toggleTheme(); draw(); refocus('[data-theme-toggle]'); document.dispatchEvent(new Event('themechange')); return; }
  const ls = t.closest('[data-setlang]') as HTMLElement | null;
  if (ls) {
    const lang = ls.dataset.setlang as 'nl' | 'en';
    if (lang !== getLang()) { setLang(lang); draw(); refocus('[data-setlang]'); document.dispatchEvent(new Event('langchange')); }
    return;
  }
  const b = t.closest('[data-wz]') as HTMLElement | null;
  if (!b || (b instanceof HTMLButtonElement && b.disabled)) return;
  // One action at a time: a double click must not save twice or skip a step.
  if (busy) return;
  busy = true;
  const btn = b instanceof HTMLButtonElement ? b : null;
  if (btn) btn.disabled = true;
  try {
    switch (b.dataset.wz) {
      case 'later': return next();
      case 'next': return next();
      case 'back': return back();
      case 'restore': (root!.querySelector('#wzRestoreFile') as HTMLInputElement | null)?.click(); return;
      case 'login': deps.toast(L('Log in via het NS-venster. Het sluit vanzelf zodra je bent ingelogd.', 'Log in using the NS window. It closes by itself once you are logged in.')); await deps.call('open_login'); return;
      case 'saveservices':
        if (st.otherService === null) { deps.toast(L('Kies Ja of Nee.', 'Choose Yes or No.')); return; }
        await deps.call('set_flag', { key: 'other_service', value: st.otherService ? (st.importDone ? 'import_done' : 'yes') : 'none' });
        return next();
      case 'saveautologin':
        if (st.autoLogin === null) { deps.toast(L('Kies Ja of Nee.', 'Choose Yes or No.')); return; }
        if (st.autoLogin && keepNsPw()) return next(); // the stored password stays
        if (st.autoLogin) {
          const email = val('wzNsEmail').trim(), pw = val('wzNsPw');
          if (email && !isEmail(email)) { deps.toast(L('Dit e-mailadres klopt niet. Controleer het adres.', 'This e-mail address is not valid. Check the address.')); return; }
          if (!email || !pw) { deps.toast(L('Vul je e-mailadres en wachtwoord voor Mijn NS in, of kies “Nee”.', 'Enter your Mijn NS e-mail address and password, or choose “No”.')); return; }
          await deps.call('set_ns_login', { email, password: pw });
          st.nsPw = ''; st.nsPwSaved = true; st.nsSavedEmail = email;
        } else {
          await deps.call('set_auto_login', { enabled: false });
          st.nsPw = ''; st.nsPwSaved = false;
        }
        return next();
      case 'savebank':
        if (st.bank && !val('wzIban').trim()) return next(); // keep the stored account
        await deps.call('set_bank_details', { name: val('wzBankName'), iban: val('wzIban') });
        st.bank = { last4: val('wzIban').replace(/\s/g, '').slice(-4), name: val('wzBankName').trim() };
        deps.toast(L('Rekening opgeslagen', 'Bank account saved'));
        return next();
      case 'saveauto': {
        st.autoChecks = chk('wzAuto'); st.autoSubmit = automaticSubmitAllowed(st.autoChecks, st.otherService, st.importDone) && chk('wzSubmit');
        if (st.autoSubmit) {
          const unverified = !(await deps.call<any>('overview'))?.gateway_verified_by_submission;
          if (!(await confirmAutoSubmit(unverified))) {
            // Declined: untick the box (draw() keeps ticks from the screen), so Next doesn't ask again.
            st.autoSubmit = false;
            const box = root?.querySelector('#wzSubmit') as HTMLInputElement | null;
            if (box) box.checked = false;
            draw(); setTimeout(() => refocus('#wzSubmit'), 0); return;
          }
        }
        await saveSettings(null, null);
        // Recorded with how it was enabled: the explicit "not yet verified" override from this warning.
        if (st.autoSubmit) await deps.call('set_unverified_override', { enabled: true });
        return next();
      }
      case 'skipall':
        if (!(await askConfirm({ title: L('Instellen overslaan?', 'Skip setup?'), body: L('Je kunt alles later instellen onder Instellingen. Zonder login en rekening kan OV Refund niets controleren of indienen.', 'You can set everything up later under Settings. Without a login and bank account OV Refund can’t check or submit anything.'), ok: L('Overslaan', 'Skip') }))) return;
        await deps.call('set_flag', { key: 'wizard_done', value: '1' });
        closeRoot();
        await deps.done();
        return;
      case 'accept':
        if (!chk('wzAccept')) { deps.toast(L('Vink eerst aan dat je dit begrijpt.', 'Please tick the box to confirm you understand.')); return; }
        await deps.call('set_flag', { key: 'disclaimer_accepted', value: new Date().toISOString() });
        return next();
      case 'openlink': await deps.call('open_external', { url: b.dataset.url }); return;
      case 'testmail':
      case 'savenotify': {
        st.windowsNotify = chk('wzWin');
        const to = val('wzMail').trim(); const p = mailPreset(to);
        if (b.dataset.wz === 'savenotify' && (!root?.querySelector<HTMLDetailsElement>('#wzMailBox')?.open || (!to && !val('wzMailPw').trim()))) { await saveSettings(null, null, true); return next(); }
        if (!isEmail(to)) { deps.toast(L('Dit e-mailadres klopt niet. Controleer het adres.', 'This e-mail address is not valid. Check the address.')); return; }
        if (isUnsupportedMail(to) || p?.unsupported) { deps.toast(unsupportedMailText(), 'err'); return; }
        if (!p) { deps.toast(L('Mailprovider niet herkend. Maak het e-mailveld leeg en stel e-mail later in.', 'Mail provider not recognised. Clear the e-mail field and set it up later.')); return; }
        const smtp = { host: p.host, port: p.port, security: p.security, username: to, from: to, to };
        const pw = val('wzMailPw').trim();
        const saved = await deps.call<any>('get_settings');
        const stored = saved?.smtp_password_set;
        if (!pw && !stored) { deps.toast(L('Vul eerst het wachtwoord in.', 'Enter the password first.')); return; }
        // A stored password belongs to the server and username it was saved for (the backend refuses it too).
        if (!pw && stored && smtpLoginChanged(saved?.smtp, smtp)) { deps.toast(smtpPasswordAgainText(getLang()), 'err'); return; }
        await saveSettings(smtp, pw || null);
        savedMail = { smtp, smtp_password_set: true };
        if (b.dataset.wz === 'testmail') {
          const out = root!.querySelector('#wzMailResult')!;
          out.textContent = L('Versturen…', 'Sending…');
          try { await invoke<string>('test_email'); out.textContent = L('Verstuurd. Kijk in je inbox.', 'Sent. Check your inbox.'); }
          catch (e) { out.textContent = friendlyError(e); }
          return;
        }
        if (pw) {
          // First set-up: the welcome mail (with the current claim overview). If it fails, stay here so it can be fixed.
          const out = root!.querySelector('#wzMailResult');
          if (out) out.textContent = L('Welkomstmail versturen…', 'Sending welcome e-mail…');
          try { await invoke<string>('test_email'); }
          catch (e) { deps.toast(friendlyError(e), 'err'); if (out) out.textContent = friendlyError(e); return; }
          deps.toast(L('Welkomstmail verstuurd. Kijk in je inbox.', 'Welcome e-mail sent. Check your inbox.'));
        }
        return next();
      }
      case 'hownext': st.howto = Math.min(st.howto + 1, HOWTO.length - 1); return draw();
      case 'howprev': st.howto = Math.max(st.howto - 1, 0); return draw();
      case 'finish':
        await deps.call('set_flag', { key: 'wizard_done', value: '1' });
        closeRoot();
        await deps.done();
        return;
    }
  } catch {
    if (b.dataset.wz === 'saveauto') { st.autoSubmit = false; const box = root?.querySelector<HTMLInputElement>('#wzSubmit'); if (box) box.checked = false; setTimeout(() => refocus('#wzSubmit'), 0); }
  } finally {
    busy = false;
    if (btn?.isConnected) btn.disabled = false;
    updateGate();
    // A dialog that was cancelled (e.g. "Skip setup?") gave focus back while the button was disabled.
    if (btn?.isConnected && !btn.disabled && (!document.activeElement || document.activeElement === document.body)) btn.focus({ preventScroll: true });
  }
}

async function saveSettings(smtp: any, smtpPassword: string | null, mailOff = false) {
  const s = await deps.call<any>('get_settings');
  await deps.call('save_settings', { s: {
    mode: st.autoSubmit ? 'automatic' : 'manual', auto_consent: st.autoSubmit, interval_days: st.interval,
    // The e-mail summary only with an address and a stored (or just typed) password.
    notify_windows: st.windowsNotify, notify_email: !mailOff && !!(smtp ?? (s.smtp?.to ? s.smtp : null)) && (!!smtpPassword || !!s.smtp_password_set), schedule_enabled: st.autoChecks,

    smtp: smtp ?? s.smtp ?? { host: '', port: 587, security: 'starttls', username: '', from: '', to: '' },
    smtp_password: smtpPassword,
  } });
}

/** The "how to submit" walkthrough on its own (Settings → Help → How to submit a claim). */
export function openHowto(d: WizardDeps) {
  if (root) return;
  deps = { ...d, done: async () => {} };
  st.step = 'howto'; st.howto = 0;
  // (The opener may already have lost focus because it is disabled while busy: fall back to the button itself.)
  opener = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : (document.querySelector('[data-act=howto]') as HTMLElement | null);
  root = document.createElement('div');
  root.className = 'wizard';
  document.body.appendChild(root);
  document.documentElement.classList.add('wz-open');
  setInert(true);
  root.addEventListener('keydown', onKey);
  // Esc closes the walkthrough; focus goes back to the button that opened it.
  root.addEventListener('keydown', (ev) => { if (ev.key === 'Escape') { ev.preventDefault(); closeRoot(); } });
  root.addEventListener('click', async (ev) => {
    const t = ev.target as HTMLElement;
    if (t.closest('[data-theme-toggle]')) { toggleTheme(); draw(); refocus('[data-theme-toggle]'); document.dispatchEvent(new Event('themechange')); return; }
    const ls = t.closest('[data-setlang]') as HTMLElement | null;
    if (ls) { setLang(ls.dataset.setlang as 'nl' | 'en'); draw(); refocus('[data-setlang]'); document.dispatchEvent(new Event('langchange')); return; }
    const b = t.closest('[data-wz]') as HTMLElement | null;
    if (!b) return;
    if (b.dataset.wz === 'next' || b.dataset.wz === 'back' || b.dataset.wz === 'finish') { closeRoot(); return; }
    if (b.dataset.wz === 'hownext') st.howto = Math.min(st.howto + 1, HOWTO.length - 1);
    if (b.dataset.wz === 'howprev') st.howto = Math.max(st.howto - 1, 0);
    draw();
  });
  draw();
}
