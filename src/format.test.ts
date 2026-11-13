import { describe, expect, it } from 'vitest';
import { localDate, localDateTime, localDay, daysUntil, escapeHtml, localTime, minutes, statusTone } from './format';

describe('format', () => {
  it('shows Amsterdam local time across DST end', () => {
    expect(localTime('2026-10-25T00:30:00Z')).toBe('02:30'); // CEST
    expect(localTime('2026-10-25T19:31:00Z')).toBe('20:31'); // CET after DST end
  });
  it('formats minute deltas', () => {
    expect(minutes(33)).toBe('+33 min');
    expect(minutes(0)).toBe('0 min');
    expect(minutes(null)).toBe('–');
  });
  it('escapes html', () => {
    expect(escapeHtml(`<img src=x onerror="a">'`)).toBe('&lt;img src=x onerror=&quot;a&quot;&gt;&#39;');
  });
  it('computes days until expiry', () => {
    expect(daysUntil('2026-11-16T22:59:59Z', new Date('2026-11-12T12:00:00Z'))).toBe(4);
  });
  it('handles malformed dates and excludes future trips from recent periods', () => {
    expect(localDay(new Date('2026-11-12T23:30:00Z'))).toBe('2026-11-13');
    expect(localDay(new Date('2026-07-12T22:30:00Z'))).toBe('2026-07-13');
    expect(localDate('broken')).toBe('–');
    expect(localDateTime('broken')).toBe('–');
    expect(localTime('broken')).toBe('–');
    expect(daysUntil('broken')).toBeNull();
    expect(inPeriod('2026-10-13T12:00:00Z', 'month', new Date('2026-11-13T12:00:00Z'))).toBe(false);
    expect(inPeriod('2026-11-14T12:00:00Z', 'week', new Date('2026-11-13T12:00:00Z'))).toBe(false);
  });
  it('maps claim states to tones', () => {
    expect(statusTone('INDETERMINATE')).toBe('bad');
    expect(statusTone('READY')).toBe('good');
  });
});

import { category, euro, inPeriod } from './format';

describe('ledger helpers', () => {
  const base = { claim_status: null, expires_at: '2027-01-23T22:59:59Z', evidence_status: 'found', delay_min: 0, proxy_min: 1 };
  const now = new Date('2026-11-12T12:00:00Z');
  it('formats euros in Dutch', () => {
    expect(euro(410).replace(/\s/g, ' ')).toBe('€ 4,10');
    expect(euro(635, 'en')).toBe('€6.35');
    expect(euro(123450, 'en')).toBe('€1,234.50');
    expect(euro(null)).toBe('–');
  });
  it('categorises rows', () => {
    expect(category({ ...base, claim_status: 'READY' }, now)).toBe('claimable');
    expect(category({ ...base, claim_status: 'SUBMITTED' }, now)).toBe('claimed');
    expect(category({ ...base, claim_status: 'EXTERNALLY_CLAIMED' }, now)).toBe('claimed');
    expect(category({ ...base, claim_status: 'NEEDS_REVIEW', expires_at: '2026-11-01T00:00:00Z' }, now)).toBe('expired');
    expect(category({ ...base, delay_min: 45, expires_at: '2026-11-01T00:00:00Z' }, now)).toBe('expired');
    expect(category({ ...base, evidence_status: 'pending', delay_min: null }, now)).toBe('pending');
    // another operator (e.g. Arriva): not waiting for train times and not claimable via NS
    expect(category({ ...base, evidence_status: 'other_operator', delay_min: null }, now)).toBe('none');
    expect(category({ ...base, evidence_status: 'other_operator', claim_status: 'CANDIDATE' }, now)).toBe('none');
    expect(category(base, now)).toBe('none');
  });
  it('filters periods', () => {
    expect(inPeriod('2026-11-08T10:00:00Z', 'week', now)).toBe(true);
    expect(inPeriod('2026-10-17T10:00:00Z', 'week', now)).toBe(false);
    expect(inPeriod('2026-10-17T10:00:00Z', 'month', now)).toBe(true);
    expect(inPeriod('2025-01-01T10:00:00Z', 'year', now)).toBe(false);
  });
});

import { checkIban, formatIbanInput } from './format';

describe('IBAN input', () => {
  it('formats as typed', () => {
    expect(formatIbanInput('nl91abna0417164300')).toBe('NL91 ABNA 0417 1643 00');
    // An extra character is never cut off silently: it stays, and the check says it is too long.
    expect(formatIbanInput('NL91 ABNA 0417 1643 00 99')).toBe('NL91 ABNA 0417 1643 0099');
    expect(checkIban('NL91 ABNA 0417 1643 009').state).toBe('invalid');
    expect(formatIbanInput('nl91-abna')).toBe('NL91 ABNA');
  });
  it('validates and names the bank', () => {
    expect(checkIban('NL91 ABNA 0417 1643 00').state).toBe('valid');
    expect(checkIban('NL91 ABNA 0417 1643 00').bank).toBe('ABN AMRO');
    expect(checkIban('NL91 ABNA 0417 1643 01').state).toBe('invalid');
    expect(checkIban('NL91 ABNA 04').state).toBe('typing');
    expect(checkIban('NL91 1234 0417 1643 00').state).toBe('invalid');
  });
});

