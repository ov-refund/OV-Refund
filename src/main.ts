import { BUG_TEXT_MAX, buildBugPayload, hasPersonalDetails, sendBugRelay } from './bug-relay';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { amountDetail, amountInput, timetableDue, arrivaVrijFareText, autoPauseText, autoRunText, autoSubmitBlockers, displayDelay, nearThresholdText, submitHint, validExternalTrip, validHolderName, decimalEntry, applyIbanFormat, BANK_NAME_MAX, backupTooLarge, backupTooLargeText, bankNameHint, carrierNotSupported, category, checkIban, daysUntil, escapeHtml as h, euro, exportInfo, helpTip, historyDetail, IBAN_MAXLENGTH, inPeriod, isEmail, isUnsupportedMail, localDate, localDateTime, localDay, localTime, mailPreset, minutes, nextStep, progressText, reasonText, redactText, refundSentence, arrivaCutoffText, arrivaUnder, type ArrivaInfo, restoreSummary, shortPath, smtpLoginChanged, smtpPasswordAgainText, subscriptionExplain, subscriptionLine, unsupportedMailText as unsupportedMailIn, type Category, type Period, type Subscription } from './format';
import { askChecked, askConfirm, askWord, dialogHead, friendlyError, getLang, ICON, installDialogTrap, installHelpTips, L, langSwitch, mailPwLabel, mailSteps, pickLang, reducedMotion, setLang, syncLang } from './i18n';
import { themeButton, toggleTheme } from './theme';
import { confirmAutoSubmit, NS_MIJNNS_URL, openHowto, openWizard, restoreBackupFile, WEBSITE_URL } from './wizard';
import { arrivaClaimState, arrivaOutcomeText, arrivaStaysClaimable, openArrivaAttachments, openArrivaDemo, operatorLabel, type ArrivaDemoState } from './arriva-demo';
import { arrivaBlocker, arrivaClaimable, arrivaMessage, arrivaPhase, arrivaRunning, expiryInPastNote, expiryProblem, isArrivaTrip, type ArrivaContact, type ArrivaRealState } from './arriva-claim';

// ---------- types (mirror Rust serde output) ----------
interface Station { uic: string; name: string }
interface Journey { source_record_id: string; check_in: string; check_out: string | null; origin: Station; destination: Station | null; carrier_code: string | null; carrier_name: string | null; carrier_kind?: 'ns' | 'arriva' | 'other' | null; product_name: string | null; card_last4: string | null; fare_cents: number | null; invoiced: boolean | null; subscription: string | null }
interface Leg { train_number: string; train_type: string; company: string; from_code: string; to_code: string; dep_planned: string; dep_actual: string; arr_planned: string; arr_actual: string; cancelled: boolean }
interface Route { legs: Leg[] }
interface Row { id: number; journey: Journey; confidence: string | null; proxy_min: number | null; delay_min: number | null; delay_band: string | null; scheduled_departure: string | null; reasons: string[]; expires_at: string | null; evidence_status: string | null; planned: Route | null; taken: Route | null; claim_id: number | null; claim_status: string | null; ns_reference: string | null; compensation_cents: number | null; compensation_basis: string | null; compensation_exact: boolean | null; paid_cents: number | null; commission_pct: number | null; ns_decision: string | null; ns_claim_near?: boolean; card_conflict?: boolean; was_uncertain?: boolean; possible_delay_min?: number | null; carrier_url?: string | null; below_minimum?: boolean; arriva?: ArrivaInfo | null }
interface ClaimEvent { at: string; from_status: string | null; to_status: string; actor: string; detail: string }
interface ClaimItem { claim: { id: number; journey_id: number; status: string; ns_reference: string | null; submitted_at: string | null; authorized_mode: string | null }; events: ClaimEvent[] }
interface Total { count: number; cents: number; unknown: number }
interface ExternalClaim { id: number; date: string; origin: string | null; destination: string | null; source: string }
interface Overview {
  auth_state: string; last_sync: string | null; mode: string; auto_opt_in_at: string | null; gateway_verified: boolean; busy: boolean; session_saved: boolean; interval_days: number;
  schedule: { installed: boolean; interval_days: number | null; next_run: string | null };
  totals: { paid: Total; pending: Total; claimable: Total; external: Total };
  account: { card_last4: string | null; card_number?: string | null; ns_email: string | null; notify_email: string; iban_last4: string | null; bank_name: string | null; bank_institution: string | null };
  uncertain: number;
  /** Uncertain NS claims (automatic NS submission waits for them) and uncertain Arriva claims. */
  uncertain_ns?: number; uncertain_arriva?: number;
  // Additive fields (gap-list contract); every one may be missing on an older backend.
  subscription?: Subscription | null; claim_list_status?: 'ok' | 'unavailable' | 'unknown' | null; iban_country?: string | null;
  bug_form_key?: string; mail_unsupported_provider?: boolean; ns_mail_skipped?: { total?: number; by_reason?: Record<string, number> } | null;
  wizard_step?: string | null; other_service?: 'none' | 'yes' | 'import_done' | null; disclaimer_accepted?: boolean;
  /** Where back-ups and CSV files go; whether the OneDrive-synced Documents folder was avoided; whether Forget everything deletes them. */
  export_dir?: string | null; onedrive_avoided?: boolean | null; forget_removes_exports?: boolean | null;
}

type Tab = 'account' | 'auto' | 'notify' | 'data' | 'help' | 'erase';
const state = {
  o: null as Overview | null, rows: [] as Row[], claims: new Map<number, ClaimItem>(),
  period: 'month' as Period, cat: 'all' as Filter, open: new Set<number>(), busy: '', edit: '' as '' | 'bank' | 'email' | 'import' | 'nslogin' | 'arriva',
  settings: null as any, mailManual: false, busySince: 0, progress: { pct: 0, stage: '' } as { pct: number; stage: string; month?: string; index?: number; total?: number; seconds?: number },
  /** The user pressed Stop during automatic submission: the check stops after the current claim. */
  stopping: false,
  /** What the busy state is for: only scans show progress (inside the "Check now" button). */
  busyKind: '' as '' | 'scan' | 'update',
  /** Busy, but not yet shown (start-up check: only show it when it takes a while). */
  busyQuiet: false,
  update: null as null | { configured: boolean; enabled?: boolean; current: string; available: boolean; version?: string; notes?: string },
  tab: 'account' as Tab,
  externals: null as ExternalClaim[] | null,
  /** The last restored backup's summary (import_backup's answer), shown as a banner until dismissed or until the next check. */
  restored: null as any,
  /** Demo only: when the last simulated automatic check ran (it does not change last_sync). */
  demoCheckedAt: '',
  /** Arriva fixture preparation state, available only in the local demo. */
  arriva: null as ArrivaDemoState | null,
  /** Claimen was pressed and the app is waiting for Arriva's outcome. */
  arrivaClaiming: false,
  /** Real Arriva claims (manual Claim): per claim the phase, the setup still needed. */
  arrivaReal: null as ArrivaRealState | null,
  /** Name and e-mail for Arriva's form (Credential Manager). */
  arrivaContact: null as ArrivaContact | null,
  /** Claim ids whose Claim was pressed in this session and is still being started. */
  arrivaStarting: new Set<number>(),
};
/** "bijgewerkt": the last sync, or in the demo a later simulated check. */
const lastUpdated = (o: Overview): string | null => {
  const s = o.last_sync ?? null, d = (o as any).demo ? state.demoCheckedAt : '';
  return d && (!s || new Date(d).getTime() > new Date(s).getTime()) ? d : s;
};
/** The backend sends auth_state "blocked" plus blocked_reason; the UI works with "blocked: <reason>". */
const authOf = (o: any): string => (o?.auth_state === 'blocked' && o?.blocked_reason ? `blocked: ${o.blocked_reason}` : o?.auth_state ?? '');
/** "blocked: <reason>" counts as blocked. */
const authKind = (s: string) => (s.startsWith('blocked') ? 'blocked' : s);
/** Filter tabs: "Te claimen" is ready to submit; "Beoordelen" (review first) is its own tab. */
type Filter = 'all' | 'review' | Category;
const app = document.getElementById('app')!;
const revealed: Record<string, string> = {};
const revealTimers: Record<string, number> = {};
const EYE = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/></svg>';
const EYE_OFF = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 3l18 18"/><path d="M10.6 5.1A10.6 10.6 0 0 1 12 5c6.4 0 10 7 10 7a17.6 17.6 0 0 1-3.2 4.1M6.6 6.6C3.8 8.4 2 12 2 12s3.6 7 10 7a9.7 9.7 0 0 0 5.4-1.6"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/></svg>';
const eyeBtn = (kind: string) => `<button class="eye" data-act="reveal" data-kind="${kind}" title="${revealed[kind] ? L('Verbergen', 'Hide') : L('Tonen', 'Show')}" aria-label="${revealed[kind] ? L('Verbergen', 'Hide') : L('Tonen', 'Show')}">${revealed[kind] ? EYE_OFF : EYE}</button>`;
/** A "?" help tip (details shown on hover, click or keyboard focus). */
const help = (id: string, body: string, topic = '') => helpTip(id, body, topic ? `${L('Uitleg', 'Help')}: ${topic}` : L('Uitleg', 'Help'));
/** A small, friendly train-on-rails drawing for empty states (brand colours). */
const TRAIN_ART = '<svg class="art" viewBox="0 0 120 56" aria-hidden="true"><path d="M4 50h112" stroke="currentColor" stroke-width="2" stroke-linecap="round" opacity=".35"/><path d="M10 54h8M30 54h8M50 54h8M70 54h8M90 54h8" stroke="currentColor" stroke-width="2" stroke-linecap="round" opacity=".25"/><rect x="26" y="14" width="64" height="30" rx="12" fill="#ffc917"/><rect x="34" y="21" width="12" height="9" rx="3" fill="#00307a"/><rect x="51" y="21" width="12" height="9" rx="3" fill="#00307a"/><rect x="68" y="21" width="14" height="9" rx="3" fill="#00307a"/><path d="M26 37h64" stroke="#00307a" stroke-width="3"/><circle cx="40" cy="46" r="3.5" fill="#00307a"/><circle cx="76" cy="46" r="3.5" fill="#00307a"/></svg>';

const wait = (ms: number) => new Promise<void>((res) => setTimeout(res, reducedMotion() ? 0 : ms));

let toastTimer = 0;
/** A short message bottom-right. A popover, so it is shown above the wizard and above modal dialogs. */
function toast(msg: string, kind: '' | 'err' = '', action?: { label: string; run: () => void }) {
  let el = document.querySelector('.toast') as HTMLDivElement | null;
  if (!el) {
    el = document.createElement('div'); el.className = 'toast';
    el.setAttribute('role', 'status'); el.setAttribute('aria-live', 'polite');
    if ('popover' in el) el.popover = 'manual';
    document.body.appendChild(el);
  }
  el.textContent = msg; el.classList.toggle('err', kind === 'err');
  // An optional action (e.g. "Bekijken"): the toast then also takes clicks.
  el.classList.toggle('has-act', !!action);
  if (action) {
    const b = document.createElement('button');
    b.type = 'button'; b.className = 'toast-act'; b.textContent = action.label;
    b.onclick = () => { hideToast(); action.run(); };
    el.append(' ', b);
  }
  el.classList.add('show');
  if (el.popover) {
    // Re-open on every toast: the most recently shown top-layer element is drawn on top (above open dialogs).
    try { if (el.matches(':popover-open')) el.hidePopover(); el.showPopover(); } catch { /* popover unsupported: z-index fallback */ }
  }
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => el!.classList.remove('show'), action ? 12000 : Math.min(9000, 3000 + msg.length * 40));
}
/** Hide the current toast (e.g. on a language switch, or when a hint is no longer true). */
function hideToast() {
  const el = document.querySelector('.toast') as HTMLDivElement | null;
  if (!el) return;
  clearTimeout(toastTimer);
  el.classList.remove('show');
  try { if (el.popover && el.matches(':popover-open')) el.hidePopover(); } catch { /* ignore */ }
}
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try { return await invoke<T>(cmd, args); } catch (e) { toast(friendlyError(e), 'err'); throw e; }
}
// call() already told the user; log the rest instead of surfacing it as an uncaught rejection.
window.addEventListener('unhandledrejection', (e) => { console.warn('An app action failed.'); e.preventDefault(); });

/** Plain decimal entry; empty means unknown, invalid input remains visible for correction. */
function parseAmount(v: string): number | null { return decimalEntry(v, 1000); }
function fieldError(el: HTMLInputElement, message: string) {
  let msg = el.parentElement?.querySelector<HTMLElement>('.field-error');
  if (!msg) { msg = document.createElement('span'); msg.className = 'field-error'; msg.setAttribute('role', 'alert'); el.after(msg); }
  msg.textContent = message; el.setAttribute('aria-invalid', String(!!message));
}
let refreshSeq = 0;
let firstLoadRetried = false;
let initialized = false;
let arrivaStatusTimer = 0, arrivaStatusBusy = false;
async function pollArrivaStatus() {
  if (arrivaStatusBusy || !(state.o as any)?.demo) return;
  arrivaStatusBusy = true;
  try {
    state.arriva = await invoke<ArrivaDemoState>('arriva_demo_state');
  } catch { /* dashboard refresh reports command availability */ }
  finally { arrivaStatusBusy = false; }
}
async function refresh() {
  const seq = ++refreshSeq;
  // Each part loads independently; a failure or hang never leaves the app stuck on "Laden…".
  const timeout = <T>(p: Promise<T>) => Promise.race([p, new Promise<T>((_, rej) => setTimeout(() => rej(new Error('timeout')), 20_000))]);
  const [o, rows, claims, settings] = await Promise.allSettled([
    timeout(invoke<Overview>('overview')), timeout(invoke<Row[]>('journeys', { limit: 2000 })), timeout(invoke<any[]>('claims')), timeout(invoke<any>('get_settings')),
  ]);
  if (seq !== refreshSeq) return; // a newer refresh is in flight; don't overwrite it with older data
  const errors = [o, rows, claims, settings].filter((r) => r.status === 'rejected').map((r) => String((r as PromiseRejectedResult).reason));
  if (o.status === 'fulfilled') state.o = o.value;
  if (rows.status === 'fulfilled') state.rows = rows.value;
  if (claims.status === 'fulfilled') state.claims = new Map(claims.value.map((c: ClaimItem) => [c.claim.id, c]));
  if (settings.status === 'fulfilled') state.settings = settings.value;
  if ((state.o as any)?.demo) {
    try {
      const demo = await timeout(invoke<ArrivaDemoState>('arriva_demo_state'));
      if (seq !== refreshSeq) return;
      state.arriva = demo;
    }
    catch (e) { if (seq !== refreshSeq) return; state.arriva = { available: false, demo_only: true, phase: 'error', steps: [], trip: { from: 'Venlo', to: 'Nijmegen', train_number: '', date: '', delay_minutes: 24, fare_cents: 1240, check_in: '', check_out: '' }, attachments: [], logs: [], last_error: friendlyError(e), window_open: false }; }
  } else state.arriva = null;
  if (seq !== refreshSeq) return;
  let arrivaReal: ArrivaRealState | null = null, arrivaContact: ArrivaContact | null = state.arrivaContact;
  try { [arrivaReal, arrivaContact] = await Promise.all([timeout(invoke<ArrivaRealState>('arriva_state')), timeout(invoke<ArrivaContact>('arriva_contact'))]); }
  catch { arrivaReal = null; }
  // A newer refresh started while these loaded: it has the newer state.
  if (seq !== refreshSeq) return;
  state.arrivaReal = arrivaReal; state.arrivaContact = arrivaContact;
  // Right after start the backend may still be opening the database: one silent retry first.
  if (!state.o && !firstLoadRetried) { firstLoadRetried = true; setTimeout(() => { void refresh(); }, 1500); return; }
  if (!state.o) {
    app.innerHTML = `<div class="top load-failure"><div class="brand" aria-label="OV Refund"><img class="logo-img" src="/logo-mark.png" alt="OV Refund" width="36" height="36"></div><span class="conn">${L('Laden mislukt', 'Loading failed')}: ${h(friendlyError(errors[0] ?? ''))}</span><span class="grow"></span><button class="btn primary" data-act="checkupdate">${L('Zoek naar updates', 'Check for updates')}</button><a class="btn" href="${WEBSITE_URL}" target="_blank" rel="noopener">${L('Download de nieuwste versie', 'Download the latest version')}</a><button class="btn" id="retry">${L('Opnieuw', 'Retry')}</button></div>`;
    app.removeAttribute('aria-busy');
    document.getElementById('retry')!.onclick = () => refresh();
    return;
  }
  if (errors.length) toast(`${L('Niet alles kon worden geladen', 'Some data could not be loaded')}: ${friendlyError(errors[0])}`, 'err');
  app.removeAttribute('aria-busy');
  render();
  if (!initialized) { initialized = true; await initialize(); }
}

// ---------- labels ----------
const STATUS: Record<string, [string, string, string]> = {
  READY: ['Te claimen', 'Claimable', 'ok'], NEEDS_REVIEW: ['Beoordelen', 'Review', 'warn'], CANDIDATE: ['Beoordelen', 'Review', 'warn'],
  FAILED: ['Niet verzonden', 'Not sent', 'bad'],
  _SUBMITTED_CONFIRM: ['', '', ''], SUBMITTING: ['Wordt ingediend…', 'Submitting…', 'info'], SUBMITTED: ['Ingediend', 'Submitted', 'info'],
  INDETERMINATE: ['Onzeker', 'Uncertain', 'bad'], APPROVED: ['Toegekend', 'Approved', 'ok'], REJECTED: ['Afgewezen', 'Rejected', 'bad'],
  PAID: ['Uitbetaald', 'Paid', 'ok'], EXTERNALLY_CLAIMED: ['Elders geclaimd', 'Claimed elsewhere', 'muted'], DISMISSED: ['Overgeslagen', 'Skipped', 'muted'],
};
/** User-settable states: mirrors Db::set_status_by_user (db.rs) within ClaimState::can_transition_to
 *  (model.rs). "Not sent" (FAILED) only for an uncertain claim; a later resubmission of it needs an
 *  explicit duplicate check in the review dialog (confirm_resubmit). Never while SUBMITTING. */
const MANUAL_NEXT: Record<string, string[]> = {
  INDETERMINATE: ['SUBMITTED', 'APPROVED', 'REJECTED', 'FAILED'],
  SUBMITTED: ['APPROVED', 'REJECTED', 'PAID'],
  APPROVED: ['PAID'],
  // NS can reverse a rejection (after the user objected): mirrors the backend rule.
  REJECTED: ['APPROVED', 'PAID'],
};
/** A status word with a small coloured dot (the word carries the meaning, not the colour). */
const stTag = (tone: string, text: string, title = '') => `<span class="st ${tone}"${title ? ` title="${h(title)}"` : ''}><i class="dot" aria-hidden="true"></i>${text}</span>`;
const canReviewSubmit = (r: Row) => !!(r.claim_id && r.delay_band && r.scheduled_departure && r.journey.check_out && r.journey.destination);
/** The demo's fictional Arriva rows (already tested, too little delay). The demo trip on the real train
 *  732284 is not one of them: it goes through the real Claim path, where the demo sends nothing. */
const isDemo = () => !!(state.o as any)?.demo;
const isArrivaDemoFixture = (r: Row) => !!(state.o as any)?.demo && r.journey.source_record_id.startsWith('DEMO-ARRIVA-') && r.journey.source_record_id !== 'DEMO-ARRIVA-732284';
/** A real Arriva trip (checked with Arriva's rules; claimed by the user with Claim, never automatically). */
const isArrivaReal = (r: Row) => !isArrivaDemoFixture(r) && isArrivaTrip(r);
const arrivaBusy = (r: Row) => r.claim_status === 'SUBMITTING' || arrivaRunning(state.arrivaReal, r.claim_id) || (r.claim_id != null && state.arrivaStarting.has(r.claim_id));
/** Status of a real Arriva trip, or '' for the general rules. */
function arrivaRealStatus(r: Row): string {
  if (arrivaBusy(r)) return stTag('info', L('Bezig…', 'In progress…'), L('De claim bij Arriva wordt afgehandeld. Hij blijft hier staan tot de uitkomst bekend is.', 'The claim at Arriva is being handled. It stays here until the outcome is known.'));
  // The demo's run through Arriva's real form ends before sending: say so on the row.
  if (isDemo() && arrivaPhase(state.arrivaReal, r.claim_id) === 'dry_run') return stTag('muted', L('Niet verzonden (demo)', 'Not sent (demo)'), L('Alles gecontroleerd en Indienen ingedrukt; de verzending is geblokkeerd. Er is niets naar Arriva gegaan.', 'Everything checked and Indienen pressed; sending was blocked. Nothing went to Arriva.'));
  if (arrivaClaimable(r)) {
    const why = arrivaMissingText();
    return `<button class="btn primary small row-act" data-act="arriva-real-claim" data-claim="${r.claim_id}" ${state.busy || why ? 'disabled' : ''}${why ? ` title="${h(why)}"` : ''}>${L('Claimen', 'Claim')}</button>`;
  }
  const blocked = arrivaBlocker(r);
  if (blocked && ['NEEDS_REVIEW', 'READY', 'FAILED', 'CANDIDATE'].includes(r.claim_status ?? 'CANDIDATE')) return stTag('warn', L('Zelf indienen bij Arriva', 'Submit yourself at Arriva'), L(blocked.nl, blocked.en));
  if (r.claim_status === 'INDETERMINATE') return stTag('bad', L('Onzeker', 'Uncertain'), L('Kijk in je e-mail van Arriva of je claim is ontvangen.', 'Check your e-mail from Arriva whether your claim was received.'));
  if (r.confidence === 'not_claimable_or_out_of_scope' && !r.claim_status) {
    const why = r.reasons[0] ? reasonText(r.reasons[0], getLang()) : '';
    return (r.delay_min ?? 0) < 1 ? stTag('calm', L('Op tijd', 'On time')) : stTag('muted', L('Geen vergoeding', 'No refund'), why);
  }
  return '';
}
/** Why Claim can't be used yet ('' when it can): Arriva's details or the bank account are missing. */
function arrivaMissingText(): string {
  const missing = state.arrivaReal?.missing ?? [];
  if (missing.includes('contact')) return L('Vul eerst je naam en e-mailadres voor Arriva in bij Jouw gegevens.', 'First enter your name and e-mail address for Arriva under Your details.');
  if (missing.includes('bank')) return L('Stel eerst je rekening in bij Jouw gegevens.', 'Set up your bank account under Your details first.');
  return '';
}
/** The demo's Arriva trips: one claimable (real public train record), one already tested, one with too little delay. */
const arrivaDemoTrip = (r: Row) => state.arriva?.demo_trips?.find(t => t.id === r.journey.source_record_id)
  ?? { id: r.journey.source_record_id, status: 'not_eligible', delay_minutes: state.arriva?.trip?.delay_minutes ?? 24, train_number: null, real_service: false, min_delay: 15, reason_nl: 'Arriva-regels nog niet geladen.', reason_en: 'Arriva rules not loaded yet.' };
const arrivaFixtureDelay = () => state.arriva?.trip?.delay_minutes ?? 24;
function arrivaStatusCell(r: Row): string {
  const t = arrivaDemoTrip(r);
  if (t.status === 'tested') return stTag('ok', L('Demo getest', 'Demo tested'), L('Fictieve demorit; er is niets verzonden.', 'Fictional demo trip; nothing was sent.'));
  if (t.status === 'not_eligible') return stTag('muted', L('Geen recht', 'Not eligible'), L(t.reason_nl ?? '', t.reason_en ?? ''));
  return arrivaClaimAction();
}
function arrivaClaimAction(): string {
  const st = arrivaClaimState(state.arriva, state.arrivaClaiming);
  if (st === 'in_progress') return stTag('info', L('Bezig…', 'In progress…'), L('De claim wordt afgehandeld. Hij blijft hier staan tot de uitkomst bekend is.', 'The claim is being handled. It stays here until the outcome is known.'));
  if (st === 'failed') return stTag('bad', L('Mislukt', 'Failed'), pickLang(state.arriva?.last_error ?? '') || arrivaOutcomeText(state.arriva) || L('Niet verzonden. Kies Controleer nu om het opnieuw te proberen.', 'Not sent. Choose Check now to try again.'));
  if (st === 'ready') return `<button class="btn primary small row-act" data-act="arriva-claim" ${state.busy ? 'disabled' : ''}>${L('Claimen', 'Claim')}</button>`;
  if (state.arriva?.phase === 'submission_blocked') return stTag('ok', L('Demo getest', 'Demo tested'), L('Verzending geblokkeerd; geen echte claim ingediend.', 'Submission blocked; no real claim submitted.'));
  return stTag('muted', L('Wacht op controle', 'Awaiting check'));
}
/** A trip to review with a delay just under 30 minutes: no delay band, so only NS's own form can take it. */
const underThreshold = (r: Row) => { const d = displayDelay(r); return isArrivaTrip(r) ? arrivaUnder(d, r.arriva) : !r.delay_band && d !== null && d < 30; };
/** The claim's carrier, for "Afgewezen door …". */
const carrierName = (r: Row) => (isArrivaTrip(r) ? 'Arriva' : 'NS');
function statusCell(r: Row): string {
  const unsupported = carrierNotSupported(r.journey, getLang());
  // A claim of its own (claimed elsewhere, or older than the carrier check) shows its state first.
  if (unsupported && r.claim_status && r.claim_status !== 'CANDIDATE' && STATUS[r.claim_status]) { const [nl, en, tone] = STATUS[r.claim_status]; return stTag(tone, L(nl, en)); }
  if (unsupported) return stTag('muted', h(unsupported), L('OV Refund claimt alleen bij NS en Arriva. Vraag je geld terug bij deze vervoerder zelf.', 'OV Refund only claims from NS and Arriva. Ask this carrier for your money back yourself.'));
  if (!r.journey.check_out) return stTag('muted', L('Geen uitcheck', 'No check-out'));
  if (isArrivaReal(r)) { const a = arrivaRealStatus(r); if (a) return a; }
  const cat = category(r);
  const byMail = L('Volgens een mail van NS', 'According to an e-mail from NS');
  if (r.ns_decision === 'rejected') return stTag('bad', L('Afgewezen', 'Rejected'), byMail);
  if (r.ns_decision === 'paid') return stTag('ok', L('Uitbetaald', 'Paid'), byMail);
  if (r.ns_decision === 'approved') return stTag('ok', L('Toegekend', 'Approved'), byMail);
  if (cat === 'expired') return stTag('muted', L('Verlopen', 'Expired'));
  // The main action sits on the row itself: one clear button for trips ready to claim.
  // One button per row; "Niet claimen" lives in the trip's details.
  // Never NS's review button for an Arriva trip: it is claimed with Claim, or submitted at Arriva.
  if (cat === 'claimable' && r.claim_id && !isArrivaTrip(r)) return `<button class="btn primary small row-act" data-act="review" data-claim="${r.claim_id}">${canReviewSubmit(r) ? L('Bekijken en indienen', 'Review and submit') : L('Bekijken', 'View')}</button>`;
  const otherOperator = r.evidence_status === 'other_operator' || (!!r.journey.carrier_code && r.journey.carrier_code !== '4');
  if (otherOperator && (!r.claim_status || r.claim_status === 'CANDIDATE')) return stTag('muted', L('Andere vervoerder', 'Other operator'));
  if (r.claim_status && STATUS[r.claim_status]) { const [nl, en, tone] = STATUS[r.claim_status]; return stTag(tone, L(nl, en)); }
  if (cat === 'pending') return stTag('muted', awaitingTag(r.journey.check_in));
  if (!r.confidence) {
    const ageDays = (Date.now() - new Date(r.journey.check_in).getTime()) / 86_400_000;
    if (ageDays <= 92) return stTag('muted', L('Wordt gecontroleerd', 'Being checked'));
    return stTag('muted', L('Te oud', 'Too old'), L('Ouder dan 3 maanden: buiten de claimtermijn', 'Older than 3 months: outside the claim window'));
  }
  if (r.confidence === 'not_claimable_or_out_of_scope' && (r.delay_min ?? 0) < 1) return stTag('calm', L('Op tijd', 'On time'));
  if (r.confidence === 'not_claimable_or_out_of_scope' && (r.delay_min ?? 0) < 30 && !isArrivaTrip(r)) return stTag('muted', L('Geen vergoeding', 'No refund'), L('Minder dan 30 minuten vertraging', 'Less than 30 minutes late'));
  return stTag('muted', L('Niet claimbaar', 'Not claimable'));
}
const recordedAmountLabel = (r: Row) => r.claim_status === 'APPROVED' || r.ns_decision === 'approved' ? L('toegekend', 'approved') : L('ontvangen', 'received');
function compCell(r: Row): string {
  if (!r.journey.check_out) return '';
  // The status column already says "Claimed elsewhere" / "Rejected": no amount to show here.
  if (r.claim_status === 'EXTERNALLY_CLAIMED' && r.paid_cents === null) return '<span class="calm">–</span>';
  if ((r.claim_status === 'REJECTED' || r.ns_decision === 'rejected') && !r.paid_cents) return '<span class="calm">–</span>';
  if (r.paid_cents !== null) return `<span class="amt got">${euro(r.paid_cents, getLang())}</span><span class="amt-sub">${recordedAmountLabel(r)}</span>`;
  if (r.compensation_cents !== null) {
    const est = r.compensation_exact === false;
    const claim = category(r) === 'claimable';
    return `<span class="amt${claim ? ' big' : ''}">${est ? '≈ ' : ''}${euro(r.compensation_cents, getLang())}</span>${claim ? `<span class="amt-sub">${L('terug', 'back')}</span>` : ''}`;
  }
  const relevant = !!r.claim_status || (r.delay_min ?? 0) >= 30;
  if (!relevant) return '';
  // Under 30 minutes (on time, or just under): OV Refund can't claim it, so no money is promised.
  const d = displayDelay(r);
  if (d !== null && d !== undefined && (isArrivaTrip(r) ? arrivaUnder(d, r.arriva) : d < 30)) return '<span class="calm">–</span>';
  return `<span class="amt-sub">${L('volgt nog', 'to follow')}</span>`;
}
/** Delay in the list: 30+ min clear red and bold, 1–29 min a softer red, otherwise "op tijd" in plain text. */
function delayCell(r: Row): string {
  const d = displayDelay(r);
  if (d === null || d === undefined) return '<span class="calm">–</span>';
  if (d >= (isArrivaTrip(r) ? r.arriva?.min_delay ?? 30 : 30)) return `<span class="delay big">+${d} min</span>`;
  if (d > 0) return `<span class="delay">+${d} min</span>`;
  return `<span class="calm">${L('op tijd', 'on time')}</span>`;
}
function cutoffText(d: number, r?: Row): string {
  if (r && isArrivaTrip(r)) return arrivaCutoffText(d, r.arriva, getLang());
  const base = d >= 28 && d < 30 ? nearThresholdText(d, getLang()) : d >= 60 ? L('Vanaf 60 minuten krijg je de hele ritprijs terug.', 'From 60 minutes you get the full fare back.') : d >= 30 ? L('Bij 30–59 minuten krijg je de helft van de ritprijs terug. Vanaf 60 minuten krijg je alles terug.', 'For 30–59 minutes you get half the fare back. From 60 minutes you get all of it back.') : L('Onder 30 minuten geeft NS geen vergoeding.', 'Under 30 minutes NS gives no refund.');
  return `${L(`Geschatte aankomstvertraging: ${d} min, volgens de treintijden van die dag (niet je uitchecktijd).`, `Estimated arrival delay: ${d} min, from that day’s train times (not your check-out time).`)} ${base} ${L('Met een abonnement: het deel dat daarbij hoort.', 'With a subscription: the matching share.')}`;
}
const routeOf = (j: Journey) => `${j.origin.name} → ${j.destination?.name ?? '?'}`;
const mins = (a: string, b: string) => Math.round((+new Date(a) - +new Date(b)) / 60000);

