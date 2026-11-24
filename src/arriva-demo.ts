import { carrierKind, escapeHtml as h } from './format';
import { dialogHead, friendlyError, getLang, installDialogTrap, L, pickLang } from './i18n';
import { invoke } from '@tauri-apps/api/core';

export function operatorLabel(code?: string | null, name?: string | null): { label: string; kind: 'ns' | 'arriva' | 'other' } {
  const kind = carrierKind(code, name), n = (name ?? '').trim();
  if (kind === 'ns') return { label: 'NS', kind };
  if (kind === 'arriva') return { label: 'Arriva', kind };
  return { label: n || (getLang() === 'nl' ? 'Onbekend' : 'Unknown'), kind };
}

type Attachment = { name: string; file?: string | null; path: string; kind: string; size?: number; index?: number };
type DemoState = { available: boolean; demo_only: boolean; phase: string; steps: { id: string; label: string; status: string; detail: string }[]; trip: { from: string; to: string; train_number: string; date: string; delay_minutes: number; fare_cents: number; check_in: string; check_out: string }; attachments: Attachment[]; logs: { at: string; step: string; message: string }[]; last_error?: string; window_open: boolean; unverified?: string[]; captcha?: 'puzzle' | 'no_puzzle' | 'failed' | 'challenge_closed' | null; verified_fields?: number; demo_trips?: DemoTrip[] };
/** One demo trip with Arriva's rules (carrier policy) applied by the backend. */
export type DemoTrip = { id: string; status: 'claimable' | 'tested' | 'not_eligible'; delay_minutes: number; train_number: string | null; real_service: boolean; region?: string | null; eligible?: boolean; min_delay?: number; refund_percent?: number | null; form_option?: string | null; deadline?: string | null; reason_nl?: string; reason_en?: string };
type Call = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
export type ArrivaDemoState = DemoState;
export const canClaimArrivaDemo = (data: DemoState | null): boolean => !!data?.available && data.window_open && ['ready', 'captcha_attention', 'captcha_needed'].includes(data.phase);
/** Where an Arriva claim stands. A claim stays under Claimable while it is in progress or failed;
 *  only a final outcome (the guarded final write reached: "Demo getest") takes it off. */
export type ArrivaClaimState = 'waiting' | 'ready' | 'in_progress' | 'failed' | 'done';
export function arrivaClaimState(data: DemoState | null, claiming = false): ArrivaClaimState {
  if (!data?.available) return 'waiting';
  if (data.phase === 'submission_blocked') return 'done';
  if (claiming || data.phase === 'verifying' || (data.phase === 'captcha_attention' && data.captcha === 'puzzle')) return 'in_progress';
  if (data.phase === 'error' || data.phase === 'window_closed' || data.captcha === 'failed' || data.captcha === 'challenge_closed') return 'failed';
  if (canClaimArrivaDemo(data)) return 'ready';
  return 'waiting';
}
/** States that keep the trip in the Claimable tab. */
export const arrivaStaysClaimable = (s: ArrivaClaimState) => s === 'ready' || s === 'in_progress' || s === 'failed';
const nl = () => getLang() === 'nl';

/** Form checks reported by the backend (field ids, see arriva.rs) in the app's language. */
const FIELDS: Record<string, [string, string]> = {
  subject: ['Onderwerp', 'Subject'], region: ['Regio', 'Region'], ticket: ['Vervoerbewijs', 'Ticket'],
  card_number: ['OV-chipkaartnummer', 'OV-chipkaart number'], iban: ['IBAN', 'IBAN'], valid_until: ['Geldig tot', 'Valid until'],
  attachment: ['Bijlage', 'Attachment'], transport: ['Vervoermiddel', 'Mode of transport'], departure_station: ['Vertrekstation', 'Departure station'],
  arrival_station: ['Aankomststation', 'Arrival station'], travel_time: ['Reisdatum en -tijd', 'Travel date and time'], student_product: ['Studentenreisproduct', 'Student travel product'],
  fare: ['Ritprijs', 'Fare'], delay: ['Vertraging', 'Delay'], first_name: ['Voornaam', 'First name'], last_name: ['Achternaam', 'Last name'],
  email: ['E-mailadres', 'Email address'], consent: ['Toestemming', 'Consent'], no_form_errors: ['Foutmeldingen op het formulier', 'Errors on the form'], submit_button: ['Knop Indienen', 'Submit button'],
};
export const fieldLabel = (id: string) => { const f = FIELDS[id]; return f ? L(f[0], f[1]) : id; };