import { reasonText } from './format';

describe('reason translation', () => {
  it('translates policy reasons to Dutch', () => {
    expect(reasonText('estimated arrival delay 30 min (30 t/m 59 min)', 'nl')).toBe('Geschatte aankomstvertraging 30 min (30 t/m 59 min)');
    expect(reasonText('estimated delay 30 min is within ±2 min of a threshold', 'nl')).toContain('vlak bij de grens van 30 minuten');
    expect(reasonText('compensation amount unknown (fare not in travel history)', 'nl')).toBe('Vergoeding onbekend (ritprijs staat niet in de reishistorie)');
    expect(reasonText('arrived on time or with minor delay', 'en')).toBe('Arrived on time or with minor delay');
  });
});

import { mailPreset } from './format';

describe('mail presets', () => {
  it('recognises providers from the address', () => {
    expect(mailPreset('test-me@gmail.com')?.host).toBe('smtp.gmail.com');
    expect(mailPreset('test-Me@Ziggo.nl')?.host).toBe('smtp.ziggo.nl');
    expect(mailPreset('test-x@hotmail.nl')?.name).toBe('Outlook.com');
    expect(mailPreset('test-a@planet.nl')?.host).toBe('smtp.kpnmail.nl');
    expect(mailPreset('test-someone@university.example')).toBeNull();
  });
});

import { applyIbanFormat, isEmail, IBAN_MAXLENGTH } from './format';

describe('input helpers', () => {
  it('validates e-mail addresses', () => {
    expect(isEmail('naam@example.com')).toBe(true);
    expect(isEmail(' a.b@sub.example.com ')).toBe(true);
    expect(isEmail('naamgmail.com')).toBe(false);
    expect(isEmail('naam@gmail')).toBe(false);
    expect(isEmail('na am@example.com')).toBe(false);
  });
  it('fits the longest IBAN with spaces', () => {
    expect(formatIbanInput('MT84MALT011000012345MTLCAST001S').length).toBeLessThanOrEqual(IBAN_MAXLENGTH);
    expect(formatIbanInput('X'.repeat(40)).length).toBe(IBAN_MAXLENGTH);
  });
  it('keeps the caret when formatting mid-edit', () => {
    // typing "X" after "NL91 AB" in an already grouped value
    const el = { value: 'NL91 ABXNA 0417 1643 00', selectionStart: 8, setSelectionRange(a: number) { this.selectionStart = a; } };
    applyIbanFormat(el);
    expect(el.value).toBe('NL91 ABXN A041 7164 300'); // nothing is cut off: the check shows it is too long
    expect(el.selectionStart).toBe(8);
  });
});

import { basisText, historyDetail, isUnsupportedMail, nextStep, redactText, subscriptionExplain, subscriptionLine, type Subscription } from './format';

describe('subscription', () => {
  const wv = { code: 'WV', nl: 'Weekend Vrij', en: 'Weekend Vrij', kind: 'free' as const, fractions: [12, 6] as [number, number], verified: true };
  const dv = { code: 'DV', nl: 'Dal Voordeel', en: 'Off-Peak Discount', kind: 'discount' as const, fractions: null, verified: true };
  it('describes the current contract', () => {
    expect(subscriptionLine({ parts: [dv, wv], fee_cents: 4100, class: 1 }, 'nl')).toBe('Dal Voordeel + Weekend Vrij · € 41,00/mnd (volgens NS) · 1e klas');
    expect(subscriptionLine({ parts: [wv], fee_cents: null, class: 2 }, 'en')).toBe('Weekend Vrij · monthly fee not known yet');
    expect(subscriptionLine({ parts: [dv], fee_cents: null, class: null }, 'nl')).toBe('Dal Voordeel');
    expect(subscriptionLine(null, 'nl')).toBe('');
  });
  it('explains what it means for refunds', () => {
    const s: Subscription = { current: { parts: [dv, wv], fee_cents: 4100, class: 2 }, previous: [{ parts: [dv], fee_cents: 650, class: 2, until: '2026-08-31T12:00:00Z' }], none: false };
    const nl = subscriptionExplain(s, 'nl');
    expect(nl[0]).toBe('Gratis reizen met Weekend Vrij: bij 30–59 min krijg je 1/12, bij 60+ min 1/6 van je maandbedrag.');
    expect(nl[1]).toContain('Betaalde reizen: de helft');
    expect(nl[2]).toBe('Tot 31 aug 2026: Dal Voordeel · € 6,50/mnd');
    expect(subscriptionExplain({ current: null, none: true }, 'nl')).toEqual(['Geen abonnement: bij 30–59 min vertraging krijg je de helft van de ritprijs terug, bij 60 min of meer de hele ritprijs.']);
    expect(subscriptionExplain({ current: { parts: [{ code: 'X9', nl: '', en: '', kind: 'unknown', fractions: null }], fee_cents: null, class: null } }, 'en')[0]).toBe('Subscription not recognised (code X9). Amounts are shown as unknown.');
    expect(subscriptionExplain(null, 'nl')).toEqual([]);
  });
});

