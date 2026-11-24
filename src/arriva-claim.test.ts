import { describe, expect, it } from 'vitest';
import { arrivaClaimable, arrivaRunning, expiryInPastNote, isArrivaTrip, validExpiry, type ArrivaRealState } from './arriva-claim';

const row = (over: Partial<Parameters<typeof arrivaClaimable>[0]> = {}) => ({
  claim_id: 7, claim_status: 'NEEDS_REVIEW', confidence: 'needs_review',
  reasons: ['Arriva: estimated arrival delay 24 min in Limburg; 50% of the fare; claim until 29-01-2027', 'Arriva claims are manual: press Claim yourself'],
  journey: { carrier_code: '12', carrier_name: 'Arriva' }, ...over,
});

describe('real Arriva claims', () => {
  it('recognises Arriva trips by code or name', () => {
    expect(isArrivaTrip(row())).toBe(true);
    expect(isArrivaTrip(row({ journey: { carrier_code: null, carrier_name: 'Arriva' } }))).toBe(true);
    expect(isArrivaTrip(row({ journey: { carrier_code: '4', carrier_name: 'NS' } }))).toBe(false);
  });
  it('offers Claim only for an open claim that meets Arriva’s rules', () => {
    expect(arrivaClaimable(row())).toBe(true);
    expect(arrivaClaimable(row({ claim_status: 'FAILED' }))).toBe(true);
    expect(arrivaClaimable(row({ claim_status: 'SUBMITTING' }))).toBe(false);
    expect(arrivaClaimable(row({ claim_status: 'INDETERMINATE' }))).toBe(false);
    expect(arrivaClaimable(row({ claim_id: null }))).toBe(false);
    expect(arrivaClaimable(row({ reasons: ['historical timetable for this month not yet published; will re-check automatically'] }))).toBe(false);
    expect(arrivaClaimable(row({ confidence: 'supported_for_auto' }))).toBe(false);
    expect(arrivaClaimable(row({ journey: { carrier_code: '4', carrier_name: 'NS' } }))).toBe(false);
  });
  it('stops offering Claim after the deadline even if the cached assessment is eligible', () => {
    const expires_at = '2027-01-29T22:59:59Z';
    expect(arrivaClaimable(row({ expires_at }), new Date(expires_at))).toBe(true);
    expect(arrivaClaimable(row({ expires_at }), new Date('2027-01-29T23:00:00Z'))).toBe(false);
  });
  it('knows when a Claim is running', () => {
    const s: ArrivaRealState = { claims: { '7': { phase: 'verifying', message: '', at: '' } }, missing: [], submit_enabled: true };
    expect(arrivaRunning(s, 7)).toBe(true);
    expect(arrivaRunning(s, 8)).toBe(false);
    expect(arrivaRunning({ ...s, claims: { '7': { phase: 'failed', message: '', at: '' } } }, 7)).toBe(false);
    expect(arrivaRunning(null, 7)).toBe(false);
  });
  it('checks the card expiry date', () => {
    expect(validExpiry('')).toBe(true);
    expect(validExpiry('31-12-2030')).toBe(true);
    expect(validExpiry('31-02-2030')).toBe(false);
    expect(validExpiry('2030-12-31')).toBe(false);
    // A past date is accepted (a trip on a card that has expired since); only a note when it changed.
    expect(validExpiry('31-01-2020')).toBe(true);
    const today = new Date(2026, 10, 23);
    expect(expiryInPastNote('31-01-2020', null, today)).toBe(true);
    expect(expiryInPastNote('31-01-2020', '31-01-2020', today)).toBe(false);
    expect(expiryInPastNote('23-11-2026', null, today)).toBe(false);
    expect(expiryInPastNote('31-12-2030', null, today)).toBe(false);
    expect(expiryInPastNote('31-02-2020', null, today)).toBe(false);
    expect(expiryInPastNote('', '31-01-2020', today)).toBe(false);
  });
});