/** What happened at Arriva, for the app: only the CAPTCHA is ever shown in the provider window. */
export function arrivaOutcomeText(data: DemoState | null): string {
  if (data?.unverified?.length) {
    const list = data.unverified.map(fieldLabel).join(', ');
    return L(`Niet alle velden konden worden gecontroleerd: ${list}. De CAPTCHA is niet gevraagd.`, `Not all fields could be verified: ${list}. The CAPTCHA was not requested.`);
  }
  if (data?.captcha === 'no_puzzle') return L('Geen puzzel nodig: de CAPTCHA vroeg niets en het Arriva-venster bleef verborgen. Verzending blijft geblokkeerd (demo).', 'No puzzle needed: the CAPTCHA asked nothing and the Arriva window stayed hidden. Submission stays blocked (demo).');
  if (data?.captcha === 'challenge_closed') return L('De puzzel is gesloten en het Arriva-venster is weer verborgen. Verzending blijft geblokkeerd (demo).', 'The puzzle closed and the Arriva window is hidden again. Submission stays blocked (demo).');
  if (data?.captcha === 'puzzle' && data.phase === 'captcha_attention') return L('Los de puzzel op in het Arriva-venster. Daar staat alleen de CAPTCHA.', 'Solve the puzzle in the Arriva window. It shows only the CAPTCHA.');
  if (data?.captcha === 'failed') return L('Arriva kon de CAPTCHA niet valideren. In de demo is niets naar Arriva geüpload en is de claim niet ingediend.', 'Arriva could not validate the CAPTCHA. In the demo nothing was uploaded to Arriva and the claim was not submitted.');
  return '';
}

/** "12,3 kB" / "12.3 kB"; bytes below 1 kB as "512 B". */
export function fileSize(bytes?: number | null): string {
  if (bytes == null || !Number.isFinite(bytes)) return '—';
  if (bytes < 1024) return `${bytes} B`;
  const f = new Intl.NumberFormat(nl() ? 'nl-NL' : 'en-GB', { maximumFractionDigits: 1 });
  return bytes < 1024 * 1024 ? `${f.format(bytes / 1024)} kB` : `${f.format(bytes / 1024 / 1024)} MB`;
}

/** Caption under each attachment thumbnail (by position: travel proof, then delay proof). */
export function attachmentCaption(a: Attachment, i: number, demo = true): string {
  if (a.kind === 'png' && i === 0) return demo
    ? L('Je reis zoals in het reisoverzicht (in- en uitchecken, ritprijs), met daaronder je volledige OV-chipkaartnummer, reisdatum en traject. In de demo: fictieve gegevens.', 'Your trip as in the travel history (check-in, check-out, fare), with your full OV-chipkaart number, travel date and route below it. In the demo: fictional data.')
    : L('Je reisoverzicht in Mijn NS met deze reis uitgeklapt, met daaronder je volledige OV-chipkaartnummer, reisdatum en traject.', 'Your travel history in Mijn NS with this trip expanded, with your full OV-chipkaart number, travel date and route below it.');
  if (a.kind === 'png' && i === 1) return L('De vertraging van deze trein volgens de openbare gegevens van Rijden de Treinen (CC BY 4.0).', 'The delay of this train from the public Rijden de Treinen data (CC BY 4.0).');
  return '';
}

