import { describe, expect, it, vi } from 'vitest';
import * as ts from 'typescript';
import main from './main.ts?raw';
import wizard from './wizard.ts?raw';
import i18n from './i18n.ts?raw';
import { readFileSync } from 'node:fs';
const css = readFileSync(new URL('./styles.css', import.meta.url), 'utf8');
import { category, displayDelay, euro, localDate, nearThresholdText, refundSentence } from './format';

// Run isolated production rendering/keyboard functions without loading the application or IPC.
function isolated(source: string, name: string, endMarker: string, deps: Record<string, unknown> = {}): any {
  const start = source.indexOf(`function ${name}(`), end = source.indexOf(endMarker, start);
  expect(start).toBeGreaterThanOrEqual(0); expect(end).toBeGreaterThan(start);
  const code = ts.transpileModule(source.slice(start, end), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText;
  return new Function(...Object.keys(deps), `${code}; return ${name};`)(...Object.values(deps));
}
const L = (nl: string, _en: string) => nl;
const deps = { category, displayDelay, L, euro, localDate, nearThresholdText, refundSentence, getLang: () => 'nl', help: () => '', h: (s: string) => s, note: (s: string) => s };
const row = () => ({ journey: { check_out: '2026-10-01T12:00:00Z' }, delay_min: 29, reasons: [], evidence_status: 'found', claim_status: 'NEEDS_REVIEW', paid_cents: null, compensation_cents: null, compensation_basis: null });

describe('offline view regressions', () => {
  it('keeps adjusted, near-threshold, missing-checkout and external amounts consistent', () => {
    const cutoffText = isolated(main, 'cutoffText', '\nconst routeOf', deps);
    const amount = isolated(main, 'amountBlock', '/** Short notes', { ...deps, cutoffText });
    const delay = isolated(main, 'delayCell', '\nfunction cutoffText', deps);
    const adjusted = { ...row(), delay_min: 0, reasons: ['timetable was adjusted that day; against the regular timetable you arrived about 42 min later — NS may compensate this, check yourself'], compensation_cents: 615 };
    expect(delay(adjusted)).toContain('+42 min');
    expect(amount(adjusted)).toContain('42 minuten');
    const near = amount(row());
    expect(near).toContain('vlak bij de grens van 30');
    expect(near).not.toContain('Onder 30 minuten');
    // OV Refund can't claim it: no amount "to follow"
    expect(near).toContain('Net onder 30 minuten');
    expect(near).not.toContain('Bedrag volgt');
    expect(amount({ ...row(), delay_min: 0 })).toContain('Geen vertraging gevonden');
    expect(amount({ ...row(), journey: { check_out: null }, delay_min: null, evidence_status: 'pending' })).toBe('');
    expect(amount({ ...row(), claim_status: 'EXTERNALLY_CLAIMED', compensation_cents: 910 })).toContain('Elders geclaimd');
    expect(amount({ ...row(), claim_status: 'EXTERNALLY_CLAIMED', compensation_cents: 910 })).not.toContain('9,10');
  });

  it('closes More with Escape from inside the menu and returns focus to its summary', () => {
    const focus = vi.fn(), preventDefault = vi.fn(), stopPropagation = vi.fn();
    const menu = { open: true, querySelector: () => ({ focus }) };
    const onKey = isolated(main, 'onKeydown', '\napp.addEventListener');
    onKey({ key: 'Escape', target: { closest: () => menu }, preventDefault, stopPropagation });
    expect(menu.open).toBe(false); expect(focus).toHaveBeenCalledOnce();
    expect(preventDefault).toHaveBeenCalledOnce(); expect(stopPropagation).toHaveBeenCalledOnce();
  });

  it('keeps modal headers in place, body scroll contained and hidden toasts absent', () => {
    expect(css).toMatch(/dialog:modal\s*\{[^}]*position:\s*fixed/);
    expect(css).toMatch(/html:has\(dialog:modal\)\s*\{[^}]*overflow:\s*hidden/);
    expect(css).toMatch(/dialog \.bd\s*\{[^}]*overflow:\s*auto/);
    expect(css).toMatch(/dialog \.hd\s*\{[^}]*flex:\s*none/);
    expect(css).toMatch(/\.toast:not\(\.show\)\s*\{[^}]*display:\s*none/);
    expect(css).toMatch(/\.ledger thead\s*\{[^}]*background:\s*var\(--panel\)/);
    expect(css).toMatch(/\.qpop\s*\{[^}]*width:\s*min\(280px/);
    expect(main.match(/id="reachYes"[^>]*>/)?.[0]).not.toContain('checked');
    expect(main.match(/id="bugDiag"[^>]*>/)?.[0]).not.toContain('checked');
  });

  it('sets the document language as well as the saved preference', () => {
    const document = { documentElement: { lang: 'nl' } }, localStorage = { setItem: vi.fn() };
    const start = i18n.indexOf('export function setLang('), end = i18n.indexOf('/** Inline bilingual', start);
    const code = ts.transpileModule(i18n.slice(start, end).replaceAll('export ', ''), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText;
    const set = new Function('document', 'localStorage', 'invoke', `let lang='nl'; ${code}; return setLang;`)(document, localStorage, () => Promise.resolve());
    set('en'); expect(document.documentElement.lang).toBe('en');
    set('nl'); expect(document.documentElement.lang).toBe('nl');
  });

  it('returns focus after either accepting or escaping the automatic submission warning', async () => {
    vi.useFakeTimers();
    try {
      for (const accept of [false, true]) {
        const focus = vi.fn(), listeners: Record<string, () => void> = {};
        const yes: any = { disabled: true, focus: vi.fn() }, no: any = { focus: vi.fn() };
        const check = { checked: accept, addEventListener: vi.fn() };
        const dialog = {
          className: '', innerHTML: '', setAttribute: vi.fn(), remove: vi.fn(), showModal: vi.fn(),
          querySelector: (s: string) => s === '#awOk' ? check : s === '[data-x=yes]' ? yes : no,
          querySelectorAll: () => [no], addEventListener: (name: string, fn: () => void) => { listeners[name] = fn; },
          close: () => listeners.close(),
        };
        const doc = { activeElement: { isConnected: true, focus }, createElement: () => dialog, body: { appendChild: vi.fn() } };
        const confirm = isolated(wizard, 'confirmAutoSubmit', '\nfunction draw', { document: doc, L, help: () => '', dialogHead: () => '', ICON: { warn: '' } });
        const answer = confirm(false);
        if (accept) yes.onclick(); else dialog.close();
        expect(await answer).toBe(accept);
        vi.runAllTimers(); expect(focus).toHaveBeenCalledOnce();
      }
    } finally { vi.useRealTimers(); }
  });
});

describe('messages after a busy action', () => {
  it('says when a trip claimed elsewhere is marked and why Check now left claims', () => {
    const added = isolated(main, 'externalAddedText', '\n/** Show one trip', deps);
    expect(added(2, true)).toBe('Toegevoegd. 2 reizen worden bij de volgende controle gemarkeerd als elders geclaimd.');
    expect(added(1, true)).toContain('1 reis wordt bij de volgende controle');
    // Busy, but no open trip matches this entry: the same text as when nothing was busy.
    expect(added(0, true)).toBe(added(0, false));
    const marked = ['Leiden Centraal', 'Utrecht Centraal', 'Zwolle'].map(from => ({ date: '2026-11-01', from, to: 'Amsterdam Centraal' }));
    const confirmation = added(3, false, marked);
    expect(confirmation).toContain('Leiden Centraal → Amsterdam Centraal');
    expect(confirmation).toContain('Utrecht Centraal → Amsterdam Centraal');
    expect(confirmation).not.toContain('Zwolle');
    expect(confirmation).toContain('+1');
    const hint = isolated(main, 'checkNowHint', '\n/** Why an automatic check', deps);
    expect(hint(0, 0)).toBe('');
    expect(hint(0, 2)).toContain('binnen 14 dagen');
    expect(hint(1, 2)).toContain('NS toont een claim rond die datum');
    expect(hint(2, 0)).toMatch(/^2 claims zijn niet automatisch ingediend/);
  });
});
