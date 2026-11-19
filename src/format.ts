// Pure formatting helpers (unit-tested in format.test.ts).

export type Lang = 'nl' | 'en';

const TZ = 'Europe/Amsterdam';

/** Date-input value for the user's travel day, including just after midnight in Amsterdam. */
export function localDay(now: Date = new Date()): string {
  const parts = new Intl.DateTimeFormat('en-GB', { timeZone: TZ, year: 'numeric', month: '2-digit', day: '2-digit' }).formatToParts(now);
  const part = (kind: string) => parts.find((p) => p.type === kind)?.value ?? '';
  return `${part('year')}-${part('month')}-${part('day')}`;
}

export function localDateTime(iso: string | null | undefined, lang: Lang = 'nl'): string {
  if (!iso || !Number.isFinite(new Date(iso).getTime())) return '–';
  const d = new Date(iso);
  return d.toLocaleString(lang === 'nl' ? 'nl-NL' : 'en-GB', {
    timeZone: TZ, weekday: 'short', day: 'numeric', month: 'short', year: 'numeric', hour: '2-digit', minute: '2-digit',
  });
}

export function localTime(iso: string | null | undefined): string {
  if (!iso || !Number.isFinite(new Date(iso).getTime())) return '–';
  return new Date(iso).toLocaleTimeString('nl-NL', { timeZone: TZ, hour: '2-digit', minute: '2-digit' });
}

export function localDate(iso: string | null | undefined, lang: Lang = 'nl'): string {
  if (!iso || !Number.isFinite(new Date(iso).getTime())) return '–';
  return new Date(iso).toLocaleDateString(lang === 'nl' ? 'nl-NL' : 'en-GB', { timeZone: TZ, day: 'numeric', month: 'short', year: 'numeric' });
}

export function minutes(m: number | null | undefined): string {
  if (m === null || m === undefined) return '–';
  return `${m > 0 ? '+' : ''}${m} min`;
}

export function daysUntil(iso: string | null | undefined, now: Date = new Date()): number | null {
  if (!iso) return null;
  const delta = new Date(iso).getTime() - now.getTime();
  return Number.isFinite(delta) ? Math.floor(delta / 86_400_000) : null;
}

export function escapeHtml(s: unknown): string {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));
}

export type Tone = 'good' | 'warn' | 'bad' | 'neutral' | 'info';

export function confidenceTone(c: string | null | undefined): Tone {
  switch (c) {
    case 'supported_for_auto': return 'good';
    case 'needs_review': return 'warn';
    case 'not_claimable_or_out_of_scope': return 'neutral';
    default: return 'neutral';
  }
}

export function statusTone(s: string | null | undefined): Tone {
  switch (s) {
    case 'READY': return 'good';
    case 'NEEDS_REVIEW': case 'CANDIDATE': return 'warn';
    case 'SUBMITTING': case 'SUBMITTED': return 'info';
    case 'APPROVED': case 'PAID': return 'good';
    case 'REJECTED': case 'FAILED': case 'INDETERMINATE': return 'bad';
    default: return 'neutral';
  }
}

/** The app's one money format: Dutch "€ 6,35", English "€6.35". */
export function euro(cents: number | null | undefined, lang: Lang = 'nl'): string {
  if (cents === null || cents === undefined) return '–';
  return new Intl.NumberFormat(lang === 'nl' ? 'nl-NL' : 'en-IE', { style: 'currency', currency: 'EUR' }).format(cents / 100);
}
/** Every "€ 18.20" / "€18,20" inside a text in the app's one money format (euro() for that language). */
export const euroIn = (text: string, lang: Lang = 'nl'): string => String(text ?? '').replace(/€\s?(\d+)[.,](\d{2})(?!\d)/g, (_, a: string, b: string) => euro(Number(a) * 100 + Number(b), lang));
/** An amount for an input field, in the same format as euro() but without the sign: "4,17" / "4.17". */
export const amountInput = (cents: number | null | undefined, lang: Lang = 'nl'): string => (cents == null ? '' : (cents / 100).toFixed(2).replace('.', lang === 'nl' ? ',' : '.'));

export type Category = 'claimable' | 'claimed' | 'expired' | 'pending' | 'none';
const CLAIMED = ['SUBMITTING', 'SUBMITTED', 'INDETERMINATE', 'APPROVED', 'REJECTED', 'PAID', 'EXTERNALLY_CLAIMED'];
const OPEN = ['READY', 'NEEDS_REVIEW', 'FAILED', 'CANDIDATE'];

export interface CategorizableRow { claim_status: string | null; expires_at: string | null; evidence_status: string | null; delay_min: number | null; proxy_min: number | null }

export function category(r: CategorizableRow, now: Date = new Date()): Category {
  if (r.claim_status && CLAIMED.includes(r.claim_status)) return 'claimed';
  const expired = !!r.expires_at && new Date(r.expires_at).getTime() < now.getTime();
  const wasDelayed = (r.delay_min ?? r.proxy_min ?? 0) >= 30;
  if (expired && ((r.claim_status && OPEN.includes(r.claim_status)) || wasDelayed)) return 'expired';
  // Another operator (e.g. Arriva): NS can't refund it and no train times are awaited.
  if (r.evidence_status === 'other_operator' && (!r.claim_status || r.claim_status === 'CANDIDATE')) return 'none';
  if (r.evidence_status === 'pending' && (!r.claim_status || r.claim_status === 'CANDIDATE')) return 'pending';
  if (r.claim_status && OPEN.includes(r.claim_status)) return 'claimable';
  if (r.evidence_status === 'pending') return 'pending';
  return 'none';
}

export type Period = 'week' | 'month' | 'year' | 'all';
export function inPeriod(iso: string, p: Period, now: Date = new Date()): boolean {
  if (p === 'all') return true;
  const days = { week: 7, month: 30, year: 365 }[p];
  const age = now.getTime() - new Date(iso).getTime();
  return Number.isFinite(age) && age >= 0 && age <= days * 86_400_000;
}

// ---------- IBAN input helpers ----------
const NL_BANKS: Record<string, string> = {
  ABNA: 'ABN AMRO', INGB: 'ING', RABO: 'Rabobank', SNSB: 'SNS', ASNB: 'ASN Bank', RBRB: 'RegioBank', TRIO: 'Triodos Bank',
  KNAB: 'Knab', BUNQ: 'bunq', REVO: 'Revolut', FVLB: 'Van Lanschot', NNBA: 'Nationale-Nederlanden', HAND: 'Svenska Handelsbanken', ADYB: 'Adyen',
};

/** Normalise and group an IBAN as typed: "nl59rabo0123456789" -> "NL59 RABO 0123 4567 89". Never cut to the
 *  NL length: an extra character stays visible, and the check says the IBAN is too long. */
export function formatIbanInput(raw: string): string {
  const s = raw.toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 34);
  return s.replace(/(.{4})(?=.)/g, '$1 ');
}

/** Max length of an IBAN input: 34 characters plus the grouping spaces. */
export const IBAN_MAXLENGTH = 42;

/** Format an IBAN input field in place, keeping the caret at the same character (also mid-edit). */
export function applyIbanFormat(el: { value: string; selectionStart: number | null; setSelectionRange(a: number, b: number): void }): void {
  const pos = el.selectionStart ?? el.value.length;
  const alnumBefore = el.value.slice(0, pos).replace(/[^a-zA-Z0-9]/g, '').length;
  const v = formatIbanInput(el.value);
  if (v === el.value) return;
  el.value = v;
  let p2 = 0, seen = 0;
  while (p2 < v.length && seen < alnumBefore) { if (/[A-Z0-9]/.test(v[p2])) seen++; p2++; }
  try { el.setSelectionRange(p2, p2); } catch { /* not a text input */ }
}

