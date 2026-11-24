import { kindOf, type ArrivaInfo, type CarrierOf } from './format';

/** Backend state of real Arriva claims (arriva_claim.rs `state`). */
export type ArrivaClaimPhase = 'preparing' | 'preview' | 'verifying' | 'submitted' | 'uncertain' | 'failed' | 'dry_run';
export type ArrivaRealState = {
  claims: Record<string, { phase: ArrivaClaimPhase; message: string; at: string }>;
  /** Setup still needed before a claim: 'contact' (name and e-mail for Arriva), 'bank'. */
  missing: string[];
  /** This build can send a claim (false in developer builds and dry runs: the final write is blocked). */
  submit_enabled: boolean;
};
export type ArrivaContact = { first_name: string; last_name: string; email: string; card_valid_until: string | null; expiry_card_last4?: string | null; expiry_card_key?: string | null; set: boolean };

/** Start of the core reason of an Arriva trip that meets Arriva's rules (eligibility.rs ARRIVA_ELIGIBLE_PREFIX). */
export const ARRIVA_ELIGIBLE_PREFIX = 'Arriva: estimated arrival delay ';
const OPEN = ['NEEDS_REVIEW', 'READY', 'FAILED'];

type RowLike = { claim_id: number | null; claim_status: string | null; confidence: string | null; reasons: string[]; expires_at?: string | null; journey: CarrierOf; arriva?: ArrivaInfo | null };

export const isArrivaTrip = (r: RowLike) => kindOf(r.journey) === 'arriva';

/** An Arriva trip that meets Arriva's rules (but OV Refund can't fill in Arriva's form for it): the
 *  user submits it at Arriva. The reason ("NL", "EN") or null. */
export const arrivaBlocker = (r: RowLike) => (isArrivaTrip(r) && arrivaEligible(r) ? r.arriva?.blocker ?? null : null);
const arrivaEligible = (r: RowLike) => (r.reasons[0] ?? '').startsWith(ARRIVA_ELIGIBLE_PREFIX) && / of the fare; claim until /.test(r.reasons[0] ?? '');

/** An Arriva trip that meets Arriva's rules and can be claimed now with the Claim button. */
export function arrivaClaimable(r: RowLike, now = new Date()): boolean {
  return isArrivaTrip(r) && !!r.claim_id && OPEN.includes(r.claim_status ?? '') && r.confidence === 'needs_review' && !r.arriva?.blocker
    && (!r.expires_at || Date.parse(r.expires_at) >= now.getTime()) && arrivaEligible(r);
}

/** The Claim of this claim is running in the app (attachments, form, verification). */
export function arrivaRunning(s: ArrivaRealState | null, claimId: number | null): boolean {
  const p = arrivaPhase(s, claimId);
  return p === 'preparing' || p === 'preview' || p === 'verifying';
}
export const arrivaPhase = (s: ArrivaRealState | null, claimId: number | null): ArrivaClaimPhase | undefined => (claimId != null ? s?.claims?.[String(claimId)]?.phase : undefined);

/** The last message of a Claim of this claim ("NL / EN"), if any. */
export const arrivaMessage = (s: ArrivaRealState | null, claimId: number | null): string =>
  (claimId != null ? s?.claims?.[String(claimId)]?.message : '') ?? '';

/** "DD-MM-YYYY" or empty; anything else is refused before saving. A date in the past is fine (the
 *  trip may have been made on a card that has expired since). */
export function validExpiry(v: string): boolean {
  return expiryProblem(v) === '';
}
/** Why an expiry date can't be saved: '' (fine or empty) or 'format'. */
export function expiryProblem(v: string): '' | 'format' {
  return parseExpiry(v) !== null || !v.trim() ? '' : 'format';
}
function parseExpiry(v: string): Date | null {
  const m = /^(\d{2})-(\d{2})-(\d{4})$/.exec(v.trim());
  if (!m) return null;
  const d = new Date(Number(m[3]), Number(m[2]) - 1, Number(m[1]));
  return d.getFullYear() === Number(m[3]) && d.getMonth() === Number(m[2]) - 1 && d.getDate() === Number(m[1]) ? d : null;
}
/** A non-blocking note: a changed (valid) expiry date lies before today. */
export function expiryInPastNote(v: string, stored: string | null | undefined, today: Date = new Date()): boolean {
  const d = parseExpiry(v);
  if (!d || v.trim() === (stored ?? '').trim()) return false;
  return d < new Date(today.getFullYear(), today.getMonth(), today.getDate());
}
