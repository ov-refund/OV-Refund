import { redactText } from './format';

export const BUG_TEXT_MAX = 500;
export const BUG_SUBJECT = 'OV Refund bug report';

/** A reminder only: names and addresses cannot all be recognised automatically. */
export function hasPersonalDetails(text: string): boolean {
  return /@|\b\d{4}\s?[A-Z]{2}\b|\b[\p{L}]*(?:straat|laan|weg|plein|singel|kade|gracht)\s+\d+[a-z]?\b|\b(?:\d{1,2}[-/.]\d{1,2}[-/.](?:\d{4}|\d{2})|(?:19|20)\d{2}[-/.]\d{1,2}[-/.]\d{1,2})\b/iu.test(text);
}

/** One message for preview, copy and relay. No user contact fields are accepted. */
export async function buildBugPayload(key: string, text: string, diagnostics = '', redactRemote?: (text: string) => Promise<string>) {
  const clean = async (value: string) => {
    let redacted = value;
    try { if (redactRemote) redacted = await redactRemote(value); } catch { /* local redaction still applies */ }
    return redactText(redacted);
  };
  const description = (await clean(text)).slice(0, BUG_TEXT_MAX).trim();
  const summary = diagnostics ? await clean(diagnostics) : '';
  return {
    access_key: key, subject: BUG_SUBJECT,
    message: description + (summary ? `\n\n--- diagnose ---\n${summary}` : ''),
    from_name: 'OV Refund bug reports',
  };
}

/** A report is delivered only after an explicit acknowledgement; a stalled relay must release the dialog. */
export async function sendBugRelay(url: string, payload: Record<string, unknown>, fetcher: typeof fetch = fetch): Promise<void> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 20_000);
  try {
    const response = await fetcher(url, {
      method: 'POST', headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify(payload), signal: controller.signal,
    });
    const result = await response.json();
    if (!response.ok || result?.success !== true) throw new Error('relay did not acknowledge the report');
  } finally { clearTimeout(timeout); }
}