// ---------- sidebar: only "Your details" plus the Settings button ----------
function sidebar(o: Overview): string {
  const a = o.account;
  const ok = o.auth_state === 'ok';
  const cardMasked = a.card_last4 ? `•••• ${h(a.card_last4)}` : '';
  const card = a.card_last4 ? `<span class="mono">${revealed.card && a.card_number ? h(a.card_number) : cardMasked}</span>${a.card_number ? eyeBtn('card') : ''}` : `<span class="none">${L('nog niet opgehaald', 'not fetched yet')}</span>`;
  // Masked like the real IBAN's country (not always NL); an unknown country shows only dots.
  const cc = (o.iban_country ?? '').toUpperCase();
  const iban = a.iban_last4 ? `<span class="mono">${revealed.iban ? h(revealed.iban) : `${/^[A-Z]{2}$/.test(cc) ? h(cc) : '••'}•• •••• ${h(a.iban_last4)}`}</span>${eyeBtn('iban')}` : `<span class="none">${L('niet ingesteld', 'not set')}</span>`;
  const sub = o.subscription;
  const subLine = subscriptionLine(sub?.current, getLang());
  const subLines = subscriptionExplain(sub, getLang());
  const subValue = subLine ? h(subLine) : `<span class="none">${sub?.none && o.last_sync ? L('geen', 'none') : L('nog niet opgehaald', 'not fetched yet')}</span>`;
  const editBtn = (what: string, has: boolean) => `<button class="btn ghost small" data-act="edit" data-what="${what}">${ICON.edit}${has ? L('Wijzigen', 'Change') : L('Instellen', 'Set up')}</button>`;
  const field = (k: string, v: string, extra = '') => `<div class="field"><div class="k">${k}</div><div class="v">${v}${extra}</div></div>`;
  const bankHelp = `${a.bank_name ? `${h(a.bank_name)}${a.bank_institution ? `, ${h(a.bank_institution)}` : ''}. ` : ''}${L('Versleuteld bewaard in Windows en alleen in het claimformulier van NS of Arriva ingevuld.', 'Stored encrypted in Windows and only entered in the claim form of NS or Arriva.')}`;
  const ac = state.arrivaContact;
  const arrivaVal = ac?.set ? h(`${ac.first_name} ${ac.last_name}, ${ac.email}`) : `<span class="none">${L('niet ingesteld', 'not set')}</span>`;
  return `<section class="card details" aria-labelledby="detT"><h2 id="detT">${L('Jouw gegevens', 'Your details')}</h2>
    ${field(L('Mijn NS-account', 'Mijn NS account'), `<span class="val">${a.ns_email ? h(a.ns_email) : '–'}${ok ? '' : ` <span class="none">(${L('uitgelogd', 'logged out')})</span>`}</span>`)}
    ${field('OV-chipkaart', `<span class="val">${card}</span>`)}
    ${sub === undefined ? '' : field(L('Abonnement', 'Subscription'), `<span class="val">${subValue}</span>`, subLines.length ? help('sub', subLines.map(h).join('<br>'), L('abonnement', 'subscription')) : '')}
    ${field(L('Rekening voor vergoedingen', 'Bank account for refunds'), `<span class="val">${iban}</span>`, `${a.iban_last4 ? help('bank', bankHelp, L('rekening', 'bank account')) : ''}${editBtn('bank', !!a.iban_last4)}`)}
    ${state.edit === 'bank' ? bankForm(a) : ''}
    ${field(L('Gegevens voor Arriva', 'Details for Arriva'), `<span class="val">${arrivaVal}</span>`, `${help('arrivaH', L('Alleen nodig voor treinreizen met Arriva. Arriva vraagt je naam en e-mailadres; je IBAN en OV-chipkaartnummer komen uit je andere gegevens. Versleuteld bewaard in Windows en alleen ingevuld als je zelf op Claimen drukt.', 'Only needed for train trips with Arriva. Arriva asks for your name and e-mail address; your IBAN and OV-chipkaart number come from your other details. Stored encrypted in Windows and only entered when you press Claim yourself.'), 'Arriva')}${editBtn('arriva', !!ac?.set)}`)}
    ${state.edit === 'arriva' ? arrivaForm(ac) : ''}
    ${field(L('E-mail voor meldingen', 'E-mail for notifications'), `<span class="val">${a.notify_email ? `${h(a.notify_email)}${state.settings && !state.settings.smtp_password_set ? ` <span class="none warn-text" title="${h(L(`${mailPwLabel(a.notify_email)} ontbreekt, dus je krijgt geen e-mail.`, `${mailPwLabel(a.notify_email)} missing, so you get no e-mail.`))}">(${L('onvolledig', 'incomplete')})</span>` : ''}` : `<span class="none">${L('niet ingesteld', 'not set')}</span>`}</span>`, editBtn('email', !!a.notify_email))}
    ${o.mail_unsupported_provider || (a.notify_email && isUnsupportedMail(a.notify_email)) ? `<p class="msg bad">${unsupportedMailText()}</p>` : ''}
    ${state.edit === 'email' ? emailForm(state.settings) : ''}
  </section>
  <button class="btn settings-btn" data-act="settings">${ICON.gear}${L('Instellingen', 'Settings')}</button>`;
}
function arrivaForm(c: ArrivaContact | null): string {
  const email = c?.email || state.o?.account.notify_email || state.o?.account.ns_email || '';
  const fld = (id: string, label: string, input: string, hint = '') => `<label>${label}${input}${hint ? `<span class="msg hint" id="${id}Hint">${hint}</span>` : ''}<span class="msg bad field-err" id="${id}Err" role="alert" hidden></span></label>`;
  const desc = (id: string, hint = false) => `aria-describedby="${hint ? `${id}Hint ` : ''}${id}Err"`;
  return `<div class="inline-form">${fld('arrFirst', L('Voornaam', 'First name'), `<input class="in" id="arrFirst" autocomplete="given-name" maxlength="80" ${desc('arrFirst')} value="${h(c?.first_name ?? '')}">`)}
    ${fld('arrLast', L('Achternaam', 'Last name'), `<input class="in" id="arrLast" autocomplete="family-name" maxlength="80" ${desc('arrLast')} value="${h(c?.last_name ?? '')}">`)}
    ${fld('arrEmail', L('E-mailadres (voor de bevestiging van Arriva)', 'E-mail address (for Arriva’s confirmation)'), `<input class="in" id="arrEmail" type="email" autocomplete="email" maxlength="254" ${desc('arrEmail')} value="${h(email)}">`)}
    ${fld('arrValid', L('Vervaldatum OV-chipkaart (optioneel)', 'OV-chipkaart expiry date (optional)'), `<input class="in mono" id="arrValid" data-card-key="${h(c?.expiry_card_key ?? '')}" autocomplete="off" maxlength="10" inputmode="numeric" aria-describedby="arrValidHint arrValidNote arrValidErr" value="${h(c?.card_valid_until ?? '')}"><span class="msg warn" id="arrValidNote" role="status" hidden>${pastExpiryNote()}</span>`, c?.expiry_card_last4 ? L(`Voor kaart •••• ${h(c.expiry_card_last4)}. Vul de datum in als DD-MM-JJJJ.`, `For card •••• ${h(c.expiry_card_last4)}. Enter the date as DD-MM-YYYY.`) : L('Laat leeg zolang er geen kaart aan dit veld gekoppeld kan worden.', 'Leave blank while a card cannot be linked to this field.'))}
    <div class="form-btns"><button class="btn primary" data-act="savearriva">${L('Opslaan', 'Save')}</button><button class="btn" data-act="edit" data-what="">${L('Annuleren', 'Cancel')}</button>${c?.set ? `<button class="btn danger" data-act="cleararriva">${L('Verwijderen', 'Remove')}</button>` : ''}</div></div>`;
}
/** What the consent box on Arriva's form says (read on 2026-11-23): Arriva's own sentence, quoted. */
const ARRIVA_CONSENT_NL = 'Wij gebruiken deze persoonsgegevens uitsluitend voor de in onze privacyverklaring omschreven doeleinden.';
const arrivaConsentText = () => L(
  `Het verplichte vakje “Ik ga akkoord” bij Arriva’s zin: “${ARRIVA_CONSENT_NL}”`,
  `The required “Ik ga akkoord” (I agree) box under Arriva’s sentence that it uses these personal details only for the purposes described in its privacy statement. In Dutch: “${ARRIVA_CONSENT_NL}”`,
);
/** Non-blocking note under a changed expiry date that lies in the past. */
const pastExpiryNote = () => L('Deze datum ligt in het verleden; klopt dat voor de kaart waarmee je reisde?', 'This date is in the past; is that right for the card you travelled with?');
/** Shows (or clears, with '') the error under one field of the Arriva details and marks the field. */
function arrivaFieldError(id: string, text: string): boolean {
  const inp = document.getElementById(id) as HTMLInputElement | null, msg = document.getElementById(`${id}Err`);
  if (msg) { msg.textContent = text; msg.hidden = !text; }
  if (inp) { if (text) inp.setAttribute('aria-invalid', 'true'); else inp.removeAttribute('aria-invalid'); }
  return !text;
}
function bankForm(a: Overview['account']): string {
  return `<div class="inline-form"><label>${L('Naam rekeninghouder', 'Account holder')}<input class="in" id="bankName" autocomplete="off" maxlength="${BANK_NAME_MAX}" aria-describedby="bankNameMsg"><span class="msg namemsg" id="bankNameMsg"></span></label>
    <label>IBAN<input class="in mono" id="bankIban" autocomplete="off" spellcheck="false" inputmode="text" maxlength="${IBAN_MAXLENGTH}" placeholder="NL00 BANK 0123 4567 89"><span class="ibanmsg msg" id="ibanMsg"></span></label>
    <div class="form-btns"><button class="btn primary" data-act="savebank" id="saveBank" disabled>${L('Opslaan', 'Save')}</button><button class="btn" data-act="edit" data-what="">${L('Annuleren', 'Cancel')}</button>${a.iban_last4 ? `<button class="btn danger" data-act="clearbank">${L('Verwijderen', 'Remove')}</button>` : ''}${a.iban_last4 ? help('bankform', paidToOldAccount()) : ''}</div></div>`;
}
const paidToOldAccount = () => L('Claims die al zijn ingediend, betaalt NS uit op de rekening die je toen gaf.', 'Claims already submitted are paid by NS into the account you gave at the time.');
const unsupportedMailText = () => unsupportedMailIn(getLang());
const tooLargeText = () => backupTooLargeText(getLang());
function emailForm(s: any): string {
  const m = s?.smtp ?? {};
  const known = !!mailPreset(m.to || '');
  const manual = state.mailManual || (!!m.to && !known);
  const to = m.to || '';
  return `<div class="inline-form">
    <label>${L('Jouw e-mailadres', 'Your e-mail address')}<input class="in" id="smtpTo" value="${h(to)}" placeholder="${L('naam@example.com', 'name@example.com')}" autocomplete="email"></label>
    <div id="mailHint">${manual ? '' : mailSteps(to, 'data-act="openlink"')}</div>
    <label><span id="smtpPwLabel">${mailPwLabel(to)}</span><input class="in" id="smtpPw" type="password" autocomplete="off" placeholder="${s?.smtp_password_set ? L('opgeslagen', 'stored') : ''}"></label>
    ${manual ? `<label>${L('Server voor uitgaande mail', 'Outgoing mail server')}<input class="in" id="smtpHost" value="${h(m.host)}"></label>
    <label>${L('Poort en beveiliging', 'Port and security')}<span class="row-in"><input class="in" id="smtpPort" type="number" value="${h(m.port || 587)}"><select class="in" id="smtpSec"><option value="starttls" ${m.security !== 'tls' ? 'selected' : ''}>STARTTLS</option><option value="tls" ${m.security === 'tls' ? 'selected' : ''}>TLS</option></select></span></label>
    <label>${L('Gebruikersnaam', 'Username')}<input class="in" id="smtpUser" value="${h(m.username)}"></label>` : `<button class="btn ghost small" data-act="mailmanual">${L('Geavanceerd', 'Advanced')}</button>`}
    <div class="form-btns"><button class="btn primary" data-act="saveemail">${L('Opslaan', 'Save')}</button><button class="btn" data-act="testmail">${L('Testmail', 'Test e-mail')}</button><button class="btn ghost" data-act="edit" data-what="">${L('Sluiten', 'Close')}</button>${help('mailform', L('OV Refund stuurt het overzicht vanaf je eigen mailbox naar jezelf. Het wachtwoord wordt versleuteld bewaard in Windows.', 'OV Refund sends the summary from your own mailbox to yourself. The password is stored encrypted in Windows.'))}</div>
  </div>`;
}

// ---------- Settings dialog ----------
let setDlg: HTMLDialogElement | null = null;
let setOpener: HTMLElement | null = null;
const TABS: [Tab, string, string][] = [['account', 'Account & NS', 'Account & NS'], ['auto', 'Automatisch', 'Automatic'], ['notify', 'Meldingen', 'Notifications'], ['data', 'Gegevens & back-up', 'Data & backup'], ['help', 'Hulp', 'Help'], ['erase', 'Gegevens wissen', 'Erase data']];
/** A setting with a switch: label, optional "?" and an optional short line under it. */
const toggle = (id: string, label: string, on: boolean, opts: { disabled?: boolean; help?: string; note?: string; extra?: string } = {}) =>
  `<div class="set-row"><label class="check"><input type="checkbox" class="switch" id="${id}" ${on ? 'checked' : ''} ${opts.disabled ? 'disabled' : ''}><span>${label}</span></label>${opts.help ?? ''}${opts.extra ? `<span class="grow"></span>${opts.extra}` : ''}</div>${opts.note ?? ''}`;
const actRow = (title: string, sub: string, btn: string) => `<div class="set-row"><div class="set-t"><b>${title}</b>${sub ? `<span class="msg">${sub}</span>` : ''}</div><span class="grow"></span>${btn}</div>`;
/** A button after a blocker that goes to where it can be fixed (none when the control is right above it). */
const blockerButton = (key: string) => key === 'bank' ? ` <button class="btn small" data-act="gobank">${L('Rekening instellen', 'Set up bank account')}</button>`
  : key === 'import' ? ` <button class="btn small" data-act="gotab" data-what="data">${L('Naar Gegevens & back-up', 'Go to Data & backup')}</button>` : '';
/** Claims with an uncertain outcome (or stuck while being submitted): automatic submission waits for them. */
const unresolvedRows = () => state.rows.filter((r) => !isArrivaTrip(r) && (r.claim_status === 'INDETERMINATE' || (r.claim_status === 'SUBMITTING' && !state.busy)));
/** Why automatic submission is paused although it is on ('' when it isn't); a newer backend may say so itself. */
function autoPause(o: Overview | null): string {
  const own = (o as any)?.auto_paused_reason;
  if (typeof own === 'string' && own.trim()) return pickLang(own);
  return autoPauseText({ unresolved: Math.max(unresolvedRows().length, o?.uncertain_ns ?? o?.uncertain ?? 0), claimListUnavailable: o?.claim_list_status === 'unavailable' }, getLang());
}
/** "Bekijken" for a paused run: the first claim it waits for (none when the reason is not a claim). */
const pauseTarget = () => unresolvedRows().find((r) => r.claim_status === 'INDETERMINATE') ?? unresolvedRows()[0];
const pauseButton = () => { const r = pauseTarget(); return r ? ` <button class="btn small" data-act="gofirst" data-status="${r.claim_status}">${L('Bekijken', 'Review')}</button>` : ''; };
const pauseNote = (o: Overview) => { const t = autoPause(o); return t ? `<p class="msg warn sub-row">${h(t)}${pauseButton()}</p>` : ''; };
function settingsPanel(o: Overview): string {
  const a = o.account; const s = state.settings;
  const sched = o.schedule;
  // The claim-service question must be answered ("none", or "import_done" after a service) before automatic submission.
  const svc = o.other_service ?? null;
  const otherService = svc === 'yes';
  const svcOk = svc === 'none' || svc === 'import_done';
  const auto = sched.installed && svcOk && o.mode === 'automatic' && !!o.auto_opt_in_at;
  const mailReady = !!a.notify_email && !!s?.smtp_password_set;
  const silent = !!s && sched.installed && !s.notify_windows && !(s.notify_email && mailReady)
    ? `<p class="note warn">${ICON.warn}<span>${L('Alle meldingen staan uit. Je hoort dan niets van de automatische controles.', 'All notifications are off. You won’t hear anything from the automatic checks.')}</span></p>` : '';
  const busy = state.busy ? 'disabled' : '';
  switch (state.tab) {
    case 'account': return `
      ${actRow('Mijn NS', [a.ns_email ? h(a.ns_email) : '', o.auth_state === 'ok' ? L('verbonden', 'connected') : L('niet verbonden', 'not connected')].filter(Boolean).join(' · '), `<button class="btn" data-act="login" ${busy}>${L('Opnieuw inloggen', 'Log in again')}</button>`)}
      ${(o as any).autologin_disabled ? `<p class="note warn">${ICON.warn}<span>${L('Automatisch inloggen staat uit na twee mislukte pogingen.', 'Automatic login was turned off after two failed attempts.')} ${help('aldis', L('Klopt je NS-wachtwoord nog? Vul het opnieuw in en zet het weer aan.', 'Is your NS password still right? Enter it again and switch it back on.'))}</span></p>` : ''}
      ${toggle('autoLogin', L('Automatisch inloggen bij NS', 'Log in to NS automatically'), !!(o as any).auto_login || state.edit === 'nslogin', { help: help('al', L('Dan loggen automatische controles zelf in. Je NS-wachtwoord wordt versleuteld bewaard in Windows en alleen op de inlogpagina van NS ingevuld. Als je dit uitzet, wordt het wachtwoord gewist.', 'Automatic checks then log in by themselves. Your NS password is stored encrypted in Windows and only entered on the NS login page. Turning this off deletes the password.') + (sched.installed && !(o as any).auto_login ? `<br>${L('Zonder dit krijg je een herinnering om OV Refund te openen.', 'Without it you get a reminder to open OV Refund.')}` : '')) })}
      ${state.edit === 'nslogin' ? `<div class="inline-form"><label>${L('E-mailadres voor Mijn NS', 'Mijn NS e-mail address')}<input class="in" id="nsEmail" autocomplete="off" value="${h(a.ns_email ?? '')}"></label><label>${L('NS-wachtwoord', 'NS password')}<input class="in" id="nsPw" type="password" autocomplete="off"></label><div class="form-btns"><button class="btn primary" data-act="savenslogin">${L('Opslaan', 'Save')}</button><button class="btn" data-act="edit" data-what="">${L('Annuleren', 'Cancel')}</button></div></div>` : ''}
      ${actRow(L('Volledige reishistorie', 'Full travel history'), L('Alles opnieuw ophalen bij NS', 'Fetch everything from NS again'), `<button class="btn" data-act="fullscan" ${busy}>${L('Opnieuw ophalen', 'Fetch again')}</button>`)}`;
    case 'auto': {
      const blockers = autoSubmitBlockers({ checksOn: sched.installed, bank: !!a.iban_last4, otherService: svc }, getLang());
      return `
      ${toggle('schedOn', L('Automatisch controleren', 'Check automatically'), sched.installed, {
        help: help('sched', L('Dit gebeurt op de achtergrond. Stond je computer uit? Dan gebeurt het de volgende keer dat je inlogt op Windows.', 'This runs in the background. Was your computer off? Then it runs the next time you sign in to Windows.')),
        extra: `<select class="in small" id="interval" aria-label="${h(L('Hoe vaak', 'How often'))}" ${sched.installed ? '' : 'disabled'}>${[1, 3, 7, 14, 30].map((d) => `<option value="${d}" ${(sched.interval_days ?? o.interval_days) === d ? 'selected' : ''}>${d === 1 ? L('elke dag', 'daily') : L(`elke ${d} dagen`, `every ${d} days`)}</option>`).join('')}</select>`,
        note: sched.installed && sched.next_run ? `<p class="msg sub-row">${L('Volgende controle', 'Next check')}: ${localDateTime(sched.next_run, getLang())}</p>` : '',
      })}
      <div class="set-row svc-q" role="radiogroup" aria-labelledby="svcQT"><span id="svcQT">${L('Gebruik je een claimdienst, nu of eerder?', 'Do you use a claim service, now or before?')}</span>${help('svcq', L('Bijvoorbeeld Trein-vertraging.nl. Zeg die dienst eerst op, anders wordt dezelfde reis twee keer geclaimd.', 'For example Trein-vertraging.nl. Cancel that service first, or the same trip gets claimed twice.'))}<span class="grow"></span>
        <label class="check"><input type="radio" name="svcQ" id="svcQYes" ${svc === 'yes' || svc === 'import_done' ? 'checked' : ''}><span>${L('Ja', 'Yes')}</span></label>
        <label class="check"><input type="radio" name="svcQ" id="svcQNo" ${svc === 'none' ? 'checked' : ''}><span>${L('Nee', 'No')}</span></label></div>
      ${toggle('autoSubmit', L('Claims automatisch indienen', 'Submit claims automatically'), auto, {
        disabled: !auto && blockers.length > 0,
        help: help('autosub', [L('OV Refund dient alleen reizen met duidelijk bewijs in. Twijfelgevallen wachten op jou.', 'OV Refund only submits trips with clear evidence. Unclear cases wait for you.'), (o as any).demo ? L('In de demo wordt niets echt ingediend.', 'Nothing is really submitted in the demo.') : (o as any).gateway_verified_by_submission ? '' : L('Automatisch indienen is nog niet bevestigd door NS. Kijk na de eerste automatische claim even in Mijn teruggaven.', 'Automatic submission hasn’t been confirmed with NS yet. After the first automatic claim, have a look in Mijn teruggaven.')].filter(Boolean).join('<br>')),
        // Always say why it can't be switched on, or why it is paused.
        note: auto ? `${pauseNote(o)}<p class="msg sub-row">${L('Automatische controles dienen elke geschikte claim één voor één in. Bij Controleer nu worden alleen claims die binnen 14 dagen verlopen automatisch ingediend. Loopt een termijn af vóór de volgende controle, dan plant OV Refund één extra controle.', 'Automatic checks submit every qualifying claim, one by one. Check now only submits claims that expire within 14 days automatically. If a deadline falls before the next check, OV Refund plans one extra check.')}</p>${(sched as any).deadline_run ? `<p class="msg sub-row">${L('Extra controle', 'Extra check')}: ${localDateTime((sched as any).deadline_run, getLang())}</p>` : ''}`
          : blockers.map((b) => `<p class="msg${svc === 'yes' || svc == null ? ' warn' : ''} sub-row">${b.text}${blockerButton(b.key)}</p>`).join(''),
      })}

      ${silent}`;
    }
    case 'notify': {
      const skipped = o.ns_mail_skipped?.total ?? 0;
      const nm = (o as any).ns_mail;
      const mailInfo = nm?.total || skipped ? `<br>${nm?.total ?? 0} ${L('NS-mails verwerkt', 'NS e-mails processed')}${nm?.unmatched ? `, ${nm.unmatched} ${L('niet aan een reis gekoppeld', 'not linked to a trip')}` : ''}${skipped ? `, ${skipped} ${L('overgeslagen', 'skipped')}` : ''}.` : '';
      // The e-mail summary needs an address and its (app) password; the button opens the e-mail form.
      const needMail = !a.notify_email
        ? `<p class="msg sub-row">${L('Stel eerst je e-mail in bij Jouw gegevens.', 'Set up your e-mail under Your details first.')} <button class="btn small" data-act="gomail">${L('E-mail instellen', 'Set up e-mail')}</button></p>`
        : !s?.smtp_password_set ? `<p class="msg warn sub-row">${mailPwLabel(a.notify_email)} ${L('ontbreekt.', 'missing.')} <button class="btn small" data-act="gomail">${L('Wachtwoord invullen', 'Enter password')}</button></p>` : '';
      // NS-looking mails skipped because the provider's headers can't prove they came from NS.
      const notAuth = o.ns_mail_skipped?.by_reason?.not_authenticated ?? 0;
      const unsupported = notAuth && (o as any).read_ns_mail ? `<p class="msg warn sub-row">${L(`${notAuth === 1 ? '1 NS-mail' : `${notAuth} NS-mails`} overgeslagen. OV Refund kan bij jouw mailprovider niet controleren of een mail echt van NS komt.`, `${notAuth === 1 ? '1 NS e-mail' : `${notAuth} NS e-mails`} skipped. OV Refund can’t verify with your mail provider that an e-mail really comes from NS.`)}</p>` : '';
      const winOff = !s?.notify_windows ? ` disabled title="${h(L('Zet eerst Windows-meldingen aan', 'Turn on Windows notifications first'))}"` : '';
      return `
      ${toggle('nWin', L('Windows-meldingen', 'Windows notifications'), !!s?.notify_windows, { help: help('nwin', L('Zie je geen melding? Kijk in Windows bij Instellingen → Systeem → Meldingen → OV Refund.', 'No notification? In Windows, check Settings → System → Notifications → OV Refund.')), extra: `<button class="btn small" data-act="testnotify"${winOff}>${L('Testmelding', 'Test notification')}</button>` })}
      ${toggle('nMail', L('E-mailoverzicht', 'E-mail summary'), !!s?.notify_email && mailReady, { disabled: !mailReady, note: needMail })}
      ${toggle('readMail', L('NS-mails lezen', 'Read NS e-mails'), !!(o as any).read_ns_mail, { disabled: !a.notify_email, help: help('readmail', L('OV Refund leest in je mailbox alleen de mails van NS over Geld terug bij vertraging. Zo werkt het statussen en bedragen vanzelf bij, ook voor claims via andere diensten.', 'OV Refund only reads the NS delay-refund e-mails in your mailbox. It uses them to update statuses and amounts, also for claims made through other services.') + mailInfo), extra: a.notify_email ? `<button class="btn small" data-act="syncmail" ${busy}>${L('Nu lezen', 'Read now')}</button>` : '', note: unsupported })}
      ${silent}`;
    }
    case 'data': {
      const dir = o.export_dir || L('Documenten\\OV Refund', 'Documents\\OV Refund');
      const ex = state.externals;
      return `
      <h3 class="set-h">${L('Back-up', 'Backup')}</h3>
      <div class="btn-row"><button class="btn" data-act="backup">${L('Back-up maken', 'Make a backup')}</button><button class="btn" data-act="restorebackup">${L('Back-up terugzetten', 'Restore backup')}</button><button class="btn" data-act="csv">${L('Claims exporteren (CSV)', 'Export claims (CSV)')}</button><input type="file" id="restoreFile" accept=".ovrbackup" hidden></div>
      <div class="set-row"><span class="msg path" title="${h(dir)}">${L('Map', 'Folder')}: ${h(shortPath(dir, 44))}</span>${help('export', exportInfo(o, getLang()).map(h).join('<br>'))}<span class="grow"></span><button class="btn small" data-act="datafolder">${L('Open map', 'Open folder')}</button></div>
      <h3 class="set-h">${L('Elders geclaimd', 'Claimed elsewhere')} ${help('ext', L('Reizen die je zelf of via een claimdienst al hebt geclaimd. OV Refund dient die nooit in.', 'Trips you already claimed yourself or through a claim service. OV Refund never submits them.'))}</h3>
      ${otherService ? `<div class="note warn">${ICON.warn}<span>${L('Je gebruikte een claimdienst. Voeg de reizen toe die de dienst al claimde en klik dan op Import klaar.', 'You used a claim service. Add the trips it already claimed, then click Import done.')}</span><button class="btn primary small" data-act="importdone">${L('Import klaar', 'Import done')}</button></div>` : ''}
      <h4>${L('Zelf toegevoegde reizen', 'Manually added trips')}</h4>${ex === null ? `<p class="msg">${L('Laden…', 'Loading…')}</p>` : ex.length ? `<ul class="ext-list">${ex.map((x) => `<li><span>${h(localDate(x.date, getLang()))}</span><span class="grow">${h(x.origin ?? '?')} → ${h(x.destination ?? '?')}</span><button class="btn ghost small" data-act="delext" data-id="${x.id}" aria-label="${h(L(`Verwijderen: ${x.origin ?? ''} → ${x.destination ?? ''}`, `Remove: ${x.origin ?? ''} → ${x.destination ?? ''}`))}">${L('Verwijderen', 'Remove')}</button></li>`).join('')}</ul>` : `<p class="msg">${L('Nog geen zelf toegevoegde reizen.', 'No manually added trips yet.')}</p>`}
      ${state.edit === 'import' ? importForm() : `<button class="btn" data-act="edit" data-what="import">${L('Reis toevoegen', 'Add a trip')}</button>`}`;
    }
    case 'help': return `
      <div class="btn-row"><button class="btn" data-act="howto">${L('Zo dien je een claim in', 'How to submit a claim')}</button><button class="btn" data-act="bugreport">${L('Bug melden', 'Report a bug')}</button><button class="btn" data-act="website">${L('Website', 'Website')} ${ICON.ext}</button></div>
      ${actRow(L('Diagnosegegevens', 'Diagnostics'), L('Controleer het bestand voordat je het deelt', 'Check the file before sharing it'), `<button class="btn" data-act="diag">${L('Exporteren', 'Export')}</button>`)}
      ${actRow(`OV Refund ${h((o as any).version ?? '')}`, '', updateButton())}
      <p class="msg">${L('OV Refund werkt met een OV-chipkaart die aan Mijn NS is gekoppeld. Reizen betaald met je bankpas of de nieuwe OV-pas (OVpay) worden nog niet ondersteund.', 'OV Refund works with an OV-chipkaart linked to Mijn NS. Trips paid with a bank card or the new OV-pas (OVpay) are not supported yet.')}</p>
      <p class="msg">${L('Je gegevens blijven op deze computer. Geen account, geen commissie. Niet verbonden aan NS of Arriva. Meer over privacy staat in de disclaimer op de website.', 'Your data stays on this computer. No account, no commission. Not affiliated with NS or Arriva. More about privacy is in the disclaimer on the website.')}</p>`;
    case 'erase': return `<div class="danger-zone">
      ${actRow(L('Uitloggen bij NS', 'Log out of NS'), L('Wist je opgeslagen NS-wachtwoord', 'Deletes your saved NS password'), `<button class="btn danger" data-act="disconnect" ${busy}>${L('Uitloggen', 'Log out')}</button>`)}
      ${actRow(L('Lokale geschiedenis wissen', 'Erase local history'), L('Ingediende claims blijven bewaard', 'Submitted claims are kept'), `<button class="btn danger" data-act="wipe">${L('Wissen', 'Erase')}</button>`)}
      ${actRow(L('Alles vergeten', 'Forget everything'), L('Alle gegevens en wachtwoorden, ook welke claims al zijn ingediend', 'All data and passwords, including which claims were already submitted'), `<button class="btn danger" data-act="forget">${L('Alles wissen', 'Erase all')}</button>`)}
    </div>`;
  }
}
function settingsHtml(o: Overview): string {
  return `${dialogHead(L('Instellingen', 'Settings'), 'setT')}<div class="set-body">
    <div class="tabs" role="tablist" aria-orientation="vertical" aria-label="${h(L('Instellingen', 'Settings'))}">${TABS.map(([k, nl, en]) => `<button role="tab" class="tab${k === 'erase' ? ' tab-danger' : ''}" id="tab-${k}" data-tab="${k}" aria-selected="${state.tab === k}" aria-controls="setPanel" tabindex="${state.tab === k ? 0 : -1}">${L(nl, en)}</button>`).join('')}</div>
    <div class="panel" id="setPanel" role="tabpanel" aria-labelledby="tab-${state.tab}">${state.busy ? `<p class="msg" role="status">${L('Even wachten: er loopt een controle.', 'Wait a moment: a check is running.')}</p>` : ''}${settingsPanel(o)}</div></div>`;
}
async function loadExternals() {
  try { state.externals = await invoke<ExternalClaim[]>('external_claims'); } catch { state.externals = []; }
  if (setDlg?.open) render();
}
function openSettings(tab?: Tab, opener?: HTMLElement | null) {
  if (!state.o) return;
  if (tab) state.tab = tab;
  if (!setDlg) {
    setDlg = document.createElement('dialog');
    setDlg.className = 'settings';
    setDlg.setAttribute('aria-labelledby', 'setT');
    document.body.appendChild(setDlg);
    setDlg.addEventListener('input', onInput);
    setDlg.addEventListener('change', onChange);
    setDlg.addEventListener('click', onClick);
    setDlg.addEventListener('keydown', onKeydown);
    setDlg.addEventListener('cancel', (e) => { if (state.edit === 'nslogin' || state.edit === 'import') { e.preventDefault(); closeInlineForm(); } });
    setDlg.addEventListener('close', () => {
      if (state.edit === 'nslogin' || state.edit === 'import') { state.edit = ''; renderedEdit = ''; }
      const back = setOpener?.isConnected ? setOpener : app.querySelector<HTMLElement>('[data-act="settings"]');
      setOpener = null;
      // A jump out of Settings (e.g. to a trip or the bank form) already put focus where it belongs.
      const a = document.activeElement;
      if (a && a !== document.body && a.isConnected && !setDlg?.contains(a)) return;
      back?.focus({ preventScroll: true });
    });
  }
  if (!setDlg.open) {
    setOpener = opener ?? (document.activeElement as HTMLElement | null);
    paint(setDlg, settingsHtml(state.o));
    setDlg.showModal();
    setDlg.querySelector<HTMLElement>('[role=tab][aria-selected=true]')?.focus({ preventScroll: true });
    if (state.edit === 'nslogin') setDlg.querySelector<HTMLElement>('#nsEmail')?.focus({ preventScroll: true });
  } else render();
  if (state.tab === 'data') loadExternals();
}
const closeSettings = () => { if (setDlg?.open) setDlg.close(); };