describe('next step banner', () => {
  const base = { auth: 'ok', claimList: 'ok', claimable: 0, claimableCents: 0, claimableUnknown: 0, review: 0, hasRows: true };
  it('uses proper plurals and amounts', () => {
    expect(nextStep({ ...base, claimable: 2, claimableCents: 830 }, 'nl')?.text).toBe('2 reizen te claimen (≈ € 8,30)');
    expect(nextStep({ ...base, claimable: 2, claimableCents: 830, review: 1 }, 'en')?.text).toBe('2 trips to claim (≈ €8.30), 1 to review');
    expect(nextStep({ ...base, claimable: 1, claimableCents: 0, claimableUnknown: 1 }, 'en')?.text).toBe('1 trip to claim');
    expect(nextStep({ ...base, review: 1 }, 'nl')?.text).toBe('1 reis om te beoordelen');
    expect(nextStep({ ...base, review: 1 }, 'nl')?.help).toContain('beslis zelf');
    expect(nextStep(base, 'nl')?.text).toContain('Niets te doen');
  });
  it('puts login problems and an unreadable claim list first', () => {
    expect(nextStep({ ...base, auth: 'account_mismatch', claimable: 3 }, 'nl')?.action).toBe('login');
    expect(nextStep({ ...base, auth: 'autologin_failed' }, 'en')?.action).toBe('autologin');
    expect(nextStep({ ...base, claimList: 'unavailable', claimable: 2 }, 'nl')?.text).toBe('Mijn teruggaven kon niet worden gelezen.');
    expect(nextStep({ ...base, claimList: 'unavailable', claimable: 2 }, 'nl')?.help).toContain('Zelf indienen kan wel');
    expect(nextStep({ ...base, auth: 'not_connected', hasRows: false }, 'nl')).toBeNull();
    expect(nextStep({ ...base, auth: 'blocked: verificatiecode' }, 'en')?.text).toContain('extra check');
    expect(nextStep({ ...base, auth: 'blocked: NS-website gewijzigd / NS website changed' }, 'nl')?.action).toBeUndefined();
    expect(nextStep({ ...base, auth: 'blocked' }, 'nl')?.tone).toBe('bad');
  });
});

describe('texts from the core', () => {
  it('translates history notes', () => {
    expect(historyDetail('Niet gelukt / Failed', 'en')).toBe('Failed');
    expect(historyDetail('no longer claimable: arrived on time or with minor delay; station not in timetable data', 'nl')).toBe('Niet meer claimbaar: Op tijd of met kleine vertraging aangekomen; Station niet gevonden in de dienstregelingsgegevens');
    expect(historyDetail('waiting for timetable data', 'en')).toBe('Awaiting timetable');
    expect(historyDetail('refund below €2.30: NS does not pay this out', 'nl')).toBe('Vergoeding lager dan € 2,30: NS betaalt dit niet uit');
  });
  it('renders the refund basis', () => {
    expect(basisText('NS-regel: 1/12 van je maandbedrag Weekend Vrij (€ 41.00, volgens NS)', 'nl').replace(/ /g, ' ')).toBe('1/12 van je maandbedrag Weekend Vrij (€ 41,00, volgens NS)');
    // The money format of the language, as euro() shows it.
    expect(basisText('NS-regel: 1/12 van je maandbedrag Weekend Vrij (€ 41.00, volgens NS)', 'en')).toBe('1/12 of your monthly fee for Weekend Vrij (€41.00, according to NS)');
    expect(basisText('50% van de ritprijs (€ 8.20)', 'en')).toBe(`half the fare (${euro(820, 'en')})`);
    expect(basisText(null, 'nl')).toBe('');
  });
});