/** Max length of the account holder's name. */
export const BANK_NAME_MAX = 70;
/** Live hint under the account-holder field once the name gets near the limit ('' otherwise). */
export function bankNameHint(v: string, lang: Lang = 'nl'): string {
  const n = v.length, nl = lang === 'nl';
  if (v && !validHolderName(v)) return nl ? 'Gebruik alleen letters, spaties en . \' -' : 'Use only letters, spaces and . \' -';
  if (n >= BANK_NAME_MAX) return nl ? `Maximaal ${BANK_NAME_MAX} tekens` : `At most ${BANK_NAME_MAX} characters`;
  if (n >= BANK_NAME_MAX - 15) return nl ? `Nog ${BANK_NAME_MAX - n} ${n === BANK_NAME_MAX - 1 ? 'teken' : 'tekens'}` : `${BANK_NAME_MAX - n} ${n === BANK_NAME_MAX - 1 ? 'character' : 'characters'} left`;
  return '';
}

/** A plausible e-mail address (name@domain.tld). */
export const isEmail = (s: string) => /^[^@\s]+@[^@\s]+\.[^@\s.]{2,}$/.test(s.trim());

/** The project's own mailbox (used by the demo): never shown as if it were the user's address. */

export function ibanMod97(iban: string): boolean {
  const s = iban.replace(/\s/g, '').toUpperCase();
  if (!/^[A-Z]{2}\d{2}[A-Z0-9]{10,30}$/.test(s)) return false;
  const r = s.slice(4) + s.slice(0, 4);
  let rem = 0;
  for (const ch of r) {
    const v = /\d/.test(ch) ? ch : String(ch.charCodeAt(0) - 55);
    for (const d of v) rem = (rem * 10 + Number(d)) % 97;
  }
  return rem === 1;
}

export type IbanCheck = { state: 'empty' | 'typing' | 'invalid' | 'valid'; bank?: string; message: string };

export function checkIban(input: string, lang: Lang = 'nl'): IbanCheck {
  const s = input.replace(/\s/g, '').toUpperCase();
  const nl = lang === 'nl';
  if (!s) return { state: 'empty', message: '' };
  if (s.startsWith('NL')) {
    if (s.length < 18) return { state: 'typing', message: nl ? `Nog ${18 - s.length} ${s.length === 17 ? 'teken' : 'tekens'}` : `${18 - s.length} ${s.length === 17 ? 'character' : 'characters'} to go` };
    if (s.length > 18) return { state: 'invalid', message: nl ? `Te lang: een Nederlands IBAN heeft 18 tekens, dit zijn er ${s.length}` : `Too long: a Dutch IBAN has 18 characters, this has ${s.length}` };
    if (!/^NL\d{2}[A-Z]{4}\d{10}$/.test(s)) return { state: 'invalid', message: nl ? 'Opbouw: NL + 2 cijfers + 4 letters (bank) + 10 cijfers' : 'Format: NL + 2 digits + 4 letters (bank) + 10 digits' };
  }
  if (!ibanMod97(s)) return { state: s.length < 15 ? 'typing' : 'invalid', message: nl ? 'Dit IBAN klopt niet. Controleer het nummer.' : 'This IBAN is not valid. Check the number.' };
  const bank = s.startsWith('NL') ? NL_BANKS[s.slice(4, 8)] : undefined;
  return { state: 'valid', bank, message: bank ? `✓ ${bank}` : nl ? '✓ geldig IBAN' : '✓ valid IBAN' };
}

// ---------- Dutch rendering of policy reasons (core emits stable English strings) ----------
const REASONS_NL: [RegExp, string][] = [
  [/^already claimed \(this app or externally\)$/, 'Al geclaimd (via deze app of elders)'],
  [/^arrived on time or with minor delay$/, 'Op tijd of met kleine vertraging aangekomen'],
  [/^carrier is not NS \((.*)\)$/, 'Vervoerder is niet NS ($1)'],
  [/^check-in and check-out at the same station$/, 'In- en uitgecheckt op hetzelfde station'],
  [/^check-out suggests a delay but the train actually taken could not be identified$/, 'De uitchecktijd wijst op vertraging, maar het is niet duidelijk welke trein je hebt genomen'],
  [/^check-out (-?\d+) min after the matched arrival; train identification uncertain$/, 'Je checkte $1 min na de gevonden aankomst uit. Het is niet zeker welke trein je nam'],
  [/^claim window \(3 months\) has passed — missed claim$/, 'De claimtermijn van 3 maanden is voorbij. Deze claim is gemist'],
  [/^compensation amount unknown \(fare not in travel history\)$/, 'Vergoeding onbekend (ritprijs staat niet in de reishistorie)'],
  [/^a later train than planned was taken while the planned journey ran \(nearly\) on time — check whether there was a disruption$/, 'Je nam een latere trein dan gepland, terwijl de geplande reis (bijna) op tijd reed. Controleer of er een storing was'],
  [/^estimated arrival delay (-?\d+) min \(< 30\)$/, 'Geschatte aankomstvertraging $1 min (minder dan 30)'],
  [/^estimated arrival delay (-?\d+) min \((.*)\)$/, 'Geschatte aankomstvertraging $1 min ($2)'],
  [/^high-speed line \(different thresholds\/form\)$/, 'Hogesnelheidslijn (andere grenzen en formulier)'],
  [/^high-speed line \(different thresholds\/form\); see (\S+)$/, 'Hogesnelheidslijn (andere grenzen en formulier). Zie $1'],
  [/^historical timetable for this month not yet published; will re-check automatically$/, 'De dienstregelingsgegevens van deze maand zijn nog niet gepubliceerd. OV Refund controleert deze reis later vanzelf opnieuw'],
  [/^journey longer than 6 h; check data$/, 'Reis langer dan 6 uur. Controleer de gegevens'],
  [/^late check-out but the trains ran on time; not automatically claimable$/, 'Je checkte laat uit, maar de treinen reden op tijd. OV Refund dient dit niet automatisch in'],
  [/^late check-out but trains were only (0|-\d+) min late; not automatically claimable$/, 'Je checkte laat uit, maar de treinen reden op tijd'],
  [/^late check-out but trains were only (-?\d+) min late; not automatically claimable$/, 'Je checkte laat uit, maar de treinen waren maar $1 min te laat. OV Refund dient dit niet automatisch in'],
  [/^no check-out recorded \(use NS 'vergeten uit te checken' instead\)$/, 'Geen uitcheck geregistreerd (gebruik bij NS ‘Vergeten uit te checken’)'],
  [/^no destination station$/, 'Geen eindstation bekend'],
  [/^no matching scheduled route found; check manually$/, 'Geen passende geplande route gevonden. Controleer dit zelf'],
  [/^no planned direct or 1-transfer route found in archive$/, 'Geen geplande directe route of route met één overstap gevonden'],
  [/^no realised route consistent with check-in\/check-out$/, 'Geen gereden route die past bij de in- en uitchecktijden'],
  [/^outside the 3-month claim window$/, 'Buiten de claimtermijn van 3 maanden'],
  [/^planned train was cancelled — check it was not announced in advance \(planned works\)$/, 'Geplande trein is uitgevallen. Controleer of dit niet vooraf was aangekondigd (bijvoorbeeld door werkzaamheden)'],
  [/^route includes a non-NS train$/, 'Route bevat een trein van een andere vervoerder'],
  [/^schedule is not the historical timetable for the travel date$/, 'De gebruikte dienstregeling is niet die van de reisdag'],
  [/^station not in timetable data$/, 'Station niet gevonden in de dienstregelingsgegevens'],
  [/^NS accepts claims only 24 h after travel$/, 'NS accepteert claims pas 24 uur na de reis'],
  [/^adjusted timetable: also less than 30 minutes late against the normal timetable$/, 'Aangepaste dienstregeling: ook volgens de normale dienstregeling minder dan 30 minuten te laat'],
  [/^adjusted timetable that day; also against the regular timetable only (-?\d+) min later$/, 'De dienstregeling was die dag aangepast. Ook volgens de normale dienstregeling kwam je maar $1 min later aan'],
  [/^timetable was adjusted that day; against the regular timetable you arrived about (-?\d+) min later — NS may compensate this, check yourself$/, 'De dienstregeling was die dag aangepast. Volgens de normale dienstregeling kwam je ongeveer $1 min later aan. NS kan dit vergoeden. Kijk zelf of je claimt'],
  [/^timetable was adjusted that day \(diverted trains, planned works\); NS may compensate against the regular timetable — check yourself$/, 'De dienstregeling was die dag aangepast (omgeleide treinen, werkzaamheden). NS vergoedt soms op basis van de normale dienstregeling. Kijk zelf of je claimt'],
  [/^refund below €\s?2[.,]30: NS does not pay this out$/, 'Vergoeding lager dan € 2,30: NS betaalt dit niet uit'],
  [/^a different train was selected — train identification uncertain$/, 'Een andere trein past bij je reis. Kijk zelf welke trein je nam'],
  [/^planned train was cancelled — check the replacement train you took$/, 'Je geplande trein viel uit. Kijk zelf welke trein je in plaats daarvan nam'],
  [/^planned train was cancelled; replacement train matches check-in\/check-out$/, 'Je geplande trein viel uit. De vervangende trein past bij je in- en uitchecktijden. NS beoordeelt je claim'],
  [/^connection was missed; replacement train matches check-in\/check-out$/, 'Je overstap werd gemist. De vervangende trein past bij je in- en uitchecktijden. NS beoordeelt je claim'],
  [/^matched arrival uses a cancelled train; no operated replacement confirmed$/, 'De gevonden trein viel uit. Er is nog geen gereden vervangende trein gevonden'],
  [/^connection was missed — check the replacement train you took$/, 'Je miste je overstap. Kijk zelf welke trein je daarna nam'],
  [/^check-out well after the earliest fitting arrival — train identification uncertain$/, 'Je checkte veel later uit dan de vroegste passende aankomst. Kijk zelf welke trein je nam'],
  [/^waiting for timetable data$/, 'Wacht op treintijden'],
  [/^automatic retry \(previous attempt was not sent\)$/, 'Automatisch opnieuw geprobeerd (de vorige poging is niet verzonden)'],
  [/^reviewed and approved by user \(resubmission confirmed\)$/, 'Door jou bekeken en goedgekeurd (opnieuw indienen bevestigd)'],
  [/^reviewed and approved by user$/, 'Door jou bekeken en goedgekeurd'],
  [/^automatic submission is no longer enabled — stopped$/, 'Gestopt: automatisch indienen staat niet meer aan'],
  [/^.*replacement bus.*$/i, 'Een deel van de reis ging met een vervangende bus. Controleer de tijden zelf'],
];