function importForm(): string {
  const today = localDay();
  const names = [...new Set(state.rows.flatMap((r) => [r.journey.origin.name, r.journey.destination?.name ?? '']).filter(Boolean))].sort();
  return `<div class="inline-form">
    <label>${L('Reisdatum', 'Travel date')}<input class="in" id="exDate" type="date" max="${today}"></label>
    <label>${L('Van', 'From')}<input class="in" id="exFrom" maxlength="60" list="stationList" autocomplete="off"></label>
    <label>${L('Naar', 'To')}<input class="in" id="exTo" maxlength="60" list="stationList" autocomplete="off"></label>
    <datalist id="stationList">${names.map((n) => `<option value="${h(n)}">`).join('')}</datalist>
    <div class="form-btns"><button class="btn primary" data-act="addext">${L('Toevoegen', 'Add')}</button><button class="btn" data-act="edit" data-what="">${L('Annuleren', 'Cancel')}</button></div>
  </div>`;
}

// ---------- main ----------
function summary(o: Overview): string {
  const t = o.totals;
  const got = t.paid.cents + t.external.cents, gotN = t.paid.count + t.external.count;
  const claims = (n: number) => (n === 1 ? '1 claim' : `${n} claims`);
  return `<div class="summary">
    <div class="tile"><div class="l">${L('Ontvangen', 'Received')} ${help('tot', L('Alle periodes samen. Telt ook de bedragen mee van claims via een andere dienst.', 'All periods together. Also counts the amounts from claims made through another service.'))}</div><div class="n">${euro(got, getLang())}</div><div class="s">${claims(gotN)}</div></div>
    <div class="tile"><div class="l">${L('Onderweg', 'On its way')}</div><div class="n">${euro(t.pending.cents, getLang())}</div><div class="s">${claims(t.pending.count)}${t.pending.unknown ? L(` + ${t.pending.unknown} ${t.pending.unknown === 1 ? 'bedrag' : 'bedragen'} onbekend`, ` + ${t.pending.unknown} ${t.pending.unknown === 1 ? 'amount' : 'amounts'} unknown`) : ''}</div></div>
  </div>`;
}
/** Whether a row belongs to a filter tab. */
function inFilter(r: Row, f: Filter): boolean {
  if (f === 'all') return true;
  if (isArrivaDemoFixture(r)) return f === 'claimable' && arrivaDemoTrip(r).status === 'claimable' && arrivaStaysClaimable(arrivaClaimState(state.arriva, state.arrivaClaiming));
  // An Arriva trip that meets Arriva's rules is claimable (Claim), also after a failed attempt.
  if (isArrivaReal(r) && arrivaClaimable(r)) return f === 'claimable';
  // A claim being submitted stays under Claimable (shown as in progress) until NS's outcome is final.
  if (r.claim_status === 'SUBMITTING') return f === 'claimable';
  const c = category(r);
  if (f === 'review') return c === 'claimable' && isReview(r);
  if (f === 'claimable') return c === 'claimable' && !isReview(r);
  return c === f;
}
/** The "next step" banner above the list: one short sentence and at most one main button. */
function nextStepBanner(o: Overview, topLogin: boolean): string {
  // Trips still to claim: not those already being handled.
  const ready = state.rows.filter((r) => inFilter(r, 'claimable') && r.claim_status !== 'SUBMITTING' && !(isArrivaDemoFixture(r) && arrivaClaimState(state.arriva, state.arrivaClaiming) === 'in_progress'));
  const ns = nextStep({
    auth: authOf(o), claimList: o.claim_list_status ?? null, hasRows: state.rows.length > 0,
    claimable: ready.length, claimableCents: ready.reduce((n, r) => n + (r.compensation_cents ?? 0), 0),
    claimableUnknown: ready.filter((r) => r.compensation_cents === null).length, review: state.rows.filter((r) => inFilter(r, 'review')).length,
  }, getLang());
  if (!ns) return '';
  const review = state.rows.filter((r) => inFilter(r, 'review')).length;
  // The counts cover all periods: say so when the list shows a shorter period.
  const scope = state.period !== 'all' && (ns.action === 'claimable' || ns.action === 'review') ? L(' (alle periodes)', ' (all periods)') : '';
  const reviewBtn = ns.action === 'claimable' && review ? `<button class="btn" data-act="gofirst" data-kind="review">${L('Beoordelen', 'Review')}</button>` : '';
  const btn = ns.action === 'login' && !topLogin ? `<button class="btn primary" data-act="login" ${state.busy ? 'disabled' : ''}>${L('Inloggen', 'Log in')}</button>`
    : ns.action === 'autologin' ? `<button class="btn primary" data-act="fixautologin">${L('Wachtwoord aanpassen', 'Update password')}</button>`
    : ns.action === 'claimable' ? `<button class="btn primary" data-act="gofirst" data-kind="claimable">${L('Claimen', 'Claim')}</button>`
    : ns.action === 'review' ? `<button class="btn" data-act="gofirst" data-kind="review">${L('Beoordelen', 'Review')}</button>` : '';
  return `<div class="banner ${ns.tone}" role="status"><span class="b-text">${h(ns.text + scope)}${ns.help ? ` ${help('next', h(ns.help))}` : ''}</span><span class="grow"></span>${reviewBtn}${btn}</div>`;
}
function uncertainBanner(o: Overview): string {
  const arriva = o.uncertain_arriva ?? 0;
  const ns = o.uncertain_ns ?? Math.max(0, o.uncertain - arriva);
  const arrivaBanner = arriva ? `<div class="banner bad" role="status"><span class="b-text">${arriva === 1 ? L('1 Arriva-claim met een onzekere uitkomst', '1 Arriva claim with an uncertain outcome') : L(`${arriva} Arriva-claims met een onzekere uitkomst`, `${arriva} Arriva claims with an uncertain outcome`)} ${help('unca', L('Kijk in je e-mail of Arriva een bevestiging stuurde, en zet de status in de reis. OV Refund probeert een Arriva-claim nooit vanzelf opnieuw.', 'Check your e-mail for a confirmation from Arriva, and set the status in the trip. OV Refund never retries an Arriva claim by itself.'))}</span><span class="grow"></span><button class="btn" data-act="gofirst" data-status="INDETERMINATE" data-carrier="arriva">${L('Beoordelen', 'Review')}</button></div>` : '';
  if (!ns) return arrivaBanner;
  return arrivaBanner + `<div class="banner bad" role="status"><span class="b-text">${ns === 1 ? L('1 claim met een onzekere uitkomst', '1 claim with an uncertain outcome') : L(`${ns} claims met een onzekere uitkomst`, `${ns} claims with an uncertain outcome`)} ${help('unc', L('Automatisch indienen wacht tot dit is opgehelderd. Open de reis en volg de stap daar.', 'Automatic submission waits until this is resolved. Open the trip and follow the step there.'))}</span><span class="grow"></span><button class="btn" data-act="gofirst" data-status="INDETERMINATE" data-carrier="ns">${L('Beoordelen', 'Review')}</button></div>`;
}
/** After restoring a backup: what still needs doing, until the next check or until closed. */
function restoreBanner(o: Overview): string {
  if (!state.restored) return '';
  const loggedIn = o.auth_state === 'ok';
  const lines = restoreSummary(state.restored, getLang(), { recheck: true, loggedIn });
  if (!lines.length) return '';
  // "Check now" only when the restored trips still need a new assessment (and you are logged in).
  const recheck = lines.length > restoreSummary(state.restored, getLang()).length;
  const btn = recheck && loggedIn ? `<button class="btn primary" data-act="scan" data-kind="restore" ${state.busy ? 'disabled' : ''}>${L('Controleer nu', 'Check now')}</button>` : '';
  return `<div class="banner${recheck ? ' warn' : ' ok'}" role="status"><span class="b-text">${h(lines.join(' '))}</span><span class="grow"></span>${btn}<button class="x" data-act="dismissrestore" aria-label="${h(L('Melding sluiten', 'Close notice'))}">${ICON.close}</button></div>`;
}
function toolbar(): string {
  const inP = state.rows.filter((r) => inPeriod(r.journey.check_in, state.period));
  const n = (f: Filter) => inP.filter((r) => inFilter(r, f)).length;
  const periods: [Period, string, string][] = [['week', '7 dagen', '7 days'], ['month', '30 dagen', '30 days'], ['year', 'Jaar', 'Year'], ['all', 'Alles', 'All']];
  const cats: [Filter, string, string, number][] = [['all', 'Alle reizen', 'All trips', inP.length], ['claimable', 'Te claimen', 'Claimable', n('claimable')], ['review', 'Beoordelen', 'Review', n('review')], ['claimed', 'Geclaimd', 'Claimed', n('claimed')], ['expired', 'Verlopen', 'Expired', n('expired')]];
  const tabsHelp = L('Te claimen: klaar om in te dienen. Beoordelen: kijk eerst zelf of het klopt. Geclaimd: ingediend of elders geclaimd. Verlopen: de 3 maanden zijn voorbij.<br>De periode filtert alleen de lijst.', 'Claimable: ready to submit. Review: check it yourself first. Claimed: submitted or claimed elsewhere. Expired: the 3 months have passed.<br>The period only filters the list.');
  return `<div class="toolbar">
    <div class="seg cats" role="group" aria-label="${h(L('Toon', 'Show'))}">${cats.map(([k, nl, en, c]) => `<button data-cat="${k}" class="${state.cat === k ? 'on' : ''}" aria-pressed="${state.cat === k}">${L(nl, en)}${c ? `<span class="c">${c}</span>` : ''}</button>`).join('')}</div>${help('tabs', tabsHelp, L('tabbladen', 'tabs'))}
    <span class="grow"></span>
    <div class="seg periods" role="group" aria-label="${h(L('Periode', 'Period'))}">${periods.map(([k, nl, en]) => `<button data-period="${k}" class="${state.period === k ? 'on' : ''}" aria-pressed="${state.period === k}">${L(nl, en)}</button>`).join('')}</div>
  </div>`;
}
/** A row whose claim still needs the user's own review ("Beoordelen"). */
const isReview = (r: Row) => r.claim_status === 'NEEDS_REVIEW' || r.claim_status === 'CANDIDATE' || r.claim_status === 'FAILED';
/** Whether an earlier submission of this claim may have reached NS (backend flag, else derived from its history). */
function wasUncertain(r: Row): boolean {
  if (typeof r.was_uncertain === 'boolean') return r.was_uncertain;
  const ev = r.claim_id ? state.claims.get(r.claim_id)?.events ?? [] : [];
  return ev.some((e) => e.to_status === 'INDETERMINATE' || e.from_status === 'INDETERMINATE' || (e.from_status === 'SUBMITTING' && e.to_status === 'FAILED' && e.actor !== 'gateway'));
}
const statusLabel = (code: string) => (STATUS[code]?.[0] ? L(STATUS[code][0], STATUS[code][1]) : code);
const note = (text: string, extra = '', tone = '') => `<p class="note${tone ? ` ${tone}` : ''}">${tone === 'warn' ? ICON.warn : ''}<span>${text} ${extra}</span></p>`;
/** A check-out and new check-in at the same station within 15 min splits a trip: NS assesses each part separately. */
function splitNote(r: Row): string {
  const t = +new Date(r.journey.check_in);
  const prev = state.rows.find((x) => x.journey.destination?.uic === r.journey.origin.uic && x.journey.check_out && t - +new Date(x.journey.check_out) >= 0 && t - +new Date(x.journey.check_out) <= 15 * 60000);
  const next = state.rows.find((x) => r.journey.destination?.uic === x.journey.origin.uic && r.journey.check_out && +new Date(x.journey.check_in) - +new Date(r.journey.check_out) >= 0 && +new Date(x.journey.check_in) - +new Date(r.journey.check_out) <= 15 * 60000);
  const other = prev ?? next;
  if (!other) return '';
  const st = prev ? r.journey.origin.name : r.journey.destination?.name ?? '';
  const gap = Math.round(Math.abs((prev ? t - +new Date(prev.journey.check_out!) : +new Date(next!.journey.check_in) - +new Date(r.journey.check_out!)) / 60000));
  return note(L('Gesplitste reis', 'Split trip'), help(`split${r.id}`, L(`Je checkte in ${h(st)} uit en binnen ${gap} min weer in. NS beoordeelt beide delen apart. Tip: blijf ingecheckt als je overstapt tussen NS-treinen.`, `You checked out at ${h(st)} and back in within ${gap} min. NS assesses each part separately. Tip: stay checked in when you change between NS trains.`)));
}

function trainsHtml(r: Row): string {
  // station names instead of codes for the start and end of the trip
  const stationName = (code: string, i: number, n: number, end: 'from' | 'to') =>
    end === 'from' && i === 0 ? r.journey.origin.name : end === 'to' && i === n - 1 ? (r.journey.destination?.name ?? code) : code;
  // The planned route shows the timetable only; the delays belong to the train actually taken.
  const leg = (late: boolean) => (l: Leg, i: number, all: Leg[]) => {
    const dd = mins(l.dep_actual, l.dep_planned), ad = mins(l.arr_actual, l.arr_planned);
    const op = operatorLabel('', l.company);
    return `${h(l.train_type)} ${h(l.train_number)} <span class="operator-badge ${op.kind}">${h(op.label)}</span>: ${h(stationName(l.from_code, i, all.length, 'from'))} ${localTime(l.dep_planned)}${late && dd > 0 ? ` <span class="delay">+${dd}</span>` : ''} → ${h(stationName(l.to_code, i, all.length, 'to'))} ${localTime(l.arr_planned)}${late && ad > 0 ? ` <span class="delay${ad >= 30 ? ' big' : ''}">+${ad}</span>` : ''}${l.cancelled ? ` <span class="delay big">${L('uitgevallen', 'cancelled')}</span>` : ''}`;
  };
  const route = (x: Route | null, late: boolean) => (x ? x.legs.map(leg(late)).join('<br>') : '–');
  const d = displayDelay(r);
  const planned = route(r.planned, false), taken = route(r.taken, true);
  const threshold = isArrivaTrip(r) ? r.arriva?.min_delay : 30;
  // The same trains without any delay: say so instead of repeating the planned lines.
  const takenText = r.planned && r.taken && planned === taken ? L('Zoals gepland, zonder vertraging', 'As planned, without delay') : taken;
  return `<details class="tech" data-k="tech-${r.id}"><summary>${L('Treindetails', 'Train details')}</summary><div class="trains">
    <span class="k">${L('Gepland', 'Planned')}</span><span>${planned}</span>
    <span class="k">${L('Genomen', 'Taken')}</span><span>${takenText}</span>
    ${d !== null && threshold != null && (d >= 20 || r.claim_status) ? `<span class="k">${L('Drempel', 'Threshold')}</span><span>${d >= 60 ? 60 : threshold} min</span>` : ''}
    <span class="k">${L('Uitgecheckt', 'Checked out')}</span><span>${localTime(r.journey.check_out)}${r.proxy_min !== null ? ` <span class="msg">(${minutes(r.proxy_min)} ${L('t.o.v. geplande aankomst', 'vs planned arrival')})</span>` : ''}</span>
  </div>${!isReview(r) ? whyList(r) : ''}</details>`;
}
/** NS help pages (www.ns.nl is on the open_external allowlist). */
const NS_HELP_URL_FALLBACK = 'https://www.ns.nl/service-en-contact';
const nsHelpUrl = () => (state.o as any)?.ns_help_url || NS_HELP_URL_FALLBACK;
/** "Wacht op treintijden (± 4 december)": when the trip's month of train data is expected. */
function awaitingTag(checkIn: string): string {
  const d = timetableDue(checkIn, getLang());
  if (!d) return L('Wacht op treintijden', 'Awaiting train times');
  return d.late ? L('Wacht op treintijden (elk moment)', 'Awaiting train times (any day now)') : L(`Wacht op treintijden (± ${d.due})`, `Awaiting train times (± ${d.due})`);
}

function awaitingHelp(checkIn: string): string {
  const d = timetableDue(checkIn, getLang());
  if (!d) return L('De treintijden verschijnen begin volgende maand. Dan controleert OV Refund deze reis vanzelf.', 'The train times are published early next month. OV Refund then checks this trip by itself.');
  return d.late
    ? L(`De treintijden van ${d.month} komen van Rijden de Treinen, één keer per maand. Ze worden elk moment verwacht. Daarna controleert OV Refund deze reis vanzelf.`, `The train times for ${d.month} come from Rijden de Treinen, once a month. They are expected any day now. OV Refund then checks this trip by itself.`)
    : L(`De treintijden van ${d.month} komen van Rijden de Treinen, één keer per maand, rond ${d.due}. Daarna controleert OV Refund deze reis vanzelf.`, `The train times for ${d.month} come from Rijden de Treinen, once a month, around ${d.due}. OV Refund then checks this trip by itself.`);
}