describe('mail and bug-report helpers', () => {
  it('flags Microsoft consumer mail as unsupported', () => {
    expect(isUnsupportedMail('test-a@hotmail.com')).toBe(true);
    expect(isUnsupportedMail('test-a@Outlook.be')).toBe(true);
    expect(isUnsupportedMail('test-a@gmail.com')).toBe(false);
  });
  it('masks numbers that could identify someone', () => {
    expect(redactText('mijn IBAN NL91 ABNA 0417 1643 00 klopt niet')).toBe('mijn IBAN [IBAN] klopt niet');
    const secret = redactText('{"password":"four word secret","access_token":"abcdef"} mail a@example.org Bearer token123');
    for (const value of ['four word secret', 'abcdef', 'a@example.org', 'token123']) expect(secret).not.toContain(value);
    expect(redactText('kaart 3528 0000 0000 1234')).toBe('kaart [nummer]');
    expect(redactText('trein om 18:12, 42 min te laat')).toBe('trein om 18:12, 42 min te laat');
  });

});

import { backupTooLarge, backupTooLargeText, exportInfo, MAX_BACKUP_BYTES, restoreSummary, smtpLoginChanged, unsupportedMailText } from './format';

describe('English reasons', () => {
  it('has friendly English for core reasons', () => {
    expect(reasonText('journey longer than 6 h; check data', 'en')).toBe('Trip longer than 6 hours. Check the details');
    expect(reasonText('no planned direct or 1-transfer route found in archive', 'en')).toBe('No planned direct route or route with one change found');
    expect(reasonText('no realised route consistent with check-in/check-out', 'en')).toBe('No train that ran matches your check-in and check-out times');
    expect(reasonText('claim window (3 months) has passed — missed claim', 'en')).toBe('The 3-month claim window has passed. This claim was missed');
    expect(reasonText('check-out 12 min after the matched arrival; train identification uncertain', 'en')).toBe('You checked out 12 min after the arrival found. It is not certain which train you took');
    expect(reasonText("no check-out recorded (use NS 'vergeten uit te checken' instead)", 'en')).toBe('No check-out recorded (use NS’s ‘Forgot to check out’ instead)');
    expect(reasonText('estimated arrival delay 42 min (30 t/m 59 min)', 'en')).toBe('Estimated arrival delay 42 min (30–59 min)');
    expect(reasonText('high-speed line (different thresholds/form)', 'en')).toBe('High-speed line (different thresholds and form)');
    expect(reasonText('replacement bus instead of a train on this route — check the times yourself', 'nl')).toBe('Een deel van de reis ging met een vervangende bus. Controleer de tijden zelf');
  });
  it('covers every Dutch reason in English too', () => {
    const samples = [
      'already claimed (this app or externally)', 'carrier is not NS (Arriva)', 'check-in and check-out at the same station',
      'check-out suggests a delay but the train actually taken could not be identified', 'compensation amount unknown (fare not in travel history)',
      'a later train than planned was taken while the planned journey ran (nearly) on time — check whether there was a disruption',
      'estimated arrival delay 12 min (< 30)', 'estimated delay 31 min is within ±2 min of a threshold',
      'historical timetable for this month not yet published; will re-check automatically', 'late check-out but trains were only 3 min late; not automatically claimable',
      'no destination station', 'no matching scheduled route found; check manually',
      'planned train was cancelled — check it was not announced in advance (planned works)', 'route includes a non-NS train',
      'schedule is not the historical timetable for the travel date', 'station not in timetable data', 'NS accepts claims only 24 h after travel',
      'adjusted timetable that day; also against the regular timetable only 5 min later',
      'timetable was adjusted that day; against the regular timetable you arrived about 40 min later — NS may compensate this, check yourself',
      'timetable was adjusted that day (diverted trains, planned works); NS may compensate against the regular timetable — check yourself',
      'refund below €2.30: NS does not pay this out', 'automatic retry (previous attempt was not sent)', 'reviewed and approved by user',
      'automatic submission is no longer enabled — stopped',
    ];
    for (const s of samples) {
      const en = reasonText(s, 'en');
      expect(en.charAt(0)).toBe(en.charAt(0).toUpperCase());
      expect(en).not.toMatch(/ — |vergeten|t\/m|minuten/);
      expect(reasonText(s, 'nl')).not.toBe(s);
    }
    expect(reasonText('timetable was adjusted that day; against the regular timetable you arrived about 40 min later — NS may compensate this, check yourself', 'en')).toContain('NS may refund this. Decide yourself whether to claim');
    expect(reasonText('late check-out but trains were only 0 min late; not automatically claimable', 'nl')).toBe('Je checkte laat uit, maar de treinen reden op tijd');
    expect(reasonText('late check-out but trains were only 0 min late; not automatically claimable', 'en')).toBe('You checked out late, but the trains ran on time');
    expect(reasonText('late check-out but the trains ran on time; not automatically claimable', 'nl')).toBe('Je checkte laat uit, maar de treinen reden op tijd. OV Refund dient dit niet automatisch in');
    expect(reasonText('planned train was cancelled — check the replacement train you took', 'en')).toBe('Your planned train was cancelled. Check which train you took instead');
    expect(reasonText('connection was missed — check the replacement train you took', 'nl')).toBe('Je miste je overstap. Kijk zelf welke trein je daarna nam');
    expect(reasonText('refund below €2.30: NS does not pay this out', 'en')).toBe('Refund below €2.30: NS does not pay this out');
  });
});