/** The same core reasons in plain English (the raw core strings are terse and technical). */
const REASONS_EN: [RegExp, string][] = [
  [/^already claimed \(this app or externally\)$/, 'Already claimed (through this app or elsewhere)'],
  [/^arrived on time or with minor delay$/, 'Arrived on time or with minor delay'],
  [/^carrier is not NS \((.*)\)$/, 'The carrier is not NS ($1)'],
  [/^check-in and check-out at the same station$/, 'Checked in and out at the same station'],
  [/^check-out suggests a delay but the train actually taken could not be identified$/, 'Your check-out time suggests a delay, but it is not clear which train you took'],
  [/^check-out (-?\d+) min after the matched arrival; train identification uncertain$/, 'You checked out $1 min after the arrival found. It is not certain which train you took'],
  [/^claim window \(3 months\) has passed — missed claim$/, 'The 3-month claim window has passed. This claim was missed'],
  [/^compensation amount unknown \(fare not in travel history\)$/, 'Refund amount unknown (fare not in your travel history)'],
  [/^a later train than planned was taken while the planned journey ran \(nearly\) on time — check whether there was a disruption$/, 'You took a later train while the planned one ran (nearly) on time. Check whether there was a disruption'],
  [/^estimated arrival delay (-?\d+) min \(< 30\)$/, 'Estimated arrival delay $1 min (less than 30)'],
  [/^estimated arrival delay (-?\d+) min \(30 t\/m 59 min\)$/, 'Estimated arrival delay $1 min (30–59 min)'],
  [/^estimated arrival delay (-?\d+) min \(60 minuten of meer\)$/, 'Estimated arrival delay $1 min (60 min or more)'],
  [/^estimated arrival delay (-?\d+) min \((.*)\)$/, 'Estimated arrival delay $1 min ($2)'],
  [/^high-speed line \(different thresholds\/form\)$/, 'High-speed line (different thresholds and form)'],
  [/^high-speed line \(different thresholds\/form\); see (\S+)$/, 'High-speed line (different thresholds and form). See $1'],
  [/^historical timetable for this month not yet published; will re-check automatically$/, 'This month’s timetable data is not published yet. OV Refund checks this trip again by itself later'],
  [/^journey longer than 6 h; check data$/, 'Trip longer than 6 hours. Check the details'],
  [/^late check-out but the trains ran on time; not automatically claimable$/, 'You checked out late, but the trains ran on time. OV Refund doesn’t submit this automatically'],
  [/^late check-out but trains were only (0|-\d+) min late; not automatically claimable$/, 'You checked out late, but the trains ran on time'],
  [/^late check-out but trains were only (-?\d+) min late; not automatically claimable$/, 'You checked out late, but the trains were only $1 min late. OV Refund doesn’t submit this automatically'],
  [/^no check-out recorded \(use NS 'vergeten uit te checken' instead\)$/, 'No check-out recorded (use NS’s ‘Forgot to check out’ instead)'],
  [/^no destination station$/, 'No destination station known'],
  [/^no matching scheduled route found; check manually$/, 'No matching planned route found. Check this yourself'],
  [/^no planned direct or 1-transfer route found in archive$/, 'No planned direct route or route with one change found'],
  [/^no realised route consistent with check-in\/check-out$/, 'No train that ran matches your check-in and check-out times'],
  [/^outside the 3-month claim window$/, 'Outside the 3-month claim window'],
  [/^planned train was cancelled — check it was not announced in advance \(planned works\)$/, 'Planned train was cancelled. Check it was not announced in advance (e.g. planned works)'],
  [/^route includes a non-NS train$/, 'The route includes a train of another carrier'],
  [/^schedule is not the historical timetable for the travel date$/, 'The timetable used is not the one for the travel date'],
  [/^station not in timetable data$/, 'Station not found in the timetable data'],
  [/^NS accepts claims only 24 h after travel$/, 'NS only accepts claims 24 hours after the trip'],
  [/^adjusted timetable: also less than 30 minutes late against the normal timetable$/, 'Adjusted timetable: also less than 30 minutes late against the normal timetable'],
  [/^adjusted timetable that day; also against the regular timetable only (-?\d+) min later$/, 'The timetable was adjusted that day. Against the regular timetable too, you arrived only $1 min later'],
  [/^timetable was adjusted that day; against the regular timetable you arrived about (-?\d+) min later — NS may compensate this, check yourself$/, 'The timetable was adjusted that day. Against the regular timetable you arrived about $1 min later. NS may refund this. Decide yourself whether to claim'],
  [/^timetable was adjusted that day \(diverted trains, planned works\); NS may compensate against the regular timetable — check yourself$/, 'The timetable was adjusted that day (diverted trains, planned works). NS sometimes refunds based on the regular timetable. Decide yourself whether to claim'],
  [/^refund below €\s?2[.,]30: NS does not pay this out$/, 'Refund below €2.30: NS does not pay this out'],
  [/^a different train was selected — train identification uncertain$/, 'A different train fits your trip. Check which train you took'],
  [/^planned train was cancelled — check the replacement train you took$/, 'Your planned train was cancelled. Check which train you took instead'],
  [/^planned train was cancelled; replacement train matches check-in\/check-out$/, 'Your planned train was cancelled. The replacement train fits your check-in and check-out times. NS assesses your claim'],
  [/^connection was missed; replacement train matches check-in\/check-out$/, 'Your connection was missed. The replacement train fits your check-in and check-out times. NS assesses your claim'],
  [/^matched arrival uses a cancelled train; no operated replacement confirmed$/, 'The matched train was cancelled. No replacement train that ran has been confirmed'],
  [/^connection was missed — check the replacement train you took$/, 'You missed your connection. Check which train you took next'],
  [/^check-out well after the earliest fitting arrival — train identification uncertain$/, 'You checked out well after the earliest fitting arrival. Check which train you took'],
  [/^waiting for timetable data$/, 'Awaiting timetable'],
  [/^automatic retry \(previous attempt was not sent\)$/, 'Retried automatically (the previous attempt was not sent)'],
  [/^reviewed and approved by user \(resubmission confirmed\)$/, 'Reviewed and approved by you (submitting again confirmed)'],
  [/^reviewed and approved by user$/, 'Reviewed and approved by you'],
  [/^automatic submission is no longer enabled — stopped$/, 'Stopped: automatic submission is no longer on'],
  [/^.*replacement bus.*$/i, 'Part of the trip was by replacement bus. Check the times yourself'],
];