/** The refund for this trip, warm and prominent, with the reasoning in one sentence. */
function amountBlock(r: Row): string {
  if (!r.journey.check_out) return '';
  if (r.claim_status === 'EXTERNALLY_CLAIMED' && r.paid_cents === null) return `<p class="big-amt muted">${L('Elders geclaimd', 'Claimed elsewhere')}</p>`;
  // Rejected: no "terug" amount, the decision is the news.
  if ((r.claim_status === 'REJECTED' || r.ns_decision === 'rejected') && !r.paid_cents) return `<p class="big-amt muted">${L(`Afgewezen door ${carrierName(r)}`, `Rejected by ${carrierName(r)}`)}</p>`;
  if (r.evidence_status === 'pending' && r.delay_min === null) return note(awaitingTag(r.journey.check_in), help(`pend${r.id}`, awaitingHelp(r.journey.check_in)));
  const arriva = isArrivaTrip(r);
  const sentence = refundSentence(displayDelay(r), r.compensation_basis, r.compensation_cents, getLang(), arriva ? r.arriva ?? null : undefined);
  const delayHelp = r.delay_min !== null ? help(`delay${r.id}`, cutoffText(displayDelay(r) ?? r.delay_min, r), L('vertraging', 'delay')) : '';
  // Expired: the money can no longer be claimed, so it is not shown as "back".
  if (r.paid_cents === null && category(r) === 'expired') {
    const c = r.compensation_cents;
    const head = c !== null ? L(`Verlopen · ${euro(c, 'nl')} niet meer te claimen`, `Expired · ${euro(c, 'en')} can no longer be claimed`) : L('Verlopen: niet meer te claimen', 'Expired: can no longer be claimed');
    return `<p class="big-amt muted">${head}</p>${sentence ? `<p class="sentence">${h(sentence)} ${delayHelp}</p>` : ''}`;
  }
  // Skipped ("Niet claimen"): the amount is not on its way, so not "back".
  if (r.claim_status === 'DISMISSED' && r.paid_cents === null && r.compensation_cents !== null) return `<p class="big-amt muted">${L('Overgeslagen', 'Skipped')} · ${r.compensation_exact === false ? '≈ ' : ''}${euro(r.compensation_cents, getLang())}</p>${sentence ? `<p class="sentence">${h(sentence)} ${delayHelp}</p>` : ''}`;
  if (r.paid_cents !== null) return `<p class="big-amt">${euro(r.paid_cents, getLang())} <span>${recordedAmountLabel(r)}</span></p>${sentence ? `<p class="sentence">${h(sentence)} ${delayHelp}</p>` : ''}`;
  if (r.compensation_cents !== null) return `<p class="big-amt">${r.compensation_exact === false ? '≈ ' : ''}${euro(r.compensation_cents, getLang())} <span>${L('terug', 'back')}</span></p><p class="sentence">${h(sentence)} ${delayHelp}</p>`;
  const d = displayDelay(r);
  if (arriva) {
    // Arriva's own minimum (15 min in Limburg, 30 elsewhere) and bands; Arriva decides.
    const min = r.arriva?.min_delay ?? 30;
    const where = r.arriva?.region ? ` in ${r.arriva.region}` : '';
    const under = d !== null && d < min;
    const t = d === null ? '' : d < 1 ? L('Je kwam op tijd aan.', 'You arrived on time.') : under ? L(`Je kwam ongeveer ${d} minuten later aan. Arriva vergoedt${where} vanaf ${min} minuten.`, `You arrived about ${d} minutes late. Arriva pays${where} from ${min} minutes.`) : sentence;
    if (!t) return '';
    const head = under ? `<p class="big-amt muted">${d! < 1 ? L('Geen vertraging gevonden', 'No delay found') : L(`Minder dan ${min} minuten vertraging`, `Less than ${min} minutes late`)}</p>` : r.claim_status ? `<p class="big-amt muted">${L('Bedrag volgt', 'Amount to follow')}</p>` : '';
    return `${head}<p class="sentence">${h(t)} ${delayHelp}</p>`;
  }
  const late = d === null ? '' : d < 1 ? L('Je kwam op tijd aan.', 'You arrived on time.') : d < 30 ? (d >= 28 && r.claim_status === 'NEEDS_REVIEW' ? nearThresholdText(d, getLang()) : L(`Je kwam ongeveer ${d} minuten later aan. Onder 30 minuten geeft NS geen vergoeding.`, `You arrived about ${d} minutes late. Under 30 minutes NS gives no refund.`)) : sentence;
  if (!r.claim_status && (d ?? 0) < 30) return late ? `<p class="sentence">${h(late)} ${delayHelp}</p>` : '';
  // A trip to review without a claimable delay (e.g. a late check-out while the trains ran on time,
  // or just under 30 minutes): OV Refund can't claim it, so no amount "to follow".
  if (d !== null && d < 30) {
    const head = d < 1 ? L('Geen vertraging gevonden', 'No delay found') : d >= 28 && r.claim_status === 'NEEDS_REVIEW' ? L('Net onder 30 minuten', 'Just under 30 minutes') : L('Minder dan 30 minuten vertraging', 'Less than 30 minutes late');
    const text = d < 1 ? L('Volgens de treintijden reden je treinen op tijd.', 'According to the train times, your trains ran on time.') : late;
    return `<p class="big-amt muted">${head}</p><p class="sentence">${h(text)} ${delayHelp}</p>`;
  }
  const why = r.journey.invoiced === false
    ? L('NS toont de ritprijs pas als de reis is gefactureerd. Daarna vult OV Refund het bedrag vanzelf in.', 'NS only shows the fare once the trip has been invoiced. OV Refund then fills in the amount by itself.')
    : L('Geen ritprijs of abonnementsprijs gevonden in je NS-reishistorie. NS berekent het bedrag.', 'No fare or subscription price found in your NS travel history. NS works out the amount.');
  return `<p class="big-amt muted">${L('Bedrag volgt', 'Amount to follow')} ${help(`amt${r.id}`, why)}</p>${late ? `<p class="sentence">${h(late)} ${delayHelp}</p>` : ''}`;
}
/** Short notes for special cases, details behind "?". */
function detailNotes(r: Row): string {
  const out: string[] = [];
  const j = r.journey;
  if (r.possible_delay_min != null && r.possible_delay_min > 0) out.push(note(L(`Mogelijk ${r.possible_delay_min} min vertraagd`, `Possibly ${r.possible_delay_min} min late`), help(`poss${r.id}`, L('Volgens je uitchecktijd. Je kunt nu al zelf claimen, of wachten op de treintijden.', 'Based on your check-out time. You can claim it yourself now, or wait for the train times.'))));
  if (r.below_minimum && !isArrivaTrip(r)) out.push(note(L(`Onder ${euro(230, 'nl')}: NS betaalt dit niet uit.`, `Below ${euro(230, 'en')}: NS does not pay this out.`)));
  // Restored or older trips without a full assessment: say how to get one.
  // (Not for a delay under 30 minutes: that has no band, and a new check doesn't change it.)
  if (category(r) === 'claimable' && r.claim_id && j.check_out && j.destination && r.evidence_status !== 'pending' && (!r.delay_band || !r.scheduled_departure) && !underThreshold(r) && !isArrivaTrip(r)) out.push(note(L('Klik op Controleer nu om deze reis opnieuw te beoordelen.', 'Click Check now to re-assess this trip.')));
  const unsupported = carrierNotSupported(j, getLang());
  if (unsupported) {
    const c = h(j.carrier_name?.trim() || L('die vervoerder', 'that carrier'));
    const link = r.carrier_url && /^https:\/\//.test(r.carrier_url) ? `<a href="${h(r.carrier_url)}" target="_blank" rel="noopener">${L('Naar de website', 'Go to the website')} ${ICON.ext}</a>` : '';
    out.push(note(`${h(unsupported)}. ${L(`OV Refund claimt deze reis niet. Vraag je geld terug bij ${c}.`, `OV Refund does not claim this trip. Ask ${c} for your money back.`)}`, link, 'warn'));
  }
  if (!j.check_out) out.push(note(L('Geen uitcheck gevonden.', 'No check-out recorded.'), help(`nco${r.id}`, `${L('Vergeten uit te checken? Dat regel je apart bij NS. OV Refund kan voor deze reis geen vertraging bepalen.', 'Forgot to check out? You sort that out separately with NS. OV Refund can’t determine a delay for this trip.')} <a href="${nsHelpUrl()}" target="_blank" rel="noopener">${L('NS Klantenservice', 'NS Customer Service')}</a>`)));
  const split = splitNote(r);
  if (split) out.push(split);
  return out.join('');
}
const nearHint = () => L('Een overzicht of e-mail van NS wijst op een mogelijke claim rond deze datum. Controleer deze reis in Mijn teruggaven.', 'An NS list or e-mail suggests a possible claim around this date. Check this trip in Mijn teruggaven.');
const cardConflictText = () => L('Mijn NS koppelt deze reis aan een andere kaart. Controleer de gekoppelde kaart in Mijn NS voordat je indient.', 'Mijn NS links this trip to a different card. Check the linked card in Mijn NS before submitting.');
const uncertainHint = () => L('Een eerdere indiening had een onzekere uitkomst. Misschien heeft NS hem al. Kijk eerst in Mijn NS → Mijn teruggaven. Dien alleen opnieuw in als hij daar echt niet staat.', 'An earlier submission had an uncertain outcome. NS may already have it. Check Mijn NS → Mijn teruggaven first. Only submit again if it is really not listed there.');
function actionsHtml(r: Row): string {
  const cat = category(r);
  const c = r.claim_id ? state.claims.get(r.claim_id) : undefined;
  const d = daysUntil(r.expires_at);
  const parts: string[] = [];
  const more: string[] = [];
  if (cat === 'claimable') {
    if (r.claim_status === 'FAILED' && wasUncertain(r)) parts.push(note(L('Eerdere indiening onzeker', 'Earlier submission uncertain'), help(`wu${r.id}`, uncertainHint()), 'warn'));
    if (r.card_conflict) parts.push(note(cardConflictText(), '', 'warn'));
    else if (r.ns_claim_near) parts.push(note(L('Mogelijk al geclaimd bij NS', 'Possibly already claimed with NS'), help(`near${r.id}`, nearHint()), 'warn'));
    if (d !== null) parts.push(`<p class="msg${d < 14 ? ' warn' : ''}">${d >= 0 ? (d === 1 ? L('Nog 1 dag om te claimen', '1 day left to claim') : L(`Nog ${d} dagen om te claimen`, `${d} days left to claim`)) : L('Claimtermijn verlopen', 'Claim window expired')}</p>`);
    // Under 30 minutes OV Refund can't fill in the NS form: the user can still claim it in Mijn NS.
    if (!canReviewSubmit(r) && underThreshold(r)) parts.push(`<a class="btn" href="${NS_MIJNNS_URL}" target="_blank" rel="noopener">${L('Zelf indienen via Mijn NS', 'Submit yourself via Mijn NS')} ${ICON.ext}</a>`);
    more.push(`<button class="btn ghost" data-act="external" data-claim="${r.claim_id}">${L('Al elders geclaimd', 'Already claimed elsewhere')}</button>`);
    const ready = r.claim_status === 'READY';
    (ready ? more : parts).push(`<button class="btn${ready ? ' ghost' : ''}" data-act="dismiss" data-claim="${r.claim_id}">${L('Niet claimen', 'Don’t claim')}</button>`);
  } else if (cat === 'claimed' && r.claim_id) {
    if (r.claim_status === 'INDETERMINATE') parts.push(note(L('Niet zeker of NS deze claim heeft.', 'Not sure whether NS has this claim.'), help(`ind${r.id}`, L('Kijk in Mijn NS → Mijn teruggaven. Staat hij erbij? Kies dan Ingediend. Staat hij er zeker niet? Kies dan Niet verzonden. Daarna kun je opnieuw indienen. Twijfel je? Laat hem dan op Onzeker. OV Refund kijkt bij elke controle opnieuw. Tot dan wacht automatisch indienen.', 'Check Mijn NS → Mijn teruggaven. Is it listed? Then choose Submitted. Is it definitely not listed? Then choose Not sent. You can then submit again. Not sure? Then leave it on Uncertain. OV Refund checks again every time. Until then, automatic submission waits.')), 'warn'));
    if (r.claim_status === 'SUBMITTING') parts.push(note(L('Wordt ingediend. Dien deze reis niet opnieuw in.', 'Being submitted. Don’t submit this trip again.'), help(`sub${r.id}`, L('Misschien is het indienen onderbroken. Wacht op de lijst of de mail van NS. OV Refund werkt de status daarna vanzelf bij.', 'Submitting may have been interrupted. Wait for NS’s list or e-mail. OV Refund then updates the status by itself.'))));
    if (r.claim_status === 'REJECTED' || r.ns_decision === 'rejected') parts.push(note(L('Niet eens met de afwijzing?', 'Disagree with the rejection?'), `<a href="${nsHelpUrl()}" target="_blank" rel="noopener">${L('NS Klantenservice', 'NS Customer Service')} ${ICON.ext}</a>`));
    // Claimed elsewhere: NS's reference went to that service, so no "no reference yet".
    if (r.ns_reference || r.claim_status !== 'EXTERNALLY_CLAIMED') parts.push(`<p class="msg">${r.ns_reference ? `${L('NS-referentie', 'NS reference')}: <b>${h(r.ns_reference)}</b>` : L('Nog geen NS-referentie', 'No NS reference yet')}</p>`);
    const next = MANUAL_NEXT[r.claim_status ?? ''] ?? [];
    // For an uncertain claim the options say which answer resolves it.
    const unc = r.claim_status === 'INDETERMINATE';
    const optLabel = (s: string) => unc && s === 'SUBMITTED' ? L('Ingediend (staat in Mijn teruggaven)', 'Submitted (listed in Mijn teruggaven)')
      : unc && s === 'FAILED' ? L('Niet verzonden (staat er niet in)', 'Not sent (not listed there)') : L(STATUS[s][0], STATUS[s][1]);
    const sel = next.length ? `<select class="in" data-act="setstatus" data-claim="${r.claim_id}" aria-label="${h(L('Status wijzigen', 'Change status'))}" aria-describedby="selHint${r.claim_id}"><option value="">${L('Status wijzigen…', 'Change status…')}</option>${next.map((s) => `<option value="${s}">${optLabel(s)}</option>`).join('')}</select><span class="msg sel-hint" id="selHint${r.claim_id}" hidden>${L('Druk op Enter om te bevestigen.', 'Press Enter to confirm.')}</span>` : '';
    if (sel) (r.claim_status === 'INDETERMINATE' ? parts : more).push(sel);
    const paid = `<label class="field-l">${L('Ontvangen bedrag (€)', 'Amount received (€)')}${r.claim_status === 'EXTERNALLY_CLAIMED' ? ` ${help(`paid${r.id}`, L('Bij een claimdienst gaan de mails van NS naar die dienst, niet naar jou. Vul het ontvangen bedrag daarom zelf in.', 'With a claim service, NS’s e-mails go to that service, not to you. So enter the amount received yourself.'))}` : ''}<input class="in" type="text" inputmode="decimal" placeholder="${L('0,00', '0.00')}" data-act="paid" data-claim="${r.claim_id}" value="${amountInput(r.paid_cents, getLang())}"></label>`;
    (r.claim_status === 'EXTERNALLY_CLAIMED' ? parts : more).push(paid);
    if (r.claim_status === 'EXTERNALLY_CLAIMED') more.push(`<label class="field-l">${L('Ingehouden commissie (%)', 'Commission kept (%)')} ${help(`com${r.id}`, L('Het percentage dat de andere dienst heeft ingehouden. Zo berekent OV Refund wat NS echt uitbetaalde.', 'The percentage the other service kept. OV Refund uses it to work out what NS actually paid.'))}<input class="in" type="text" inputmode="decimal" data-act="commission" data-claim="${r.claim_id}" value="${r.commission_pct !== null ? Math.round(r.commission_pct * 100) : ''}" placeholder="${L('onbekend', 'unknown')}"></label>`);
    if (r.claim_status === 'EXTERNALLY_CLAIMED' && ((!r.ns_reference && r.paid_cents === null) || c?.events?.some(e => e.actor === 'ns-mail' && e.to_status === 'EXTERNALLY_CLAIMED'))) more.push(`<button class="btn ghost" data-act="undoexternal" data-claim="${r.claim_id}">${L('Toch niet elders geclaimd', 'Not claimed elsewhere after all')}</button>`);
  } else if (r.claim_status === 'DISMISSED' && r.claim_id) {
    parts.push(`<button class="btn" data-act="restore" data-claim="${r.claim_id}">${L('Toch claimen', 'Claim after all')}</button>`);
  }
  if (c?.events?.length) more.push(`<details class="events" data-k="hist-${r.id}"><summary>${L('Geschiedenis', 'History')}</summary>${c.events.map((e) => `<div>${localDateTime(e.at, getLang())}: ${h(statusLabel(e.to_status))}${e.detail ? `. ${h(historyDetail(e.detail, getLang()))}` : ''}</div>`).join('')}</details>`);
  const menu = more.length ? `<details class="more-menu" data-k="more-${r.id}"><summary>${ICON.more}<span>${L('Meer', 'More')}</span></summary><div class="menu">${more.join('')}</div></details>` : '';
  return `<div class="acts">${parts.join('')}${menu}</div>`;
}
/** Actions of a real Arriva trip: Claim (manual only), attachments, outcome. */
function arrivaActionsHtml(r: Row): string {
  const parts: string[] = [];
  const more: string[] = [];
  const msg = arrivaMessage(state.arrivaReal, r.claim_id);
  const missing = state.arrivaReal?.missing ?? [];
  const busy = arrivaBusy(r);
  const blocked = arrivaBlocker(r);
  if (blocked && ['NEEDS_REVIEW', 'READY', 'FAILED', 'CANDIDATE'].includes(r.claim_status ?? 'CANDIDATE')) parts.push(note(L('Dien deze claim zelf in bij Arriva.', 'Submit this claim yourself at Arriva.'), `${h(L(blocked.nl, blocked.en))} <a href="https://klantenservice.arriva.nl/geldterug" target="_blank" rel="noopener">${L('Naar Arriva', 'Go to Arriva')} ${ICON.ext}</a>`, 'warn'));
  if (arrivaClaimable(r)) parts.push(note(L('Arriva-claim: je dient hem zelf in met Claimen.', 'Arriva claim: you submit it yourself with Claim.'), help(`arm${r.id}`, L('OV Refund dient Arriva-claims nooit automatisch in. Na Claimen maakt OV Refund de bijlagen (je reis in Mijn NS met je OV-chipkaartnummer en de vertraging volgens Rijden de Treinen), vult het formulier van Arriva in met jouw gegevens, controleert elk veld en verstuurt het. Soms vraagt Google om een puzzel (CAPTCHA): dan verschijnt alleen die puzzel.', 'OV Refund never submits Arriva claims automatically. After Claim, OV Refund makes the attachments (your trip in Mijn NS with your OV-chipkaart number and the delay from Rijden de Treinen), fills in Arriva’s form with your details, checks every field and sends it. Sometimes Google asks for a puzzle (CAPTCHA): then only that puzzle appears.'))));
  if (isDemo() && arrivaClaimable(r)) parts.push(`<p class="msg warn">${L('Demo: Claimen doorloopt het echte formulier van Arriva met fictieve gegevens, maar verzending is geblokkeerd. Er gaat niets naar Arriva.', 'Demo: Claim goes through Arriva’s real form with fictional data, but sending is blocked. Nothing goes to Arriva.')}</p>`);
  else if (state.arrivaReal && !state.arrivaReal.submit_enabled && arrivaClaimable(r)) parts.push(`<p class="msg warn">${L('Proefversie: Claimen controleert alles en maakt de bijlagen, maar opent het formulier van Arriva niet en verstuurt niets.', 'Test build: Claim checks everything and makes the attachments, but does not open Arriva’s form and sends nothing.')}</p>`);
  if (arrivaClaimable(r) && wasUncertain(r) && !busy) parts.push(`<p class="msg warn">${L('Een eerdere poging is misschien al bij Arriva aangekomen. Opnieuw claimen kan pas als je in je e-mail van Arriva hebt gezien dat er geen bevestiging is.', 'An earlier attempt may already have reached Arriva. You can claim again only after checking your e-mail from Arriva for a confirmation and finding none.')}</p>`);
  if (busy) parts.push(`<p class="msg" role="status">${L('Bezig… De uitkomst volgt hier.', 'In progress… The outcome follows here.')}</p>`);
  // The puzzle window may be hidden behind the app: one click brings it back (only while verifying).
  if (busy && arrivaPhase(state.arrivaReal, r.claim_id) === 'verifying') parts.push(`<button class="btn" data-act="arriva-show-puzzle">${L('Toon puzzel', 'Show puzzle')}</button>`);
  if (msg && !busy) parts.push(`<p class="msg" role="status">${h(pickLang(msg))}</p>`);
  if (arrivaClaimable(r) && !busy) {
    if (missing.includes('contact')) parts.push(`<p class="msg warn">${L('Vul eerst je naam en e-mailadres voor Arriva in.', 'First enter your name and e-mail address for Arriva.')} <button class="btn small" data-act="goarriva">${L('Gegevens voor Arriva', 'Details for Arriva')}</button></p>`);
    if (missing.includes('bank')) parts.push(`<p class="msg warn">${L('Stel eerst je rekening in.', 'Set up your bank account first.')} <button class="btn small" data-act="gobank">${L('Rekening instellen', 'Set up bank account')}</button></p>`);
    const d = daysUntil(r.expires_at);
    if (d !== null && d >= 0) parts.push(`<p class="msg${d < 14 ? ' warn' : ''}">${d === 1 ? L('Nog 1 dag om te claimen', '1 day left to claim') : L(`Nog ${d} dagen om te claimen`, `${d} days left to claim`)}</p>`);
    parts.push(`<button class="btn primary" data-act="arriva-real-claim" data-claim="${r.claim_id}" ${state.busy || missing.length ? 'disabled' : ''}>${L('Claimen', 'Claim')}</button>`);
    more.push(`<button class="btn ghost" data-act="external" data-claim="${r.claim_id}">${L('Al elders geclaimd', 'Already claimed elsewhere')}</button>`);
    parts.push(`<button class="btn" data-act="dismiss" data-claim="${r.claim_id}">${L('Niet claimen', 'Don’t claim')}</button>`);
  }
  if (r.claim_status === 'SUBMITTED') parts.push(`<p class="msg">${r.ns_reference ? `${L('Arriva-referentie', 'Arriva reference')}: <b>${h(r.ns_reference)}</b>` : L('Bewaar een eventuele bevestigingsmail van Arriva.', 'Keep the confirmation e-mail if Arriva sends one.')}</p>`);
  if (r.claim_status === 'INDETERMINATE') parts.push(note(L('Niet zeker of Arriva deze claim heeft.', 'Not sure whether Arriva has this claim.'), help(`aind${r.id}`, L('Kijk in je e-mail van Arriva. Kreeg je een bevestiging? Kies dan Ingediend. Zeker niet? Kies dan Niet verzonden; daarna kun je opnieuw claimen, nadat je bevestigt dat er geen bevestiging van Arriva is. OV Refund probeert het nooit vanzelf opnieuw.', 'Check your e-mail from Arriva. Got a confirmation? Choose Submitted. Certainly not? Choose Not sent; you can then claim again after confirming that there is no confirmation from Arriva. OV Refund never tries again by itself.')), 'warn'));
  const next = r.claim_id ? MANUAL_NEXT[r.claim_status ?? ''] ?? [] : [];
  if (next.length) parts.push(`<select class="in" data-act="setstatus" data-claim="${r.claim_id}" aria-label="${h(L('Status wijzigen', 'Change status'))}"><option value="">${L('Status wijzigen…', 'Change status…')}</option>${next.map((s) => `<option value="${s}">${h(r.claim_status === 'INDETERMINATE' && s === 'SUBMITTED' ? L('Ingediend (bevestiging van Arriva)', 'Submitted (confirmation from Arriva)') : r.claim_status === 'INDETERMINATE' && s === 'FAILED' ? L('Niet verzonden', 'Not sent') : L(STATUS[s][0], STATUS[s][1]))}</option>`).join('')}</select>`);
  if (r.claim_id) parts.push(`<button class="btn ghost" data-act="arriva-real-attachments" data-claim="${r.claim_id}">${L('Bijlagen', 'Attachments')}</button>`);
  if (r.claim_status === 'DISMISSED' && r.claim_id) parts.push(`<button class="btn" data-act="restore" data-claim="${r.claim_id}">${L('Toch claimen', 'Claim after all')}</button>`);
  const c = r.claim_id ? state.claims.get(r.claim_id) : undefined;
  if (c?.events?.length) more.push(`<details class="events" data-k="hist-${r.id}"><summary>${L('Geschiedenis', 'History')}</summary>${c.events.map((e) => `<div>${localDateTime(e.at, getLang())}: ${h(statusLabel(e.to_status))}${e.detail ? `. ${h(historyDetail(e.detail, getLang()))}` : ''}</div>`).join('')}</details>`);
  const menu = more.length ? `<details class="more-menu" data-k="more-${r.id}"><summary>${ICON.more}<span>${L('Meer', 'More')}</span></summary><div class="menu">${more.join('')}</div></details>` : '';
  return `<div class="acts">${parts.join('')}${menu}</div>`;
}
/** Reasons, minus what the sentences above already say. */
function whyList(r: Row): string {
  const rs = r.reasons.filter((x) => !(r.delay_min !== null && /^estimated arrival delay \d+ min/.test(x)) && !(r.below_minimum && /^refund below/.test(x)) && !(!r.journey.check_out && /^no check-out recorded/.test(x)) && !/^compensation amount unknown/.test(x));
  // Each reason is a sentence: end it with a full stop.
  const sentence = (t: string) => (/[.!?]$/.test(t.trim()) ? t.trim() : `${t.trim()}.`);
  return rs.length ? `<ul class="why">${rs.map((x) => `<li>${h(sentence(reasonText(x, getLang())))}</li>`).join('')}</ul>` : '';
}

/** Claimable rows stand out (soft yellow), rows to review get a lighter mark, the rest stays calm. */
const rowClass = (r: Row) => { if (isArrivaDemoFixture(r)) return arrivaDemoTrip(r).status === 'claimable' ? ' is-arriva-demo' : ''; if (isArrivaReal(r) && (arrivaClaimable(r) || r.claim_status === 'SUBMITTING')) return ' is-claim'; const c = category(r); return c === 'claimable' ? (isReview(r) || r.claim_status === 'FAILED' ? ' is-review' : ' is-claim') : ''; };
function detailRow(r: Row): string {
  if (isArrivaDemoFixture(r) && arrivaDemoTrip(r).status !== 'claimable') {
    const t = arrivaDemoTrip(r);
    return `<tr class="detail arriva-demo-detail" data-detail="${r.id}"><td colspan="6"><div class="detail-grid"><div class="d-main">
      <p class="note warn"><span>${L('Fictieve Arriva-demorit. Er wordt niets naar Arriva gestuurd.', 'Fictional Arriva demo trip. Nothing is sent to Arriva.')}</span></p>
      <p>${L('Vertraging', 'Delay')} <b>+${t.delay_minutes} min</b> · ${t.status === 'tested' ? L('Deze demo-aanvraag is al getest; verzending werd geblokkeerd.', 'This demo application was already tested; submission was blocked.') : h(L(`Geen recht op geld terug. ${t.reason_nl ?? ''}`, `No refund. ${t.reason_en ?? ''}`))}</p>
      </div><div class="acts"><button class="btn ghost" data-act="arriva-demo">${L('Demo inspecteren', 'Inspect demo')}</button></div></div></td></tr>`;
  }
  if (isArrivaDemoFixture(r)) {
    const claimState = arrivaClaimState(state.arriva, state.arrivaClaiming);
    const ready = claimState === 'ready';
    const trip = state.arriva?.trip;
    const fare = trip?.fare_cents != null ? euro(trip.fare_cents, getLang()) : euro(1240, getLang());
    return `<tr class="detail arriva-demo-detail" data-detail="${r.id}"><td colspan="6"><div class="detail-grid"><div class="d-main">
      <p class="note warn"><span>${L('Dit is een fictieve Arriva-demorit, buiten de NS-claimroute. Er wordt geen echte claim aangemaakt.', 'This is a fictional Arriva demo trip, outside the NS claim flow. No real claim is created.')}</span></p>
      <p>${L('Trein', 'Train')} <b>${h(trip?.train_number || '732284')}</b> · ${L('vertraging', 'delay')} <b>+${arrivaFixtureDelay()} min</b> · ${L('Fictieve ritprijs', 'Example fare')} ${fare}</p>
      ${arrivaOutcomeText(state.arriva) ? `<p class="msg" role="status">${h(arrivaOutcomeText(state.arriva))}</p>` : ''}
      </div><div class="acts">${ready ? `<button class="btn primary" data-act="arriva-claim" ${state.busy ? 'disabled' : ''}>${L('Claimen', 'Claim')}</button><p class="msg">${L('Alleen de verificatie bij Arriva wacht nog op jou.', 'Only verification at Arriva needs your attention.')}</p>` : claimState === 'in_progress' ? `<p class="msg" role="status">${L('Bezig… De uitkomst volgt hier.', 'In progress… The outcome follows here.')}</p>` : state.arriva?.phase === 'submission_blocked' ? `<p class="msg">${L('Demo getest. Er is geen echte claim ingediend.', 'Demo tested. No real claim was submitted.')}</p>` : `<p class="msg">${L('Beschikbaar nadat je Controleer nu kiest.', 'Available after you choose Check now.')}</p>`}<button class="btn ghost" data-act="arriva-attachments">${L('Bijlagen', 'Attachments')} (${state.arriva?.attachments?.length ?? 0})</button><button class="btn ghost" data-act="arriva-demo">${L('Demo inspecteren', 'Inspect demo')}</button></div></div></td></tr>`;
  }
  if (isArrivaReal(r)) return `<tr class="detail${rowClass(r)}" data-detail="${r.id}"><td colspan="6"><div class="detail-grid"><div class="d-main">${r.compensation_cents !== null ? amountBlock(r) : ''}${detailNotes(r)}${whyList(r)}${trainsHtml(r)}</div>${arrivaActionsHtml(r)}</div></td></tr>`;
  return `<tr class="detail${rowClass(r)}" data-detail="${r.id}"><td colspan="6"><div class="detail-grid"><div class="d-main">${amountBlock(r)}${detailNotes(r)}${isReview(r) ? whyList(r) : ''}${r.evidence_status === 'pending' && r.delay_min === null ? '' : trainsHtml(r)}</div>${actionsHtml(r)}</div></td></tr>`;
}

/** Open/close one trip's details in place (no full redraw, so nothing else on the page moves). */
function toggleRow(tr: HTMLElement, id: number) {
  const r = state.rows.find((x) => x.id === id);
  if (!r) return;
  const btn = tr.querySelector('.chev-btn');
  if (state.open.has(id)) {
    state.open.delete(id);
    tr.classList.remove('open'); btn?.setAttribute('aria-expanded', 'false');
    tr.nextElementSibling?.matches(`[data-detail="${id}"]`) && tr.nextElementSibling.remove();
  } else {
    state.open.add(id);
    tr.classList.add('open'); btn?.setAttribute('aria-expanded', 'true');
    tr.insertAdjacentHTML('afterend', detailRow(r));
    tr.nextElementSibling?.classList.add('det-new'); // animate only a freshly opened panel, not re-renders
  }
  layoutSticky();
}

function ledger(): string {
  const rows = state.rows.filter((r) => inPeriod(r.journey.check_in, state.period) && inFilter(r, state.cat));
  if (!state.rows.length && busyUI()) return `<div class="ledger"><div class="empty">${TRAIN_ART}<p class="e-t">${L('Eerste controle…', 'First check…')}</p><p class="msg">${L('Je reizen worden opgehaald en vergeleken met de treintijden. De eerste keer duurt dat een paar minuten.', 'Your trips are being fetched and compared with the train times. The first time takes a few minutes.')}</p></div></div>`;
  if (!state.rows.length) {
    return state.o?.auth_state === 'ok'
      ? `<div class="ledger"><div class="empty">${TRAIN_ART}<p class="e-t">${L('Nog geen reizen', 'No trips yet')}</p><p class="msg">${L('Haal je reizen op bij Mijn NS.', 'Fetch your trips from Mijn NS.')}</p><button class="btn primary" data-act="scan" ${state.busy ? 'disabled' : ''}>${L('Controleer nu', 'Check now')}</button></div></div>`
      : `<div class="ledger"><div class="empty">${TRAIN_ART}<p class="e-t">${L('Nog geen reizen', 'No trips yet')}</p><p class="msg">${L('Log in bij Mijn NS. Daarna haalt OV Refund je reizen op.', 'Log in to Mijn NS. OV Refund then fetches your trips.')}</p></div></div>`;
  }
  if (!rows.length) return `<div class="ledger"><div class="empty">${TRAIN_ART}<p class="e-t">${emptySelectionText()}</p><button class="btn" data-act="showall">${L('Toon alle reizen', 'Show all trips')}</button></div></div>`;
  const delayHelp = help('delayH', L('Hoeveel later je aankwam dan gepland, volgens de treintijden van die dag (niet je uitchecktijd). Vanaf 30 minuten betaalt NS de helft terug, vanaf 60 minuten alles. Met een abonnement: het deel dat daarbij hoort.', 'How much later you arrived than planned, from that day’s train times (not your check-out time). From 30 minutes NS refunds half, from 60 minutes everything. With a subscription: the matching share.') + ' ' + L('Arriva: vanaf 15 minuten in Limburg (ook de Maaslijn), elders vanaf 30 minuten de helft van de ritprijs; vanaf 60 minuten alles. Arriva beslist.', 'Arriva: from 15 minutes in Limburg (including the Maaslijn), elsewhere from 30 minutes half the fare; from 60 minutes everything. Arriva decides.'), L('vertraging', 'delay'));
  return `<div class="ledger"><table>
    <colgroup><col class="c-date"><col class="c-route"><col class="c-delay"><col class="c-refund"><col class="c-status"><col class="c-chev"></colgroup>
    <thead><tr><th>${L('Datum', 'Date')}</th><th>${L('Reis', 'Trip')}</th><th class="num"><span class="th-help">${L('Vertraging', 'Delay')}${delayHelp}</span></th><th class="num">${L('Vergoeding', 'Refund')}</th><th>${L('Status', 'Status')}</th><th><span class="sr-only">${L('Details', 'Details')}</span></th></tr></thead>
    <tbody>${rows.map((r) => {
      const open = state.open.has(r.id);
      return `<tr class="row${rowClass(r)}${open ? ' open' : ''}" data-row="${r.id}">
        <td class="date"><b>${localDate(r.journey.check_in, getLang())}</b><span class="msg">${localTime(r.journey.check_in)} – ${localTime(r.journey.check_out)}</span></td>
        <td><div class="route">${h(routeOf(r.journey))}</div>${(() => { const op = operatorLabel(r.journey.carrier_code, r.journey.carrier_name); return `<span class="operator-badge ${op.kind}">${h(op.label)}</span>`; })()}</td>
        <td class="num">${isArrivaDemoFixture(r) ? `<span class="${arrivaDemoTrip(r).delay_minutes >= (arrivaDemoTrip(r).min_delay ?? 15) ? 'delay' : 'calm'}">+${arrivaDemoTrip(r).delay_minutes} min</span>` : delayCell(r)}</td><td class="num">${isArrivaDemoFixture(r) ? '<span class="calm">–</span>' : compCell(r)}</td><td class="status">${isArrivaDemoFixture(r) ? arrivaStatusCell(r) : statusCell(r)}</td>
        <td class="chev"><button class="chev-btn" data-toggle="${r.id}" aria-expanded="${open}" aria-label="${h(`${L('Details', 'Details')}: ${routeOf(r.journey)}, ${localDate(r.journey.check_in, getLang())}`)}"><span class="chev-i" aria-hidden="true">›</span></button></td></tr>
        ${open ? detailRow(r) : ''}`;
    }).join('')}</tbody></table></div>`;
}