describe('backup and export helpers', () => {
  it('refuses backups over 20 MB before reading', () => {
    expect(MAX_BACKUP_BYTES).toBe(20 * 1024 * 1024);
    expect(backupTooLarge(5_000_000)).toBe(false);
    expect(backupTooLarge(MAX_BACKUP_BYTES)).toBe(false);
    expect(backupTooLarge(MAX_BACKUP_BYTES + 1)).toBe(true);
    expect(backupTooLarge(NaN)).toBe(true);
    expect(backupTooLargeText('en')).toContain('20 MB');
  });
  it('summarises a restore', () => {
    // Current backend: what came from the file, totals, kept claims, unassessed trips and the safety backup.
    const r = { journeys: 31, claims: 19, restored: { journeys: 29, claims: 18 }, kept: { claims: 2, journeys: 2, external: 0 }, replaced_claims: 1, unassessed: 4, auto_backup_path: 'C:\\x\\b.ovrbackup' };
    expect(restoreSummary(r, 'nl', { recheck: true, loggedIn: true })).toEqual([
      '29 reizen en 18 claims uit de back-up teruggezet.', '2 claims die al bij NS waren, zijn bewaard.', 'Je vorige gegevens staan in C:\\x\\b.ovrbackup.',
      'Klik op Controleer nu om je reizen opnieuw te beoordelen.',
    ]);
    expect(restoreSummary({ ...r, unassessed: 0 }, 'en', { recheck: true, loggedIn: true })).not.toContain('Click Check now to re-assess your trips.');
    expect(restoreSummary({ ...r, auto_backup_path: null }, 'nl', { recheck: true, loggedIn: false }).at(-1)).toBe('Log in en klik op Controleer nu om je reizen opnieuw te beoordelen.');
    expect(restoreSummary({ restored: { journeys: 1, claims: 0 }, kept: { claims: 1 } }, 'en')).toEqual(['1 trip restored from the backup.', '1 claim that had already reached NS was kept.']);
    // Older backends: totals only; "replaced" as a number is never shown as a trip count.
    expect(restoreSummary({ journeys: 3, claims: 1 }, 'en')).toEqual(['3 trips and 1 claim restored from the backup.']);
    expect(restoreSummary({ kept: 2, replaced: 40 }, 'nl')).toEqual(['2 claims die al bij NS waren, zijn bewaard.']);
    expect(restoreSummary({ replaced: { journeys: 4 } }, 'en')).toEqual(['4 trips restored from the backup.']);
    expect(restoreSummary(null, 'nl')).toEqual([]);
    expect(restoreSummary({ kept: 0 }, 'nl')).toEqual([]);
  });
  it('says where exports go and what a backup holds', () => {
    const nl = exportInfo({ export_dir: 'C:\\Users\\a\\OV Refund', onedrive_avoided: true }, 'nl');
    expect(nl[0]).toBe('Back-ups en CSV-bestanden komen in C:\\Users\\a\\OV Refund.');
    expect(nl[1]).toContain('OneDrive');
    expect(nl.join(' ')).toContain('volledige reishistorie');
    expect(exportInfo(null, 'en').join(' ')).toContain('full travel history');
    expect(nl.join(' ')).toContain('geen wachtwoorden, IBAN, kaartnummer of e-mailinstellingen');
    expect(nl.join(' ')).toContain('blijven staan na Alles vergeten');
    const en = exportInfo(null, 'en');
    expect(en[0]).toBe('Backups and CSV files go to Documents\\OV Refund.');
    expect(en.join(' ')).not.toContain('OneDrive');
    expect(exportInfo({ forget_removes_exports: true }, 'en').join(' ')).toContain('Forget everything also deletes these files');
  });
});

describe('mail settings helpers', () => {
  it('detects a changed SMTP login', () => {
    expect(smtpLoginChanged({ host: 'smtp.gmail.com', username: 'a@example.com' }, { host: 'smtp.gmail.com', username: 'A@example.com ' })).toBe(false);
    expect(smtpLoginChanged({ host: 'smtp.gmail.com', username: 'a@example.com' }, { host: 'smtp.gmail.com', username: 'b@example.com' })).toBe(true);
    expect(smtpLoginChanged({ host: 'smtp.gmail.com', username: 'a@example.com' }, { host: 'smtp.ziggo.nl', username: 'a@example.com' })).toBe(true);
    expect(smtpLoginChanged(null, { host: 'x', username: 'y' })).toBe(false);
    expect(smtpLoginChanged({ host: '', username: '' }, { host: 'x', username: 'y' })).toBe(false);
  });
  it('has one shared text for unsupported mail', () => {
    expect(unsupportedMailText('nl')).toBe('Hotmail, Outlook.com en Live laten apps zoals OV Refund geen mail meer versturen. Gebruik Windows-meldingen of een ander adres, zoals Gmail.');
    expect(unsupportedMailText('en')).toBe('Hotmail, Outlook.com and Live no longer let apps like OV Refund send mail. Use Windows notifications or another address, such as Gmail.');
    expect(mailPreset('test-x@outlook.com')?.note?.[0]).toBe(unsupportedMailText('nl'));
  });
});