/** Step names of the demo inspection, by step id (the backend's labels are Dutch). */
const STEP_NAMES: Record<string, [string, string]> = {
  guard: ['Verzending blokkeren', 'Block sending'], evidence: ['Bijlagen maken', 'Make attachments'],
  form: ['Arriva-formulier invullen', 'Fill in Arriva’s form'], captcha: ['CAPTCHA en controle', 'CAPTCHA and check'],
};
/** Status words a person reads instead of the backend's codes. */
const STATUS_WORDS: Record<string, [string, string]> = {
  pending: ['Nog niet', 'Not yet'], running: ['Bezig', 'In progress'], done: ['Klaar', 'Done'], ok: ['Klaar', 'Done'],
  error: ['Mislukt', 'Failed'], failed: ['Mislukt', 'Failed'], attention: ['Wacht op jou', 'Waiting for you'], blocked: ['Geblokkeerd', 'Blocked'], skipped: ['Overgeslagen', 'Skipped'],
};
export const stepName = (id: string, fallback: string) => (STEP_NAMES[id] ? L(...STEP_NAMES[id]) : fallback);
export const statusWord = (s: string) => (STATUS_WORDS[s] ? L(...STATUS_WORDS[s]) : s);
/** Step texts with an untranslated pending detail: the backend's default details are Dutch-only. */
const STEP_DETAILS: Record<string, [string, string]> = {
  guard: ['Alle definitieve aanvragen worden voor netwerkverkeer geblokkeerd.', 'Every final request is blocked before it reaches the network.'],
  evidence: ['Reisbewijs (demogegevens) en vertragingsbewijs (Rijden de Treinen open data).', 'Travel proof (demo data) and delay proof (Rijden de Treinen open data).'],
  form: ['Officieel formulier, synthetische gegevens.', 'Official form, synthetic data.'],
  captcha: ['Alleen in het officiële formulier; de claim wordt niet ingediend.', 'Only in the official form; the claim is not submitted.'],
};
const stepDetail = (s: { id: string; status: string; detail: string }) => (s.status === 'pending' && STEP_DETAILS[s.id] && s.detail === STEP_DETAILS[s.id][0] ? L(...STEP_DETAILS[s.id]) : pickLang(s.detail));
function stepsHtml(steps: DemoState['steps']): string {
  return steps.map(s => `<li><b>${h(stepName(s.id, s.label))}</b><span class="arriva-status">${h(statusWord(s.status))}</span><p>${h(stepDetail(s))}</p></li>`).join('') || `<li>${L('Nog geen stappen uitgevoerd.', 'No steps run yet.')}</li>`;
}
/** The log in time order, in local time, with repeated lines collapsed ("×3"). */
export function logLines(logs: { at: string; step: string; message: string }[]): { time: string; step: string; message: string; count: number }[] {
  const sorted = [...logs].sort((a, b) => Date.parse(a.at) - Date.parse(b.at));
  const out: { time: string; step: string; message: string; count: number }[] = [];
  for (const x of sorted) {
    const message = pickLang(x.message), step = stepName(x.step, x.step);
    const last = out[out.length - 1];
    if (last && last.step === step && last.message === message) { last.count++; continue; }
    const d = new Date(x.at);
    const time = Number.isNaN(d.getTime()) ? x.at : d.toLocaleTimeString(nl() ? 'nl-NL' : 'en-GB', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
    out.push({ time, step, message, count: 1 });
  }
  return out;
}
function logHtml(logs: { at: string; step: string; message: string }[]): string {
  return logLines(logs).map(x => `<li><time>${h(x.time)}</time> · ${h(x.step)} · ${h(x.message)}${x.count > 1 ? ` <b>×${x.count}</b>` : ''}</li>`).join('') || '<li>—</li>';
}

/** Where the attachments come from: the demo (default) or a real claim (`arriva_claim_*` with its claim id). */
export type AttachmentSource = { data: string; open: string; args: Record<string, unknown>; demo: boolean; fareNote?: string };
const DEMO_SOURCE: AttachmentSource = { data: 'arriva_demo_attachment_data', open: 'arriva_demo_attachment', args: {}, demo: true };
export function canConfirmArrivaAttachments(decoded: readonly string[], total: number): boolean {
  return total === 2 && !!decoded[0] && !!decoded[1];
}

/** Lists the files that will be attached to the Arriva claim as thumbnails; a click enlarges one. */
export function openArrivaAttachments(call: Call, data: Pick<DemoState, 'attachments'> | null, opener?: HTMLElement, src: AttachmentSource = DEMO_SOURCE, preview = false): Promise<boolean> {
  let resolveGo: (go: boolean) => void = () => {};
  const answer = new Promise<boolean>((res) => { resolveGo = res; });
  let go = false;
  const files = data?.attachments ?? [];
  const caption = (a: Attachment, i: number) => attachmentCaption(a, a.index ?? i, src.demo);
  const dlg = document.createElement('dialog'); dlg.className = 'arriva-dialog arriva-files';
  dlg.innerHTML = `${dialogHead(L('Bijlagen', 'Attachments'), 'arrivaFilesTitle')}<div class="arriva-body">
    ${files.length ? `<p>${preview
      ? L(`Controleer ${files.length === 1 ? 'deze afbeelding' : 'deze afbeeldingen'}. ${files.length === 1 ? 'Hij wordt' : 'Ze worden'} bij de Arriva-claim gevoegd. Andere reizen en je naam zijn grijs gemaakt (verborgen).`, `Check ${files.length === 1 ? 'this image' : 'these images'}. ${files.length === 1 ? 'It is' : 'They are'} attached to the Arriva claim. Other trips and your name are greyed out (hidden).`)
      : L(`Deze ${files.length === 1 ? 'afbeelding wordt' : 'afbeeldingen worden'} bij de Arriva-claim gevoegd.`, `${files.length === 1 ? 'This image is' : 'These images are'} attached to the Arriva claim.`)}</p>` : ''}
    ${preview && src.fareNote ? `<p>${h(src.fareNote)}</p>` : ''}
    ${files.length ? `<ul class="arriva-file-list">${files.map((a, i) => `<li><figure>
      <button type="button" class="arriva-thumb" data-zoom="${i}" aria-label="${h(L('Vergroten', 'Enlarge'))}: ${h(pickLang(a.name))}"><span class="arriva-thumb-ph" aria-hidden="true"></span></button>
      <figcaption><b>${h(pickLang(a.name))}</b>${caption(a, i) ? `<span>${h(caption(a, i))}</span>` : ''}<small>${h(a.file ?? a.kind.toUpperCase())} · ${fileSize(a.size)}</small>
      <span class="arriva-file-acts"><button type="button" class="btn small" data-zoom="${i}">${L('Vergroten', 'Enlarge')}</button><button type="button" class="btn ghost small" data-file="${a.index ?? i}">${L('Openen in viewer', 'Open in viewer')}</button></span></figcaption></figure></li>`).join('')}</ul>`
      : `<p class="msg">${src.demo ? L('Nog geen bijlagen. Kies eerst Controleer nu.', 'No attachments yet. Choose Check now first.') : L('Er zijn nu geen bijlagen bewaard. OV Refund maakt ze als je op Claimen drukt, laat ze je eerst zien en verwijdert ze weer na de uitkomst.', 'No attachments are stored now. OV Refund makes them when you press Claim, shows them to you first and deletes them again after the outcome.')}</p>`}
    <p class="arriva-inline" role="status" aria-live="polite"></p>
    <div class="arriva-actions end">${preview
      ? `<button class="btn" data-x="close">${L('Annuleren', 'Cancel')}</button><button class="btn primary" data-x="go" disabled>${L('Klopt, ga verder', 'Correct, continue')}</button>`
      : `<button class="btn ghost" data-x="close">${L('Sluiten', 'Close')}</button>`}</div></div>`;
  const urls: string[] = [];
  const confirm = dlg.querySelector<HTMLButtonElement>('[data-x="go"]');
  const status = () => dlg.querySelector<HTMLElement>('.arriva-inline')!;
  files.forEach((a, i) => {
    if (a.kind !== 'png') return;
    call<string>(src.data, { ...src.args, index: a.index ?? i }).then(async url => {
      const img = document.createElement('img');
      img.alt = '';
      img.src = url;
      await img.decode();
      urls[i] = url;
      dlg.querySelector<HTMLElement>(`.arriva-thumb[data-zoom="${i}"]`)?.replaceChildren(img);
      if (confirm) confirm.disabled = !canConfirmArrivaAttachments(urls, files.length);
    }).catch(err => { status().textContent = friendlyError(err); });
  });
  const zoom = (i: number) => {
    if (!urls[i]) return;
    const a = files[i];
    const box = document.createElement('dialog'); box.className = 'arriva-lightbox';
    box.innerHTML = `${dialogHead(h(pickLang(a.name)), 'arrivaZoomTitle')}<div class="arriva-lightbox-img"><img src="${h(urls[i])}" alt="${h(pickLang(a.name))}"></div>${caption(a, i) ? `<p class="arriva-lightbox-cap">${h(caption(a, i))}</p>` : ''}`;
    box.addEventListener('click', e => { const t = e.target as HTMLElement; if (t === box || t.closest('[data-x="close"]')) box.close(); });
    box.addEventListener('close', () => { box.remove(); dlg.querySelector<HTMLElement>(`.arriva-thumb[data-zoom="${i}"]`)?.focus({ preventScroll: true }); }, { once: true });
    box.setAttribute('aria-labelledby', 'arrivaZoomTitle'); document.body.append(box); box.showModal(); box.querySelector<HTMLElement>('[data-x="close"]')?.focus();
  };
  dlg.addEventListener('click', async e => {
    const t = e.target as HTMLElement;
    if (t.closest('[data-x="close"]')) { dlg.close(); return; }
    if (t.closest('[data-x="go"]')) {
      if (confirm && !confirm.disabled) { go = true; dlg.close(); }
      return;
    }
    const z = t.closest<HTMLElement>('[data-zoom]');
    if (z) { zoom(Number(z.dataset.zoom)); return; }
    const b = t.closest<HTMLButtonElement>('[data-file]');
    if (!b) return;
    try { await call(src.open, { ...src.args, index: Number(b.dataset.file) }); status().textContent = L('Geopend in je standaardprogramma.', 'Opened in your default app.'); }
    catch (err) { status().textContent = friendlyError(err); }
  });
  dlg.addEventListener('cancel', e => { e.preventDefault(); dlg.close(); });
  // The opener may have been re-rendered meanwhile: find its replacement by its data attributes.
  const back = opener ? ['act', 'id', 'claim', 'what'].map((k) => opener.dataset[k] ? `[data-${k}="${CSS.escape(opener.dataset[k]!)}"]` : '').join('') : '';
  dlg.addEventListener('close', () => {
    dlg.remove();
    const target = opener?.isConnected ? opener : back ? document.querySelector<HTMLElement>(back) : null;
    (target ?? document.getElementById('trips'))?.focus({ preventScroll: true });
    resolveGo(go);
  }, { once: true });
  dlg.setAttribute('aria-labelledby', 'arrivaFilesTitle'); document.body.append(dlg); dlg.showModal(); installDialogTrap();
  dlg.querySelector<HTMLElement>('[data-zoom], [data-x="close"].btn')?.focus();
  return answer;
}

export async function openArrivaDemo(call: Call, runCheck: () => Promise<void>, explicitOpener?: HTMLElement) {
  const dlg = document.createElement('dialog'); dlg.className = 'arriva-dialog';
  const opener = explicitOpener ?? document.activeElement as HTMLElement | null;
  let data: DemoState | null = null, busy = false, inspect = false, loadError = '';
  let pollActive = false, closed = false;
  const canOpen = () => !!data?.available && ['ready', 'captcha_needed', 'captcha_attention', 'submission_blocked'].includes(data.phase);
  const canInspectForm = () => !!data?.available && !!data.window_open;
  const phaseLabel = () => data?.phase === 'ready' ? L('Klaar voor controle.', 'Ready for review.') : '';
  const primaryAction = () => canOpen() ? 'open' : 'check';
  const render = () => {
    const trip = data?.trip;
    dlg.innerHTML = `${dialogHead(L('Arriva-demo', 'Arriva demo'), 'arrivaDemoTitle')}<div class="arriva-body">
      <p class="note warn"><span>${L('Dit is een simulatie. Er wordt geen echte claim ingediend.', 'This is a simulation. No real claim is submitted.')}</span></p>
      <section class="arriva-scenario"><span class="operator-badge arriva">Arriva</span><h3>${h(trip?.from ?? 'Venlo')} <span aria-hidden="true">→</span> ${h(trip?.to ?? 'Nijmegen')}</h3><p>${L('Trein', 'Train')} ${h(trip?.train_number ?? '—')} · ${trip?.delay_minutes ?? 24} ${L('min vertraging', 'min late')} · ${L('Fictieve ritprijs', 'Example fare')} ${trip?.fare_cents != null ? new Intl.NumberFormat(nl() ? 'nl-NL' : 'en-GB', { style: 'currency', currency: 'EUR' }).format(trip.fare_cents / 100) : '€ 12,40'}</p></section>
      <p class="arriva-inline" role="status" aria-live="polite">${busy ? L('Bezig… dit kan even duren.', 'Working… this may take a moment.') : h(pickLang(data?.last_error || loadError || phaseLabel()))}${data && !data.available ? `<br>${L('Arriva-demo is hier niet beschikbaar.', 'Arriva demo is not available here.')}` : ''}</p>
      <div class="arriva-actions"><button class="btn primary" data-demo="${primaryAction()}" ${busy || !data?.available ? 'disabled' : ''}>${primaryAction() === 'open' ? L('Afronden bij Arriva', 'Continue at Arriva') : L('Controleer nu', 'Check now')}</button></div>
      <details class="arriva-inspect" ${inspect ? 'open' : ''}><summary>${L('Demo inspecteren', 'Inspect demo')}</summary><div class="arriva-actions"><button class="btn small" data-demo="prepare" ${busy || (data !== null && !data.available) ? 'disabled' : ''}>${L('Alleen Arriva opnieuw voorbereiden', 'Prepare Arriva only again')}</button><button class="btn small" data-demo="form" ${busy || !canInspectForm() ? 'disabled' : ''}>${L('Formulier bekijken', 'View form')}</button><button class="btn small" data-demo="probe" ${busy || !canOpen() ? 'disabled' : ''}>${L('CAPTCHA controleren (demo)', 'Check CAPTCHA (demo)')}</button><button class="btn small" data-demo="reset" ${busy || (data !== null && !data.available) ? 'disabled' : ''}>${L('Reset demo', 'Reset demo')}</button></div>
        <ol class="arriva-steps">${stepsHtml(data?.steps ?? [])}</ol>
        ${(data?.attachments?.length ?? 0) ? `<h4>${L('Bijlagen', 'Attachments')}</h4><ul>${data!.attachments.map((a,i)=>`<li><button class="btn ghost small" data-demo="attachment" data-index="${i}">${h(pickLang(a.name))} · ${fileSize(a.size)}</button></li>`).join('')}</ul>` : ''}
        <h4>${L('Log', 'Log')}</h4><ul class="arriva-log">${logHtml(data?.logs ?? [])}</ul>
      </details><div class="arriva-actions end"><button class="btn ghost" data-demo="close">${L('Sluiten', 'Close')}</button></div></div>`;
    dlg.querySelector('summary')?.addEventListener('click', () => { inspect = !inspect; });
  };
  const refresh = async () => { data = await call<DemoState>('arriva_demo_state'); };
  const updateProgressNodes = () => {
    const status = dlg.querySelector<HTMLElement>('.arriva-inline');
    if (status) status.innerHTML = `${busy ? L('Bezig… dit kan even duren.', 'Working… this may take a moment.') : h(pickLang(data?.last_error || loadError || phaseLabel()))}${data && !data.available ? `<br>${L('Arriva-demo is hier niet beschikbaar.', 'Arriva demo is not available here.')}` : ''}`;
    const steps = dlg.querySelector<HTMLOListElement>('.arriva-steps');
    if (steps && data) steps.innerHTML = stepsHtml(data.steps);
  };
  const act = async (action: string, index?: number) => {
    if (busy || action === 'close') { if (action === 'close') dlg.close(); return; }
    busy = true; render();
    pollActive = true;
    const poll = async () => {
      while (pollActive && !closed) {
        await new Promise(resolve => window.setTimeout(resolve, 700));
        if (!pollActive || closed) return;
        try { const latest = await invoke<DemoState>('arriva_demo_state'); if (closed) return; data = latest; updateProgressNodes(); } catch { /* command completion will surface any error */ }
      }
    };
    void poll();
    try {
      if (action === 'prepare') data = await call<DemoState>('arriva_demo_prepare');
      else if (action === 'check') await runCheck();
      else if (action === 'open') data = await call<DemoState>('arriva_demo_open', { view: 'captcha' });
      else if (action === 'form') data = await call<DemoState>('arriva_demo_open', { view: 'form' });
      else if (action === 'probe') data = await call<DemoState>('arriva_demo_probe');
      else if (action === 'reset') data = await call<DemoState>('arriva_demo_reset');
      else if (action === 'attachment') await call('arriva_demo_attachment', { index });
      await refresh();
    } catch (e) { if (data) data.last_error = friendlyError(e); else data = { available: false, demo_only: true, phase: 'error', steps: [], trip: { from: 'Venlo', to: 'Nijmegen', train_number: '', date: '', delay_minutes: 24, fare_cents: 1240, check_in: '', check_out: '' }, attachments: [], logs: [], last_error: friendlyError(e), window_open: false }; }
    finally { pollActive = false; busy = false; if (!closed) { render(); dlg.querySelector<HTMLElement>(`[data-demo="${action}"]`)?.focus({ preventScroll: true }); } }
  };
  dlg.addEventListener('click', e => { const b = (e.target as HTMLElement).closest<HTMLButtonElement>('[data-demo]'); if (b) void act(b.dataset.demo!, Number(b.dataset.index)); });
  dlg.addEventListener('click', e => { if ((e.target as HTMLElement).closest('[data-x="close"]')) dlg.close(); });
  dlg.addEventListener('cancel', e => { e.preventDefault(); dlg.close(); });
  dlg.addEventListener('close', () => { closed = true; pollActive = false; dlg.remove(); const fallback = document.querySelector<HTMLElement>('[data-act="arriva-demo"]'); (opener?.isConnected ? opener : fallback)?.focus({ preventScroll: true }); }, { once: true });
  try { await call('arriva_demo_enable'); } catch (e) { loadError = friendlyError(e); }
  try { await refresh(); } catch (e) { loadError = friendlyError(e); }
  render(); dlg.setAttribute('aria-labelledby', 'arrivaDemoTitle'); document.body.append(dlg); dlg.showModal(); installDialogTrap(); dlg.querySelector<HTMLElement>('.arriva-actions .primary')?.focus();
}