/** Copy for an empty filter result, specific to the chosen category and period. */
function emptySelectionText(): string {
  const all = state.period === 'all';
  switch (state.cat) {
    case 'claimable': return all ? L('Niets te claimen. Mooi zo!', 'Nothing to claim. Nice!') : L('Niets te claimen in deze periode. Mooi zo!', 'Nothing to claim in this period. Nice!');
    case 'review': return all ? L('Niets om te beoordelen.', 'Nothing to review.') : L('Niets om te beoordelen in deze periode.', 'Nothing to review in this period.');
    case 'claimed': return all ? L('Nog niets geclaimd.', 'Nothing claimed yet.') : L('Niets geclaimd in deze periode.', 'Nothing claimed in this period.');
    case 'expired': return all ? L('Geen verlopen claims. Goed bijgehouden!', 'No expired claims. All on track!') : L('Geen verlopen claims in deze periode.', 'No expired claims in this period.');
    default: return L('Geen reizen in deze periode.', 'No trips in this period.');
  }
}

/** Busy and visible (the start-up check stays quiet for its first moments). */
const busyUI = () => !!state.busy && !state.busyQuiet;

/** The container (main window or Settings dialog) a control lives in. */
const boxOf = (el: Element | null): HTMLElement => (setDlg && el && setDlg.contains(el) ? setDlg : app);
/** A stable selector for the focused control, so focus survives a full redraw (also without an id). */
function focusKey(el: Element | null, box: HTMLElement = boxOf(el)): string {
  if (!(el instanceof HTMLElement) || !box.contains(el)) return '';
  if (el.id) return '#' + CSS.escape(el.id);
  for (const a of ['data-toggle', 'data-period', 'data-cat', 'data-tab', 'data-help']) { const v = el.getAttribute(a); if (v !== null) return `[${a}="${CSS.escape(v)}"]`; }
  const act = el.getAttribute('data-act');
  if (act) {
    let sel = `[data-act="${CSS.escape(act)}"]`;
    for (const a of ['data-claim', 'data-what', 'data-kind', 'data-id', 'data-status']) { const v = el.getAttribute(a); if (v !== null) sel += `[${a}="${CSS.escape(v)}"]`; }
    return sel;
  }
  if (el.matches('details > summary')) { const k = el.parentElement?.getAttribute('data-k'); if (k) return `[data-k="${CSS.escape(k)}"] > summary`; }
  if (el.classList.contains('skip')) return 'a.skip';
  if (el.hasAttribute('data-theme-toggle')) return '[data-theme-toggle]';
  if (el.hasAttribute('data-setlang')) return '[data-setlang]';
  return '';
}
/** Focus to restore when the focused control was disabled (and so lost focus) while it was working. */
let pendingFocus = '';
let pendingBox: HTMLElement | null = null;
/** Row of the control that started the running action: focus falls back to it (or the next row) if the control is gone. */
let pendingRow = 0;
/** Focus a trip row; if it left the list, the row that took its place (or the last one), else the filter buttons. */
function focusRow(id: number, before: number[]) {
  const rowEl = (x: number) => app.querySelector<HTMLElement>(`tr[data-row="${x}"] .chev-btn`);
  let el = rowEl(id);
  if (!el) {
    const i = before.indexOf(id);
    for (const x of i >= 0 ? [...before.slice(i + 1), ...before.slice(0, i).reverse()] : []) { el = rowEl(x); if (el) break; }
  }
  (el ?? app.querySelector<HTMLElement>('[data-cat].on'))?.focus({ preventScroll: !!el && isVisible(el) });
}
const isVisible = (el: HTMLElement) => { const r = el.getBoundingClientRect(); return r.top >= 0 && r.bottom <= window.innerHeight; };
/** The trip row (id) that a control belongs to, if any. */
function rowOf(el: Element | null): number {
  const tr = el?.closest('tr[data-detail], tr[data-row]') as HTMLElement | null;
  return Number(tr?.dataset.detail ?? tr?.dataset.row ?? 0) || 0;
}
/** Give focus back after a dialog closes: the control that opened it, else its row (or the next row). */
const rowIds = () => [...app.querySelectorAll<HTMLElement>('tr[data-row]')].map((x) => Number(x.dataset.row));
function focusBack(sel: string, row: number, order: number[] = rowIds(), el: HTMLElement | null = null) {
  if (document.activeElement && document.activeElement !== document.body && document.activeElement.isConnected) return;
  if (el?.isConnected && !(el as HTMLButtonElement).disabled) { el.focus({ preventScroll: true }); return; }
  const f = sel ? ((setDlg?.open ? setDlg.querySelector<HTMLElement>(sel) : null) ?? app.querySelector<HTMLElement>(sel)) : null;
  if (f && !(f as HTMLButtonElement).disabled) { f.focus({ preventScroll: true }); return; }
  if (row) focusRow(row, order);
}
/** Which inline form was open at the last render (focus/animate a newly opened one only). */
let renderedEdit = '';

/** Crossfade a view change where supported (no-op otherwise, and with reduced motion).
 *  `focusSel`: the control that started it keeps focus (also when clicked twice quickly). */
function withTransition(fn: () => void, focusSel = focusKey(document.activeElement)) {
  const box = boxOf(document.activeElement);
  const after = () => {
    const a = document.activeElement;
    if (focusSel && (!a || a === document.body || !a.isConnected)) box.querySelector<HTMLElement>(focusSel)?.focus({ preventScroll: true });
  };
  const d = document as Document & { startViewTransition?: (cb: () => void) => { finished?: Promise<unknown> } };
  if (typeof d.startViewTransition === 'function' && !reducedMotion() && !setDlg?.open) {
    try { const t = d.startViewTransition(() => { fn(); after(); }); t?.finished?.then(after, after); return; } catch { /* fall through */ }
  }
  fn(); after();
}

/** Replace a container's content, keeping half-typed input, focus (and caret) and open <details>. */
function paint(box: HTMLElement, html: string, rowOrder: number[] = []) {
  const anchor = box === app && state.busyKind === 'scan' ? [...box.querySelectorAll<HTMLElement>('tr[data-row]')].find(r => r.getBoundingClientRect().top >= 120) : undefined;
  const anchorId = anchor?.dataset.row;
  const anchorTop = anchor?.getBoundingClientRect().top;
  const expiryCardKey = box.querySelector<HTMLInputElement>('#arrValid')?.dataset.cardKey;
  const typed = new Map<string, string>();
  box.querySelectorAll<HTMLInputElement>('input.in[id], textarea.in[id]').forEach((i) => { if (i.type !== 'checkbox') typed.set(i.id, i.value); });
  const active = document.activeElement as HTMLInputElement | null;
  const lost = !active || active === document.body || !active.isConnected;
  const focusSel = active && box.contains(active) ? focusKey(active, box) : lost && pendingFocus && (pendingBox ?? app) === box ? pendingFocus : '';
  let caret: number | null = null;
  try { caret = active && active.id && box.contains(active) ? active.selectionStart ?? null : null; } catch { /* not a text input */ }
  const openDet = [...box.querySelectorAll<HTMLDetailsElement>('details[data-k][open]')].map((d) => d.dataset.k!);
  box.innerHTML = html;
  typed.forEach((value, id) => {
    const input = box.querySelector<HTMLInputElement>('#' + CSS.escape(id));
    if (input && (id !== 'arrValid' || input.dataset.cardKey === expiryCardKey)) input.value = value;
  });
  // Re-run the live checks on restored input (IBAN message, Save button, mail steps, name hint).
  for (const id of ['bankIban', 'bankName', 'smtpTo']) { const i = box.querySelector<HTMLInputElement>('#' + id); if (i && i.value) i.dispatchEvent(new Event('input', { bubbles: true })); }
  openDet.forEach((k) => { const d = box.querySelector<HTMLDetailsElement>(`details[data-k="${CSS.escape(k)}"]`); if (d) d.open = true; });
  if (anchorId && anchorTop !== undefined) {
    const restored = box.querySelector<HTMLElement>(`tr[data-row="${anchorId}"]`);
    if (restored) window.scrollBy({ top: restored.getBoundingClientRect().top - anchorTop, behavior: 'instant' });
  }
  if (focusSel) {
    const f = box.querySelector(focusSel) as HTMLInputElement | null;
    if (f && !f.disabled) { f.focus({ preventScroll: true }); if (caret !== null && 'setSelectionRange' in f) { try { f.setSelectionRange(caret, caret); } catch { /* not a text input */ } } }
    else if (pendingRow && box === app) focusRow(pendingRow, rowOrder);
  }
}

/** The "Check now" button. While a check runs, the same box becomes the progress indicator (ring + stage + %). */
const RING_R = 7, RING_C = 2 * Math.PI * RING_R;
/** Dash offset for the yellow progress arc; never shorter than ~12% so the turning arc stays visible early on. */
const ringOffset = (pct: number) => RING_C * (1 - Math.max(12, Math.min(100, pct)) / 100);
function scanButton(primary: boolean): string {
  const cls = primary ? 'primary' : '';
  if (busyUI() && state.busyKind === 'scan') {
    const p = state.progress;
    return `<button class="btn scan-btn busy ${cls}" data-act="scan" disabled aria-busy="true" aria-disabled="true"><span class="prog" role="progressbar" id="progRing" aria-label="${h(L('Voortgang van de controle', 'Check progress'))}" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${p.pct}" aria-valuetext="${h(ringText())}"><svg viewBox="0 0 18 18" aria-hidden="true"><circle cx="9" cy="9" r="${RING_R}" class="track"/><circle cx="9" cy="9" r="${RING_R}" class="fill" id="progFill" stroke-dasharray="${RING_C}" stroke-dashoffset="${ringOffset(p.pct)}"/><circle class="activity-dot" cx="9" cy="9" r="2"/></svg></span><span class="prog-t" id="progTxt" role="status" aria-live="polite" aria-atomic="true">${h(ringText())}</span></button>${stopButton()}`;
  }
  if (busyUI() && state.busyKind === 'update') return `<button class="btn scan-btn ${cls}" data-act="scan" disabled>${L('Bijwerken…', 'Updating…')}</button>`;
  return `<button class="btn scan-btn ${cls}" data-act="scan" title="${h(L('Nieuwe reizen ophalen bij Mijn NS', 'Fetch new trips from Mijn NS'))}" ${state.busy ? 'disabled' : ''}>${L('Controleer nu', 'Check now')}</button>`;
}

/** "Stop" next to the progress ring, only while a check submits claims automatically. */
const submitting = () => state.progress.stage === 'submit' || state.progress.stage === 'pause';
/** During a pause nothing is being sent: "Stoppen…"; while a claim is sent it still goes through. */
const stoppingText = () =>
  state.progress.stage === 'pause' ? L('Stoppen…', 'Stopping…')
  : state.progress.stage === 'submit' ? L('Automatisch indienen stoppen…', 'Stopping automatic submission…')
  : L('Gestopt: deze controle dient niets automatisch in.', 'Stopped: this check submits nothing automatically.');
/** Automatic submission switched off during a check: a claim being sent right now still goes through. */
const switchedOffText = () => `${L('Automatisch indienen is uitgezet.', 'Automatic submission was switched off.')}${state.progress.stage === 'submit' ? ` ${L('De claim die nu wordt ingediend, gaat nog door.', 'The claim being submitted now still goes through.')}` : ''}`;
/** The progress box text: while stopping only "Stoppen…" (the toast says the rest), else the stage and progress. */
const ringText = () => state.stopping ? L('Stoppen…', 'Stopping…') : progressText(state.progress.stage, state.progress.pct, getLang(), state.progress);
function stopButton(): string {
  const label = state.stopping ? stoppingText() : L('Stop', 'Stop');
  return `<button class="btn small stop-btn" data-act="stopauto" id="stopBtn" title="${h(L('Stop automatisch indienen. Een claim die al wordt verstuurd, kan nog doorgaan.', 'Stop automatic submissions. A claim already being sent may still go through.'))}" ${submitting() && !state.stopping ? '' : 'hidden'}>${label}</button>`;
}

function render() {
  const o = state.o; if (!o) return;
  const scroll = window.scrollY;
  const ok = o.auth_state === 'ok';
  const AUTH: Record<string, [string, string]> = {
    not_connected: ['Niet verbonden', 'Not connected'], logged_out: ['Uitgelogd', 'Logged out'],
    autologin_failed: ['Inloggen mislukt', 'Login failed'], blocked: ['Tijdelijk geblokkeerd', 'Temporarily blocked'],
    account_mismatch: ['Ander NS-account', 'Different NS account'],
    account_unreadable: ['NS-account niet gecontroleerd', 'NS account could not be verified'],
  };
  const conn = ok ? L('Verbonden met Mijn NS', 'Connected to Mijn NS') : AUTH[authKind(o.auth_state)] ? L(...AUTH[authKind(o.auth_state)]) : L('Log opnieuw in', 'Please log in again');
  const rowOrder = rowIds();
  // Logged out: logging in is the main action, checking comes second (in the same place, so progress stays there).
  const topLogin = !ok;
  paint(app, `
  <a class="skip" href="#trips">${L('Naar je reizen', 'Skip to your trips')}</a>
  <div class="top"><div class="brand" aria-label="OV Refund"><img class="logo-img" src="/logo-mark.png" alt="OV Refund" width="36" height="36"></div>
    <span class="conn"><span class="cdot ${ok ? 'ok' : 'bad'}" aria-hidden="true"></span>${conn}${lastUpdated(o) ? `<span class="msg"> · ${L('bijgewerkt', 'updated')} ${localDateTime(lastUpdated(o)!, getLang())}</span>` : ''}</span>
    <span class="grow"></span>
    ${topLogin ? `<button class="btn primary" data-act="login" ${state.busy ? 'disabled' : ''}>${L('Inloggen bij Mijn NS', 'Log in to Mijn NS')}</button>` : ''}
    ${scanButton(!topLogin)}
  </div>
  ${(o as any).demo ? `<div class="demo-banner">DEMO: ${L('voorbeeldgegevens; er wordt niets naar NS gestuurd.', 'example data; nothing is sent to NS.')} &nbsp; ${L('Abonnement', 'Subscription')}: <select class="in small" id="demoSub" ${state.busy ? 'disabled' : ''}>${[['none', L('geen (losse reizen)', 'none (pay per trip)')], ['dal_voordeel', 'Dal Voordeel'], ['weekend_vrij', 'Weekend Vrij'], ['dal_vrij', 'Dal Vrij'], ['altijd_vrij', 'Altijd Vrij']].map(([k, n]) => `<option value="${k}" ${(o as any).demo_subscription === k ? 'selected' : ''}>${n}</option>`).join('')}</select> &nbsp; <button class="btn small" data-act="demoauto" ${state.busy ? 'disabled' : ''}>${L('Simuleer automatische controle', 'Simulate automatic check')}</button> &nbsp; <button class="btn small" data-act="arriva-demo" ${state.busy ? 'disabled' : ''}>${L('Arriva-demo inspecteren', 'Inspect Arriva demo')}</button></div>` : ''}
  ${state.update?.available ? `<div class="update-banner"><b>${L(`Nieuwe versie ${h(state.update.version ?? '')} beschikbaar`, `New version ${h(state.update.version ?? '')} available`)}</b><span class="grow"></span><button class="btn primary" data-act="update" ${state.busy ? 'disabled' : ''}>${L('Nu bijwerken', 'Update now')}</button></div>` : ''}
  <div class="layout">
  <main id="trips" tabindex="-1" aria-label="${h(L('Je reizen', 'Your trips'))}"><div class="mainhead-notices">
    ${busyUI() && !state.rows.length ? '' : nextStepBanner(o, topLogin)}
    ${uncertainBanner(o)}${restoreBanner(o)}</div>
    ${summary(o)}<div class="mainhead">${toolbar()}</div>${ledger()}
  </main>
  <aside>${sidebar(o)}</aside></div>
  <footer><span class="foot-ver">OV Refund ${h((o as any).version ?? '')}</span>${updateButton()}<span class="foot-data">${L('Treingegevens', 'Train data')}: Rijden de Treinen (CC BY 4.0)</span><span class="grow"></span>
    <div class="footbox">
      <button class="fb" data-act="website" title="${L('Website van OV Refund openen', 'Open the OV Refund website')}"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/></svg>Web</button>
      <span class="fb-sep"></span>
      <button class="fb" data-act="bugreport" title="${L('Meld een probleem', 'Report a problem')}">${L('Bug melden', 'Report a bug')}</button>
      <span class="fb-sep"></span>
      ${themeButton()}
      <span class="fb-sep"></span>
      ${langSwitch()}
    </div></footer>`, rowOrder);
  if (setDlg?.open) paint(setDlg, settingsHtml(o));
  if (settingsSaving) lockSettings(true);
  // A newly opened inline form: fade it in and put the cursor in its first field.
  if (state.edit !== renderedEdit) {
    const box = state.edit === 'nslogin' || state.edit === 'import' ? setDlg : app;
    const form = state.edit && box ? box.querySelector<HTMLElement>('.inline-form') : null;
    if (form) { form.classList.add('enter'); form.querySelector<HTMLInputElement>('input.in')?.focus({ preventScroll: true }); }
    renderedEdit = state.edit;
  }
  window.scrollTo(0, scroll);
  layoutSticky();
}

/** Sticky offsets: the toolbar and table header stick below the top bar; a sidebar taller than the window
 *  scrolls with the page until its end is visible, then stays put. */
function layoutSticky() {
  const root = document.documentElement.style;
  // The sticky demo banner sits below the top bar: everything else sticks below both.
  const bar = (document.querySelector('.top') as HTMLElement | null)?.offsetHeight ?? 60;
  const demo = (document.querySelector('.demo-banner') as HTMLElement | null)?.offsetHeight ?? 0;
  const top = bar + demo;
  const mh = (document.querySelector('.mainhead') as HTMLElement | null)?.offsetHeight ?? 0;
  // The demo banner sits right under the top bar, and the filter bar overlaps it by a pixel: no gap shows rows.
  root.setProperty('--top-h', `${bar}px`);
  const headTop = demo ? top - 1 : top;
  root.setProperty('--head-top', `${headTop}px`);
  // One pixel of overlap: rounded heights never leave a gap where rows show through.
  root.setProperty('--th-top', `${headTop + mh - 1}px`);
  const aside = document.querySelector('aside') as HTMLElement | null;
  const foot = (document.querySelector('footer') as HTMLElement | null)?.offsetHeight ?? 0;
  const room = window.innerHeight - top - foot;
  if (aside) root.setProperty('--aside-top', `${aside.scrollHeight > room ? window.innerHeight - foot - aside.scrollHeight : top + 16}px`);
}
window.addEventListener('resize', layoutSticky);

// ---------- bug report (the user sees everything; no personal data is added) ----------
/** Web3Forms relay, called from the WebView; the access key is public by design (it can only mail the project). */
const BUG_RELAY_URL = 'https://api.web3forms.com/submit';
let bugOpening = false;
/** Redact free text before it leaves the computer: the backend's redactor (redact_text) when this build has it,
 *  then always the local one too (so an older backend, or a failure, still masks IBANs and long numbers). */
async function redactRemote(s: string): Promise<string> {
  let out = s;
  try { const r = await invoke<unknown>('redact_text', { text: s }); if (typeof r === 'string') out = r; } catch { /* older backend: local redaction only */ }
  return redactText(out);
}
async function openBugReport(opener: HTMLElement | null) {
  if (bugOpening || document.querySelector('dialog.bugdlg')) return; // a double click opens one dialog
  bugOpening = true;
  const backSel = focusKey(opener) || '[data-act="bugreport"]';
  let ctx: any;
  try { ctx = await invoke<any>('bug_report_context'); }
  catch { ctx = null; }
  finally { bugOpening = false; }
  const key = (state.o?.bug_form_key ?? '').trim();
  const dlg = document.createElement('dialog');
  dlg.className = 'bugdlg';
  dlg.setAttribute('aria-labelledby', 'bugT');
  dlg.innerHTML = `${dialogHead(L('Bug melden', 'Report a bug'), 'bugT')}<div class="bd">
    <label class="field-l" for="bugText">${L('Wat ging er mis?', 'What went wrong?')}</label>
    <textarea class="in" id="bugText" rows="6" maxlength="${BUG_TEXT_MAX}" aria-describedby="bugCount bugWarning" placeholder="${L('Wat deed je, en wat verwachtte je?', 'What did you do, and what did you expect?')}"></textarea>
    <small class="bug-count" id="bugCount">0 / ${BUG_TEXT_MAX}</small>
    <p class="msg" id="bugWarning" role="status" hidden>${L('Haal persoonlijke gegevens zoals adres of naam uit je tekst.', 'Remove personal details such as an address or name from your text.')}</p>
    <label class="check"><input type="checkbox" id="bugDiag" ${ctx?.diagnostics ? '' : 'disabled'}><span>${L('Technische gegevens meesturen (zonder persoonsgegevens)', 'Include technical details (no personal data)')}</span></label>
    <details class="preview"><summary>${L('Bekijk wat er meegaat', 'See what is included')}</summary><pre id="bugPreview"></pre></details>
    <p class="msg">${L('Je melding gaat via Web3Forms, zonder e-mailadres of contactgegevens. OV Refund haalt IBAN-, kaart- en telefoonnummers en e-mailadressen eruit. Web3Forms ziet je IP-adres. De makers wissen je melding als het probleem is opgelost.', 'Your report goes through Web3Forms, without an e-mail address or contact details. OV Refund removes IBANs, card and phone numbers and e-mail addresses. Web3Forms sees your IP address. The authors delete your report once the problem is solved.')}</p>
    <label class="check decl"><input type="checkbox" id="bugConsent"><span>${L('Ik stuur dit vrijwillig en er staan geen persoonsgegevens in.', 'I’m sending this voluntarily and it contains no personal data.')}</span></label>
    <div class="bug-fallback" id="bugFallback" hidden></div>
  </div><div class="ft"><span class="grow"></span><button class="btn" data-x="cancel">${L('Annuleren', 'Cancel')}</button><button class="btn primary" data-x="send" disabled>${L('Versturen', 'Send')}</button></div>`;
  document.body.appendChild(dlg); dlg.showModal();
  const $ = (s: string) => dlg.querySelector(s) as HTMLElement;
  const text = () => (($('#bugText') as HTMLTextAreaElement).value || '').trim();
  const diag = () => ($('#bugDiag') as HTMLInputElement).checked;
  const consent = () => ($('#bugConsent') as HTMLInputElement).checked;
  const payload = () => buildBugPayload(key, text(), diag() ? String(ctx?.diagnostics ?? '') : '', redactRemote);
  let sending = false, done = false, previewVersion = 0;
  const preview = async () => {
    const version = ++previewVersion;
    const report = await payload();
    if (version === previewVersion) $('#bugPreview').textContent = report.message;
  };
  const update = () => {
    const raw = ($('#bugText') as HTMLTextAreaElement).value;
    $('#bugCount').textContent = `${raw.length} / ${BUG_TEXT_MAX}`;
    $('#bugWarning').hidden = !hasPersonalDetails(raw);
    ($('[data-x=send]') as HTMLButtonElement).disabled = sending || !consent() || !text();
    void preview();
  };
  $('#bugText').addEventListener('input', update);
  $('#bugDiag').addEventListener('change', update);
  $('#bugConsent').addEventListener('change', update);
  update();
  ($('#bugText') as HTMLTextAreaElement).focus();
  /** Closing with typed text asks first, so a report is never lost by an accidental Esc. */
  let asking = false;
  const tryClose = async () => {
    if (sending || asking) return;
    asking = true;
    try {
      if (!done && text() && !(await askConfirm({ title: L('Melding weggooien?', 'Discard report?'), body: L('Wat je hebt getypt, gaat verloren.', 'What you typed will be lost.'), ok: L('Weggooien', 'Discard'), cancel: L('Verder typen', 'Keep typing'), danger: true }))) return;
    } finally { asking = false; }
    dlg.close();
  };
  // Esc is handled here: Chromium's close watcher may skip a "cancel" event (e.g. the first Esc after reopening).
  dlg.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || e.defaultPrevented || e.isComposing) return;
    e.preventDefault(); e.stopPropagation(); tryClose();
  });
  dlg.addEventListener('cancel', (e) => { e.preventDefault(); tryClose(); });
  dlg.addEventListener('close', () => { dlg.remove(); focusBack(backSel, 0, undefined, opener); });
  dlg.querySelectorAll<HTMLElement>('[data-x=cancel],[data-x=close]').forEach((b) => { b.onclick = () => { tryClose(); }; });
  /** The fallback never sends through the user's mailbox. */
  const showFallback = (why: string) => {
    const fb = $('#bugFallback');
    fb.hidden = false;
    fb.innerHTML = `${why ? `<p class="msg bad">${h(why)}</p>` : ''}
      <div class="btn-row"><button class="btn" data-x="copy">${L('Tekst kopiëren', 'Copy text')}</button><button class="btn" data-x="github">${L('Melden op GitHub', 'Report on GitHub')} ${ICON.ext}</button></div>
      <p class="msg">${L('Op GitHub is je melding openbaar en zichtbaar onder je eigen account.', 'On GitHub your report is public and shown under your own account.')}</p>`;
    $('[data-x=github]').onclick = () => { void call('open_external', { url: 'https://github.com/ov-refund/OV-Refund/issues/new' }).catch(() => {}); };
    $('[data-x=copy]').onclick = async () => {
      const report = await payload();
      $('#bugPreview').textContent = report.message;
      try { await navigator.clipboard.writeText(report.message); }
      catch { const ta = document.createElement('textarea'); ta.value = report.message; dlg.appendChild(ta); ta.select(); document.execCommand('copy'); ta.remove(); }
      toast(L('Tekst gekopieerd.', 'Text copied.'));
    };
    $('[data-x=send]').hidden = true;
  };
  $('[data-x=send]').onclick = async () => {
    if (sending || !consent() || !text() || !key) return;
    sending = true;
    dlg.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>('input, textarea').forEach(el => { el.disabled = true; });
    update();
    try {
      const report = await payload();
      ++previewVersion;
      $('#bugPreview').textContent = report.message;
      await sendBugRelay(BUG_RELAY_URL, report);
      toast(L('Je melding is verstuurd.', 'Your report has been sent.'));
      sending = false; done = true; dlg.close();
    } catch {
      showFallback(L('Versturen lukte niet.', 'Sending failed.'));
    } finally {
      sending = false;
      dlg.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>('input, textarea').forEach(el => { el.disabled = false; });
      update();
    }
  };
  if (!key) showFallback('');
}

/** Footer update button: quiet normally, yellow when a new version is available. */
function updateButton(): string {
  const u = state.update;
  if (u?.available) return `<button class="fb-update on" data-act="update" title="${h(L('Nieuwe versie installeren', 'Install the new version'))}">${L(`Update beschikbaar: ${h(u.version ?? '')}`, `Update available: ${h(u.version ?? '')}`)}</button>`;
  return `<button class="fb-update" data-act="checkupdate">${L('Zoeken naar updates', 'Check for updates')}</button>`;
}

// ---------- updates (signed; the app only installs what the maintainer's key signed) ----------
async function checkUpdates(manual: boolean) {
  if ((state.o as any)?.demo) { if (manual) toast(L('In de demo staan updates uit', 'Updates are off in the demo')); return; }
  try {
    const u = await invoke<any>('check_update');
    state.update = u;
    if (state.o) render();
    if (!state.o && (!u.configured || u.enabled === false)) return call('open_external', { url: WEBSITE_URL });
    if (u.available) return showUpdateDialog(); // at launch and on "Check for updates"
    if (manual) toast(!u.configured ? L('Deze versie kan niet automatisch bijwerken', 'This version cannot update automatically') : u.enabled === false ? L('Zoeken naar updates staat uit', 'Update checks are turned off') : L(`Je hebt de nieuwste versie (${u.current})`, `You have the latest version (${u.current})`));
  } catch (e) { if (manual) { toast(friendlyError(e), 'err'); if (!state.o) await call('open_external', { url: WEBSITE_URL }); } }
}

/** "New version available" popup: Update now, or Later (the banner stays; asked again next launch). */
function showUpdateDialog() {
  const u = state.update;
  if (!u?.available || document.querySelector('dialog.update-dlg')) return;
  const dlg = document.createElement('dialog');
  dlg.className = 'update-dlg';
  dlg.innerHTML = `${dialogHead(L(`Nieuwe versie ${h(u.version ?? '')}`, `New version ${h(u.version ?? '')}`))}<div class="bd">
    <p>${L('Bijwerken duurt ongeveer een halve minuut. Daarna start de app vanzelf opnieuw. Je gegevens en instellingen blijven bewaard.', 'Updating takes about half a minute. The app then restarts by itself. Your data and settings are kept.')}</p>
    ${u.notes ? `<details class="preview"><summary>${L('Wat is er nieuw', 'What’s new')}</summary>
<div class="update-notes">${h(pickLang(u.notes))}</div></details>` : ''}
  </div><div class="ft"><span class="grow"></span><button class="btn" data-x="later">${L('Later', 'Later')}</button><button class="btn primary" data-x="now">${L('Nu bijwerken', 'Update now')}</button></div>`;
  document.body.appendChild(dlg);
  const back = document.activeElement as HTMLElement | null;
  dlg.addEventListener('close', () => { dlg.remove(); focusBack('', 0, undefined, back); });
  dlg.showModal();
  (dlg.querySelector('[data-x=now]') as HTMLButtonElement).focus();
  dlg.querySelectorAll<HTMLButtonElement>('[data-x=later],[data-x=close]').forEach((b) => { b.onclick = () => dlg.close(); });
  (dlg.querySelector('[data-x=now]') as HTMLButtonElement).onclick = async () => { dlg.close(); await installUpdate(); };
}

