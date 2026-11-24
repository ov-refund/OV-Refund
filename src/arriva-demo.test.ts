import { describe, expect, it } from 'vitest';
import { arrivaClaimState, arrivaOutcomeText, arrivaStaysClaimable, attachmentCaption, canClaimArrivaDemo, canConfirmArrivaAttachments, fileSize, logLines, operatorLabel, statusWord, stepName, type ArrivaDemoState } from './arriva-demo';

describe('Arriva claim availability', () => {
  const state = (phase: string, window_open = true, available = true): ArrivaDemoState => ({
    phase, window_open, available, demo_only: true, steps: [], attachments: [], logs: [],
    trip: { from: 'Venlo', to: 'Nijmegen', train_number: '732284', date: '2026-10-29', delay_minutes: 24, fare_cents: 1240, check_in: '', check_out: '' },
  });
  it('offers the prepared claim while verification awaits the user', () => {
    for (const phase of ['ready', 'captcha_attention', 'captcha_needed']) expect(canClaimArrivaDemo(state(phase))).toBe(true);
  });
  it('does not offer unprepared, closed, failed or already tested claims', () => {
    for (const phase of ['idle', 'preparing', 'error', 'window_closed', 'submission_blocked']) expect(canClaimArrivaDemo(state(phase))).toBe(false);
    expect(canClaimArrivaDemo(state('ready', false))).toBe(false);
    expect(canClaimArrivaDemo(state('ready', true, false))).toBe(false);
    expect(canClaimArrivaDemo(null)).toBe(false);
  });
});

describe('Arriva claim stays claimable until the outcome is final', () => {
  const st = (phase: string, captcha: ArrivaDemoState['captcha'] = null): ArrivaDemoState => ({
    phase, captcha, window_open: true, available: true, demo_only: true, steps: [], attachments: [], logs: [],
    trip: { from: 'Venlo', to: 'Nijmegen', train_number: '732284', date: '2026-10-29', delay_minutes: 24, fare_cents: 1240, check_in: '', check_out: '' },
  });
  it('is in progress right after Claimen and while Arriva shows a puzzle', () => {
    expect(arrivaClaimState(st('ready'), true)).toBe('in_progress');
    expect(arrivaClaimState(st('verifying'))).toBe('in_progress');
    expect(arrivaClaimState(st('captcha_attention', 'puzzle'))).toBe('in_progress');
    expect(arrivaStaysClaimable('in_progress')).toBe(true);
  });
  it('stays claimable with an error when it fails, leaves only when the final write was reached', () => {
    for (const s of [st('error'), st('window_closed'), st('captcha_attention', 'failed'), st('captcha_attention', 'challenge_closed')]) expect(arrivaClaimState(s)).toBe('failed');
    expect(arrivaStaysClaimable('failed')).toBe(true);
    expect(arrivaClaimState(st('submission_blocked'), true)).toBe('done');
    expect(arrivaStaysClaimable('done')).toBe(false);
    expect(arrivaClaimState(st('ready'))).toBe('ready');
    expect(arrivaClaimState(st('preparing'))).toBe('waiting');
  });
});