import { helpTip, nearThresholdText, placePopover, progressText, refundSentence, shortPath, stageWord } from './format';

describe('progress button text', () => {
  it('uses one short word per stage', () => {
    expect(stageWord('fetch', 'nl')).toBe('Ophalen');
    expect(stageWord('timetable:2026-10', 'en')).toBe('Timetables');
    expect(stageWord('whatever', 'nl')).toBe('Bezig');
    expect(progressText('fetch', 34, 'nl')).toBe('Ophalen… 34%');
    expect(progressText('done', 100, 'en')).toBe('Done 100%');
    expect(progressText('assess', 140, 'en')).toBe('Checking… 100%');
    expect(progressText('login', NaN, 'nl')).toBe('Inloggen… 0%');
    // automatic submission: which claim, and the pause before the next one
    expect(progressText('submit', 93, 'nl', { index: 2, total: 5 })).toBe('Claim 2 van 5');
    expect(progressText('submit', 93, 'en', { index: 2, total: 5 })).toBe('Claim 2 of 5');
    expect(progressText('pause', 94, 'nl', { index: 3, total: 5, seconds: 42 })).toBe('Pauze · 3/5');
    expect(progressText('pause', 94, 'en', { index: 3, total: 5, seconds: 60 })).toBe('Pause · 3/5');
    expect(progressText('pause', 94, 'nl', { seconds: 42 })).toBe('Pauze 0:42');
    expect(progressText('pause', 94, 'en', { seconds: 60 })).toBe('Pause 1:00');
    expect(progressText('submit', 92, 'en')).toBe('Submitting… 92%');
    expect(stageWord('pause', 'nl')).toBe('Pauze');
  });
});

describe('help tips', () => {
  it('renders a labelled button tied to its popover', () => {
    const t = helpTip('delay 1', 'Uitleg <b>hier</b>', 'Uitleg: "Vertraging"');
    expect(t).toContain('aria-controls="qp-delay-1"');
    expect(t).toContain('id="qp-delay-1" popover="manual"');
    expect(t).toContain('aria-label="Uitleg: &quot;Vertraging&quot;"');
    expect(t).toContain('aria-expanded="false"');
  });
  it('keeps popovers inside the window', () => {
    const vp = { width: 800, height: 600 };
    expect(placePopover({ left: 100, top: 100, bottom: 116, width: 16 }, { width: 200, height: 80 }, vp)).toEqual({ left: 8, top: 122 });
    expect(placePopover({ left: 780, top: 100, bottom: 116, width: 16 }, { width: 200, height: 80 }, vp)).toEqual({ left: 592, top: 122 });
    // no room below: above the anchor
    expect(placePopover({ left: 400, top: 560, bottom: 576, width: 16 }, { width: 100, height: 80 }, vp)).toEqual({ left: 358, top: 474 });
  });
  it('shortens long paths from the left', () => {
    expect(shortPath('C:\\a\\b')).toBe('C:\\a\\b');
    expect(shortPath('C:\\Users\\someone-with-a-long-name\\Documents\\OV Refund', 30)).toBe('…\\Documents\\OV Refund');
  });
});

describe('refund sentence', () => {
  it('explains a fare refund with the amounts', () => {
    expect(refundSentence(42, '50% van de ritprijs (€ 18.20)', 910, 'nl').replace(/ /g, ' ')).toBe('Je kwam ongeveer 42 minuten later aan. Bij 30–59 minuten krijg je de helft van de ritprijs (€ 9,10 van € 18,20) terug. NS beslist.');
    expect(refundSentence(65, '100% van de ritprijs (€ 9.00)', 900, 'en')).toBe('You arrived about 65 minutes late. For 60 minutes or more you get the full fare back. NS decides.');
  });
  it('explains a free-travel subscription share', () => {
    expect(refundSentence(70, 'NS-regel: 1/21 van je maandbedrag Altijd Vrij (€ 400.00, volgens NS)', 1905, 'nl')).toBe('Je kwam ongeveer 70 minuten later aan. Je reisde gratis met Altijd Vrij. Bij 60 minuten of meer krijg je 1/21 van je maandbedrag terug (≈ €\u00a019,05). NS beslist.');
  });
  it('falls back to the general rule, or nothing', () => {
    expect(refundSentence(35, null, null, 'en')).toContain('half the fare back');
    expect(refundSentence(10, null, null, 'nl')).toBe('');
  });
  it('gives one clear near-threshold explanation', () => {
    expect(nearThresholdText(31, 'nl')).toBe('De geschatte vertraging (31 min) ligt vlak bij de grens van 30 minuten. Weet je zeker dat je 30 minuten of meer te laat was? Dien dan in. NS beslist. Twijfel je? Kies dan Niet claimen.');
    expect(reasonText('estimated delay 59 min is within ±2 min of a threshold', 'en')).toContain('60-minute threshold');
  });
});