async function installUpdate() {
  if (state.busy) { toast(L('Even wachten tot de controle klaar is.', 'Please wait until the check has finished.')); return; }
  startBusy(L('Bijwerken…', 'Updating…'), 'update');
  try { await invoke('install_update'); } catch { endBusy(); render(); updateFailed(); }
}
/** Updating failed: say so plainly and point to the website download. */
async function updateFailed() {
  if (await askConfirm({ title: L('Bijwerken lukte niet', 'Updating didn’t work'), body: L('Download de nieuwste versie op de website. Je gegevens en instellingen blijven bewaard.', 'Download the latest version from the website. Your data and settings are kept.'), ok: L('Naar de website', 'Go to the website'), cancel: L('Sluiten', 'Close') })) {
    try { await call('open_external', { url: WEBSITE_URL }); } catch { /* shown */ }
  }
}

// ---------- one-time notice for users who installed before the first-start notice existed ----------
async function showDisclaimerOnce() {
  const dlg = document.createElement('dialog');
  dlg.innerHTML = `<div class="hd"><img class="hd-logo" src="/logo-mark.png" alt="" width="24" height="24"><span class="hd-t">${L('Goed om te weten', 'Good to know')}</span></div><div class="bd">
    <p>${L('OV Refund is gratis en niet verbonden aan NS of Arriva. Je gebruikt de app op eigen risico: de makers zijn niet aansprakelijk voor gemiste claims of andere schade. De volledige disclaimer staat op de website.', 'OV Refund is free and not affiliated with NS or Arriva. You use it at your own risk: the authors are not liable for missed claims or any other damage. The full disclaimer is on the website.')}</p>
    <label class="check"><input type="checkbox" id="dsAccept"><span>${L('Ik begrijp dit en gebruik OV Refund op eigen risico', 'I understand and use OV Refund at my own risk')}</span></label>
  </div><div class="ft"><span class="grow"></span><button class="btn primary" data-x="ok" disabled>${L('Doorgaan', 'Continue')}</button></div>`;
  document.body.appendChild(dlg);
  dlg.addEventListener('cancel', (e) => e.preventDefault()); // must be acknowledged
  dlg.showModal();
  const cb = dlg.querySelector('#dsAccept') as HTMLInputElement, ok = dlg.querySelector('[data-x=ok]') as HTMLButtonElement;
  cb.addEventListener('change', () => { ok.disabled = !cb.checked; });
  await new Promise<void>((res) => { ok.onclick = async () => { try { await call('set_flag', { key: 'disclaimer_accepted', value: new Date().toISOString() }); } catch { return; } dlg.close(); dlg.remove(); res(); }; });
}

// ---------- submit dialog ----------
let reviewOpening = false;
async function openReview(claimId: number, opener: HTMLElement | null) {
  if (reviewOpening || document.querySelector('dialog[open]:not(.settings)')) return; // a double click opens one dialog
  reviewOpening = true;
  // Focus goes back to the button that opened the dialog, or to its trip row (or the next row) if that is gone.
  const backSel = focusKey(opener) || `[data-act="review"][data-claim="${claimId}"]`;
  const backRow = rowOf(opener) || (state.rows.find((x) => x.claim_id === claimId)?.id ?? 0);
  const order = rowIds();
  let f: any;
  try { f = await call<any>('claim_facts', { claimId }); } finally { reviewOpening = false; }
  const row = state.rows.find((x) => x.claim_id === claimId);
  const band = ['min60_plus', '60_plus'].includes(f.facts.delay_band) ? L('60 minuten of meer', '60 minutes or more') : L('30 tot en met 59 minuten', '30 to 59 minutes');
  const amount = row?.compensation_cents != null ? h(amountDetail(row.compensation_cents, row.compensation_exact, row.compensation_basis, getLang())) : `<span class="none">${L('NS berekent het bedrag', 'NS works out the amount')}</span>`;
  const cardConflict = !!f.card_conflict;
  let near = !!f.ns_claim_near && !cardConflict;
  // Adjusted timetable that day: the same "decide yourself" caveat as in the trip details.
  const adjusted = row?.reasons.find((x) => /^timetable was adjusted that day/.test(x));
  const dest = h(f.facts.destination_name);
  const date = row ? localDate(row.journey.check_in, getLang()) : h(f.date_local);
  const dlg = document.createElement('dialog');
  dlg.className = 'review';
  dlg.setAttribute('aria-labelledby', 'revT');
  dlg.innerHTML = `${dialogHead(L('Claim indienen bij NS', 'Submit claim to NS'), 'revT')}<div class="bd">
    <dl class="facts">
      <dt>${L('Reis', 'Trip')}</dt><dd>${h(f.facts.origin_name)} → ${dest}</dd>
      <dt>${L('Datum', 'Date')}</dt><dd>${date}, ${h(f.check_in_local)} – ${h(f.check_out_local)} ${help('revdate', `${L('Geplande vertrektijd', 'Scheduled departure')}: ${h(f.facts.scheduled_departure_local)}.<br>${L('OV Refund vult deze gegevens in op het officiële NS-formulier.', 'OV Refund enters these details in the official NS form.')}`)}</dd>
      <dt>${L('Vertraging', 'Delay')}</dt><dd>${h(band)}</dd>
      <dt>${L('Vergoeding', 'Refund')}</dt><dd class="amt-dd">${amount}</dd>
      <dt>${L('Rekening', 'Bank account')}</dt><dd>${f.bank ? h(f.bank) : `<span class="none">${L('niet ingesteld', 'not set')}</span> <button class="btn small" data-x="setbank">${L('Rekening instellen', 'Set up bank account')}</button>`}</dd>
    </dl>
    <fieldset class="reach"><legend>${L(`Heb je je reis tot ${dest} afgemaakt?`, `Did you complete your trip to ${dest}?`)}</legend>
      <label class="check"><input type="radio" name="reach" id="reachYes"><span>${L('Ja', 'Yes')}</span></label>
      <label class="check"><input type="radio" name="reach" id="reachNo"><span>${L('Nee, ik ben door de vertraging eerder gestopt of teruggereisd', 'No, I stopped early or turned back because of the delay')}</span></label>
      <p class="note warn" id="reachNote" hidden>${ICON.warn}<span>${L('Dien deze reis dan zelf in via Mijn NS. OV Refund kan dit geval niet invullen.', 'Then submit this trip yourself via Mijn NS. OV Refund can’t fill in this case.')} <a href="${NS_MIJNNS_URL}" target="_blank" rel="noopener">Mijn NS ${ICON.ext}</a></span></p>
    </fieldset>
    ${adjusted ? `<p class="note warn">${ICON.warn}<span>${h(reasonText(adjusted, getLang()).replace(/[.]?$/, '.'))}</span></p>` : ''}
    ${f.was_uncertain ? `<p class="note warn" role="alert">${ICON.warn}<span>${L('Eerdere indiening onzeker', 'Earlier submission uncertain')} ${help('revwu', uncertainHint())}</span></p><label class="check"><input type="checkbox" id="resub"><span>${L('Ik heb gecontroleerd: hij staat niet in Mijn teruggaven', 'I checked: it is not listed in Mijn teruggaven')}</span></label>` : ''}
    ${cardConflict ? note(cardConflictText(), '', 'warn') : ''}
    <div id="nearCheck" ${near ? '' : 'hidden'}><p class="note warn">${ICON.warn}<span>${L('Mogelijk al geclaimd bij NS', 'Possibly already claimed with NS')} ${help('revnear', nearHint())}</span></p><label class="check"><input type="checkbox" id="nearOk"><span>${L('Ik heb Mijn teruggaven gecontroleerd en deze reis staat er niet bij', 'I checked Mijn teruggaven and this trip is not listed')}</span></label></div>
    <div id="unavailableCheck" hidden><p class="note warn">${L('OV Refund kon Mijn teruggaven niet lezen. Kijk in Mijn NS of deze reis er al staat.', 'OV Refund could not read Mijn teruggaven. Check in Mijn NS whether this trip is already listed.')} <a href="${NS_MIJNNS_URL}" target="_blank" rel="noopener">Mijn NS ${ICON.ext}</a></p><label class="check"><input type="checkbox" id="listOk"><span>${L('Ik heb Mijn teruggaven gecontroleerd en deze reis staat er niet bij', 'I checked Mijn teruggaven and this trip is not listed')}</span></label></div>
    <label class="check decl"><input type="checkbox" id="decl"><span>${L('Ik verklaar dat deze gegevens kloppen en dat ik deze reis nog niet eerder heb geclaimd (ook niet via een andere dienst).', 'I declare that these details are correct and that I have not claimed this trip before (not even through another service).')}</span></label>
    <div class="pf" id="pf" hidden role="status"></div>
  </div><div class="ft"><span class="msg sub-hint" id="subHint" role="status"></span><button class="btn" data-x="pf">${L('Eerst proberen (verstuurt niets)', 'Try first (sends nothing)')}</button>${help('revpf', L('OV Refund vult het NS-formulier op de achtergrond in en stopt vóór het versturen. Zo zie je of alles werkt.', 'OV Refund fills in the NS form in the background and stops before sending. So you can see that everything works.'))}<span class="grow"></span><button class="btn primary" data-x="submit" disabled>${L('Indienen bij NS', 'Submit to NS')}</button></div>`;
  document.body.appendChild(dlg); dlg.showModal();
  const $ = (s: string) => dlg.querySelector(s) as HTMLElement;
  const sub = $('[data-x=submit]') as HTMLButtonElement, decl = $('#decl') as HTMLInputElement, pf = $('#pf');
  const pfBtn = $('[data-x=pf]') as HTMLButtonElement;
  const closeBtn = $('[data-x=close]') as HTMLButtonElement;
  const resub = dlg.querySelector('#resub') as HTMLInputElement | null;
  const nearOk = $('#nearOk') as HTMLInputElement;
  const listOk = $('#listOk') as HTMLInputElement;
  let listUnavailable = false;
  const reached = () => ($('#reachYes') as HTMLInputElement).checked;
  const canSubmit = () => !cardConflict && decl.checked && !!f.bank && reached() && (!f.was_uncertain || !!resub?.checked) && (!near || nearOk.checked) && (!listUnavailable || listOk.checked);
  const reachAnswer = () => (reached() ? true : ($('#reachNo') as HTMLInputElement).checked ? false : null);
  const update = () => {
    sub.disabled = !canSubmit(); pfBtn.disabled = cardConflict || !reached(); ($('#reachNote')).hidden = !($('#reachNo') as HTMLInputElement).checked;
    // A disabled Submit button always says what is still missing.
    $('#subHint').textContent = sub.hidden || cardConflict ? '' : listUnavailable && !listOk.checked
      ? L('Controleer eerst Mijn teruggaven.', 'Check Mijn teruggaven first.')
      : submitHint({ reached: reachAnswer(), bank: !!f.bank, resubNeeded: !!f.was_uncertain, resubOk: !!resub?.checked, nearNeeded: near, nearOk: !!nearOk?.checked, declared: decl.checked }, getLang());
  };
  dlg.addEventListener('change', update);
  update();
  // While the NS form is being filled or sent, Esc / Close must not hide what is happening.
  let working = false;
  dlg.addEventListener('cancel', (e) => { if (working) e.preventDefault(); });
  dlg.addEventListener('close', () => { dlg.remove(); focusBack(backSel, backRow, order, opener); });
  dlg.addEventListener('click', (ev) => {
    const a = (ev.target as HTMLElement).closest('a[target=_blank]') as HTMLAnchorElement | null;
    if (a) { ev.preventDefault(); call('open_external', { url: a.href }).catch(() => {}); }
  });
  closeBtn.onclick = () => { if (!working) dlg.close(); };
  dlg.querySelector<HTMLButtonElement>('[data-x=setbank]')?.addEventListener('click', () => { dlg.close(); state.edit = 'bank'; render(); });
  pfBtn.onclick = async () => {
    if (working) return;
    working = true; pfBtn.disabled = true; closeBtn.disabled = true; pf.hidden = false;
    pf.innerHTML = `<span class="spin"></span>${L('NS-formulier invullen…', 'Filling in the NS form…')}`;
    try { const n = await invoke<string[]>('preflight_claim', { claimId }); pf.innerHTML = `<b>${L('Gelukt, er is niets verstuurd.', 'Done, nothing was sent.')}</b><details class="tech"><summary>${L('Details', 'Details')}</summary>${n.map((x) => h(friendlyError(x))).join('<br>')}</details>`; } catch (e) { pf.textContent = friendlyError(e); }
    finally { working = false; pfBtn.disabled = !reached(); closeBtn.disabled = false; if (dlg.open) pfBtn.focus({ preventScroll: true }); }
  };
  sub.onclick = async () => {
    if (working || !canSubmit()) return;
    working = true; closeBtn.disabled = true; pfBtn.disabled = true;
    sub.disabled = true; sub.innerHTML = `<span class="spin"></span>${L('Indienen…', 'Submitting…')}`;
    try {
      const r = await call<any>('submit_claim', { req: {
        claim_id: claimId, confirmed: f.facts, declaration_accepted: decl.checked,
        confirm_resubmit: !!f.was_uncertain && !!resub?.checked, reached_destination: reached(),
        confirm_near: near && nearOk.checked, confirm_claim_list_unavailable: listUnavailable && listOk.checked,
      } });
      if (r.needs_claim_list_confirmation || r.needs_near_confirmation) {
        if (r.needs_claim_list_confirmation) {
          listUnavailable = true;
          $('#unavailableCheck').hidden = false;
          listOk.focus();
        } else {
          near = true;
          $('#nearCheck').hidden = false;
          nearOk.focus();
        }
        sub.textContent = L('Indienen bij NS', 'Submit to NS');
        update();
        return;
      }
      const last = (r.events ?? []).slice(-1).map((e: ClaimEvent) => h(historyDetail(e.detail, getLang()))).join('');
      const next = r.status === 'INDETERMINATE'
        ? L('Controleer Mijn teruggaven voordat je opnieuw indient.', 'Check Mijn teruggaven before submitting again.')
        : r.status === 'EXTERNALLY_CLAIMED'
          ? L('Voor deze reis staat al een claim geregistreerd. Controleer Mijn teruggaven of je claimdienst.', 'A claim is already recorded for this trip. Check Mijn teruggaven or your claim service.')
          : last;
      const tech = `<details class="tech"><summary>${L('Details', 'Details')}</summary>${h(statusLabel(r.status))}${r.reference ? `, ${L('referentie', 'reference')} ${h(r.reference)}` : ''}${last ? `<br>${last}` : ''}</details>`;
      pf.hidden = false;
      pf.innerHTML = r.status === 'SUBMITTED'
        ? `<b>${L('Ingediend!', 'Submitted!')}${(state.o as any)?.demo ? ' (demo)' : ''}</b> ${L('NS stuurt je meestal een bevestiging per mail.', 'NS usually e-mails you a confirmation.')}${tech}`
        : `<b>${h(statusLabel(r.status))}</b> ${next}${tech}`;
      // Done: nothing in the form can be changed any more.
      dlg.querySelectorAll<HTMLInputElement>('.bd input').forEach((i) => { i.disabled = true; });
      sub.textContent = L('Klaar', 'Done'); sub.hidden = true; pfBtn.hidden = true; dlg.querySelector('.ft .qhelp')?.remove(); $('#subHint').textContent = '';
      const fin = document.createElement('button'); fin.className = 'btn primary'; fin.textContent = L('Sluiten', 'Close'); fin.onclick = () => dlg.close();
      $('.ft').appendChild(fin); fin.focus();
      refresh();
    } catch { sub.textContent = L('Indienen bij NS', 'Submit to NS'); update(); }
    finally { working = false; closeBtn.disabled = false; }
  };
}

// ---------- events ----------
const val = (id: string) => (document.getElementById(id) as HTMLInputElement | null)?.value ?? '';
const chk = (id: string) => (document.getElementById(id) as HTMLInputElement | null)?.checked ?? false;

// ---------- busy state (one check or update at a time) ----------
let busyShow = 0;
/** When the last foreground check ended: its own refresh covers the backend's "run-finished" event. */
let lastRunEnd = 0;
/** Start a busy period. With `showAfterMs`, the busy UI only appears if it is still running by then. */
function startBusy(label: string, kind: 'scan' | 'update', showAfterMs = 0) {
  state.busy = label; state.busyKind = kind; state.busySince = Date.now(); state.stopping = false;
  state.progress = { pct: 0, stage: kind === 'scan' ? 'login' : '' };
  clearTimeout(busyShow);
  state.busyQuiet = showAfterMs > 0;
  if (state.busyQuiet) busyShow = window.setTimeout(() => { if (state.busy && state.busyQuiet) { state.busyQuiet = false; render(); } }, showAfterMs);
  else render();
  if (kind === 'scan' && (state.o as any)?.demo) {
    clearInterval(arrivaStatusTimer);
    arrivaStatusTimer = window.setInterval(() => { void pollArrivaStatus(); }, 900);
  }
}
function endBusy() {
  clearInterval(arrivaStatusTimer); arrivaStatusTimer = 0;
  clearTimeout(monthRefreshTimer); monthRefreshTimer = 0;
  clearTimeout(busyShow);
  if (state.busyKind === 'scan') lastRunEnd = Date.now();
  state.busy = ''; state.busyKind = ''; state.busyQuiet = false;
}

async function scan(full = false) {
  if (state.busy) { toast(L('Er loopt al een controle', 'A check is already running')); return; }
  state.restored = null; // the check re-assesses the restored trips
  startBusy(full ? L('Volledige reishistorie ophalen…', 'Fetching full travel history…') : L('Controleren…', 'Checking…'), 'scan');
  try {
    const s = await call<any>('scan_now', { full });
    const errs: string[] = s?.errors ?? [];
    // A login problem: only that message. Otherwise the result, then the first problem.
    if (errs.length && s?.auth_state !== 'ok') { toast(friendlyError(errs[0]), 'err'); return; }
    await refresh();
    const claimable = state.rows.filter(r => inFilter(r, 'claimable')).length;
    const review = state.rows.filter(r => inFilter(r, 'review')).length;
    // With automatic submission on, also say how many went to NS by themselves.
    const autoSent = s.mode === 'automatic' ? Number(s.submitted ?? 0) : null;
    const refused = Number(s.refused ?? 0);
    const sent = (autoSent === null ? '' : L(`, ${autoSent} automatisch ingediend`, `, ${autoSent} submitted automatically`))
      + (refused > 0 ? L(`, ${refused} niet automatisch verstuurd`, `, ${refused} not sent automatically`) : '');
    const issue = errs.length ? `. ${friendlyError(errs[0])}` : '';
    // Automatic submission on but paused: say why, with a jump to the claim it waits for.
    let paused = autoSent !== null && s.auth_state === 'ok' ? runPause(s) : '';
    const target = paused ? runPauseTarget(s) : undefined;
    // Automatic submission on and nothing else paused it: why claims were left (none, or the run's own counts).
    if (!paused && autoSent !== null && !errs.length) paused = checkNowHint(Number(s.near_skipped ?? 0), Number(s.auto_later ?? 0));
    toast(s.auth_state === 'ok' ? `${L(`${s.journeys_seen} ${s.journeys_seen === 1 ? 'reis' : 'reizen'} gecontroleerd: ${claimable} te claimen, ${review} te beoordelen${sent}`, `${s.journeys_seen} ${s.journeys_seen === 1 ? 'trip' : 'trips'} checked: ${claimable} claimable, ${review} to review${sent}`)}${paused ? `. ${paused}` : ''}${issue}` : L('Log eerst in bij Mijn NS.', 'Log in to Mijn NS first.'),
      errs.length ? 'err' : '', target ? { label: L('Bekijken', 'Review'), run: () => goFirst('all', target.claim_status ?? undefined) } : undefined);
  } finally { endBusy(); await refresh(); }
}

// ---------- progress (updated in place inside the "Check now" button) ----------
function updateRing() {
  const p = state.progress;
  const fill = document.getElementById('progFill'), txt = document.getElementById('progTxt'), ring = document.getElementById('progRing');
  const t = ringText();
  if (fill) fill.setAttribute('stroke-dashoffset', String(ringOffset(p.pct)));
  const stop = document.getElementById('stopBtn') as HTMLButtonElement | null;
  if (stop) stop.hidden = !submitting() || state.stopping;
  if (txt && txt.textContent !== t) txt.textContent = t;
  if (ring) { ring.setAttribute('aria-valuenow', String(p.pct)); ring.setAttribute('aria-valuetext', t); }
}
listen<{ pct: number; stage: string; month?: string; index?: number; total?: number; seconds?: number }>('scan-progress', (e) => {
  // never jump backwards within one check (stages can report out of order)
  state.progress = { ...e.payload, pct: Math.max(e.payload.pct, state.busy ? state.progress.pct : 0) };
  updateRing();
});

// Use the normal redraw: paint/render preserve focus, input, open rows and scroll.
// Coalesce fast monthly batches; a trailing refresh includes the newest results.
let monthRefreshTimer = 0, lastMonthRefresh = 0;
// Also after every automatic submission, so its new state shows while the check goes on.
listen('claim-submitted', () => throttledRefresh());
listen('scan-month-done', () => throttledRefresh());
function throttledRefresh() {
  if (monthRefreshTimer) return;
  monthRefreshTimer = window.setTimeout(() => {
    monthRefreshTimer = 0;
    lastMonthRefresh = Date.now();
    void refresh();
  }, Math.max(0, 1500 - (Date.now() - lastMonthRefresh)));
}

/** A note from the last settings save (e.g. the e-mail summary was switched off), shown instead of "Saved". */
let saveNote = '';
const mailPasswordMissingText = () => L('E-mailoverzicht staat uit: app-wachtwoord ontbreekt.', 'E-mail summary is off: app password missing.');
/** Returns true when the settings were saved (false when the user cancelled a confirmation). */
async function saveSettingsFromPanel(): Promise<boolean> {
  const s = state.settings;
  if (!s) { toast(L('Instellingen nog niet geladen. Probeer het zo opnieuw.', 'Settings not loaded yet. Please try again shortly.')); refresh(); return false; }
  const o = state.o!;
  // Each save sends every setting: a control that is not on screen keeps its stored value.
  const has = (id: string) => !!document.getElementById(id);
  const checksOn = has('schedOn') ? chk('schedOn') : o.schedule.installed;
  // Unanswered claim-service question (null) blocks automatic submission, just like an unfinished import.
  const svcOk = o.other_service === 'none' || o.other_service === 'import_done';
  const autoSubmit = checksOn && svcOk && (has('autoSubmit') ? chk('autoSubmit') : o.mode === 'automatic' && !!o.auto_opt_in_at);
  // The e-mail summary needs a stored (app) password.
  const mailOn = (has('nMail') ? chk('nMail') : !!s.notify_email) && !!s.smtp_password_set && !!o.account.notify_email;
  const firstTimeAuto = autoSubmit && !(o.mode === 'automatic' && o.auto_opt_in_at);
  const unverified = !(o as any)?.gateway_verified_by_submission;
  if (firstTimeAuto && !(await confirmAutoSubmit(unverified))) { render(); return false; }
  try {
    const res = await call<any>('save_settings', { s: {
      mode: autoSubmit ? 'automatic' : 'manual', auto_consent: firstTimeAuto, auto_unverified_consent: firstTimeAuto && unverified, interval_days: Number(has('interval') ? val('interval') : (o.schedule.interval_days ?? o.interval_days)) || 7,
      notify_windows: has('nWin') ? chk('nWin') : !!s.notify_windows, notify_email: mailOn, schedule_enabled: has('schedOn') ? chk('schedOn') : o.schedule.installed,
      smtp: s?.smtp ?? { host: '', port: 587, security: 'starttls', username: '', from: '', to: '' }, smtp_password: null,
    } });
    saveNote = res?.mail_password_missing ? mailPasswordMissingText() : '';
    return true;
  } finally { await refresh(); lockSettings(false); }
}

/** Save the e-mail form. `keepOpen`: for "Test e-mail" (no welcome mail, form stays open). */
async function saveEmail(keepOpen: boolean): Promise<boolean> {
  const s = state.settings;
  if (!s) { toast(L('Instellingen nog niet geladen. Probeer het zo opnieuw.', 'Settings not loaded yet. Please try again shortly.')); return false; }
  const typedTo = val('smtpTo').trim();
  // An untouched demo mailbox (not shown in the field) stays as it is.
  const to = typedTo;
  if (!isEmail(to)) { toast(L('Dit e-mailadres klopt niet. Controleer het adres.', 'This e-mail address is not valid. Check the address.')); return false; }
  if (isUnsupportedMail(to)) { toast(unsupportedMailText(), 'err'); return false; }
  const p = mailPreset(to);
  const manual = !!document.getElementById('smtpHost');
  const smtp = manual
    ? { host: val('smtpHost'), port: Number(val('smtpPort') || 587), security: val('smtpSec'), username: val('smtpUser') || to, from: to, to }
    : p ? { host: p.host, port: p.port, security: p.security, username: to, from: to, to } : null;
  if (!smtp) { toast(L('Mailprovider niet herkend. Vul de server in via Geavanceerd.', 'Mail provider not recognised. Fill in the server under Advanced.')); return false; }
  if (!Number.isInteger(smtp.port) || smtp.port < 1 || smtp.port > 65535) { toast(L('Poort moet tussen 1 en 65535 liggen (meestal 587)', 'Port must be between 1 and 65535 (usually 587)')); return false; }
  if (manual && !smtp.host.trim()) { toast(L('Vul de server voor uitgaande mail in', 'Fill in the outgoing mail server')); return false; }
  const pw = val('smtpPw');
  if (keepOpen && !pw && !s.smtp_password_set) { toast(L('Vul eerst het wachtwoord in.', 'Enter the password first.')); return false; }
  // A stored password only belongs to the server and username it was saved for (the backend refuses it too).
  if (!pw && s.smtp_password_set && smtpLoginChanged(s.smtp, smtp)) { toast(smtpPasswordAgainText(getLang()), 'err'); document.getElementById('smtpPw')?.focus(); return false; }
  // Without a (stored) password the address is saved, but the e-mail summary stays off.
  const hasPw = !!pw || (!!s.smtp_password_set && !smtpLoginChanged(s.smtp, smtp));
  const res = await call<any>('save_settings', { s: { mode: s.mode, auto_consent: false, interval_days: s.schedule.interval_days ?? s.interval_days, notify_windows: s.notify_windows, notify_email: hasPw && (!keepOpen || !!s.notify_email), schedule_enabled: s.schedule.installed, smtp_password: pw || null, smtp } });
  if (!keepOpen) {
    state.edit = ''; toast(res?.mail_password_missing ? mailPasswordMissingText() : hasPw ? L('E-mail opgeslagen', 'E-mail saved') : L(`E-mailadres opgeslagen. ${mailPwLabel(to)} ontbreekt, dus het e-mailoverzicht staat uit.`, `E-mail address saved. ${mailPwLabel(to)} missing, so the e-mail summary is off.`));
    if (pw) call<string>('test_email').then(() => toast(L('Welkomstmail verstuurd. Kijk in je inbox.', 'Welcome e-mail sent. Check your inbox.')), () => {});
  }
  await refresh();
  return true;
}

/** Bank form: name and a valid IBAN are both required (as in the wizard). */
function updateBankSave() {
  const b = document.getElementById('saveBank') as HTMLButtonElement | null;
  if (b) b.disabled = checkIban(val('bankIban')).state !== 'valid' || !validHolderName(val('bankName'));
}

/** Close the open inline form and give focus back to whatever opened it. */
function closeInlineForm() {
  const was = state.edit;
  state.edit = '';
  pendingFocus = was === 'nslogin' ? '#autoLogin' : `[data-act="edit"][data-what="${was}"]`;
  pendingBox = was === 'nslogin' || was === 'import' ? setDlg : app;
  (document.activeElement as HTMLElement | null)?.blur();
  render();
  pendingFocus = ''; pendingBox = null;
}

/** A status change can make the row leave the current filter: say what happened and fade it out first. */
async function statusChanged(claimId: number, newStatus: string, msg: string, action?: { label: string; run: () => void }) {
  toast(msg, '', action);
  const r = state.rows.find((x) => x.claim_id === claimId);
  if (r && !inFilter({ ...r, claim_status: newStatus }, state.cat)) {
    const rows = app.querySelectorAll(`tr[data-row="${r.id}"], tr[data-detail="${r.id}"]`);
    rows.forEach((x) => x.classList.add('vanish'));
    if (rows.length) await wait(160);
  }
  await refresh();
}