describe('Arriva outcome and attachments', () => {
  const base = (extra: Partial<ArrivaDemoState>): ArrivaDemoState => ({
    phase: 'captcha_attention', window_open: true, available: true, demo_only: true, steps: [], attachments: [], logs: [],
    trip: { from: 'Venlo', to: 'Nijmegen', train_number: '732284', date: '2026-10-29', delay_minutes: 24, fare_cents: 1240, check_in: '', check_out: '' }, ...extra,
  });
  it('names the fields that could not be verified instead of showing the form', () => {
    const t = arrivaOutcomeText(base({ unverified: ['iban', 'submit_button'], captcha: null }));
    expect(t).toMatch(/IBAN/);
    expect(t).toMatch(/Indienen|Submit button/);
    expect(t).not.toMatch(/submit_button/);
  });
  it('says plainly when no puzzle was needed', () => {
    expect(arrivaOutcomeText(base({ captcha: 'no_puzzle' }))).toMatch(/geen puzzel nodig|no puzzle needed/i);
    expect(arrivaOutcomeText(base({}))).toBe('');
  });
  it('captions the travel and delay proof images', () => {
    const a = { name: 'x / x', path: 'p', kind: 'png' };
    expect(attachmentCaption(a, 0)).toMatch(/fictie|fictional/);
    expect(attachmentCaption(a, 1)).toMatch(/Rijden de Treinen.*CC BY 4\.0/);
  });
  it('requires both decoded claim attachments before confirmation', () => {
    expect(canConfirmArrivaAttachments([], 0)).toBe(false);
    expect(canConfirmArrivaAttachments(['travel'], 1)).toBe(false);
    expect(canConfirmArrivaAttachments(['travel'], 2)).toBe(false);
    expect(canConfirmArrivaAttachments(['', 'delay'], 2)).toBe(false);
    expect(canConfirmArrivaAttachments(['travel', 'delay'], 2)).toBe(true);
  });
  it('formats attachment sizes', () => {
    expect(fileSize(512)).toBe('512 B');
    expect(fileSize(2048)).toMatch(/^2 kB$/);
    expect(fileSize(undefined)).toBe('—');
  });
});

describe('operatorLabel', () => {
  it('recognizes NS from its carrier code and leg company', () => {
    expect(operatorLabel('4', null)).toEqual({ label: 'NS', kind: 'ns' });
    expect(operatorLabel(null, 'Nederlandse Spoorwegen')).toEqual({ label: 'NS', kind: 'ns' });
  });
  it('recognizes Arriva numeric and textual records', () => {
    expect(operatorLabel('12', null)).toEqual({ label: 'Arriva', kind: 'arriva' });
    expect(operatorLabel('ARR', null)).toEqual({ label: 'Arriva', kind: 'arriva' });
    expect(operatorLabel(null, 'Arriva Personenvervoer')).toEqual({ label: 'Arriva', kind: 'arriva' });
  });
  it('keeps unknown operators neutral and preserves their recorded name', () => {
    expect(operatorLabel('99', 'Keolis')).toEqual({ label: 'Keolis', kind: 'other' });
    expect(operatorLabel(null, null).kind).toBe('other');
    expect(operatorLabel(null, 'unrecognized operator')).toEqual({ label: 'unrecognized operator', kind: 'other' });
    expect(operatorLabel('99', 'Nederlandse Spoorwegen')).toEqual({ label: 'Nederlandse Spoorwegen', kind: 'other' });
  });
  it('matches Arriva only: another name decides, also with an Arriva code', () => {
    expect(operatorLabel('12', 'RRReis')).toEqual({ label: 'RRReis', kind: 'other' });
    expect(operatorLabel(null, 'RRReis')).toEqual({ label: 'RRReis', kind: 'other' });
    expect(operatorLabel('ARR', 'Keolis').kind).toBe('other');
    expect(operatorLabel(null, 'Arrivals').kind).toBe('other');
    expect(operatorLabel('12', 'Arriva').kind).toBe('arriva');
  });
});

describe('demo inspection log', () => {
  it('shows steps and statuses in words, the log in time order with repeats collapsed', () => {
    expect(stepName('guard', 'x')).toBe('Block sending');
    expect(stepName('unknown', 'Eigen stap')).toBe('Eigen stap');
    expect(statusWord('attention')).toBe('Waiting for you');
    expect(statusWord('done')).toBe('Done');
    const lines = logLines([
      { at: '2026-11-23T10:00:05Z', step: 'captcha', message: 'Puzzel zichtbaar / Puzzle shown' },
      { at: '2026-11-23T10:00:00Z', step: 'guard', message: 'Bewaking aan / Guard on' },
      { at: '2026-11-23T10:00:06Z', step: 'captcha', message: 'Puzzel zichtbaar / Puzzle shown' },
      { at: '2026-11-23T10:00:07Z', step: 'captcha', message: 'Puzzel zichtbaar / Puzzle shown' },
    ]);
    expect(lines.map(l => l.step)).toEqual(['Block sending', 'CAPTCHA and check']);
    expect(lines[1]).toMatchObject({ message: 'Puzzle shown', count: 3 });
    expect(lines[0].time).toMatch(/^\d\d:\d\d:\d\d$/);
  });
});
