import { checkIban, validHolderName, isEmail, isUnsupportedMail, mailPreset, smtpLoginChanged } from './format';

export const validBank = (name: string, iban: string): boolean => validHolderName(name) && checkIban(iban).state === 'valid';
export const validAutoLogin = (choice: boolean | null, email: string, password: string): boolean =>
  choice === false || (choice === true && isEmail(email.trim()) && !!password);

/** A stored SMTP password is usable only for its original server and user. */
export function validWizardMail(enabled: boolean, address: string, password: string, saved: { smtp_password_set?: boolean; smtp?: any }): boolean {
  if (!enabled || (!address.trim() && !password.trim())) return true;
  const to = address.trim(), preset = mailPreset(to);
  if (!isEmail(to) || isUnsupportedMail(to) || !preset || preset.unsupported) return false;
  const smtp = { ...preset, username: to };
  return !!password.trim() || (!!saved.smtp_password_set && !!saved.smtp?.host && !!saved.smtp?.username && !smtpLoginChanged(saved.smtp, smtp));
}

export const wizardStepIds = (autoChecks: boolean): string[] => ['welcome', 'login', 'services', 'bank', 'auto', ...(autoChecks ? ['autologin'] : []), 'notify', 'finish'];
export const automaticSubmitAllowed = (autoChecks: boolean, otherService: boolean | null, importDone = false): boolean => autoChecks && (otherService === false || (otherService === true && importDone));