function onInput(ev: Event) {
  const el = ev.target as HTMLInputElement;
  if (el.id === 'exDate' || el.id === 'exFrom' || el.id === 'exTo') { clearExtErrors(el.id); return; }
  if (el.id === 'smtpTo') {
    const hint = document.getElementById('mailHint'); if (hint && !document.getElementById('smtpHost')) hint.innerHTML = mailSteps(el.value, 'data-act="openlink"');
    const lbl = document.getElementById('smtpPwLabel'); if (lbl) lbl.textContent = mailPwLabel(el.value);
    return;
  }
  if (el.id === 'arrValid') {
    const note = document.getElementById('arrValidNote');
    if (note) note.hidden = !expiryInPastNote(el.value, state.arrivaContact?.card_valid_until);
    return;
  }
  if (el.id === 'bankName') {
    const m = document.getElementById('bankNameMsg'), bad = !!el.value && !validHolderName(el.value);
    if (m) { m.textContent = bankNameHint(el.value, getLang()); m.className = `msg namemsg${bad ? ' bad' : ''}`; }
    el.setAttribute('aria-invalid', String(bad)); updateBankSave(); return;
  }
  if (el.id !== 'bankIban') return;
  applyIbanFormat(el);
  const c = checkIban(el.value, getLang());
  const msg = document.getElementById('ibanMsg')!;
  msg.textContent = c.message;
  msg.className = `ibanmsg msg iban-${c.state}`;
  updateBankSave();
}

let settingsSaving = false;
const SETTING_TOGGLES = ['schedOn', 'interval', 'autoSubmit', 'nWin', 'nMail'];
/** Disable the settings while one is being saved (each save sends all of them). */
function lockSettings(on: boolean) {
  settingsSaving = on;
  [...SETTING_TOGGLES, 'readMail', 'autoLogin', 'svcQYes', 'svcQNo'].forEach((id) => {
    const x = document.getElementById(id) as HTMLInputElement | null;
    if (!x) return;
    if (on) { x.dataset.wasDisabled ??= String(x.disabled); x.disabled = true; }
    else if (x.dataset.wasDisabled !== undefined) { x.disabled = x.dataset.wasDisabled === 'true'; delete x.dataset.wasDisabled; }
  });
  if (!on && pendingFocus && !document.querySelector('dialog[open]:not(.settings)')) {
    (pendingBox ?? app).querySelector<HTMLElement>(pendingFocus)?.focus({ preventScroll: true });
  }
}
/** Keep focus on a control across the save + redraw. */
const holdFocus = (sel: string, el: Element) => { pendingFocus = sel; pendingBox = boxOf(el); };
const releaseFocus = () => { pendingFocus = ''; pendingBox = null; };

/** Restore a back-up: let the backend check the file first, then ask before replacing anything. */
async function restoreBackup(file: File) {
  if (backupTooLarge(file.size)) { toast(tooLargeText(), 'err'); return; }
  const quiet = <T,>(cmd: string, args?: Record<string, unknown>) => invoke<T>(cmd, args);
  try {
    let r: any;
    try { r = await restoreBackupFile(file, false, quiet, toast); }
    catch (e) {
      // "Already has data" means the file itself passed the backend's checks: now ask before replacing.
      if (!/Bevestig dat de back-up|Confirm that the backup/.test(String(e))) { toast(friendlyError(e), 'err'); return; }
      if (!(await askConfirm({ title: L('Back-up terugzetten?', 'Restore the backup?'), body: `${L('De back-up vervangt je huidige reizen. Claims die al bij NS zijn, blijven bewaard. OV Refund maakt eerst zelf een back-up.', 'The backup replaces your current trips. Claims that already reached NS are kept. OV Refund first makes a backup itself.')} ${help('rest', L('Wachtwoorden, IBAN, kaartnummer en e-mailinstellingen zitten niet in een back-up.', 'Passwords, IBAN, card number and e-mail settings are not part of a backup.'))}`, ok: L('Terugzetten', 'Restore'), focusCancel: true }))) return;
      r = await restoreBackupFile(file, true, call, toast);
    }
    if (r) {
      // The details stay on the main screen (a banner with Check now); the toast just confirms.
      state.restored = r;
      closeSettings();
      toast(L('Back-up teruggezet.', 'Backup restored.'));
    }
  } catch { /* shown by call() */ } finally { await refresh(); }
}

async function onChange(ev: Event) {
  const el = ev.target as HTMLInputElement;
  if (el.id === 'readMail') {
    holdFocus('#readMail', el); lockSettings(true);
    try {
      await call('set_read_ns_mail', { enabled: el.checked });
      if (el.checked) { toast(L('NS-mails lezen…', 'Reading NS e-mails…')); const r = await call<string[]>('sync_ns_mail'); toast(r.map(pickLang).join(' · ')); }
      else toast(L('Opgeslagen', 'Saved'));
    } finally { lockSettings(false); await refresh(); releaseFocus(); }
    return;
  }
  if (el.id === 'restoreFile' && el.files?.[0]) { const file = el.files[0]; el.value = ''; return restoreBackup(file); }
  if (el.name === 'svcQ') {
    const yes = el.id === 'svcQYes';
    // From "yes" (import not done yet) to "no" unlocks automatic submission: ask first.
    if (!yes && state.o?.other_service === 'yes' && !(await askConfirm({ title: L('Nooit een claimdienst gebruikt?', 'Never used a claim service?'), body: L('Kies dit alleen als je nog nooit een claimdienst hebt gebruikt. Anders kan een reis twee keer worden geclaimd.', 'Only choose this if you have never used a claim service. Otherwise a trip could be claimed twice.'), ok: L('Klopt, nooit', 'That’s right, never'), focusCancel: true }))) {
      const back = document.getElementById('svcQYes') as HTMLInputElement | null;
      if (back) { back.checked = true; back.focus({ preventScroll: true }); }
      return;
    }
    // "Yes" after a finished import stays "import_done"; a new "yes" waits for Import done again.
    const value = yes ? (state.o?.other_service === 'import_done' ? 'import_done' : 'yes') : 'none';
    const wasAuto = state.o?.mode === 'automatic';
    holdFocus('#' + el.id, el); lockSettings(true);
    try {
      await call('set_flag', { key: 'other_service', value });
      // A claim service means a risk of double claims: the app switches automatic submission off
      // for real right away (mode manual, consent withdrawn), also while a check runs.
      // "Nee" later leaves it off; it is switched on again with the toggle.
      let off = false;
      if (value === 'yes' && wasAuto) { await refresh(); off = state.o?.mode !== 'automatic'; }
      const importHint = L('Voeg de reizen die de dienst al claimde toe bij Gegevens & back-up en klik daar op Import klaar.', 'Add the trips that service already claimed under Data & backup, then click Import done there.');
      toast(off ? `${L('Automatisch indienen staat uit.', 'Automatic submission is off.')} ${importHint}` : value === 'yes' ? `${L('Opgeslagen.', 'Saved.')} ${importHint}` : L('Opgeslagen', 'Saved'));
    } finally { lockSettings(false); await refresh(); releaseFocus(); }
    return;
  }
  if (el.id === 'demoSub') { await call('demo_set_subscription', { product: el.value }); toast(L('Abonnement gewijzigd. Reizen worden opnieuw berekend.', 'Subscription changed. Recalculating trips.')); return scan(); }
  if (el.id === 'autoLogin') {
    if (el.checked) { state.edit = 'nslogin'; return render(); }
    // Only the open (unsaved) form: just close it. A stored password is only deleted after confirmation.
    if (!(state.o as any)?.auto_login) { closeInlineForm(); return; }
    if (!(await askConfirm({ title: L('Automatisch inloggen uitzetten?', 'Turn off automatic login?'), body: L('Je opgeslagen NS-wachtwoord wordt gewist. Als je NS-sessie verloopt, log je zelf weer in.', 'Your saved NS password is deleted. When your NS session expires, you’ll need to log in yourself.'), ok: L('Uitzetten', 'Turn off') }))) { el.checked = true; el.focus(); return; }
    state.edit = ''; holdFocus('#autoLogin', el); lockSettings(true);
    try { await call('set_auto_login', { enabled: false }); toast(L('Opgeslagen NS-wachtwoord gewist', 'Saved NS password deleted')); }
    finally { lockSettings(false); await refresh(); releaseFocus(); }
    return;
  }
  if (SETTING_TOGGLES.includes(el.id)) {
    // Switched off while a check submits claims: the claim being sent now still goes through.
    const stopsRun = !el.checked && (el.id === 'autoSubmit' || (el.id === 'schedOn' && state.o?.mode === 'automatic')) && !!state.busy && state.busyKind === 'scan';
    holdFocus('#' + el.id, el); lockSettings(true);
    let saved = false;
    try { saved = await saveSettingsFromPanel(); if (saved) toast(saveNote || (stopsRun ? switchedOffText() : L('Opgeslagen', 'Saved'))); }
    finally {
      // "Stopping…" only once automatic submission is really off (a refused save changes nothing).
      if (stopsRun && state.busy && (saved || state.o?.mode !== 'automatic')) { state.stopping = true; updateRing(); }
      lockSettings(false); releaseFocus();
    }
    return;
  }
  if (el.dataset.act === 'setstatus' && el.value) {
    // Arrow keys on a closed list change the value at once: wait for Enter (see onKeydown).
    if (el.dataset.kbd === '1') return;
    return confirmStatus(el as unknown as HTMLSelectElement);
  }
  if (el.dataset.act === 'commission') {
    const pct = parseAmount(el.value.replace('%', ''));
    if (pct !== null && (Number.isNaN(pct) || pct < 0 || pct > 90)) { fieldError(el, L('Vul een percentage tussen 0 en 90 in (bijv. 25)', 'Enter a percentage between 0 and 90 (e.g. 25)')); return; }
    try { await call('set_commission', { claimId: Number(el.dataset.claim), percent: pct }); } finally { refresh(); }
    return;
  }
  if (el.dataset.act === 'paid') {
    const eur = parseAmount(el.value);
    if (eur !== null && (Number.isNaN(eur) || eur > 1000)) { fieldError(el, L('Vul een bedrag tussen 0 en 1000 in, bijv. 4,17', 'Enter an amount between 0 and 1000, e.g. 4.17')); return; }
    try { await call('set_paid_amount', { claimId: Number(el.dataset.claim), euros: eur }); } finally { refresh(); }
    return;
  }
}

/** The "Press Enter to confirm" hint under a status list moved with the arrow keys. */
function selHint(sel: HTMLSelectElement, on: boolean) { const m = document.getElementById(`selHint${sel.dataset.claim}`); if (m) m.hidden = !on; }
/** Ask before a manual status change, then save it. */
async function confirmStatus(sel: HTMLSelectElement) {
  const el = sel;
  {
    selHint(sel, false);
    const label = statusLabel(el.value);
    const target = state.rows.find((x) => x.claim_id === Number(el.dataset.claim));
    const q = el.value === 'FAILED' && target && isArrivaReal(target)
      ? L('Kies dit alleen als je in je e-mail van Arriva hebt gezien dat er geen bevestiging is. Daarna kun je opnieuw claimen. OV Refund vraagt je dan nog een keer om dit te bevestigen.', 'Only choose this if you have seen in your e-mail from Arriva that there is no confirmation. You can then claim again. OV Refund will ask you to confirm this once more.')
      : el.value === 'FAILED'
      ? L('Kies dit alleen als je in Mijn NS → Mijn teruggaven hebt gezien dat de claim daar niet staat. Daarna kun je hem opnieuw indienen. OV Refund vraagt je dan nog een keer om dit te controleren.', 'Only choose this if you have seen in Mijn NS → Mijn teruggaven that the claim is not listed there. You can then submit it again. OV Refund will ask you to check this once more.')
      : L('Daarna kun je alleen verder naar een volgende status.', 'After this you can only move it forward.');
    if (!(await askConfirm({ title: L(`Status wijzigen naar “${label}”?`, `Change the status to “${label}”?`), body: q, ok: L('Wijzigen', 'Change') }))) { el.value = ''; sel.focus(); return; }
    const claimId = Number(el.dataset.claim), status = el.value;
    const order = rowIds(), row = rowOf(sel);
    holdFocus(focusKey(sel), sel); pendingRow = row;
    sel.disabled = true;
    try {
      try { await call('set_claim_status', { claimId, status }); } catch { sel.disabled = false; el.value = ''; sel.focus(); return; }
      // "Niet verzonden": the trip leaves this list and can be claimed again; say where it went.
      const r = state.rows.find((x) => x.claim_id === claimId);
      const to: Filter | null = r && status === 'FAILED' ? (inFilter({ ...r, claim_status: status }, 'review') ? 'review' : 'claimable') : null;
      const moved = !!r && !!to && !inFilter({ ...r, claim_status: status }, state.cat);
      const movedText = to === 'review' ? L('Verplaatst naar Beoordelen', 'Moved to Review') : L('Verplaatst naar Te claimen', 'Moved to Claimable');
      await statusChanged(claimId, status, moved ? movedText : L(`Status gewijzigd naar ${label}`, `Status changed to ${label}`),
        moved && r && to ? { label: L('Bekijken', 'View'), run: () => showTrip(r.id, to) } : undefined);
      focusBack('', row, order);
    } finally { releaseFocus(); pendingRow = 0; }
  }
}

async function onClick(ev: Event) {
  const t = ev.target as HTMLElement;
  // Form fields, labels and <summary> toggles handle themselves.
  if (t.closest('input,select,label,summary') && !t.closest('[data-act]')) return;
  const link = t.closest('a[target=_blank]') as HTMLAnchorElement | null;
  if (link) { ev.preventDefault(); await call('open_external', { url: link.href }); return; }
  if (t.closest('details') && !t.closest('[data-act]')) return;
  if (t.closest('a.skip')) { ev.preventDefault(); document.getElementById('trips')?.focus(); return; }
  if (setDlg?.contains(t) && t.closest('[data-x=close]')) { closeSettings(); return; }
  if (t.closest('[data-theme-toggle]')) { withTransition(() => { toggleTheme(); render(); }, '[data-theme-toggle]'); return; }
  const ls = t.closest('[data-setlang]') as HTMLElement | null;
  if (ls) { hideToast(); withTransition(() => { setLang(ls.dataset.setlang as 'nl' | 'en'); render(); }, '[data-setlang]'); return; }
  const tab = t.closest('[data-tab]') as HTMLElement | null;
  if (tab) { selectTab(tab.dataset.tab as Tab); return; }
  const el = t.closest('[data-act],[data-period],[data-cat],[data-toggle],[data-row]') as HTMLElement | null;
  if (!el) return;
  if (el.dataset.period) { const p = el.dataset.period as Period; if (p !== state.period) withTransition(() => { state.period = p; render(); }, `[data-period="${p}"]`); return; }
  if (el.dataset.cat) { const c = el.dataset.cat as typeof state.cat; if (c !== state.cat) withTransition(() => { state.cat = c; render(); }, `[data-cat="${c}"]`); return; }
  if (el.dataset.toggle || (el.dataset.row && !el.dataset.act)) {
    const tr = el.closest('tr[data-row]') as HTMLElement | null;
    if (tr) toggleRow(tr, Number(tr.dataset.row));
    return;
  }
  // Async actions: the button shows it is working and ignores further clicks until done.
  const btn = el instanceof HTMLButtonElement ? el : null;
  const order = rowIds();
  if (btn) {
    if (btn.getAttribute('aria-busy') === 'true') return;
    btn.setAttribute('aria-busy', 'true'); btn.disabled = true;
    holdFocus(focusKey(btn), btn); pendingRow = rowOf(btn);
  }
  try { await runAction(el); } catch { /* call() already showed the error */ } finally {
    if (btn) {
      btn.removeAttribute('aria-busy'); btn.disabled = false;
      const lost = !document.activeElement || document.activeElement === document.body;
      const modal = !!document.querySelector('dialog[open]:not(.settings)') || (!!setDlg?.open && !setDlg.contains(btn));
      if (btn.isConnected && lost && !modal) btn.focus({ preventScroll: true });
      else if (!btn.isConnected && lost && !modal) focusBack(pendingFocus, pendingRow, order);
    }
    releaseFocus(); pendingRow = 0;
  }
}
function selectTab(k: Tab) {
  if (settingsSaving || state.tab === k) return;
  if (state.edit === 'nslogin' || state.edit === 'import') state.edit = '';
  state.tab = k;
  render();
  setDlg?.querySelector<HTMLElement>(`[data-tab="${k}"]`)?.focus({ preventScroll: true });
  if (k === 'data') loadExternals();
}

/** Jump to the first trip of a banner (all periods, so the counts match) and put focus on its action. */
function goFirst(kind: Filter, status?: string, carrier?: string) {
  const pick = state.rows.find((r) => (status ? r.claim_status === status : inFilter(r, kind)) && (!carrier || (carrier === 'arriva') === isArrivaTrip(r)));
  if (!pick) return;
  closeSettings(); // also from Settings → Automatisch (a paused automatic submission)
  state.period = 'all'; state.cat = status === 'SUBMITTING' ? 'claimable' : status ? 'claimed' : kind; state.open.add(pick.id);
  render();
  const tr = app.querySelector<HTMLElement>(`tr[data-row="${pick.id}"]`);
  const target = tr?.querySelector<HTMLElement>('[data-act=review], [data-act=arriva-claim], [data-act=arriva-real-claim]') ?? app.querySelector<HTMLElement>(`tr[data-detail="${pick.id}"] [data-act=review], tr[data-detail="${pick.id}"] select`) ?? tr?.querySelector<HTMLElement>('.chev-btn');
  tr?.scrollIntoView({ block: 'center', behavior: reducedMotion() ? 'auto' : 'smooth' });
  target?.focus({ preventScroll: true });
}
/** "Add trip" form: the error sits under its own field (and is read out), not only in a toast. */
function clearExtErrors(id?: string) {
  for (const k of id ? [id] : ['exDate', 'exFrom', 'exTo']) {
    document.getElementById(`${k}Err`)?.remove();
    const inp = document.getElementById(k); inp?.removeAttribute('aria-invalid'); inp?.removeAttribute('aria-describedby');
  }
}
function extError(id: string, text: string) {
  const inp = document.getElementById(id);
  if (!inp) { toast(text); return; }
  const m = document.createElement('span');
  m.className = 'msg bad'; m.id = `${id}Err`; m.setAttribute('role', 'alert'); m.textContent = text;
  inp.insertAdjacentElement('afterend', m);
  inp.setAttribute('aria-invalid', 'true'); inp.setAttribute('aria-describedby', m.id);
  inp.focus({ preventScroll: true });
}
/** After adding a trip claimed elsewhere: did it match one of your trips (now, or by the check that is running)? */
function externalAddedText(matched: number, pending: boolean, marked: { date: string; from: string; to: string }[] = []): string {
  const trips = (n: number, nl: boolean) => (nl ? (n === 1 ? '1 reis' : `${n} reizen`) : n === 1 ? '1 trip' : `${n} trips`);
  const routes = marked.slice(0, 2).map((t) => `${localDate(t.date + 'T12:00:00Z', getLang())}, ${t.from} → ${t.to}`).join('; ');
  const detail = routes ? ` ${routes}${marked.length > 2 ? ` +${marked.length - 2}` : ''}` : '';
  // Busy (a check or another change): marked at the next check; it is never submitted meanwhile.
  if (pending && matched > 0) return L(`Toegevoegd. ${trips(matched, true)} ${matched === 1 ? 'wordt' : 'worden'} bij de volgende controle gemarkeerd als elders geclaimd.`, `Added. ${trips(matched, false)} ${matched === 1 ? 'is' : 'are'} marked as claimed elsewhere at the next check.`) + detail;
  if (matched > 0) return L(`Toegevoegd. ${trips(matched, true)} gemarkeerd als elders geclaimd.`, `Added. ${trips(matched, false)} marked as claimed elsewhere.`) + detail;
  return L('Toegevoegd. Nog geen reis gevonden op die dag. Hij wordt gemarkeerd zodra hij verschijnt.', 'Added. No trip found on that day yet. It will be marked once it appears.');
}
/** Show one trip in a list (all periods), opened, with focus on its action. */
function showTrip(id: number, cat: Filter) {
  closeSettings();
  state.period = 'all'; state.cat = cat; state.open.add(id);
  render();
  const tr = app.querySelector<HTMLElement>(`tr[data-row="${id}"]`);
  tr?.scrollIntoView({ block: 'center', behavior: reducedMotion() ? 'auto' : 'smooth' });
  (tr?.querySelector<HTMLElement>('[data-act=review]') ?? tr?.querySelector<HTMLElement>('.chev-btn'))?.focus({ preventScroll: true });
}
/** Check now with automatic submission on: claims NS may already have (near) come first, then claims a later scheduled check submits (later). */
function checkNowHint(near: number, later: number): string {
  if (near > 0) return L(`${near === 1 ? '1 claim is' : `${near} claims zijn`} niet automatisch ingediend: NS toont een claim rond die datum. Kijk eerst in Mijn NS.`, `${near === 1 ? '1 claim was' : `${near} claims were`} not submitted automatically: NS shows a claim around that date. Check Mijn NS first.`);
  if (later > 0) return L('Controleer nu dient alleen claims in die binnen 14 dagen verlopen. De rest volgt bij de wekelijkse controle.', 'Check now only submits claims that expire within 14 days. The rest follows at the weekly check.');
  return '';
}
/** Why an automatic check submitted nothing (more): the run's own reason (also why it stopped), else what the UI can see. */
function runPause(r: any): string {
  if (r?.auto_off) return L('Automatisch indienen staat uit, dus er is niets ingediend.', 'Automatic submission is off, so nothing was submitted.');
  const own = r?.auto_paused_reason;
  if (typeof own === 'string' && own.trim()) return pickLang(own);
  if (r?.auto_paused_code === 'stopped') return L('Gestopt op jouw verzoek.', 'Stopped at your request.');
  return autoPause(state.o);
}
/** The claim a paused run waits for; none when it stopped for another reason (Stop pressed, time budget, switched off). */
const runPauseTarget = (r: any) => ['stopped', 'budget', 'off'].includes(r?.auto_paused_code) ? undefined : pauseTarget();
/** The result of an automatic check as one toast: what was submitted, what still waits, and why it paused
 *  (with "Bekijken" to the claim it waits for). */