import { amountDetail } from './format';
describe('amount detail', () => {
  it('shows the amount and its basis on one line', () => {
    expect(amountDetail(910, true, '50% van de ritprijs (€ 18.20)', 'nl').replace(/ /g, ' ')).toBe('€ 9,10 · 50% van € 18,20');
    expect(amountDetail(1905, false, 'NS-regel: 1/21 van je maandbedrag Altijd Vrij (€ 400.00, volgens NS)', 'en')).toBe('≈ €19.05 · 1/21 of your monthly fee');
    expect(amountDetail(500, false, null, 'nl')).toBe('≈ € 5,00');
    expect(amountDetail(null, true, null, 'nl')).toBe('');
  });
});

import { monthProgressLabel } from './format';
describe('timetable month progress', () => {
  it('localises short month names and shows the month count', () => {
    expect(monthProgressLabel('2026-10', 3, 12, 'nl')).toBe('Okt 2026 · 3/12');
    expect(monthProgressLabel('2026-10', 3, 12, 'en')).toBe('Oct 2026 · 3/12');
    expect(monthProgressLabel('2026-03', 1, 2, 'nl')).toBe('Mrt 2026 · 1/2');
    expect(monthProgressLabel('2027-01', 12, 12, 'en')).toBe('Jan 2027 · 12/12');
    expect(progressText('timetable', 50, 'en', { month: '2026-10', index: 3, total: 12 })).toBe('Oct 2026 · 3/12');
  });
  it('rejects invalid months and counters and falls back to the stage', () => {
    for (const month of ['2026-00', '2026-13', '2026-1', '', 'bad']) expect(monthProgressLabel(month, 1, 2, 'nl')).toBe('');
    for (const [i, n] of [[0, 2], [3, 2], [1.5, 2], [1, NaN]]) expect(monthProgressLabel('2026-01', i, n, 'en')).toBe('');
    expect(progressText('timetable', 35, 'en')).toBe('Timetables… 35%');
  });
});

it('translates the adjusted timetable exclusion in both languages', () => {
  const reason = 'adjusted timetable: also less than 30 minutes late against the normal timetable';
  expect(reasonText(reason, 'nl')).toBe('Aangepaste dienstregeling: ook volgens de normale dienstregeling minder dan 30 minuten te laat');
  expect(reasonText(reason, 'en')).toBe('Adjusted timetable: also less than 30 minutes late against the normal timetable');
});


describe('review and input regressions', () => {
  it('uses the regular-timetable delay for adjusted service', async () => {
    const { displayDelay } = await import('./format');
    expect(displayDelay({ delay_min: 0, reasons: ['timetable was adjusted that day; against the regular timetable you arrived about 42 min later — NS may compensate this, check yourself'] })).toBe(42);
    expect(displayDelay({ delay_min: 29, reasons: [] })).toBe(29);
    expect(displayDelay({ delay_min: null, reasons: [] })).toBeNull();
  });
  it('accepts human names and refuses numbers, controls and excessive length', async () => {
    const { validHolderName } = await import('./format');
    expect(validHolderName("É. O'Neil-Sánchez")).toBe(true);
    for (const s of ['', '123', 'Ada@bank', 'Ada\nBank', 'A'.repeat(71)]) expect(validHolderName(s)).toBe(false);
  });
  it('validates bounded decimal entries without coercing invalid input', async () => {
    const { decimalEntry } = await import('./format');
    expect(decimalEntry('4,17', 1000)).toBe(4.17);
    expect(decimalEntry('', 1000)).toBeNull();
    for (const s of ['-5', 'abc', '1e9', '1001', 'Infinity']) expect(decimalEntry(s, 1000)).toBeNaN();
  });
  it('limits manual trip dates and station alphabets', async () => {
    const { validExternalTrip } = await import('./format');
    const now = new Date('2026-11-13T12:00:00Z');
    expect(validExternalTrip('2026-11-12', "'s-Hertogenbosch", 'Utrecht Centraal', now)).toBe(true);
    for (const date of ['1999-01-01', '2026-11-14', 'invalid']) expect(validExternalTrip(date, 'Utrecht', 'Zwolle', now)).toBe(false);
    for (const station of ['123', '<script>', 'A'.repeat(61), 'Utrecht\nCentraal']) expect(validExternalTrip('2026-11-12', station, 'Zwolle', now)).toBe(false);
  });
});