/** Core policy reasons are stable English strings: shown in plain words from the table of the language. */
export function reasonText(r: string, lang: Lang): string {
  const near = /^estimated delay (-?\d+) min is within ±(\d+) min of a threshold$/.exec(r);
  if (near) return nearThresholdText(Number(near[1]), lang);
  for (const [re, txt] of lang === 'nl' ? REASONS_NL : REASONS_EN) if (re.test(r)) return r.replace(re, txt);
  if (lang === 'nl') return r;
  const en = r.replace('30 t/m 59 min', '30–59 min').replace('60 minuten of meer', '60 min or more');
  return en.charAt(0).toUpperCase() + en.slice(1);
}

/** "Nederlands / English" -> the half for `lang`; other text unchanged (same rule as i18n.pickLang). */
export function pickIn(s: string, lang: Lang): string {
  const m = String(s ?? '').match(/^(.*?) \/ (.*)$/s);
  return m ? (lang === 'nl' ? m[1] : m[2]) : String(s ?? '');
}

/** A claim-history note: bilingual backend text is split; policy notes ("a; b", optionally
 *  "no longer claimable: a; b") are translated reason by reason. */
export function historyDetail(detail: string, lang: Lang): string {
  const d = String(detail ?? '').trim();
  if (!d) return '';
  if (d.includes(' / ')) return pickIn(d, lang);
  const m = /^no longer claimable: (.*)$/s.exec(d);
  const body = (m ? m[1] : d).split('; ').map((x) => reasonText(x.trim(), lang)).join('; ');
  return m ? `${lang === 'nl' ? 'Niet meer claimbaar' : 'No longer claimable'}: ${body}` : body;
}

/** The one explanation for a delay close to 30 or 60 minutes, with what to do. */
export function nearThresholdText(d: number, lang: Lang): string {
  const g = Math.abs(d - 60) < Math.abs(d - 30) ? 60 : 30;
  const nl = lang === 'nl';
  const head = nl ? `De geschatte vertraging (${d} min) ligt vlak bij de grens van ${g} minuten.` : `The estimated delay (${d} min) is close to the ${g}-minute threshold.`;
  const doubt = nl ? 'NS beslist. Twijfel je? Kies dan Niet claimen.' : 'NS decides. If in doubt, choose Don’t claim.';
  // At or above the threshold OV Refund can submit it; below, the claim needs a band it can't fill in.
  if (d >= g) return `${head} ${nl ? `Weet je zeker dat je ${g} minuten of meer te laat was? Dien dan in.` : `Are you sure you were ${g} minutes or more late? Then submit.`} ${doubt}`;
  if (g === 60) return `${head} ${nl ? 'OV Refund dient hem in als 30–59 minuten. Was je 60 minuten of meer te laat? Dien hem dan zelf in via Mijn NS.' : 'OV Refund submits it as 30–59 minutes. Were you 60 minutes or more late? Then submit it yourself via Mijn NS.'} ${doubt}`;
  return `${head} ${nl ? 'Was je 30 minuten of meer te laat? Dien de claim dan zelf in via Mijn NS. OV Refund kan dat niet onder de 30 minuten.' : 'Were you 30 minutes or more late? Then submit the claim yourself via Mijn NS. OV Refund can’t do that under 30 minutes.'} ${doubt}`;
}

// ---------- refund basis (core writes it in Dutch) ----------
/** The core's Dutch `compensation_basis` in the user's language, with Dutch decimal commas in Dutch. */
export function basisText(basis: string | null | undefined, lang: Lang): string {
  let b = String(basis ?? '').trim().replace(/^NS-regel:\s*/, '');
  if (!b) return '';
  if (lang === 'nl') return euroIn(b, lang);
  const rules: [RegExp, string][] = [
    [/^50% van de ritprijs \((.*)\)$/, 'half the fare ($1)'],
    [/^100% van de ritprijs \((.*)\)$/, 'the full fare ($1)'],
    [/^1\/(\d+) van je maandbedrag (.*) \((€ [\d.,]+), volgens NS\)$/, '1/$1 of your monthly fee for $2 ($3, according to NS)'],
    [/^1\/(\d+) van de maandprijs (.*) \((€ [\d.,]+) op de reisdatum, prijs 2e klas: 1e klas is hoger; bron (.*)\)$/, '1/$1 of the monthly price of $2 ($3 on the travel date, 2nd-class price: 1st class is higher; source $4)'],
    [/^1\/(\d+) van de maandprijs (.*) \((€ [\d.,]+) op de reisdatum; bron (.*)\)$/, '1/$1 of the monthly price of $2 ($3 on the travel date; source $4)'],
    [/^wat NS betaalde voor een vergelijkbare reis rond die tijd \((.*)\)$/, 'what NS paid for a similar trip around that time ($1)'],
  ];
  for (const [re, en] of rules) if (re.test(b)) { b = b.replace(re, en); break; }
  return euroIn(b, lang);
}

// ---------- subscription (overview.subscription) ----------
export interface SubPart { code: string; nl: string; en: string; kind: 'free' | 'discount' | 'unknown'; fractions: [number, number] | null; verified?: boolean }
export interface SubContract { parts: SubPart[]; fee_cents: number | null; class: 1 | 2 | null; since?: string; last_seen?: string; until?: string }
export interface Subscription { current: SubContract | null; previous?: SubContract[]; none?: boolean }

const subNames = (c: SubContract, lang: Lang) => (c.parts ?? []).map((p) => (lang === 'nl' ? p.nl || p.code : p.en || p.nl || p.code)).join(' + ');
const monthFee = (cents: number, lang: Lang) => `${euro(cents, lang).replace(/\s/g, ' ')}/${lang === 'nl' ? 'mnd' : 'month'}`;

/** "Dal Voordeel + Weekend Vrij · € 41,00/mnd (volgens NS) · 1e klas". The fee only matters for
 *  free-travel (Vrij) parts, so "not known yet" is only said for those. */
export function subscriptionLine(c: SubContract | null | undefined, lang: Lang): string {
  if (!c || !c.parts?.length) return '';
  const nl = lang === 'nl';
  const free = c.parts.some((p) => p.kind === 'free');
  const fee = c.fee_cents != null && c.fee_cents > 0
    ? ` · ${monthFee(c.fee_cents, lang)} (${nl ? 'volgens NS' : 'according to NS'})`
    : free ? ` · ${nl ? 'maandbedrag nog niet bekend' : 'monthly fee not known yet'}` : '';
  const cls = c.class === 1 ? ` · ${nl ? '1e klas' : '1st class'}` : '';
  return `${subNames(c, lang)}${fee}${cls}`;
}

