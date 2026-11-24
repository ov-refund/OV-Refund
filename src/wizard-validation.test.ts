import { describe, expect, it } from 'vitest';
import { validAutoLogin, validBank, validWizardMail } from './wizard-validation';

describe('wizard gates', () => {
  it('requires a holder and the existing IBAN validation', () => {
    expect(validBank('Ada', 'NL91 ABNA 0417 1643 00')).toBe(true);
    expect(validBank('  ', 'NL91ABNA0417164300')).toBe(false);
    expect(validBank('Ada', 'NL91ABNA0417164301')).toBe(false);
    expect(validBank('Ada', '')).toBe(false);
  });
  it('requires a choice and valid credentials only for automatic login', () => {
    expect(validAutoLogin(null, '', '')).toBe(false);
    expect(validAutoLogin(false, '', '')).toBe(true);
    expect(validAutoLogin(true, 'a@example.org', 'secret')).toBe(true);
    expect(validAutoLogin(true, 'wrong', 'secret')).toBe(false);
    expect(validAutoLogin(true, 'a@example.org', '')).toBe(false);
  });
  it('allows closed mail settings and validates every required open field', () => {
    expect(validWizardMail(false, 'wrong', '', {})).toBe(true);
    expect(validWizardMail(true, '', '', {})).toBe(true);
    expect(validWizardMail(true, 'test-a@gmail.com', '', {})).toBe(false);
    expect(validWizardMail(true, 'test-a@gmail.com', 'app password', {})).toBe(true);
    expect(validWizardMail(true, 'a@unknown.invalid', 'secret', {})).toBe(false);
    expect(validWizardMail(true, 'test-a@outlook.com', 'secret', {})).toBe(false);
  });
  it('reuses a stored mail password only for the same SMTP login', () => {
    const saved = { smtp_password_set: true, smtp: { host: 'smtp.gmail.com', username: 'test-a@gmail.com' } };
    expect(validWizardMail(true, 'test-a@gmail.com', '', saved)).toBe(true);
    expect(validWizardMail(true, 'test-b@gmail.com', '', saved)).toBe(false);
    expect(validWizardMail(true, 'test-a@gmail.com', '', { smtp_password_set: true })).toBe(false);
    expect(validWizardMail(true, 'test-b@gmail.com', 'new secret', saved)).toBe(true);
  });
});


it('shows only active steps and never allows automatic submission without checks or migration', async () => {
  const { wizardStepIds, automaticSubmitAllowed } = await import('./wizard-validation');
  expect(wizardStepIds(false)).toHaveLength(7);
  expect(wizardStepIds(false)).not.toContain('autologin');
  expect(wizardStepIds(true)).toHaveLength(8);
  expect(automaticSubmitAllowed(true, false)).toBe(true);
  expect(automaticSubmitAllowed(true, true, true)).toBe(true);
  expect(automaticSubmitAllowed(false, true, true)).toBe(false);
  for (const checks of [true, false]) for (const service of [true, null]) expect(automaticSubmitAllowed(checks, service)).toBe(false);
  expect(automaticSubmitAllowed(false, false)).toBe(false);
});