import { amountInput, autoPauseText, autoRunText, autoSubmitBlockers, euroIn, submitHint } from './format';
describe('money, restore and hint helpers', () => {
  const sp = (s: string) => s.replace(/\u00a0/g, ' ');
  it('uses one money format in every language', () => {
    expect(sp(euroIn('half the fare (€ 18.20) and €2,30'))).toBe('half the fare (€ 18,20) and € 2,30');
    expect(euroIn('no amount here')).toBe('no amount here');
    expect(euroIn('€ 400.00')).toBe(euro(40000));
    expect(amountInput(417)).toBe('4,17');
    expect(amountInput(0)).toBe('0,00');
    expect(amountInput(null)).toBe('');
    expect(amountInput(417, 'en')).toBe('4.17');
    expect(euroIn('half the fare (€ 18,20)', 'en')).toBe('half the fare (€18.20)');
  });
  it('says why automatic submission is blocked, treating an unanswered claim-service question as blocking', () => {
    expect(autoSubmitBlockers({ checksOn: true, bank: true, otherService: 'none' }, 'en')).toEqual([]);
    expect(autoSubmitBlockers({ checksOn: true, bank: true, otherService: 'import_done' }, 'nl')).toEqual([]);
    expect(autoSubmitBlockers({ checksOn: true, bank: true, otherService: null }, 'nl')).toEqual([{ key: 'service', text: 'Beantwoord eerst de vraag over claimdiensten.' }]);
    expect(autoSubmitBlockers({ checksOn: true, bank: true, otherService: undefined }, 'en').map((b) => b.text)).toEqual(['Answer the claim service question first.']);
    expect(autoSubmitBlockers({ checksOn: false, bank: false, otherService: 'yes' }, 'en')).toEqual([
      { key: 'checks', text: 'Turn on Check automatically first.' }, { key: 'bank', text: 'Set up your bank account first.' },
      { key: 'import', text: 'First add the trips your claim service already claimed, then click Import done.' },
    ]);
  });
  it('names the first missing step before a claim can be submitted', () => {
    const ok = { reached: true, bank: true, resubNeeded: false, resubOk: false, nearNeeded: false, nearOk: false, declared: true };
    expect(submitHint(ok, 'en')).toBe('');
    expect(submitHint({ ...ok, reached: null }, 'nl')).toBe('Beantwoord eerst de vraag over je bestemming.');
    expect(submitHint({ ...ok, reached: false }, 'nl')).toBe('');
    expect(submitHint({ ...ok, bank: false }, 'en')).toBe('Set up your bank account first.');
    expect(submitHint({ ...ok, resubNeeded: true }, 'en')).toBe('Tick that you checked Mijn teruggaven.');
    expect(submitHint({ ...ok, nearNeeded: true }, 'en')).toBe('Tick that it is a different trip.');
    expect(submitHint({ ...ok, declared: false }, 'nl')).toBe('Vink de verklaring aan.');
  });
  it('reports an automatic check with what still waits', () => {
    expect(autoRunText(0, 2, 1, 'nl', true)).toBe('Automatische controle: 0 claims ingediend, 2 te claimen, 1 te beoordelen (demo).');
    expect(autoRunText(1, 0, 0, 'en')).toBe('Automatic check: 1 claim submitted.');
    expect(autoRunText(0, 5, 0, 'nl', true, autoPauseText({ unresolved: 1 }, 'nl'))).toBe('Automatische controle: 0 claims ingediend, 5 te claimen (demo). Gepauzeerd: 1 claim met een onzekere uitkomst.');
  });
  it('says why automatic submission is paused', () => {
    expect(autoPauseText({ unresolved: 0 }, 'nl')).toBe('');
    expect(autoPauseText({ unresolved: 2 }, 'en')).toBe('Paused: 2 claims with an uncertain outcome.');
    expect(autoPauseText({ unresolved: 0, claimListUnavailable: true }, 'nl')).toBe('Gepauzeerd: Mijn teruggaven kon niet worden gelezen.');
  });
  it('only says "submit" near a threshold when OV Refund can submit the trip', () => {
    expect(nearThresholdText(29, 'nl')).toContain('Dien de claim dan zelf in via Mijn NS');
    expect(nearThresholdText(29, 'nl')).not.toContain('Dien dan in.');
    expect(nearThresholdText(58, 'en')).toContain('OV Refund submits it as 30–59 minutes');
    expect(nearThresholdText(61, 'en')).toContain('Then submit.');
  });
});