/** Plain lines that explain what the subscription means for refunds (plus earlier contracts). */
export function subscriptionExplain(s: Subscription | null | undefined, lang: Lang): string[] {
  const nl = lang === 'nl';
  if (!s) return [];
  const out: string[] = [];
  const c = s.current;
  if (!c || !c.parts?.length) {
    if (s.none) out.push(nl ? 'Geen abonnement: bij 30–59 min vertraging krijg je de helft van de ritprijs terug, bij 60 min of meer de hele ritprijs.' : 'No subscription: for a 30–59 min delay you get half the fare back, for 60 min or more the full fare.');
  } else {
    let paidTrips = false;
    for (const p of c.parts) {
      const name = nl ? p.nl || p.code : p.en || p.nl || p.code;
      if (p.kind === 'free' && p.fractions) {
        const [a, b] = p.fractions;
        out.push(nl ? `Gratis reizen met ${name}: bij 30–59 min krijg je 1/${a}, bij 60+ min 1/${b} van je maandbedrag.` : `Free travel with ${name}: for 30–59 min you get 1/${a}, for 60+ min 1/${b} of your monthly fee.`);
        if (!/altijd|always/i.test(`${p.nl} ${p.en} ${p.code}`)) paidTrips = true;
      } else if (p.kind === 'discount') {
        paidTrips = true;
      } else {
        out.push(nl ? `Abonnement niet herkend (code ${p.code}). Bedragen tonen we daarom als onbekend.` : `Subscription not recognised (code ${p.code}). Amounts are shown as unknown.`);
      }
    }
    if (paidTrips) out.push(nl ? 'Betaalde reizen: de helft (30–59 min) of de hele ritprijs (60+ min) die je betaalde.' : 'Paid trips: half (30–59 min) or all (60+ min) of the fare you paid.');
  }
  for (const p of s.previous ?? []) {
    if (!p?.parts?.length) continue;
    const until = p.until ? localDate(p.until, lang) : '?';
    const fee = p.fee_cents != null && p.fee_cents > 0 ? ` · ${monthFee(p.fee_cents, lang)}` : '';
    out.push(`${nl ? 'Tot' : 'Until'} ${until}: ${subNames(p, lang)}${fee}`);
  }
  return out;
}

// ---------- "next step" banner ----------
export interface NextStepInput { auth: string; claimList?: string | null; claimable: number; claimableCents: number; claimableUnknown: number; review: number; hasRows: boolean }
/** `text` is one short sentence; `help` (optional) is the detail shown behind a "?". */
export interface NextStep { tone: 'ok' | 'warn' | 'bad'; text: string; help?: string; action?: 'login' | 'claimable' | 'review' | 'autologin' }

const trips = (n: number, lang: Lang) => (lang === 'nl' ? (n === 1 ? '1 reis' : `${n} reizen`) : n === 1 ? '1 trip' : `${n} trips`);

/** One short sentence that says what to do now (or that there is nothing to do). null: nothing to say yet. */
export function nextStep(i: NextStepInput, lang: Lang): NextStep | null {
  const nl = lang === 'nl';
  // "blocked: <reason>" (reason from the backend): an extra NS check, or a changed NS website.
  if (i.auth.startsWith('blocked: ')) {
    if (/verificatiecode/.test(i.auth)) return { tone: 'bad', action: 'login', text: nl ? 'NS vraagt om een extra controle. Log zelf in.' : 'NS asks for an extra check. Please log in yourself.', help: nl ? 'NS wil een code zien. Log zelf in bij Mijn NS. Daarna gaat OV Refund verder.' : 'NS wants a code. Log in to Mijn NS yourself. OV Refund then carries on.' };
    if (/website/i.test(i.auth)) return { tone: 'bad', text: nl ? 'Mijn NS is veranderd. Er is niets verstuurd.' : 'Mijn NS has changed. Nothing was sent.', help: nl ? 'OV Refund herkent de NS-website niet meer. Probeer het later opnieuw of wacht op een update.' : 'OV Refund no longer recognises the NS website. Try again later or wait for an update.' };
  }
  switch (i.auth.startsWith('blocked') ? 'blocked' : i.auth) {
    case 'account_mismatch': return { tone: 'bad', action: 'login', text: nl ? 'Je bent ingelogd met een ander NS-account.' : 'You are logged in with a different NS account.', help: nl ? 'Log in met je eigen Mijn NS-account. Tot dan controleert en dient OV Refund niets in.' : 'Log in with your own Mijn NS account. Until then OV Refund doesn’t check or submit anything.' };
    case 'blocked': return { tone: 'bad', action: 'login', text: nl ? 'NS laat OV Refund tijdelijk niet binnen.' : 'NS is temporarily blocking OV Refund.', help: nl ? 'Bijvoorbeeld na te veel pogingen of een extra controle. Log zelf in bij Mijn NS en probeer het later opnieuw.' : 'For example after too many attempts or an extra check. Log in to Mijn NS yourself and try again later.' };
    case 'autologin_failed': return { tone: 'bad', action: 'autologin', text: nl ? 'Automatisch inloggen bij NS lukte niet.' : 'Automatic login to NS failed.', help: nl ? 'Klopt je NS-wachtwoord nog? Pas het aan, of log zelf in.' : 'Is your NS password still right? Update it, or log in yourself.' };
    case 'logged_out': return { tone: 'warn', action: 'login', text: nl ? 'Je bent uitgelogd bij Mijn NS.' : 'You are logged out of Mijn NS.' };
    case 'ok': case 'not_connected': case '': break;
    default: return { tone: 'warn', action: 'login', text: nl ? 'Je NS-sessie is verlopen.' : 'Your NS session has expired.', help: nl ? 'Automatische controles wachten tot je opnieuw inlogt.' : 'Automatic checks wait until you log in again.' };
  }
  if (i.claimList === 'unavailable') return { tone: 'bad', text: nl ? 'Mijn teruggaven kon niet worden gelezen.' : 'Mijn teruggaven could not be read.', help: nl ? 'Automatisch indienen wacht tot de volgende controle. Zelf indienen kan wel.' : 'Automatic submission waits until the next check. You can still submit yourself.' };
  if (!i.hasRows) return null;
  if (i.claimable > 0) {
    const amount = i.claimableCents > 0 ? ` (≈ ${euro(i.claimableCents, lang).replace(/\s/g, ' ')}${i.claimableUnknown ? (nl ? ' + onbekend' : ' + unknown') : ''})` : '';
    const review = i.review ? (nl ? `, ${i.review} om te beoordelen` : `, ${i.review} to review`) : '';
    return { tone: 'ok', action: 'claimable', text: nl ? `${trips(i.claimable, lang)} te claimen${amount}${review}` : `${trips(i.claimable, lang)} to claim${amount}${review}` };
  }
  if (i.review > 0) return { tone: 'warn', action: 'review', text: nl ? `${trips(i.review, lang)} om te beoordelen` : `${trips(i.review, lang)} to review`, help: nl ? 'Bij deze reizen is niet alles zeker. Bekijk ze en beslis zelf.' : 'Not everything is certain for these trips. Have a look and decide yourself.' };
  if (i.auth === 'not_connected') return null;
  return { tone: 'ok', text: nl ? 'Niets te doen. Er staat niets klaar om te claimen.' : 'Nothing to do. No trips waiting to be claimed.' };
}

// ---------- progress (inside the "Check now" button) ----------
/** A very short word for the stage of a running check. */
export function stageWord(stage: string, lang: Lang): string {
  const key = String(stage ?? '').startsWith('timetable') ? 'timetable' : String(stage ?? '');
  const t: Record<string, [string, string]> = {
    login: ['Inloggen', 'Logging in'], fetch: ['Ophalen', 'Fetching'], timetable: ['Treintijden', 'Timetables'],
    assess: ['Controleren', 'Checking'], claims: ['Claims', 'Claims'], done: ['Klaar', 'Done'],
    submit: ['Indienen', 'Submitting'], pause: ['Pauze', 'Pause'],
  };
  const w = t[key] ?? ['Bezig', 'Working'];
  return lang === 'nl' ? w[0] : w[1];
}
export interface TimetableProgress { month?: string; index?: number; total?: number; seconds?: number }

/** Short localised calendar month, independent of the machine's time zone. */
export function monthProgressLabel(month: string, index: number, total: number, lang: Lang): string {
  if (!/^\d{4}-(0[1-9]|1[0-2])$/.test(month) || !Number.isInteger(index) || !Number.isInteger(total) || index < 1 || total < index) return '';
  const date = new Date(Date.UTC(2000, Number(month.slice(5)) - 1, 1));
  const short = new Intl.DateTimeFormat(lang === 'nl' ? 'nl-NL' : 'en-GB', { month: 'short', timeZone: 'UTC' }).format(date).replace(/\.$/, '');
  const name = short[0].toUpperCase() + short.slice(1);
  return `${name} ${month.slice(0, 4)} · ${index}/${total}`;
}