function runToast(r: any, demo: boolean) {
  const paused = runPause(r);
  const target = paused ? runPauseTarget(r) : undefined;
  const text = autoRunText(Number(r?.submitted ?? 0), state.rows.filter((x) => inFilter(x, 'claimable')).length, state.rows.filter((x) => inFilter(x, 'review')).length, getLang(), demo, paused);
  toast(text, '', target ? { label: L('Bekijken', 'Review'), run: () => goFirst('all', target.claim_status ?? undefined) } : undefined);
}
/** Ask for the confirmation word before erasing (WISSEN / ERASE). Returns the word for the backend, or null. */
const confirmWord = (title: string, body: string, ok: string, all = false) => askWord({ title, body, ok, danger: true, all });
async function runAction(el: HTMLElement) {
  const claimId = Number(el.dataset.claim);
  switch (el.dataset.act) {
    case 'arriva-demo': return openArrivaDemo(call, async () => { await scan(); }, el);
    case 'arriva-claim': {
      state.arrivaClaiming = true; render();
      try {
        // The demo form is prepared only now, on the user's Claim (never during a check).
        if (!state.arriva?.window_open) state.arriva = await call<ArrivaDemoState>('arriva_demo_prepare');
        const d = await call<ArrivaDemoState>('arriva_demo_open', { view: 'captcha' });
        // Only the CAPTCHA is shown at Arriva; everything else is reported here.
        const text = arrivaOutcomeText(d) || (d.last_error ? pickLang(d.last_error) : '');
        if (text) toast(text, d.unverified?.length || d.phase === 'error' ? 'err' : '');
      }
      finally { state.arrivaClaiming = false; await refresh(); }
      return;
    }
    case 'arriva-attachments': return openArrivaAttachments(call, state.arriva, el);
    case 'arriva-real-attachments': {
      const attachments = await call<any[]>('arriva_claim_attachments', { claimId });
      return openArrivaAttachments(call, { attachments }, el, { data: 'arriva_claim_attachment_data', open: 'arriva_claim_attachment', args: { claimId }, demo: false });
    }
    case 'arriva-real-claim': {
      const r = state.rows.find((x) => x.claim_id === claimId);
      if (!r || !arrivaClaimable(r) || arrivaBusy(r)) return;
      const missing = state.arrivaReal?.missing ?? [];
      if (missing.includes('contact')) { toast(L('Vul eerst je naam en e-mailadres voor Arriva in.', 'First enter your name and e-mail address for Arriva.')); state.edit = 'arriva'; render(); return; }
      if (missing.includes('bank')) { toast(L('Stel eerst je rekening in.', 'Set up your bank account first.')); state.edit = 'bank'; render(); return; }
      if (!state.arrivaReal) {
        toast(L('Arriva-status nog niet geladen. Probeer het zo opnieuw.', 'Arriva status has not loaded. Please try again shortly.'), 'err');
        await refresh();
        return;
      }
      const send = state.arrivaReal.submit_enabled || isDemo();
      // An earlier attempt may have reached Arriva: claiming again needs the user's own check.
      const resubmit = wasUncertain(r);
      const j = r.journey, acc = state.o?.account;
      const d = displayDelay(r), pct = r.arriva?.refund_percent;
      const vrijFare = arrivaVrijFareText(r.compensation_basis, getLang());
      const cell = (k: string, v: string) => `<tr><th scope="row">${h(k)}</th><td>${v}</td></tr>`;
      const summary = `<table class="sum-table">${[
        cell(L('Reis', 'Trip'), h(routeOf(j))),
        cell(L('Datum', 'Date'), h(localDate(j.check_in, getLang()))),
        cell(L('Vertraging', 'Delay'), d != null ? h(`+${d} min${pct ? L(` (${pct}% van de ritprijs)`, ` (${pct}% of the fare)`) : ''}`) : '–'),
        ...(vrijFare ? [cell(L('Ritprijs op het formulier', 'Fare on the form'), h(vrijFare))] : []),
        cell(L('Bedrag', 'Amount'), r.compensation_cents != null ? `${r.compensation_exact === false ? '≈ ' : ''}${euro(r.compensation_cents, getLang())}` : L('onbekend', 'unknown')),
        cell(L('Rekening', 'Bank account'), acc?.iban_last4 ? `<span class="mono">•••• ${h(acc.iban_last4)}</span>` : '–'),
        cell('OV-chipkaart', j.card_last4 ? `<span class="mono">•••• ${h(j.card_last4)}</span>` : '–'),
        cell(L('Toestemming', 'Consent'), arrivaConsentText()),
      ].join('')}</table>`;
      const ask = {
        title: L('Claimen bij Arriva?', 'Claim at Arriva?'),
        body: `${L('OV Refund vult het formulier van Arriva in met je naam, e-mailadres, IBAN en volledige OV-chipkaartnummer. Als Arriva “Ik ga akkoord” verplicht stelt, vinkt OV Refund dit namens jou aan. Vraagt Google om een puzzel, dan verschijnt die tijdens het indienen.', 'OV Refund enters your name, e-mail address, IBAN and full OV-chipkaart number on Arriva’s form. If Arriva requires “Ik ga akkoord” (I agree), OV Refund ticks it for you. If Google asks for a puzzle, it will appear during submission.')}${send ? '' : ` ${L('Proefversie: OV Refund controleert alles en maakt de bijlagen, maar opent het formulier van Arriva niet en verstuurt niets.', 'Test build: OV Refund checks everything and makes the attachments, but does not open Arriva’s form and sends nothing.')}`}${resubmit ? ` ${L('Een eerdere poging is misschien al bij Arriva aangekomen.', 'An earlier attempt may already have reached Arriva.')}` : ''}${isDemo() ? ` ${L('Demo: het formulier wordt ingevuld, maar niets wordt verzonden.', 'Demo: the form is filled in, but nothing is sent.')}` : ''}`,
        html: summary, ok: L('Indienen bij Arriva', 'Submit to Arriva'), focusCancel: true,
      };
      const checks = [L('Ik ga akkoord dat OV Refund namens mij “Ik ga akkoord” aanvinkt op het formulier van Arriva', 'I agree that OV Refund ticks “Ik ga akkoord” (I agree) for me on Arriva’s form')];
      if (resubmit) checks.push(L('Ik heb mijn e-mail van Arriva gecontroleerd en er is geen bevestiging', 'I checked my e-mail from Arriva and there is no confirmation'));
      state.arrivaStarting.add(claimId);
      let previewed = false;
      try {
        render();
        const files = await call<any[]>('arriva_claim_preview', {
          claimId,
          reviewed: {
            check_in: j.check_in, origin: j.origin.name, destination: j.destination?.name ?? '',
            delay: d, refund: r.compensation_cents, vrij_basis: vrijFare ? r.compensation_basis : null,
            card_last4: j.card_last4, iban_last4: acc?.iban_last4 ?? null,
          },
        });
        previewed = true;
        const go = await openArrivaAttachments(call, { attachments: files }, el, {
          data: 'arriva_claim_attachment_data', open: 'arriva_claim_attachment', args: { claimId }, demo: false,
          fareNote: vrijFare ? L('Ritprijs op het formulier', 'Fare on the form') + ' · ' + vrijFare : undefined,
        }, true);
        if (!go || !(await askChecked(ask, checks))) {
          await call('arriva_claim_cancel', { claimId });
          previewed = false;
          toast(L('Geannuleerd. Er is niets naar Arriva gestuurd.', 'Cancelled. Nothing was sent to Arriva.'));
          return;
        }
        state.arrivaReal = await call<ArrivaRealState>('arriva_claim', { claimId, confirmResubmit: resubmit, attachmentsConfirmed: true, consentAccepted: true });
        previewed = false;
        const phase = arrivaPhase(state.arrivaReal, claimId);
        const msg = pickLang(arrivaMessage(state.arrivaReal, claimId));
        if (phase === 'verifying') toast(L('Verificatie bij Arriva… De uitkomst volgt hier.', 'Verifying at Arriva… The outcome follows here.'));
        else if (phase === 'failed' || phase === 'uncertain') toast(msg || L('Niet gelukt. Bekijk de reis voor de details.', 'Did not work. See the trip for details.'), 'err');
        else if (msg) toast(msg);
      } finally {
        if (previewed) await invoke('arriva_claim_cancel', { claimId }).catch(() => {});
        state.arrivaStarting.delete(claimId);
        await refresh();
      }
      return;
    }
    case 'arriva-show-puzzle': {
      const shown = await call<boolean>('arriva_claim_show');
      if (!shown) toast(L('Er staat nu geen puzzel klaar. De uitkomst volgt hier.', 'There is no puzzle right now. The outcome follows here.'));
      return;
    }
    case 'goarriva': closeSettings(); state.edit = 'arriva'; render(); return;
    case 'savearriva': {
      const first = val('arrFirst').trim(), last = val('arrLast').trim(), email = val('arrEmail').trim(), valid = val('arrValid').trim();
      const exp = expiryProblem(valid);
      const okAll = [
        arrivaFieldError('arrFirst', first ? '' : L('Vul je voornaam in.', 'Enter your first name.')),
        arrivaFieldError('arrLast', last ? '' : L('Vul je achternaam in.', 'Enter your last name.')),
        arrivaFieldError('arrEmail', isEmail(email) ? '' : L('Dit e-mailadres klopt niet. Controleer het adres.', 'This e-mail address is not valid. Check the address.')),
        arrivaFieldError('arrValid', exp === 'format' ? L('Vul de datum in als DD-MM-JJJJ, of laat hem leeg.', 'Enter the date as DD-MM-YYYY, or leave it empty.') : ''),
      ];
      if (okAll.includes(false)) { app.querySelector<HTMLElement>('.inline-form [aria-invalid="true"]')?.focus(); return; }
      // A date in the past is saved (a trip on a card that has expired since); a changed one gets a note.
      const past = expiryInPastNote(valid, state.arrivaContact?.card_valid_until);
      state.arrivaContact = await call<ArrivaContact>('set_arriva_contact', {
        firstName: first, lastName: last, email, cardValidUntil: valid || null,
        expiryCardKey: app.querySelector<HTMLInputElement>('#arrValid')?.dataset.cardKey || null,
      });
      state.edit = ''; toast(past ? `${L('Gegevens voor Arriva opgeslagen.', 'Details for Arriva saved.')} ${pastExpiryNote()}` : L('Gegevens voor Arriva opgeslagen', 'Details for Arriva saved')); return refresh();
    }
    case 'cleararriva':
      if (!(await askConfirm({ title: L('Gegevens voor Arriva verwijderen?', 'Remove details for Arriva?'), body: L('Zonder deze gegevens kun je geen Arriva-claim indienen.', 'Without these details you can’t submit an Arriva claim.'), ok: L('Verwijderen', 'Remove'), danger: true }))) return;
      await call('clear_arriva_contact'); state.edit = ''; return refresh();
    case 'scan': return scan();
    case 'stopauto': {
      // The box shows "Stoppen…" and the Stop button goes away: focus moves to the trips.
      const text = stoppingText();
      state.stopping = true; updateRing();
      document.getElementById('trips')?.focus({ preventScroll: true });
      await call('stop_auto_run');
      toast(text);
      return;
    }
    case 'fullscan': closeSettings(); return scan(true);
    case 'dismissrestore': state.restored = null; render(); document.getElementById('trips')?.focus({ preventScroll: true }); return;
    case 'gobank': closeSettings(); state.edit = 'bank'; render(); return;
    case 'gotab': selectTab(el.dataset.what as Tab); return;
    case 'login': closeSettings(); toast(L('Log in via het NS-venster. Het sluit vanzelf zodra je bent ingelogd.', 'Log in using the NS window. It closes by itself once you are logged in.')); await call('open_login'); return;
    case 'showall': return withTransition(() => { state.period = 'all'; state.cat = 'all'; render(); });
    case 'settings': return openSettings(undefined, el);
    case 'gofirst': return goFirst((el.dataset.kind || 'claimable') as Filter, el.dataset.status, el.dataset.carrier);
    case 'edit':
      if (!el.dataset.what && state.edit) return closeInlineForm();
      state.edit = (el.dataset.what as typeof state.edit) || ''; return render();
    case 'reveal': {
      const k = el.dataset.kind!;
      clearTimeout(revealTimers[k]);
      if (revealed[k]) { delete revealed[k]; return render(); }
      // The card number is already in the overview (shown masked by default); the IBAN comes from Windows' vault.
      revealed[k] = k === 'card' ? (state.o?.account.card_number ?? '') : await call<string>('reveal', { kind: k }); render();
      revealTimers[k] = window.setTimeout(() => { delete revealed[k]; render(); }, 10_000);
      return;
    }
    case 'review': {
      const r = state.rows.find(r => r.claim_id === claimId);
      if (r && !canReviewSubmit(r)) {
        // Can't be submitted here: open its details (also when already open) and go to what can be done.
        state.open.add(r.id); render();
        const det = app.querySelector<HTMLElement>(`tr[data-detail="${r.id}"]`);
        det?.scrollIntoView({ block: 'nearest', behavior: reducedMotion() ? 'auto' : 'smooth' });
        (det?.querySelector<HTMLElement>('.acts a, .acts button, .acts select') ?? app.querySelector<HTMLElement>(`tr[data-row="${r.id}"] .chev-btn`))?.focus({ preventScroll: true });
        return;
      }
      return openReview(claimId, el);
    }
    case 'dismiss':
      if (!(await askConfirm({ title: L('Niet claimen?', 'Don’t claim this trip?'), body: L('Je kunt dit later terugdraaien bij deze reis met Toch claimen.', 'You can undo this later on this trip with Claim after all.'), ok: L('Niet claimen', 'Don’t claim') }))) return;
      await call('set_claim_status', { claimId, status: 'DISMISSED' });
      return statusChanged(claimId, 'DISMISSED', L('Deze reis wordt niet geclaimd', 'This trip will not be claimed'));
    case 'restore':
      // The backend puts the claim back in its previous open state (Te claimen or Beoordelen).
      await call('set_claim_status', { claimId, status: 'NEEDS_REVIEW' });
      return statusChanged(claimId, 'NEEDS_REVIEW', L('Teruggezet: deze reis kun je weer claimen', 'Restored: you can claim this trip again'));
    case 'external':
      if (!(await askConfirm({ title: L('Al elders geclaimd?', 'Already claimed elsewhere?'), body: L('OV Refund dient deze reis dan nooit in. Terugdraaien kan zolang er geen bedrag of NS-referentie is.', 'OV Refund will then never submit this trip. You can undo this as long as there is no amount or NS reference.'), ok: L('Markeren', 'Mark it') }))) return;
      await call('mark_external', { claimId });
      return statusChanged(claimId, 'EXTERNALLY_CLAIMED', L('Gemarkeerd als elders geclaimd', 'Marked as claimed elsewhere'));
    case 'undoexternal':
      if (!(await askConfirm({ title: L('Markering terugdraaien?', 'Undo marking?'), body: L('Controleer eerst Mijn teruggaven. Een mail die alleen op datum past, bewijst niet welke reis is geclaimd.', 'Check Mijn teruggaven first. An e-mail matched only by date does not prove which trip was claimed.'), ok: L('Terugdraaien', 'Undo') }))) return;
      await call('undo_external', { claimId });
      return statusChanged(claimId, 'NEEDS_REVIEW', L('Teruggezet: deze reis kun je weer claimen', 'Restored: you can claim this trip again'));
    case 'savebank':
      if (!validHolderName(val('bankName'))) { toast(L('Vul de naam van de rekeninghouder in.', 'Enter the account holder’s name.')); return; }
      if (checkIban(val('bankIban')).state !== 'valid') { toast(L('Vul een geldig IBAN in.', 'Enter a valid IBAN.')); return; }
    {
      const had = !!state.o?.account.iban_last4;
      await call('set_bank_details', { name: val('bankName').trim(), iban: val('bankIban') }); state.edit = '';
      toast(had ? `${L('Rekening opgeslagen.', 'Bank account saved.')} ${paidToOldAccount()}` : L('Rekening opgeslagen', 'Bank account saved')); return refresh();
    }
    case 'clearbank':
      if (!(await askConfirm({ title: L('Rekening verwijderen?', 'Remove bank account?'), body: L('Zonder rekening kun je niet indienen.', 'Without a bank account you can’t submit.'), ok: L('Verwijderen', 'Remove'), danger: true }))) return;
      await call('clear_bank_details'); state.edit = ''; return refresh();
    case 'saveemail': if (!settingsSaving) await saveEmail(false); return;
    case 'syncmail': { toast(L('NS-mails lezen…', 'Reading NS e-mails…')); const r = await call<string[]>('sync_ns_mail'); toast(r.map(pickLang).join(' · ')); return refresh(); }
    case 'savenslogin': {
      const email = val('nsEmail').trim(), pw = val('nsPw');
      if (!email) { toast(L('Vul je e-mailadres voor Mijn NS in.', 'Enter your Mijn NS e-mail address.')); return; }
      if (!isEmail(email)) { toast(L('Dit e-mailadres klopt niet. Controleer het adres.', 'This e-mail address is not valid. Check the address.')); return; }
      if (!pw) { toast(L('Vul je NS-wachtwoord in.', 'Enter your NS password.')); return; }
      await call('set_ns_login', { email, password: pw }); state.edit = ''; toast(L('NS-inloggegevens opgeslagen', 'NS login details saved')); return refresh();
    }
    case 'bugreport': return openBugReport(el);
    case 'website': await call('open_external', { url: WEBSITE_URL }); return;
    case 'openlink': if (el.dataset.url) await call('open_external', { url: el.dataset.url }); return;
    case 'checkupdate': return checkUpdates(true);
    case 'update': return showUpdateDialog();
    case 'demoauto': {
      if (state.busy) { toast(L('Er loopt al een controle', 'A check is already running')); return; }
      // Same progress, Stop and disabled buttons as a real check.
      startBusy(L('Automatische controle…', 'Automatic check…'), 'scan');
      const autoOff = state.o?.mode !== 'automatic';
      let r: any = null;
      try { r = await call<any>('demo_auto_run'); state.demoCheckedAt = new Date().toISOString(); } finally { endBusy(); await refresh(); }
      // Also say what still waits, and why nothing was submitted, so "0 submitted" is never the whole story.
      // With automatic submission off, that is the reason.
      runToast(autoOff && !Number(r?.submitted ?? 0) ? { ...r, auto_paused_reason: null, auto_paused_code: 'off', auto_off: true } : r, true);
      return;
    }
    case 'howto': {
      const fromSettings = !!setDlg?.open, tab = state.tab;
      closeSettings();
      openHowto({ call, toast, done: async () => {}, onClose: () => { if (fromSettings) { openSettings(tab); setTimeout(() => setDlg?.querySelector<HTMLElement>('[data-act=howto]')?.focus(), 0); } } });
      return;
    }
    case 'mailmanual': state.mailManual = true; return render();
    case 'testmail':
      if (settingsSaving) return;
      // Test what is typed in the form, not what was saved before.
      if (!(await saveEmail(true))) return;
      toast(L('Testmail versturen…', 'Sending test e-mail…'));
      toast(friendlyError(await call<string>('test_email'))); return;
    case 'addext': {
      const date = val('exDate'), from = val('exFrom').trim().slice(0, 60), to = val('exTo').trim().slice(0, 60);
      clearExtErrors();
      if (!date) return extError('exDate', L('Vul de reisdatum in.', 'Fill in the travel date.'));
      if (!validExternalTrip(date, 'A', 'B')) return extError('exDate', L('Kies een reisdatum in de afgelopen 18 maanden.', 'Choose a travel date in the past 18 months.'));
      if (!from) return extError('exFrom', L('Vul het station bij Van in.', 'Fill in the station under From.'));
      if (!to) return extError('exTo', L('Vul het station bij Naar in.', 'Fill in the station under To.'));
      const chars = L('Gebruik alleen letters, spaties en . \' - ( ).', 'Use only letters, spaces and . \' - ( ).');
      if (!validExternalTrip(date, from, 'B')) return extError('exFrom', chars);
      if (!validExternalTrip(date, from, to)) return extError('exTo', chars);
      if (from.toLowerCase() === to.toLowerCase()) return extError('exTo', L('Vertrek- en aankomststation zijn hetzelfde.', 'The departure and arrival station are the same.'));
      let r: { matched?: number; pending?: boolean; trips?: { date: string; from: string; to: string }[] } | null;
      try { r = await invoke<any>('add_external_claim', { date, origin: from, destination: to, source: 'elders' }); }
      catch (e) {
        // A station error goes under its own field; anything else as a message.
        const raw = String(e);
        if (/Departure station/i.test(raw)) return extError('exFrom', friendlyError(e));
        if (/Arrival station|are the same/i.test(raw)) return extError('exTo', friendlyError(e));
        toast(friendlyError(e), 'err'); return;
      }
      state.edit = '';
      toast(externalAddedText(Number(r?.matched ?? 0), !!r?.pending, r?.trips));
      await refresh(); await loadExternals();
      setDlg?.querySelector<HTMLElement>('[data-act="edit"][data-what="import"]')?.focus({ preventScroll: true });
      return;
    }
    case 'delext': {
      const id = Number(el.dataset.id);
      if (!(await askConfirm({ title: L('Reis verwijderen?', 'Remove trip?'), body: L('OV Refund kan deze reis daarna weer voorstellen om te claimen.', 'OV Refund may then suggest this trip for claiming again.'), ok: L('Verwijderen', 'Remove'), danger: true }))) return;
      const index = [...(setDlg?.querySelectorAll('[data-act=delext]') ?? [])].indexOf(el);
      await call('delete_external_claim', { id }); toast(L('Verwijderd', 'Removed'));
      await refresh(); await loadExternals();
      // Focus on the next trip's Remove button (or the previous one), else on Add a trip.
      const rest = [...(setDlg?.querySelectorAll<HTMLElement>('[data-act=delext]') ?? [])];
      (rest[Math.min(Math.max(index, 0), rest.length - 1)] ?? setDlg?.querySelector<HTMLElement>('[data-act="edit"][data-what="import"]'))?.focus({ preventScroll: true });
      return;
    }
    case 'disconnect':
      if (!(await askConfirm({ title: L('Uitloggen bij NS?', 'Log out of NS?'), body: L('Je opgeslagen NS-wachtwoord wordt gewist. Automatische controles stoppen tot je opnieuw inlogt.', 'Your saved NS password is deleted. Automatic checks stop until you log in again.'), ok: L('Uitloggen', 'Log out'), danger: true }))) return;
      // A running check: the password and automatic login are gone at once, the rest follows when it is done ('erase-finished').
      try {
        const done = await call<boolean>('disconnect');
        toast(done ? L('Uitgelogd bij NS', 'Logged out of NS') : L('Je NS-wachtwoord is gewist en automatisch inloggen staat uit. Uitloggen wordt afgerond zodra de controle klaar is.', 'Your NS password is erased and automatic login is off. Logging out finishes once the check is done.'));
      } finally { await refresh(); }
      return;
    case 'diag': {
      const path = await call<string>('export_diagnostics');
      toast(L(`Diagnosegegevens opgeslagen: ${path}. Controleer het bestand voordat je het deelt.`, `Diagnostics saved: ${path}. Check the file before sharing it.`));
      invoke('open_diagnostics_folder', { path }).catch(() => { /* the toast already says where it is */ });
      return;
    }
    case 'datafolder': await call('open_data_folder'); return;
    case 'backup': {
      const r = await call<{ path: string }>('export_backup');
      toast(r?.path ? L(`Back-up opgeslagen: ${r.path}`, `Backup saved: ${r.path}`) : L('Back-up opgeslagen.', 'Backup saved.'));
      return;
    }
    case 'csv': {
      const r = await call<{ path: string }>('export_claims_csv');
      toast(r?.path ? L(`Claims opgeslagen: ${r.path}`, `Claims saved: ${r.path}`) : L('Claims geëxporteerd.', 'Claims exported.'));
      return;
    }
    case 'restorebackup': (setDlg?.querySelector('#restoreFile') as HTMLInputElement | null)?.click(); return;
    case 'importdone':
      if (!(await askConfirm({ title: L('Import klaar?', 'Import done?'), body: L('Heb je de claimdienst opgezegd en alle reizen die hij al claimde toegevoegd? Daarna kan OV Refund weer automatisch indienen.', 'Have you cancelled the claim service and added every trip it already claimed? After this, OV Refund can submit automatically again.'), ok: L('Ja, klaar', 'Yes, done') }))) return;
      await call('set_flag', { key: 'other_service', value: 'import_done' }); toast(L('Opgeslagen', 'Saved')); await refresh();
      // The Import done button is gone: focus on Add a trip.
      setDlg?.querySelector<HTMLElement>('[data-act="edit"][data-what="import"]')?.focus({ preventScroll: true });
      return;
    case 'testnotify':
      if (!state.settings?.notify_windows) { toast(L('Windows-meldingen staan uit. Zet ze eerst aan.', 'Windows notifications are off. Turn them on first.')); return; }
      await call('test_notification'); toast(L('Testmelding verstuurd.', 'Test notification sent.')); return;
    case 'gomail': {
      // From Settings → Notifications to the e-mail form under Your details (password field when the address is set).
      const pwOnly = !!state.o?.account.notify_email;
      closeSettings(); state.edit = 'email'; render();
      setTimeout(() => app.querySelector<HTMLElement>(pwOnly ? '#smtpPw' : '#smtpTo')?.focus(), 50);
      return;
    }
    case 'fixautologin': state.edit = 'nslogin'; openSettings('account', el); return;
    case 'forget': {
      const dir = state.o?.export_dir || L('Documenten\\OV Refund', 'Documents\\OV Refund');
      const removes = !!state.o?.forget_removes_exports;
      const exportsNote = removes
        ? `${L(`Back-ups en CSV-bestanden in ${dir} worden ook gewist.`, `Backups and CSV files in ${dir} are deleted too.`)} ${L('Bewaar eerst een back-up op een andere plek (bijv. een USB-stick) als je die later wilt terugzetten.', 'First copy a backup elsewhere (e.g. a USB stick) if you want to restore it later.')}`
        : L(`Back-ups en CSV-bestanden in ${dir} blijven staan.`, `Backups and CSV files in ${dir} are kept.`);
      const folderBtn = removes ? ` <button type="button" class="btn small" data-x="openexports">${L('Exportmap openen', 'Open export folder')}</button>` : '';
      // The dialog lives outside #app: its folder button gets its own listener while it is open.
      const openExports = (e: Event) => { if ((e.target as Element | null)?.closest?.('[data-x=openexports]')) { e.preventDefault(); void call('open_data_folder').catch(() => { /* call() shows the error */ }); } };
      document.addEventListener('click', openExports);
      let c: string | null;
      try {
        c = await confirmWord(L('Alles vergeten?', 'Forget everything?'), `${L('OV Refund wist je reizen, claims, instellingen, NS-wachtwoord, rekening, e-mailwachtwoord, je gegevens voor Arriva en eventuele Arriva-bijlagen van deze computer.', 'OV Refund erases your trips, claims, settings, NS password, bank account, e-mail password, your details for Arriva and any Arriva attachments from this computer.')} ${L('Daarna weet OV Refund niet meer welke claims al zijn ingediend. Het kan dan niet voorkomen dat je een reis opnieuw indient.', 'OV Refund then no longer knows which claims were already submitted, so it can’t stop you from submitting a trip again.')} ${h(exportsNote)}${folderBtn}`, L('Alles wissen', 'Erase all'), true);
      } finally { document.removeEventListener('click', openExports); }
      if (!c) return;
      // A running check: the protective part is done at once, the rest is erased when it is done ('erase-finished').
      let done: boolean;
      try { done = await call<boolean>('forget_everything', { confirm: c }); } catch { await refresh(); return; }
      if (!done) {
        toast(L('Automatisch indienen en automatisch inloggen staan uit. Je wachtwoorden en rekening zijn gewist. De rest wordt gewist zodra de controle klaar is.', 'Automatic submission and automatic login are off. Your passwords and bank account are erased. The rest is erased once the check is done.'));
        await refresh(); return;
      }
      erasedReload();
      return;
    }
    case 'wipe': {
      const c = await confirmWord(L('Lokale geschiedenis wissen?', 'Erase local history?'), L('OV Refund wist je reizen en niet-ingediende claims op deze computer. Ingediende claims blijven bewaard, zodat niets dubbel wordt ingediend. Ook de gegevens uit e-mails van NS (referentie, bedrag en datum) blijven bewaard als bescherming tegen dubbel indienen. Alles vergeten wist die ook.', 'OV Refund erases your trips and unsubmitted claims on this computer. Submitted claims are kept, so nothing is submitted twice. The details from NS e-mails (reference, amount and date) are kept too, as protection against submitting twice. Forget everything erases them as well.'), L('Wissen', 'Erase'));
      if (c) { await call('wipe_history', { confirm: c }); toast(L('Lokale geschiedenis gewist', 'Local history erased')); await refresh(); }
      return;
    }
  }
}

function onKeydown(ev: KeyboardEvent) {
  const t = ev.target as HTMLElement;
  // Status list: arrows only move the choice; Enter confirms it (a closed list would otherwise ask at once).
  if (t.matches?.('select[data-act=setstatus]')) {
    const sel = t as HTMLSelectElement;
    if (!ev.altKey && /^(Arrow(Up|Down|Left|Right)|Home|End|Page(Up|Down))$/.test(ev.key)) { sel.dataset.kbd = '1'; selHint(sel, true); }
    else if (ev.key === 'Enter' && sel.value && sel.dataset.kbd === '1') { ev.preventDefault(); delete sel.dataset.kbd; void confirmStatus(sel); }
    return;
  }
  const menu = t.closest<HTMLDetailsElement>('.more-menu[open]');
  if (ev.key === 'Escape' && menu) { ev.preventDefault(); ev.stopPropagation(); menu.open = false; menu.querySelector<HTMLElement>('summary')?.focus(); return; }
  // Settings tabs: arrow keys move between tabs.
  if (t.matches('[role=tab]') && ['ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(ev.key)) {
    const i = TABS.findIndex(([k]) => k === state.tab);
    const n = ev.key === 'Home' ? 0 : ev.key === 'End' ? TABS.length - 1 : (i + (ev.key === 'ArrowDown' || ev.key === 'ArrowRight' ? 1 : -1) + TABS.length) % TABS.length;
    ev.preventDefault(); selectTab(TABS[n][0]); return;
  }
  // Inline forms: Enter saves (primary button), Esc cancels.
  const form = t.closest('.inline-form');
  if (!form || !state.edit) return;
  if (ev.key === 'Escape') { ev.preventDefault(); ev.stopPropagation(); closeInlineForm(); return; }
  if (ev.key === 'Enter' && t.matches('input.in')) {
    const primary = form.querySelector<HTMLButtonElement>('.btn.primary');
    if (primary && !primary.disabled) { ev.preventDefault(); primary.click(); }
  }
}
app.addEventListener('input', onInput);
app.addEventListener('change', onChange);
app.addEventListener('click', onClick);
app.addEventListener('keydown', onKeydown);
// A status chosen with the arrow keys but not confirmed with Enter is dropped when focus leaves the list.
app.addEventListener('focusout', (e) => {
  const sel = e.target as HTMLSelectElement;
  if (sel.matches?.('select[data-act=setstatus]') && sel.dataset.kbd === '1') { delete sel.dataset.kbd; sel.value = ''; selHint(sel, false); }
});
app.addEventListener('pointerdown', (e) => { const sel = (e.target as Element).closest?.('select[data-act=setstatus]') as HTMLSelectElement | null; if (sel) { delete sel.dataset.kbd; selHint(sel, false); } });
// The wizard restored a backup: show its trips now.
document.addEventListener('datachange', () => { if (state.o) refresh(); });

installHelpTips();
installDialogTrap();

// A foreground check refreshes by itself when it ends; only refresh here for other runs (scheduled task).
/** After "forget everything": restart the app with the toast shown again after the reload. */
function erasedReload() {
  try { sessionStorage.setItem('erased', '1'); } catch { /* storage unavailable */ }
  toast(L('Alles gewist', 'Everything erased')); location.reload();
}
// A logout or "forget everything" that waited for a check has finished.
listen<string>('erase-finished', async (ev) => {
  if (ev.payload === 'forget') { erasedReload(); return; }
  toast(L('Uitgelogd bij NS', 'Logged out of NS')); await refresh();
});
listen('run-finished', () => { if (state.busy || Date.now() - lastRunEnd < 3000) return; refresh(); });
listen('arriva-demo-updated', () => { if (!state.busy && (state.o as any)?.demo) void refresh(); });
listen('arriva-claim-updated', () => { if (!state.busy) void refresh(); });
// Closing the window while an Arriva claim runs: ask first, since the outcome would stay unknown.
listen('close-blocked', async () => {
  const ok = await askConfirm({
    title: L('Arriva-claim is nog bezig', 'Arriva claim still in progress'),
    body: L('Als het indienen al is begonnen, kan de claim op Onzeker komen te staan. Controleer je e-mail van Arriva voordat je opnieuw claimt.', 'If submission has already begun, the claim may show as Uncertain. Check your Arriva e-mail before claiming again.'),
    ok: L('Toch sluiten', 'Close anyway'), cancel: L('Wachten', 'Wait'), danger: true,
  });
  if (ok) await invoke('arriva_quit_anyway');
});
listen('login-complete', async () => { hideToast(); await refresh(); if (state.o?.auth_state !== 'ok' || document.querySelector('.wizard:not(.closing)')) return; toast(L('Verbonden met Mijn NS', 'Connected to Mijn NS')); if (!state.busy) scan(); });
listen('login-window-closed', async () => { try { await call('check_session'); } finally { refresh(); } });

document.addEventListener('langchange', () => { hideToast(); if (state.o) render(); });
document.addEventListener('themechange', () => { if (state.o) render(); });
syncLang(); // e-mails and notifications follow the app's language
// First paint: a skeleton in the shape of the main screen, so the layout does not jump when data arrives.
{
  const LOGO = '<div class="brand" aria-label="OV Refund"><img class="logo-img" src="/logo-mark.png" alt="OV Refund" width="36" height="36"></div>';
  const bar = (w: string) => `<span class="sk" style="width:${w}"></span>`;
  const cell = `<div class="tile">${bar('40%')}<span class="sk sk-n"></span>${bar('30%')}</div>`;
  const row = (i: number) => `<div class="sk-row">${bar('10%')}${bar(['34%', '28%', '40%', '31%', '37%', '25%'][i])}${bar('8%')}${bar('9%')}${bar('14%')}</div>`;
  app.setAttribute('aria-busy', 'true');
  app.innerHTML = `<div class="top">${LOGO}<span class="conn"><span class="spin"></span>${L('Laden…', 'Loading…')}</span></div>
  <div class="layout skeleton" aria-hidden="true"><main><div class="summary">${cell}${cell}</div><div class="toolbar">${bar('380px')}${bar('220px')}</div>
  <div class="ledger sk-ledger">${Array.from({ length: 6 }, (_, i) => row(i)).join('')}</div></main><aside><section class="card">${bar('45%')}${Array.from({ length: 4 }, () => `<div class="field">${bar('35%')}${bar('70%')}</div>`).join('')}</section></aside></div>`;
}
/** After the wizard: focus "Check now" (or the first trip), so keyboard users start in the right place. */
function focusStart() {
  const el = app.querySelector<HTMLElement>('[data-act="scan"]') ?? app.querySelector<HTMLElement>('tr[data-row] .chev-btn');
  el?.focus({ preventScroll: true });
}
async function initialize() {
  // After "Forget everything" the app reloads: confirm it here, where the toast stays visible.
  try { if (sessionStorage.getItem('erased')) { sessionStorage.removeItem('erased'); toast(L('Alles gewist', 'Everything erased')); } } catch { /* storage unavailable */ }
  // First start: a guided wizard, one question at a time.
  if (state.o && !(state.o as any).wizard_done) {
    await openWizard({ call, toast, hideToast, done: async () => { await refresh(); focusStart(); if (state.o?.auth_state === 'ok' || state.o?.account?.ns_email) scan(); } },
      { loggedIn: state.o.auth_state === 'ok', nsEmail: state.o.account?.ns_email ?? null, step: state.o.wizard_step ?? null, disclaimerAccepted: !!state.o.disclaimer_accepted, otherService: state.o.other_service ?? null, bankLast4: state.o.account?.iban_last4 ?? null, bankName: state.o.account?.bank_name ?? null });
    return;
  }
  if (state.o && !(state.o as any).disclaimer_accepted) await showDisclaimerOnce();
  checkUpdates(false);
  // Check at start-up when the last check is older than 6 hours (and a saved session exists).
  if (state.busy) return;
  // Usually this returns at once (recent check or no session): only show the busy UI if it takes a while.
  startBusy(L('Controleren…', 'Checking…'), 'scan', 400);
  let ran = false;
  try { ran = await call<boolean>('startup_check', { maxAgeHours: 6 }); if (ran) toast(L('Controle bij het opstarten uitgevoerd', 'Start-up check done')); } catch { /* shown via toast */ } finally {
    const shown = !state.busyQuiet; endBusy();
    // Always redraw: the button may have been drawn disabled while the quiet check ran.
    if (shown || ran) refresh(); else render();
  }
}
void refresh();
