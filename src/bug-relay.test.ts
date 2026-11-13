import { afterEach, expect, it, vi } from 'vitest';
import { BUG_TEXT_MAX, buildBugPayload, hasPersonalDetails, sendBugRelay } from './bug-relay';

afterEach(() => vi.useRealTimers());

it('requires a positive acknowledgement, including for HTTP 200', async () => {
  for (const body of [{ success: false }, {}, null, { success: 'true' }]) {
    const fake = vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 }));
    await expect(sendBugRelay('unused', {}, fake)).rejects.toThrow();
  }
  await expect(sendBugRelay('unused', {}, vi.fn().mockResolvedValue(new Response('invalid')))).rejects.toThrow();
  await expect(sendBugRelay('unused', {}, vi.fn().mockResolvedValue(new Response('{"success":true}', { status: 500 })))).rejects.toThrow();
  await expect(sendBugRelay('unused', {}, vi.fn().mockResolvedValue(new Response('{"success":true}')))).resolves.toBeUndefined();
});

it('aborts a stalled request after 20 seconds and clears its timer', async () => {
  vi.useFakeTimers();
  const fake = vi.fn((_url, options) => new Promise<Response>((_resolve, reject) => {
    options?.signal?.addEventListener('abort', () => reject(new Error('aborted')), { once: true });
  })) as typeof fetch;
  const outcome = expect(sendBugRelay('unused', {}, fake)).rejects.toThrow('aborted');
  await vi.advanceTimersByTimeAsync(20_000);
  await outcome;
  expect(vi.getTimerCount()).toBe(0);
});

it('clears the timeout after success', async () => {
  vi.useFakeTimers();
  await sendBugRelay('unused', {}, vi.fn().mockResolvedValue(new Response('{"success":true}')));
  expect(vi.getTimerCount()).toBe(0);
});


it('warns about postcodes, street numbers, birth dates and @ signs', () => {
  for (const text of ['1234 AB', '1234ab', 'Dorpsstraat 12', 'Lindelaan 4a', 'Spoorweg 1', 'Marktplein 7', 'Singel 12', 'Handelskade 6', 'Herengracht 3', 'geboren 01-02-1990', '1/2/90', '1990-02-01', 'mail mij @']) {
    expect(hasPersonalDetails(text), text).toBe(true);
  }
  for (const text of ['', 'De app sluit bij het scannen.', 'Trein om 18:12, 42 min te laat', 'Op de Dorpsstraat werkt scannen niet']) {
    expect(hasPersonalDetails(text), text).toBe(false);
  }
});

it('builds only relay fields, with redacted text and diagnostics', async () => {
  const remote = vi.fn(async (text: string) => text.replace('private-token', '[secret]'));
  const payload = await buildBugPayload('key', 'a@example.org NL91 ABNA 0417 1643 00 kaart 3528 0000 0000 1234 tel 06-12345678 private-token', 'support@example.org private-token', remote);
  expect(Object.keys(payload).sort()).toEqual(['access_key', 'from_name', 'message', 'subject']);
  expect(payload).not.toHaveProperty('email');
  expect(payload).not.toHaveProperty('replyto');
  expect(payload).not.toHaveProperty('contact');
  for (const value of ['a@example.org', 'support@example.org', 'NL91', '3528', '06-12345678', 'private-token']) expect(payload.message).not.toContain(value);
  expect(payload.message).toContain('--- diagnose ---');
  expect(remote).toHaveBeenCalledTimes(2);
});

it('caps free text at 500 and redacts before cutting through a contact detail', async () => {
  const payload = await buildBugPayload('key', 'x'.repeat(600));
  expect(payload.message).toHaveLength(BUG_TEXT_MAX);
  const boundary = await buildBugPayload('key', 'x '.repeat(247) + 'private@example.org');
  expect(boundary.message).not.toContain('private');
  expect(boundary.message.length).toBeLessThanOrEqual(BUG_TEXT_MAX);
});

it('keeps local redaction when the backend is unavailable', async () => {
  const payload = await buildBugPayload('', 'a@example.org 0612345678', '', async () => { throw new Error('unavailable'); });
  expect(payload.message).toBe('[email] [nummer]');
  expect(payload.message).not.toContain('diagnose');
});

it('redacts formatted telephone numbers even without the backend', async () => {
  for (const phone of ['06 12345678', '06.1234.5678', '+31 (0)6 1234 5678', '020-1234567']) {
    const payload = await buildBugPayload('', `tel: ${phone}`);
    expect(payload.message).not.toContain('1234');
  }
});