/** "Ophalen… 34%": what the progress button shows. */
export function progressText(stage: string, pct: number, lang: Lang, month: TimetableProgress = {}): string {
  if (stage === 'timetable') {
    const label = monthProgressLabel(month.month ?? '', month.index ?? 0, month.total ?? 0, lang);
    if (label) return label;
  }
  const k = month.index ?? 0, n = month.total ?? 0;
  const counted = Number.isInteger(k) && Number.isInteger(n) && k >= 1 && n >= k;
  // Automatic submission: "Claim 2 van 5", and the pause before the next one as "Pauze · 3/5"
  // (the pause takes most of the time, so it says what comes next; without a count: "Pauze 0:42").
  if (stage === 'submit' && counted) return lang === 'nl' ? `Claim ${k} van ${n}` : `Claim ${k} of ${n}`;
  if (stage === 'pause' && counted) return lang === 'nl' ? `Pauze · ${k}/${n}` : `Pause · ${k}/${n}`;
  if (stage === 'pause' && Number.isFinite(month.seconds)) {
    const t = Math.max(0, Math.round(month.seconds ?? 0));
    return `${stageWord('pause', lang)} ${Math.floor(t / 60)}:${String(t % 60).padStart(2, '0')}`;
  }
  const p = Math.max(0, Math.min(100, Math.round(Number.isFinite(pct) ? pct : 0)));
  return `${stageWord(stage, lang)}${stage === 'done' ? '' : '…'} ${p}%`;
}

// ---------- "?" help tips ----------
/** A tiny "?" button with its popover (behaviour: installHelpTips in i18n.ts). `body` is trusted HTML (inline content only). */
export function helpTip(id: string, body: string, label: string): string {
  const k = String(id).replace(/[^A-Za-z0-9_-]/g, '-');
  return `<button type="button" class="qhelp" data-help="${k}" aria-label="${escapeHtml(label)}" aria-expanded="false" aria-controls="qp-${k}">?</button><span class="qpop" id="qp-${k}" popover="manual">${body}</span>`;
}
/** Where a popover goes: below its anchor (centred, kept inside the window), above it when there is no room below. */
export function placePopover(a: { left: number; top: number; bottom: number; width: number }, p: { width: number; height: number }, vp: { width: number; height: number }, gap = 6, margin = 8): { left: number; top: number } {
  const left = Math.max(margin, Math.min(a.left + a.width / 2 - p.width / 2, vp.width - margin - p.width));
  let top = a.bottom + gap;
  if (top + p.height > vp.height - margin && a.top - gap - p.height >= margin) top = a.top - gap - p.height;
  top = Math.max(margin, Math.min(top, vp.height - margin - p.height));
  return { left: Math.round(left), top: Math.round(top) };
}
/** A long folder path, shortened from the left: "…\Documents\OV Refund". */
export function shortPath(p: string, max = 36): string {
  const s = String(p ?? '');
  if (s.length <= max) return s;
  const parts = s.split(/[\\/]/);
  let out = parts.pop() ?? '';
  while (parts.length && out.length + parts[parts.length - 1].length + 3 <= max) out = `${parts.pop()}\\${out}`;
  return `…\\${out}`;
}

/** The refund explained in one plain sentence, built from the core's (Dutch) basis. '' when there is nothing to explain. */
export function refundSentence(delayMin: number | null | undefined, basis: string | null | undefined, cents: number | null | undefined, lang: Lang): string {
  const nl = lang === 'nl';
  const b = String(basis ?? '').trim().replace(/^NS-regel:\s*/, '');
  const d = delayMin ?? null;
  const late = d !== null && d > 0 ? (nl ? `Je kwam ongeveer ${d} minuten later aan. ` : `You arrived about ${d} minutes late. `) : '';
  const band = d !== null && d >= 60 ? (nl ? 'Bij 60 minuten of meer' : 'For 60 minutes or more') : (nl ? 'Bij 30–59 minuten' : 'For 30–59 minutes');
  const nsDecides = nl ? 'NS beslist.' : 'NS decides.';
  const amt = cents != null ? euro(cents, lang) : '';
  let m = /^(50%|100%) van de ritprijs \((€ [\d.,]+)\)$/.exec(b);
  if (m) {
    const part = m[1] === '100%' ? (nl ? 'de hele ritprijs' : 'the full fare') : (nl ? 'de helft van de ritprijs' : 'half the fare');
    const of = m[1] === '100%' ? '' : ` (${amt} ${nl ? 'van' : 'of'} ${euroIn(m[2], lang)})`;
    return `${late}${band} ${nl ? 'krijg je' : 'you get'} ${part}${of} ${nl ? 'terug' : 'back'}. ${nsDecides}`;
  }
  m = /^1\/(\d+) van (?:je maandbedrag|de maandprijs) (.+?) \(/.exec(b);
  if (m) {
    const share = nl ? `1/${m[1]} van je maandbedrag` : `1/${m[1]} of your monthly fee`;
    return `${late}${nl ? `Je reisde gratis met ${m[2]}.` : `You travelled free with ${m[2]}.`} ${band} ${nl ? 'krijg je' : 'you get'} ${share} ${nl ? 'terug' : 'back'}${amt ? ` (≈ ${amt})` : ''}. ${nsDecides}`;
  }
  if (/^wat NS betaalde voor een vergelijkbare reis/.test(b)) return `${late}${nl ? `Geschat op wat NS betaalde voor een vergelijkbare reis${amt ? ` (≈ ${amt})` : ''}.` : `Estimated from what NS paid for a similar trip${amt ? ` (≈ ${amt})` : ''}.`} ${nsDecides}`;
  if (d !== null && d >= 30) return `${late}${band} ${nl ? 'krijg je' : 'you get'} ${d >= 60 ? (nl ? 'de hele ritprijs' : 'the full fare') : (nl ? 'de helft van de ritprijs' : 'half the fare')} ${nl ? 'terug (met een abonnement: het deel dat daarbij hoort)' : 'back (with a subscription: the matching share)'}. ${nsDecides}`;
  return '';
}

// ---------- e-mail ----------
/** Hotmail / Outlook.com / Live / MSN: Microsoft blocks app-password sign-in, so v1 does not support them. */
const UNSUPPORTED_MAIL: [string, string] = ['Hotmail, Outlook.com en Live laten apps zoals OV Refund geen mail meer versturen. Gebruik Windows-meldingen of een ander adres, zoals Gmail.', 'Hotmail, Outlook.com and Live no longer let apps like OV Refund send mail. Use Windows notifications or another address, such as Gmail.'];
/** The one text for an unsupported (Microsoft consumer) mail address, shared by the sidebar and the wizard. */
export const unsupportedMailText = (lang: Lang) => UNSUPPORTED_MAIL[lang === 'nl' ? 0 : 1];
export const isUnsupportedMail = (email: string) => /^(outlook|hotmail|live|msn)\.(com|nl|be)$/.test(email.trim().toLowerCase().split('@')[1] ?? '');

/** Mask likely personal numbers (IBANs, card and phone numbers) in free text before it leaves the computer. */
export function redactText(s: string): string {
  return String(s ?? '')
    .replace(/(["']?(?:authorization|cookie|set-cookie|ocp-apim-subscription-key|x-csrf-token|csrftoken|access[_-]?token|refresh[_-]?token|authtoken|password|wachtwoord)["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,;&]+)/gi, '$1[secret]')
    .replace(/bearer\s+[A-Za-z0-9._~+\/=-]+/gi, 'Bearer [secret]')
    .replace(/[\w.+-]+@[\w-]+\.[\w.-]+/g, '[email]')
    .replace(/\b[A-Z]{2}\d{2}(?:\s?[A-Z0-9]{4}){2,7}(?:\s?[A-Z0-9]{1,3})?\b/gi, '[IBAN]')
    .replace(/\b\d(?:[\s().-]*\d){9,}\b/g, '[nummer]');
}

// ---------- e-mail provider presets ----------
/** `note` is [Dutch, English]. */
export interface MailPreset { name: string; host: string; port: number; security: 'starttls' | 'tls'; passwordHelpUrl?: string; note?: [string, string]; unsupported?: boolean }
const PRESETS: [RegExp, MailPreset][] = [
  [/^(gmail|googlemail)\.com$/, { name: 'Gmail', host: 'smtp.gmail.com', port: 587, security: 'starttls', passwordHelpUrl: 'https://myaccount.google.com/apppasswords', note: ['Bij een werk- of schoolaccount (Google Workspace) of met Geavanceerde beveiliging kan Google app-wachtwoorden blokkeren. Gebruik dan een ander adres.', 'With a work or school account (Google Workspace) or Advanced Protection, Google may block app passwords. Use a different address then.'] }],
  [/^(outlook|hotmail|live|msn)\.(com|nl|be)$/, { name: 'Outlook.com', host: 'smtp-mail.outlook.com', port: 587, security: 'starttls', unsupported: true, note: UNSUPPORTED_MAIL }],
  [/^(icloud|me|mac)\.com$/, { name: 'iCloud Mail', host: 'smtp.mail.me.com', port: 587, security: 'starttls', passwordHelpUrl: 'https://account.apple.com/account/manage', note: ['Maak een app-specifiek wachtwoord.', 'Create an app-specific password.'] }],
  [/^(yahoo|ymail)\.(com|nl)$/, { name: 'Yahoo Mail', host: 'smtp.mail.yahoo.com', port: 465, security: 'tls', passwordHelpUrl: 'https://login.yahoo.com/account/security', note: ['Maak een app-wachtwoord.', 'Create an app password.'] }],
  [/^ziggo\.nl$/, { name: 'Ziggo', host: 'smtp.ziggo.nl', port: 587, security: 'starttls' }],
  [/^(kpnmail|planet|hetnet|home|kpnplanet|telfort)\.nl$/, { name: 'KPN Mail', host: 'smtp.kpnmail.nl', port: 587, security: 'starttls' }],
  [/^xs4all\.nl$/, { name: 'XS4ALL', host: 'smtp.xs4all.nl', port: 587, security: 'starttls' }],
  [/^(fastmail\.com|fastmail\.fm)$/, { name: 'Fastmail', host: 'smtp.fastmail.com', port: 465, security: 'tls', passwordHelpUrl: 'https://app.fastmail.com/settings/security/devicekeys', note: ['Maak een app-wachtwoord.', 'Create an app password.'] }],
  [/^(proton\.me|protonmail\.com|pm\.me)$/, { name: 'Proton Mail', host: '127.0.0.1', port: 1025, security: 'starttls', note: ['Hiervoor moet Proton Mail Bridge op deze computer draaien. Gebruik het wachtwoord van Bridge.', 'This needs Proton Mail Bridge running on this computer. Use the Bridge password.'] }],
];
export function mailPreset(email: string): MailPreset | null {
  const domain = email.trim().toLowerCase().split('@')[1] ?? '';
  for (const [re, p] of PRESETS) if (re.test(domain)) return p;
  return null;
}

// ---------- back-up, restore and exports ----------
/** Back-up files larger than this are refused before they are read (a real back-up is a few MB). */
export const MAX_BACKUP_BYTES = 20 * 1024 * 1024;
export const backupTooLarge = (size: number) => !(size >= 0) || size > MAX_BACKUP_BYTES;

/** A count from the restore summary: a number, or an object such as {journeys, claims}. */
function countOf(v: unknown, key: 'journeys' | 'claims'): number | null {
  if (typeof v === 'number' && Number.isFinite(v)) return v;
  if (v && typeof v === 'object' && typeof (v as any)[key] === 'number') return (v as any)[key];
  return null;
}

/** What a restore did, in plain words. `r` is import_backup's answer: {restored: {journeys, claims} (from the file),
 *  journeys, claims (totals after the restore), kept, replaced_claims, unassessed, auto_backup_path}. Older backends
 *  sent only some of these; unknown fields are left out. `recheck`: add how to re-assess the restored trips
 *  (when the backend says trips are unassessed, or does not say). */
export function restoreSummary(r: any, lang: Lang, opts: { recheck?: boolean; loggedIn?: boolean } = {}): string[] {
  const nl = lang === 'nl';
  const out: string[] = [];
  const num = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
  const journeys = num(r?.restored?.journeys) ?? num(r?.journeys) ?? (r?.replaced && typeof r.replaced === 'object' ? countOf(r.replaced, 'journeys') : null);
  const claims = num(r?.restored?.claims) ?? num(r?.claims);
  const kept = countOf(r?.kept, 'claims');
  if (journeys !== null) {
    const t = nl ? (journeys === 1 ? '1 reis' : `${journeys} reizen`) : journeys === 1 ? '1 trip' : `${journeys} trips`;
    const c = !claims ? '' : nl ? ` en ${claims === 1 ? '1 claim' : `${claims} claims`}` : ` and ${claims === 1 ? '1 claim' : `${claims} claims`}`;
    out.push(nl ? `${t}${c} uit de back-up teruggezet.` : `${t}${c} restored from the backup.`);
  }
  if (kept !== null && kept > 0) out.push(nl ? `${kept === 1 ? '1 claim die al bij NS was, is' : `${kept} claims die al bij NS waren, zijn`} bewaard.` : `${kept === 1 ? '1 claim that had already reached NS was' : `${kept} claims that had already reached NS were`} kept.`);
  const p = typeof r?.auto_backup_path === 'string' && r.auto_backup_path ? r.auto_backup_path : '';
  if (p) out.push(nl ? `Je vorige gegevens staan in ${p}.` : `Your previous data is in ${p}.`);
  const unassessed = num(r?.unassessed);
  if (opts.recheck && (unassessed === null ? (journeys ?? 0) > 0 : unassessed > 0)) out.push(opts.loggedIn
    ? (nl ? 'Klik op Controleer nu om je reizen opnieuw te beoordelen.' : 'Click Check now to re-assess your trips.')
    : (nl ? 'Log in en klik op Controleer nu om je reizen opnieuw te beoordelen.' : 'Log in and click Check now to re-assess your trips.'));
  return out;
}

/** Why automatic submission can't be turned on (empty: it can). `otherService` null/undefined = question not answered.
 *  `key` lets the UI add a button that goes to the right place. */
export type AutoBlocker = { key: 'checks' | 'bank' | 'service' | 'import'; text: string };
export function autoSubmitBlockers(g: { checksOn: boolean; bank: boolean; otherService: string | null | undefined }, lang: Lang): AutoBlocker[] {
  const nl = lang === 'nl';
  const out: AutoBlocker[] = [];
  if (!g.checksOn) out.push({ key: 'checks', text: nl ? 'Zet eerst Automatisch controleren aan.' : 'Turn on Check automatically first.' });
  if (!g.bank) out.push({ key: 'bank', text: nl ? 'Stel eerst je rekening in.' : 'Set up your bank account first.' });
  if (g.otherService == null) out.push({ key: 'service', text: nl ? 'Beantwoord eerst de vraag over claimdiensten.' : 'Answer the claim service question first.' });
  else if (g.otherService === 'yes') out.push({ key: 'import', text: nl ? 'Voeg eerst de reizen toe die je claimdienst al claimde, en klik dan op Import klaar.' : 'First add the trips your claim service already claimed, then click Import done.' });
  return out;
}

/** Why automatic submission is paused although it is on ('' when it isn't). `unresolved`: claims with an
 *  uncertain outcome (or still being submitted); `claimListUnavailable`: NS's claim list could not be read. */
export function autoPauseText(g: { unresolved: number; claimListUnavailable?: boolean }, lang: Lang): string {
  const nl = lang === 'nl';
  if (g.unresolved > 0) return nl
    ? `Gepauzeerd: ${g.unresolved === 1 ? '1 claim' : `${g.unresolved} claims`} met een onzekere uitkomst.`
    : `Paused: ${g.unresolved === 1 ? '1 claim' : `${g.unresolved} claims`} with an uncertain outcome.`;
  if (g.claimListUnavailable) return nl ? 'Gepauzeerd: Mijn teruggaven kon niet worden gelezen.' : 'Paused: Mijn teruggaven could not be read.';
  return '';
}

/** What is still missing before a claim can be submitted in the review dialog ('' when nothing is). */
export function submitHint(s: { reached: boolean | null; bank: boolean; resubNeeded: boolean; resubOk: boolean; nearNeeded: boolean; nearOk: boolean; declared: boolean }, lang: Lang): string {
  const nl = lang === 'nl';
  if (s.reached === null) return nl ? 'Beantwoord eerst de vraag over je bestemming.' : 'Answer the destination question first.';
  if (!s.reached) return '';
  if (!s.bank) return nl ? 'Stel eerst je rekening in.' : 'Set up your bank account first.';
  if (s.resubNeeded && !s.resubOk) return nl ? 'Vink aan dat je Mijn teruggaven hebt gecontroleerd.' : 'Tick that you checked Mijn teruggaven.';
  if (s.nearNeeded && !s.nearOk) return nl ? 'Vink aan dat het een andere reis is.' : 'Tick that it is a different trip.';
  if (!s.declared) return nl ? 'Vink de verklaring aan.' : 'Tick the declaration.';
  return '';
}

/** Result of an automatic check (demo simulation): submitted, plus what still waits. */
export function autoRunText(submitted: number, claimable: number, review: number, lang: Lang, demo = false, paused = ''): string {
  const nl = lang === 'nl';
  const n = (k: number, one: string, many: string) => (k === 1 ? `1 ${one}` : `${k} ${many}`);
  const parts = [nl ? `${n(submitted, 'claim', 'claims')} ingediend` : `${n(submitted, 'claim', 'claims')} submitted`];
  if (claimable) parts.push(nl ? `${claimable} te claimen` : `${claimable} claimable`);
  if (review) parts.push(nl ? `${review} te beoordelen` : `${review} to review`);
  // A pause reason says why nothing (more) was submitted.
  return `${nl ? 'Automatische controle' : 'Automatic check'}: ${parts.join(', ')}${demo ? ' (demo)' : ''}.${paused ? ` ${paused}` : ''}`;
}

/** Where back-ups and CSV files go and what a back-up holds (overview.export_dir / onedrive_avoided, both optional). */
export function exportInfo(o: { export_dir?: string | null; onedrive_avoided?: boolean | null; forget_removes_exports?: boolean | null } | null | undefined, lang: Lang): string[] {
  const nl = lang === 'nl';
  const dir = o?.export_dir || (nl ? 'Documenten\\OV Refund' : 'Documents\\OV Refund');
  const out = [nl ? `Back-ups en CSV-bestanden komen in ${dir}.` : `Backups and CSV files go to ${dir}.`];
  if (o?.onedrive_avoided) out.push(nl ? 'Je map Documenten wordt door OneDrive, Dropbox of Google Drive gesynchroniseerd. Daarom gebruikt OV Refund een map die alleen op deze computer staat.' : 'Your Documents folder is synced by OneDrive, Dropbox or Google Drive. That is why OV Refund uses a folder that stays on this computer.');
  out.push(nl ? 'Deze bestanden bevatten je volledige reishistorie. Een back-up bevat je reizen en claims, maar geen wachtwoorden, IBAN, kaartnummer of e-mailinstellingen.' : 'These files contain your full travel history. A backup contains your trips and claims, but no passwords, IBAN, card number or e-mail settings.');
  out.push(o?.forget_removes_exports
    ? (nl ? 'Alles vergeten wist ook deze bestanden. Kopieer een back-up eerst naar een andere plek als je hem wilt houden.' : 'Forget everything also deletes these files. Copy a backup elsewhere first if you want to keep it.')
    : (nl ? 'Deze bestanden blijven staan na Alles vergeten. Verwijder ze zelf als je ze niet meer nodig hebt.' : 'These files stay after Forget everything. Delete them yourself when you no longer need them.'));
  return out;
}

/** Message when a back-up file is refused for its size (before it is read). */
export const backupTooLargeText = (lang: Lang) => (lang === 'nl'
  ? 'Dit bestand is groter dan 20 MB en is geen back-up van OV Refund. Kies het .ovrbackup-bestand uit de map OV Refund.'
  : 'This file is larger than 20 MB and is not an OV Refund backup. Choose the .ovrbackup file from the OV Refund folder.');

/** Whether the SMTP server or username differs from what the stored app password was saved for. */
export function smtpLoginChanged(saved: { host?: string; username?: string } | null | undefined, next: { host: string; username: string }): boolean {
  if (!saved || (!saved.host && !saved.username)) return false;
  const norm = (s: string | undefined) => String(s ?? '').trim().toLowerCase();
  return norm(saved.host) !== norm(next.host) || norm(saved.username) !== norm(next.username);
}
export const smtpPasswordAgainText = (lang: Lang) => (lang === 'nl'
  ? 'Je hebt de mailserver of gebruikersnaam gewijzigd. Vul het app-wachtwoord opnieuw in.'
  : 'You changed the mail server or username. Enter the app password again.');

/** The refund in one short line for the review dialog: "€ 9,10 · 50% van € 18,20" (no nested brackets). */
export function amountDetail(cents: number | null | undefined, exact: boolean | null | undefined, basis: string | null | undefined, lang: Lang): string {
  if (cents == null) return '';
  const nl = lang === 'nl';
  const b = String(basis ?? '').trim().replace(/^NS-regel:\s*/, '');
  let m = /^50% van de ritprijs \((€ [\d.,]+)\)$/.exec(b);
  if (m) return `${euro(cents, lang)} · 50% ${nl ? 'van' : 'of'} ${euroIn(m[1], lang)}`;
  if (/^100% van de ritprijs/.test(b)) return `${euro(cents, lang)} · ${nl ? 'de hele ritprijs' : 'the full fare'}`;
  m = /^1\/(\d+) van (?:je maandbedrag|de maandprijs)/.exec(b);
  if (m) return `≈ ${euro(cents, lang)} · 1/${m[1]} ${nl ? 'van je maandbedrag' : 'of your monthly fee'}`;
  if (/^wat NS betaalde/.test(b)) return `≈ ${euro(cents, lang)} · ${nl ? 'geschat' : 'estimated'}`;
  return `${exact === false ? '≈ ' : ''}${euro(cents, lang)}`;
}

/** Account holder alphabet, matching the protected-storage boundary. */
export const validHolderName = (s: string): boolean => s.trim().length > 0 && [...s].length <= BANK_NAME_MAX && /\p{L}/u.test(s) && /^[\p{L} .'-]+$/u.test(s);
/** Display the delay against the regular timetable when adjusted service needs review. */
export function displayDelay(r: { delay_min: number | null; reasons: string[] }): number | null {
  const reason = r.reasons.find(s => s.startsWith('timetable was adjusted that day; against the regular timetable'));
  const match = reason?.match(/about (-?\d+) min later/);
  return match ? Number(match[1]) : r.delay_min;
}
/** Strict decimal entry; exponent notation and negative values are refused. */
export function decimalEntry(raw: string, max: number): number | null {
  if (!raw.trim()) return null;
  if (!/^\d+(?:[.,]\d{1,2})?$/.test(raw.trim())) return NaN;
  const n = Number(raw.trim().replace(',', '.'));
  return n <= max ? n : NaN;
}
export function validExternalTrip(date: string, origin: string, destination: string, now = new Date()): boolean {
  const today = localDay(now), min = new Date(today + 'T12:00:00Z'); min.setUTCMonth(min.getUTCMonth() - 18);
  const station = (s: string) => s.trim().length > 0 && s.length <= 60 && /\p{L}/u.test(s) && /^[\p{L} .()'-]+$/u.test(s);
  return /^\d{4}-\d{2}-\d{2}$/.test(date) && Number.isFinite(Date.parse(date)) && new Date(date).toISOString().slice(0, 10) === date && date >= min.toISOString().slice(0, 10) && date <= today && station(origin) && station(destination);
}
