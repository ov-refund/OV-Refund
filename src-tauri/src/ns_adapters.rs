//! Live Mijn NS adapters (journey source + claim gateway) on top of [`NsBridge`].
//! Flows and selectors were observed on 2026-11-12 (docs/integration-inventory.md).
use crate::ns_bridge::{Captured, NsBridge, SessionStatus};
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use ovrefund_core::model::*;
use ovrefund_core::ns_parse;
use ovrefund_core::pipeline::*;
use serde_json::Value;
use std::time::{Duration, Instant};

pub const GATEWAY_ID: &str = "mijnns-webview";

/// JS helper: query through open shadow roots.
const DEEPQ: &str = r#"(function(){ window.__ovq = function(sel){ const walk = (root) => { const f = root.querySelector(sel); if (f) return f; for (const el of root.querySelectorAll('*')) { if (el.shadowRoot) { const r = walk(el.shadowRoot); if (r) return r; } } return null; }; return walk(document); }; return true; })()"#;

fn wait_capture(b: &NsBridge, since: u64, timeout: Duration, pred: impl Fn(&Captured) -> bool) -> Option<Captured> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(c) = b.captured_since(since).into_iter().find(|c| pred(c)) {
            return Some(c);
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    None
}

/// How much history to read from Mijn NS (options of its own period filter).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchRange {
    /// Default view "Laatste 7 dagen": fastest, used for frequent incremental scans.
    Week,
    /// "Afgelopen jaar".
    Year,
    /// "Afgelopen 1,5 jaar": first scan.
    Max,
}

impl FetchRange {
    /// (label in the NS period filter, minimum days the response must cover)
    fn filter(&self) -> Option<(&'static str, i64)> {
        match self {
            FetchRange::Week => None,
            FetchRange::Year => Some(("Afgelopen jaar", 300)),
            FetchRange::Max => Some(("Afgelopen 1,5 jaar", 450)),
        }
    }
}

pub struct MyNsJourneySource {
    pub bridge: NsBridge,
    pub range: FetchRange,
}

fn is_tx(c: &Captured) -> bool {
    c.url.contains("/mijnns-transaction-api/couplings/") && c.url.contains("/transactions")
}

fn history_capture_matches(c: &Captured, range: FetchRange, coupling: Option<&str>) -> bool {
    is_tx(c)
        && coupling.map(|id| c.url.contains(id)).unwrap_or(true)
        // Preserve authentication/HTTP failures for the caller; never accept a successful
        // short response as a completed full-history import.
        && (c.status != 200 || range.filter().map(|(_, days)| covers_days(&c.url, days)).unwrap_or(true))
}

/// Wait for the travel-history response of the current view; for longer ranges select the period
/// in the official filter and press "Tonen", as a user would.
fn load_view(b: &NsBridge, since: u64, range: FetchRange, coupling: Option<&str>) -> Option<Captured> {
    let matches = |c: &Captured| is_tx(c) && coupling.map(|id| c.url.contains(id)).unwrap_or(true);
    let first = wait_capture(b, since, Duration::from_secs(45), matches)?;
    ovrefund_core::progress::report(15, "fetch");
    let Some((label, _)) = range.filter() else { return Some(first) };
    if history_capture_matches(&first, range, coupling) {
        return Some(first);
    }
    let m2 = b.mark();
    let js = format!(
        "(async () => {{ for (let n = 0; n < 100; n++) {{ \
         const s = document.querySelector('#filter-period-control'); \
         const i = s ? [...s.options].findIndex(o => o.text.trim() === {label:?}) : -1; \
         if (i >= 0 && !s.disabled) {{ s.selectedIndex = i; s.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }} \
         await new Promise(r => setTimeout(r, 200)); }} return false; }})()"
    );
    if b.eval(&js).ok() != Some(Value::Bool(true)) {
        // Only the filter's own option labels and the page route: no personal data.
        let seen = b
            .eval("(() => { const s = document.querySelector('#filter-period-control'); return JSON.stringify({route: location.hash.split('?')[0], select: !!s, disabled: s ? s.disabled : null, options: s ? [...s.options].map(o => o.text.trim()) : [], selects: [...document.querySelectorAll('select')].map(e => e.id || e.name || '?')}); })()")
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        log::warn!("requested history period filter unavailable; import not completed; filter: {seen}");
        return None;
    }
    let mut clicked = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(200));
        if b.click("[...document.querySelectorAll('mns-transaction-filter nes-button')].find(e => /Tonen/.test(e.innerText) && !e.hasAttribute('disabled'))").is_ok() {
            clicked = true;
            break;
        }
    }
    if !clicked {
        log::warn!("requested history period could not be applied; import not completed");
        return None;
    }
    let got = wait_capture(b, m2, Duration::from_secs(45), |c| history_capture_matches(c, range, coupling));
    if got.is_none() {
        // Which periods NS actually answered with (dates and status only, no card or account data).
        let seen: Vec<String> = b
            .captured_since(m2)
            .into_iter()
            .filter(is_tx)
            .map(|c| {
                let q = c.url.split('?').nth(1).unwrap_or("");
                let from = q.split('&').find_map(|kv| kv.strip_prefix("from=")).unwrap_or("-");
                format!("{} from={}", c.status, from.get(..10).unwrap_or(from))
            })
            .collect();
        log::warn!("requested history period not returned by NS; responses after Tonen: {seen:?}");
    }
    got
}

impl JourneySource for MyNsJourneySource {
    fn id(&self) -> &str {
        "mijnns"
    }

    fn fetch_journeys(&mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> std::result::Result<Vec<Journey>, FetchError> {
        // Technical failures (browser, page scripts) become one bilingual message with the detail.
        self.fetch_inner(from, to).map_err(|e| match e {
            FetchError::Other(e) => FetchError::Other(anyhow::anyhow!(bilingual(
                &e,
                "Je reishistorie ophalen bij Mijn NS lukte niet",
                "Fetching your travel history from Mijn NS failed"
            ))),
            other => other,
        })
    }
}

/// An OV-chipkaart number as NS sends it: 8 to 24 ASCII digits (only then are the last four
/// characters taken, so a strange value can never panic or name a credential oddly).
pub fn card_number_ok(n: &str) -> bool {
    (8..=24).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())
}

/// "NL (detail) / EN (detail)" for a technical error, unless the error is already bilingual.
pub fn bilingual(e: &anyhow::Error, nl: &str, en: &str) -> String {
    let m = e.to_string();
    if m.contains(" / ") {
        return m;
    }
    let d = ovrefund_core::redact::redact(&m).chars().take(160).collect::<String>().replace(" / ", "/");
    format!("{nl} ({d}). / {en} ({d}).")
}

impl MyNsJourneySource {
    fn fetch_inner(&mut self, _from: DateTime<Utc>, _to: DateTime<Utc>) -> std::result::Result<Vec<Journey>, FetchError> {
        let b = &self.bridge;
        match b.ensure_session(Duration::from_secs(45)).map_err(FetchError::Other)? {
            SessionStatus::LoggedIn => {}
            SessionStatus::LoginRequired => return Err(FetchError::LoginRequired),
            SessionStatus::Blocked(r) => return Err(FetchError::Blocked(r)),
            SessionStatus::AutoLoginFailed => return Err(FetchError::AutoLoginFailed),
        }
        ovrefund_core::progress::report(10, "fetch");
        // One NS account per Windows user: never mix trips of two NS accounts in one ledger.
        check_account(b).map_err(
            |e| {
                if e.to_string() == ACCOUNT_MISMATCH_MSG {
                    FetchError::AccountMismatch
                } else {
                    FetchError::Other(e)
                }
            },
        )?;
        let mut out: Vec<Journey> = vec![];
        let mark = b.mark();
        b.navigate("https://www.ns.nl/mijnns#/reishistorie").map_err(FetchError::Other)?;
        b.reload().map_err(FetchError::Other)?;
        // An NS account without a linked personal OV-chipkaart has no travel history to read.
        std::thread::sleep(Duration::from_secs(2));
        let no_card = b
            .captured_since(mark)
            .iter()
            .filter(|c| c.url.contains("/mijnns-card-coupling-api/v2/couplings/me") && c.status == 200)
            .filter_map(|c| c.body.as_deref())
            .filter_map(|body| serde_json::from_str::<Value>(body).ok())
            .any(|v| v.as_array().map(|a| a.is_empty()).unwrap_or(false));
        if let Ok(db) = crate::runner::open_db() {
            let _ = db.set_setting("no_card", if no_card { "1" } else { "0" });
        }
        if no_card {
            return Err(FetchError::Other(anyhow::anyhow!(ovrefund_core::pipeline::NO_CARD_ERROR)));
        }
        let Some(first) = load_view(b, mark, self.range, None) else {
            // Sent back to NS's login site: the session ended. Anything else (also a page whose
            // labels OV Refund does not know) is NS not answering, never "log in again".
            if let Ok(s) = b.page_state() {
                if crate::ns_bridge::is_ns_login_host(&s.host) {
                    return Err(FetchError::LoginRequired);
                }
            }
            return Err(FetchError::Other(anyhow::anyhow!(ovrefund_core::pipeline::NS_NO_RESPONSE_MSG)));
        };
        // Other coupled cards (from the couplings list the page loaded).
        let mut couplings: Vec<String> = b
            .captured_since(mark)
            .iter()
            .filter(|c| c.url.contains("/mijnns-card-coupling-api/") && c.status == 200)
            .filter_map(|c| c.body.as_deref())
            .filter_map(|body| serde_json::from_str::<Value>(body).ok())
            .flat_map(|v| v.as_array().cloned().unwrap_or_default())
            .filter(|c| c["access"].as_str().unwrap_or("GRANTED") == "GRANTED")
            .filter_map(|c| c["couplingId"].as_str().map(str::to_string))
            .collect();
        // Full card numbers -> Windows Credential Manager only (for the "view" button). Never in the DB/logs.
        for body in b
            .captured_since(mark)
            .iter()
            .filter(|c| c.url.contains("/mijnns-card-coupling-api/") && c.status == 200)
            .filter_map(|c| c.body.clone())
        {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                for t in v.as_array().into_iter().flatten().flat_map(|c| c["tokens"].as_array().cloned().unwrap_or_default()) {
                    if let Some(n) = t["cardNumber"].as_str().filter(|n| card_number_ok(n)) {
                        let _ = crate::secrets::set(&format!("ov-card-{}", &n[n.len() - 4..]), n);
                    }
                }
            }
        }
        let mut handle = |c: &Captured| -> std::result::Result<Option<String>, FetchError> {
            if c.status == 401 || c.status == 403 {
                return Err(FetchError::LoginRequired);
            }
            if c.status != 200 {
                return Err(FetchError::Blocked(format!("travel history returned HTTP {}", c.status)));
            }
            let coupling = ns_parse::coupling_id_from_url(&c.url);
            let body = c.body.as_deref().ok_or_else(|| FetchError::Other(anyhow::anyhow!("empty travel-history body")))?;
            let rep = ns_parse::parse_transactions(body, coupling.as_deref()).map_err(FetchError::Other)?;
            for _ in rep.skipped {
                log::warn!("skipped transaction: invalid structure");
            }
            out.extend(rep.journeys);
            Ok(coupling)
        };
        let done = handle(&first)?;
        ovrefund_core::progress::report(22, "fetch");
        couplings.retain(|c| Some(c) != done.as_ref());
        let total = couplings.len();
        for (index, coupling) in couplings.into_iter().enumerate() {
            let m = b.mark();
            b.navigate(&format!("https://www.ns.nl/mijnns#/reishistorie?card={coupling}")).map_err(FetchError::Other)?;
            b.reload().map_err(FetchError::Other)?;
            if let Some(c) = load_view(b, m, self.range, Some(&coupling)) {
                handle(&c)?;
            } else {
                return Err(FetchError::Other(anyhow::anyhow!(NS_NO_RESPONSE_MSG)));
            }
            ovrefund_core::progress::report(ovrefund_core::progress::within(22, 34, index + 1, total), "fetch");
        }
        Ok(out)
    }
}

#[derive(Clone)]
pub struct BankDetails {
    pub name: String,
    pub iban: String,
}

pub struct NsClaimGateway {
    pub bridge: NsBridge,
    pub verified: bool,
    pub bank: Option<BankDetails>,
}

const MONTHS_NL: [&str; 12] =
    ["januari", "februari", "maart", "april", "mei", "juni", "juli", "augustus", "september", "oktober", "november", "december"];

/// Open NS's journey-linked claim form the way a user does: travel history -> trip row ->
/// "Geld terug bij vertraging". (A direct link to the form route is not accepted by Mijn NS.)
/// The trip row is matched on date heading + departure time + origin station, and the opened
/// form route must contain the trip's NS transaction id.
fn open_claim_form(b: &NsBridge, f: &ClaimFacts) -> Result<()> {
    use chrono::{Datelike, Timelike};
    let local = f.check_in.with_timezone(&chrono_tz::Europe::Amsterdam);
    let date_txt = format!("{} {} {}", local.day(), MONTHS_NL[local.month0() as usize], local.year());
    let dep_txt = format!("{:02}:{:02}{}", local.hour(), local.minute(), f.origin_name);
    b.dismiss_cookie_banner();
    let wait_rows = "(async () => { for (let i = 0; i < 60 && !document.querySelector('mns-travel-history-list [tabindex]'); i++) await new Promise(r => setTimeout(r, 500)); return !!document.querySelector('mns-travel-history-list [tabindex]'); })()";
    b.eval("location.hash = '#/reishistorie'; true")?;
    anyhow::ensure!(b.eval(wait_rows)?.as_bool() == Some(true), "travel history did not load");
    // Older trips: choose a longer period in the official filter.
    let age = (Utc::now() - f.check_in).num_days();
    if age > 6 {
        let label = if age > 360 { "Afgelopen 1,5 jaar" } else { "Afgelopen jaar" };
        b.eval(&format!(
            "(() => {{ const s = document.querySelector('#filter-period-control'); const i = [...s.options].findIndex(o => o.text.trim() === {label:?}); s.selectedIndex = i; s.dispatchEvent(new Event('change', {{bubbles: true}})); return i }})()"
        ))?;
        std::thread::sleep(Duration::from_millis(800));
        b.click("[...document.querySelectorAll('mns-transaction-filter nes-button')].find(e => /Tonen/.test(e.innerText))")?;
    }
    // Find the row: heading (date) + departure "HH:MMStation" in the route element's shadow DOM.
    let find = format!(
        "(async () => {{ const want = {date:?}, dep = {dep:?};           for (let t = 0; t < 60; t++) {{             const l = document.querySelector('mns-travel-history-list'); let head = ''; let idx = -1, n = 0;             if (l) for (const el of l.querySelectorAll('nes-heading, [tabindex]')) {{               if (el.tagName.toLowerCase() === 'nes-heading') {{ head = el.innerText.toLowerCase(); continue; }}               const d = el.querySelector('nes-route[data-test-id=journeyDeparture]');               const txt = d && d.shadowRoot ? d.shadowRoot.textContent.trim() : '';               if (head.includes(want) && txt.replace(/ /g, '') === dep.replace(/ /g, '')) {{ idx = n; }} n++;             }}             if (idx >= 0) return idx; await new Promise(r => setTimeout(r, 500)); }} return -1; }})()",
        date = date_txt.to_lowercase(),
        dep = dep_txt
    );
    let idx = b.eval(&find)?.as_i64().unwrap_or(-1);
    anyhow::ensure!(idx >= 0, "trip {} {} not found in the NS travel history list", date_txt, dep_txt);
    let row = format!("document.querySelectorAll('mns-travel-history-list [tabindex]')[{idx}]");
    b.click(&row)?;
    std::thread::sleep(Duration::from_millis(1200));
    let link = format!("({row}).parentElement.querySelector('[data-test=gtbv-claim-link]') || ({row}).closest('ng-component').querySelector('[data-test=gtbv-claim-link]')");
    b.wait_until("refund link in trip details", Duration::from_secs(15), |b| {
        Ok(b.eval(&format!("!!({link})"))?.as_bool().unwrap_or(false))
    })?;
    b.click(&link)?;
    let tx = f.source_record_id.clone();
    b.wait_until("claim form for this trip", Duration::from_secs(30), |b| {
        Ok(b.eval("location.hash")?.as_str().map(|h| h.contains("geld-terug-bij-vertraging") && h.contains(&tx)).unwrap_or(false))
    })
    .map_err(|_| anyhow::anyhow!("the form that opened is not for this trip"))?;
    Ok(())
}

impl NsClaimGateway {
    /// Open the journey-linked claim form and fill everything except the declaration.
    /// Returns human-readable notes. Never submits.
    pub fn fill_form(&mut self, f: &ClaimFacts) -> Result<Vec<String>> {
        let b = &self.bridge;
        match b.ensure_session(Duration::from_secs(45))? {
            SessionStatus::LoggedIn => {}
            SessionStatus::LoginRequired => bail!(ovrefund_core::pipeline::LOGIN_NEEDED_MSG),
            SessionStatus::AutoLoginFailed => bail!(ovrefund_core::pipeline::AUTOLOGIN_FAILED_MSG),
            SessionStatus::Blocked(r) => {
                bail!("{}", ovrefund_core::pipeline::auth_message(&format!("blocked: {r}")).unwrap_or(ovrefund_core::pipeline::BLOCKED_MSG))
            }
        }
        // Only ever for the NS account this ledger belongs to (one NS account per Windows user).
        check_account(b)?;
        open_claim_form(b, f)?;
        b.wait_until("claim form", Duration::from_secs(45), |b| {
            b.eval(DEEPQ)?;
            Ok(b.eval("!!window.__ovq('#departure') && !!window.__ovq('#checkIn')")?.as_bool().unwrap_or(false))
        })?;
        let mut notes = vec![];
        // 1. Verify the prefilled check-in/out belong to this journey.
        let sel = b.eval("(() => { const ci = window.__ovq('#checkIn'), co = window.__ovq('#checkOut'); return {ci: ci && ci.value, co: co && co.value, cit: ci && ci.options[ci.selectedIndex] && ci.options[ci.selectedIndex].text, cot: co && co.options[co.selectedIndex] && co.options[co.selectedIndex].text} })()")?;
        let ci_iso = f.check_in.format("%Y-%m-%dT%H:%M:%S").to_string();
        let co_iso = f.check_out.format("%Y-%m-%dT%H:%M:%S").to_string();
        let ci_ok = sel["ci"].as_str().map(|s| s.contains(&ci_iso)).unwrap_or(false);
        let co_ok = sel["co"].as_str().map(|s| s.contains(&co_iso)).unwrap_or(false);
        if !ci_ok || !co_ok {
            bail!("De tijden in het NS-formulier horen niet bij deze reis. Er is niets verstuurd. / The times in NS's form do not match this trip. Nothing was sent.");
        }
        let (cit, cot) = (sel["cit"].as_str().unwrap_or(""), sel["cot"].as_str().unwrap_or(""));
        notes.push(format!(
            "Inchecken en uitchecken ingevuld door NS: {cit} – {cot} / Check-in and check-out prefilled by NS: {cit} – {cot}"
        ));
        // 2. Reached NS destination: yes / no
        b.click(if f.reached_destination { "window.__ovq('#arrival0')" } else { "window.__ovq('#arrival1')" })?;
        // 3. Scheduled departure (HH:MM, Europe/Amsterdam)
        let t = &f.scheduled_departure_local;
        anyhow::ensure!(t.len() == 5 && t.as_bytes()[2] == b':', "invalid scheduled departure {t}");
        b.click("window.__ovq('#departure')")?;
        let set = b.eval(&format!(
            "(() => {{ const i = window.__ovq('#departure'); const s = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; s.call(i, '{t}'); i.dispatchEvent(new Event('input', {{bubbles:true}})); i.dispatchEvent(new Event('change', {{bubbles:true}})); i.dispatchEvent(new Event('blur', {{bubbles:true}})); return i.value }})()"
        ))?;
        anyhow::ensure!(set.as_str() == Some(t.as_str()), "could not set scheduled departure");
        notes.push(format!("Geplande vertrektijd {t} / Scheduled departure {t}"));
        // 4. Delay band
        b.click(match f.delay_band {
            DelayBand::Min30To59 => "window.__ovq('#delay0')",
            DelayBand::Min60Plus => "window.__ovq('#delay1')",
        })?;
        notes.push(format!("Vertraging: {} / Delay: {}", f.delay_band.label_nl(), f.delay_band.label_en()));
        // 5. Bank details: always the stored name + IBAN (NS may prefill an older account), typed
        //    via trusted input after clearing the field; never logged. The IBAN is read back.
        if let Some(bank) = self.bank.clone() {
            for (sel, val) in [("#name", bank.name.as_str()), ("#iban", bank.iban.as_str())] {
                let cleared = b.eval(&format!(
                    "(() => {{ const i = window.__ovq('{sel}'); if (!i) return false; const s = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; s.call(i, ''); i.dispatchEvent(new Event('input', {{bubbles: true}})); return i.value === '' }})()"
                ))?;
                // The field must exist and be empty before typing (no leftover of an older account).
                if cleared.as_bool() != Some(true) {
                    bail!(FORM_CHANGED_MSG);
                }
                b.click(&format!("window.__ovq('{sel}')"))?;
                // Verify the top-level NS origin and the exact focused field immediately before typing.
                let url = b.window.url()?;
                if url.scheme() != "https" || url.host_str() != Some("www.ns.nl") {
                    bail!(FORM_CHANGED_MSG);
                }
                let focused_ok = b.eval(&bank_focus_script(sel))?.as_bool().unwrap_or(false);
                if !focused_ok {
                    bail!(FORM_CHANGED_MSG);
                }
                b.cdp("Input.insertText", serde_json::json!({ "text": val }), Duration::from_secs(10))?;
            }
            let shown = b.eval(
                "(() => { const q = window.__ovq; return { iban: (q('#iban')||{}).value || '', name: (q('#name')||{}).value || '' } })()",
            )?;
            if !iban_matches(shown["iban"].as_str().unwrap_or(""), &bank.iban) {
                bail!(IBAN_MISMATCH_MSG);
            }
            if !holder_matches(shown["name"].as_str().unwrap_or(""), &bank.name) {
                bail!(NAME_MISMATCH_MSG);
            }
            notes.push(
                "Rekeninggegevens ingevuld vanuit Windows Referentiebeheer / Bank details entered from Windows Credential Manager".into(),
            );
        } else {
            notes.push("Rekeninggegevens zijn NIET ingesteld / Bank details are NOT set up".into());
        }
        // 6. Read back radio state + validation messages
        let st = b.eval("(() => { const q = window.__ovq; return { arrival: (q('#arrival0')||{}).checked, d0: (q('#delay0')||{}).checked, d1: (q('#delay1')||{}).checked, dep: (q('#departure')||{}).value, nameFilled: !!(q('#name')||{}).value, ibanFilled: !!(q('#iban')||{}).value, errors: [...document.querySelectorAll('[role=alert], .error, nes-message-inline[type=error]')].map(e => e.innerText.trim()).filter(Boolean).slice(0,5) } })()")?;
        let band_ok = match f.delay_band {
            DelayBand::Min30To59 => st["d0"].as_bool() == Some(true),
            DelayBand::Min60Plus => st["d1"].as_bool() == Some(true),
        };
        anyhow::ensure!(band_ok, "delay radio not selected");
        anyhow::ensure!(st["dep"].as_str() == Some(t.as_str()), "departure field lost its value");
        if !(st["nameFilled"].as_bool().unwrap_or(false) && st["ibanFilled"].as_bool().unwrap_or(false)) {
            notes.push(
                "Naam of IBAN is leeg; het formulier kan niet worden verzonden / Name or IBAN is empty; the form cannot be submitted"
                    .into(),
            );
        }
        for e in st["errors"].as_array().cloned().unwrap_or_default() {
            let msg = ovrefund_core::redact::redact(e.as_str().unwrap_or(""));
            notes.push(format!("Melding van het NS-formulier: {msg} / Message from the NS form: {msg}"));
        }
        Ok(notes)
    }
}

/// A fill-in problem as shown to the user (bilingual reasons are kept as they are).
fn form_error(e: &anyhow::Error) -> String {
    bilingual(
        e,
        "Het NS-formulier kon niet worden geopend of ingevuld; er is niets verstuurd",
        "NS's form could not be opened or filled in; nothing was sent",
    )
}

pub const IBAN_MISMATCH_MSG: &str = "Het IBAN in het NS-formulier wijkt af van je opgeslagen rekening. Er is niets verstuurd. / The IBAN in NS's form differs from your saved bank account. Nothing was sent.";
pub const NAME_MISMATCH_MSG: &str = "De naam van de rekeninghouder in het NS-formulier wijkt af van je opgeslagen naam. Er is niets verstuurd. / The account holder's name in NS's form differs from your saved name. Nothing was sent.";

/// History imports and forms require a readable account matching the ledger binding.
/// Unknown identity fails closed even before the ledger has its first account binding.
fn check_account(b: &NsBridge) -> Result<()> {
    let db = crate::runner::open_db()?;
    let stored = db.get_setting("ns_account_email")?.filter(|s| !s.trim().is_empty());
    let mut email = None;
    for _ in 0..10 {
        email = account_email(b);
        if email.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let result = account_binding(stored.as_deref(), email.as_deref());
    if let Err(e) = &result {
        if e.to_string() == ACCOUNT_MISMATCH_MSG {
            crate::secrets::delete(crate::secrets::NS_LOGIN)?;
            db.set_setting("auto_login", "0")?;
            let _ = db.upsert_source("mijnns", "mijnns", false, "account_mismatch");
        } else {
            let _ = db.set_setting("account_unreadable_at", &Utc::now().to_rfc3339());
        }
    }
    result
}

fn account_binding(stored: Option<&str>, email: Option<&str>) -> Result<()> {
    match (stored, email) {
        (Some(s), Some(e)) if !s.trim().eq_ignore_ascii_case(e.trim()) => {
            bail!(ACCOUNT_MISMATCH_MSG)
        }
        (_, None) => {
            bail!(ACCOUNT_UNKNOWN_MSG)
        }
        _ => Ok(()),
    }
}
pub const FORM_CHANGED_MSG: &str = "Het NS-formulier is veranderd of vraagt iets extra. Er is niets verstuurd. Dien deze claim zelf in via Mijn NS, of wacht op een update. / NS's form changed or asks for something extra. Nothing was sent. Submit this claim yourself in Mijn NS, or wait for an update.";

/// Is the IBAN shown in NS's form exactly the stored one (spaces and case ignored; an empty field
/// never matches)?
pub fn iban_matches(shown: &str, stored: &str) -> bool {
    let norm = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
    let (a, b) = (norm(shown), norm(stored));
    a.len() >= 15 && a == b
}

/// Is the account holder's name shown in NS's form the stored one (trimmed, case-insensitive,
/// runs of whitespace as one space; an empty field never matches)?
pub fn holder_matches(shown: &str, stored: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let (a, b) = (norm(shown), norm(stored));
    !a.is_empty() && a == b
}

/// Everything on NS's form right before "Verzenden": validation messages, fields marked invalid,
/// and visible required fields OV Refund does not know (also inside shadow roots).
const FORM_CHECK_JS: &str = r#"(() => {
  const known = ['checkIn', 'checkOut', 'arrival0', 'arrival1', 'departure', 'delay0', 'delay1', 'name', 'iban', 'declaration'];
  const all = [];
  const walk = (r) => { for (const el of r.querySelectorAll('*')) { all.push(el); if (el.shadowRoot) walk(el.shadowRoot); } };
  walk(document);
  const vis = (el) => !!(el.offsetWidth || el.offsetHeight || (el.getClientRects && el.getClientRects().length));
  const invalid = all.filter((el) => el.getAttribute && el.getAttribute('aria-invalid') === 'true' && vis(el)).map((el) => el.id || el.name || el.tagName.toLowerCase());
  const errors = all.filter((el) => el.matches && el.matches('[role=alert], .error, nes-message-inline[type=error]') && vis(el)).map((e) => (e.innerText || '').trim()).filter(Boolean);
  const extra = all.filter((el) => el.matches && el.matches('input, select, textarea') && el.type !== 'hidden' && (el.required || (el.getAttribute && el.getAttribute('aria-required') === 'true')) && vis(el) && !known.includes(el.id)).map((el) => el.id || el.name || el.type);
  return { invalid, errors: errors.slice(0, 5), extra };
})()"#;

/// Problems found by [`FORM_CHECK_JS`] (empty = safe to press "Verzenden").
pub fn form_problems(v: &Value) -> Vec<String> {
    if ["errors", "invalid", "extra"].iter().any(|k| v[*k].as_array().map(|a| a.iter().any(|x| !x.is_string())).unwrap_or(true)) {
        return vec!["form could not be checked".into()];
    }
    let list = |k: &str| {
        v[k].as_array().cloned().unwrap_or_default().into_iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>()
    };
    let mut out = vec![];
    out.extend(list("errors").into_iter().map(|_| "validation message".to_string()));
    out.extend(list("invalid").into_iter().map(|_| "invalid field".to_string()));
    out.extend(list("extra").into_iter().map(|_| "unknown required field".to_string()));
    if v.is_null() {
        out.push("form could not be checked".into());
    }
    out
}

fn find_reference(v: &Value) -> Option<String> {
    for k in ["reference", "referenceNumber", "claimReference", "caseNumber", "claimId", "id", "requestId"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str().map(str::to_string).or_else(|| x.as_i64().map(|n| n.to_string()))) {
            return Some(s);
        }
    }
    None
}

/// Is a 2xx answer to the claim write proof that NS received the claim? Returns
/// (reference to store, proven). Proof is a claim-specific reference field in the response, or
/// confirmation text that appeared on the page after the click (not already on the form).
/// A generic `id`/`requestId` alone is not proof (it could identify anything); it is only kept as
/// the reference when the confirmation text is there as well.
/// Words on the page after "Verzenden" that mean the claim was (possibly) NOT received.
const NEGATIVE_PAGE_WORDS: [&str; 9] =
    ["niet verzonden", "niet ontvangen", "mislukt", "fout", "niet gelukt", "probeer het later", "error", "failed", "not sent"];

/// Is this captured request the claim submission itself? The write endpoint has not been observed
/// live yet (docs/integration-inventory.md): the form belongs to `mijnns-claim-api/gtbv` on NS's
/// API gateway, so only a POST to that API path (not its settings, validation or upload
/// helpers) counts. Anything else after the click leaves the outcome uncertain.
pub(crate) fn is_claim_submit_write(method: &str, url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let Some(rest) = path.strip_prefix("https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv") else { return false };
    let rest = rest.to_lowercase();
    method == "POST"
        && (rest.is_empty() || rest.starts_with('/'))
        && !["settings", "valid", "check", "upload", "attachment", "document", "draft", "preview", "calculat", "concept"]
            .iter()
            .any(|w| rest.contains(w))
}

fn submit_proof(body: Option<&str>, page_before: &str, page_after: &str) -> (Option<String>, bool) {
    let v = body.and_then(|b| serde_json::from_str::<Value>(b).ok());
    let specific = v.as_ref().and_then(|v| {
        ["reference", "referenceNumber", "claimReference", "caseNumber", "claimId"].iter().find_map(|k| {
            v.get(*k)
                .and_then(|x| x.as_str().map(str::to_string).or_else(|| x.as_i64().map(|n| n.to_string())))
                .filter(|s| !s.trim().is_empty())
        })
    });
    let (before, after) = (page_before.to_lowercase(), page_after.to_lowercase());
    // Any sign of failure on the page after the click ("niet verzonden", "mislukt", ...) means the
    // confirmation words prove nothing.
    let negative = NEGATIVE_PAGE_WORDS.iter().any(|k| after.contains(k));
    let confirmation_page = !negative
        && ["ontvangen", "bedankt", "verzonden", "we hebben je aanvraag"].iter().any(|k| after.contains(k) && !before.contains(k));
    let proven = specific.is_some() || confirmation_page;
    let reference = specific.or_else(|| if confirmation_page { v.as_ref().and_then(find_reference) } else { None });
    (reference, proven)
}

impl ClaimGateway for NsClaimGateway {
    fn id(&self) -> &str {
        GATEWAY_ID
    }
    fn verified_for_auto(&self) -> bool {
        self.verified
    }
    fn preflight(&mut self, facts: &ClaimFacts) -> Result<Vec<String>> {
        let mark = self.bridge.mark();
        let r = self.fill_form(facts);
        // Leave the form without submitting.
        let _ = self.bridge.navigate("https://www.ns.nl/mijnns#/reishistorie");
        // Same guard as submit(): a claim write seen while only trying the form means the claim
        // may have reached NS. The caller records it as uncertain; nothing continues.
        if self.bridge.claim_write_since(mark) {
            anyhow::bail!(WriteDuringPreflight);
        }
        r.map_err(|e| anyhow::anyhow!(form_error(&e)))
    }

    fn submit(&mut self, facts: &ClaimFacts) -> SubmitOutcome {
        let mark = self.bridge.mark();
        let outcome = self.submit_inner(facts);
        guard_not_sent(outcome, self.bridge.claim_write_since(mark))
    }

    fn list_remote_claims(&mut self) -> Result<Option<Vec<RemoteClaim>>> {
        self.list_remote_claims_inner()
    }
}

/// A claim write was observed during preflight ("Eerst proberen"): the claim may have reached NS.
#[derive(Debug)]
pub struct WriteDuringPreflight;
impl std::fmt::Display for WriteDuringPreflight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(ovrefund_core::db::PREFLIGHT_WRITE_DETAIL)
    }
}
impl std::error::Error for WriteDuringPreflight {}

/// A captured claim write always makes an otherwise retryable result uncertain.
fn guard_not_sent(outcome: SubmitOutcome, wrote: bool) -> SubmitOutcome {
    match outcome {
        SubmitOutcome::NotSent(_) if wrote => SubmitOutcome::Unknown("Tijdens het invullen ging er mogelijk een claim naar NS. Kijk in Mijn teruggaven. / A claim may have been sent to NS while the form was being filled in. Check Mijn teruggaven.".into()),
        other => other,
    }
}

impl NsClaimGateway {
    fn submit_inner(&mut self, facts: &ClaimFacts) -> SubmitOutcome {
        // Always the account stored NOW (it may have been changed or removed since the run started).
        self.bank = crate::secrets::bank_details();
        if self.bank.is_none() {
            return SubmitOutcome::NotSent("Rekening niet ingesteld / Bank account not set".into());
        }
        let notes = match self.fill_form(facts) {
            Ok(n) => n,
            // Bilingual reasons ("NL / EN") are shown to the user as they are.
            Err(e) => return SubmitOutcome::NotSent(form_error(&e)),
        };
        if notes.iter().any(|n| n.contains("cannot be submitted")) {
            return SubmitOutcome::NotSent("naam of IBAN ontbreekt in het NS-formulier / name or IBAN missing in NS's form".into());
        }
        let b = &self.bridge;
        // Truthfulness declaration: only reached for user-authorized or opted-in, evidence-supported claims.
        if let Err(e) = b.click("window.__ovq('#declaration')") {
            let d = ovrefund_core::redact::redact(&e.to_string()).replace(" / ", "/");
            return SubmitOutcome::NotSent(format!(
                "de verklaring in het NS-formulier kon niet worden aangevinkt ({d}) / the declaration in NS's form could not be ticked ({d})"
            ));
        }
        if b.eval("(window.__ovq('#declaration')||{}).checked").ok().and_then(|v| v.as_bool()) != Some(true) {
            return SubmitOutcome::NotSent(
                "de verklaring in het NS-formulier is niet aangevinkt / the declaration in NS's form is not ticked".into(),
            );
        }
        let main_text = |b: &NsBridge| {
            b.eval("document.querySelector('main') ? document.querySelector('main').innerText.slice(0, 600) : ''")
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        };
        // The form's own text: confirmation words already on it are no proof of anything.
        let page_before = main_text(b);
        let mark = b.mark();
        // Point of no return: the click below may reach NS.
        // Before the click: if the button isn't there, nothing can have been sent.
        if b.eval("!!window.__ovq('button[type=submit]')").ok().and_then(|v| v.as_bool()) != Some(true) {
            return SubmitOutcome::NotSent("de knop Verzenden ontbreekt / the Verzenden button is missing".into());
        }
        // Validation messages, invalid fields or required fields OV Refund does not know: the form
        // changed or wants more. Nothing is clicked.
        let problems = form_problems(&b.eval(FORM_CHECK_JS).unwrap_or(Value::Null));
        if !problems.is_empty() {
            log::warn!("NS form not submitted: validation failed ({} checks)", problems.len());
            return SubmitOutcome::NotSent(FORM_CHANGED_MSG.into());
        }
        // From here on any failure is UNCERTAIN: the click may have reached NS.
        if let Err(e) = b.click("window.__ovq('button[type=submit]')") {
            if e.is::<crate::ns_bridge::ClickNotDispatched>() {
                return SubmitOutcome::NotSent(e.to_string());
            }
            let d = ovrefund_core::redact::redact(&e.to_string()).replace(" / ", "/");
            return SubmitOutcome::Unknown(format!(
                "fout bij het klikken op Verzenden ({d}); kijk in Mijn teruggaven / error while clicking Verzenden ({d}); check Mijn teruggaven"
            ));
        }
        // Only a write request that STARTED after the click counts (seq is assigned at request start).
        let write = wait_capture(b, mark, Duration::from_secs(60), |c| is_claim_submit_write(&c.method, &c.url));
        std::thread::sleep(Duration::from_secs(3));
        let page = main_text(b);
        match write {
            Some(c) if (200..300).contains(&c.status) => {
                // A 2xx alone is not proof (the write endpoint was never observed live): require a
                // claim reference or NS's confirmation text, otherwise let the claim list settle it.
                let (reference, proven) = submit_proof(c.body.as_deref(), &page_before, &page);
                if !proven {
                    return SubmitOutcome::Unknown(format!(
                        "NS antwoordde HTTP {0} zonder nummer of bevestiging; kijk in Mijn teruggaven / NS answered HTTP {0} without a reference or confirmation; check Mijn teruggaven",
                        c.status
                    ));
                }
                SubmitOutcome::Confirmed(SubmissionReceipt {
                    reference,
                    confirmed: true,
                    detail: {
                        // Page text can contain names and addresses that a token redactor cannot
                        // recognise. Keep only protocol metadata in audit history and backups.
                        let d = format!("{} HTTP {}", c.method, c.status);
                        format!("Door NS ontvangen: {d} / Received by NS: {d}")
                    },
                })
            }
            // The write endpoint has never been observed live: treat a rejection as uncertain until it has.
            Some(c) if c.status == 400 || c.status == 422 => SubmitOutcome::Unknown(format!(
                "NS weigerde het formulier (HTTP {0}); kijk in Mijn teruggaven / NS rejected the form (HTTP {0}); check Mijn teruggaven",
                c.status
            )),
            Some(c) => SubmitOutcome::Unknown(format!(
                "NS antwoordde HTTP {0}; kijk in Mijn teruggaven / NS answered HTTP {0}; check Mijn teruggaven",
                c.status
            )),
            // After the click we cannot prove the claim did NOT reach NS (the write endpoint has never
            // been observed live), so this is always "uncertain" — never a retryable "not sent".
            None => SubmitOutcome::Unknown(
                "geen bevestiging van NS ontvangen; kijk in Mijn teruggaven / no confirmation from NS; check Mijn teruggaven".into(),
            ),
        }
    }

    fn list_remote_claims_inner(&mut self) -> Result<Option<Vec<RemoteClaim>>> {
        let b = &self.bridge;
        let mark = b.mark();
        // The guard limits metadata-only diagnostics to this operation, including error exits.
        let diagnostics = crate::ns_bridge::ClaimsDiagnostics::start();
        let on_mijnns = b.eval("location.origin === 'https://www.ns.nl' && /^\\/mijnns\\/?$/.test(location.pathname)")?;
        if on_mijnns == true {
            b.eval("location.hash = '#/claims'; true")?;
        } else {
            b.navigate("https://www.ns.nl/mijnns#/claims")?;
        }
        let started = Instant::now();
        let mut reapplied = false;
        let mut empty_since = None;
        let found = loop {
            if let Some(c) = b.captured_since(mark).into_iter().find(|c| c.url.contains("/mijnns-claim-api/gtbv/claims")) {
                break Some(c);
            }
            let mut page = b.eval(CLAIMS_PAGE_DIAGNOSTICS_JS).unwrap_or(Value::Null);
            let empty = page["visible_text"].as_str().is_some_and(empty_claims_text);
            // Retain only booleans for diagnostics; never log page text.
            if let Some(obj) = page.as_object_mut() {
                obj.remove("visible_text");
                obj.insert("empty_visible".into(), Value::Bool(empty));
            }
            if empty_claims_page(&page) {
                let since = empty_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(2) {
                    if let Some(c) = b.captured_since(mark).into_iter().find(|c| c.url.contains("/mijnns-claim-api/gtbv/claims")) {
                        break Some(c);
                    }
                    log::info!("claims list: empty (page text)");
                    return Ok(Some(vec![]));
                }
            } else {
                empty_since = None;
            }
            if !reapplied && started.elapsed() >= Duration::from_secs(5) && !claims_route(&page) {
                // Only change the SPA route on the official Mijn NS document.
                b.eval("if (location.origin === 'https://www.ns.nl' && /^\\/mijnns\\/?$/.test(location.pathname)) location.hash = '#/claims'; true")?;
                reapplied = true;
            }
            if started.elapsed() >= Duration::from_secs(40) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        drop(diagnostics);
        let Some(c) = found else {
            let captures = b.captured_since(mark);
            if captures.iter().any(|c| customer_api_unavailable(&c.url, c.status)) {
                log::warn!("NS customer service API unavailable (5xx); Mijn teruggaven may not load");
            }
            log::warn!("claims list: no response; {} API response(s) observed", captures.len());
            return Ok(None);
        };
        if c.status != 200 {
            bail!("Mijn teruggaven gaf een fout (HTTP {0}). / Mijn teruggaven returned an error (HTTP {0}).", c.status);
        }
        // Unknown or unparseable shape = "list unavailable" (never "NS has no claims").
        let Some(v) = c.body.as_deref().and_then(|b| serde_json::from_str::<Value>(b).ok()) else {
            log::warn!("claims list body missing or not JSON");
            return Ok(None);
        };
        let parsed = parse_remote_claims(&v);
        if parsed.is_none() {
            log::warn!("claims list shape not recognised; duplicate check unavailable; shape: {}", json_shape(&v, 0));
        }
        Ok(parsed)
    }
}

/// Visible deep text is transient: the caller replaces it with a boolean before diagnostics.
const CLAIMS_PAGE_DIAGNOSTICS_JS: &str = r#"(() => {
    const visible = e => !!e.getClientRects().length && getComputedStyle(e).visibility !== 'hidden'
        && getComputedStyle(e).display !== 'none';
    const all = [];
    const walk = root => { for (const e of root.querySelectorAll('*')) {
        all.push(e); if (e.shadowRoot) walk(e.shadowRoot);
    }};
    walk(document);
    const text = all.filter(e => visible(e) && !e.closest('script,style,template'))
        .flatMap(e => [...e.childNodes].filter(n => n.nodeType === Node.TEXT_NODE).map(n => n.textContent)).join(' ');
    const headingVisible = all.some(e => visible(e) && e.matches('h1,h2,h3,h4,[role=heading],nes-heading')
        && /teruggaven/i.test(e.textContent + (e.shadowRoot?.textContent || '')));
    const errorVisible = all.some(e => e.matches('[role=alert],.error,.alert-danger,.notification--error,[aria-invalid=true]') && visible(e));
    return { location: location.origin + location.pathname + location.hash.split('?')[0],
        claims_visible: /teruggaven|claims/i.test(text), heading_visible: headingVisible,
        error_visible: errorVisible, visible_text: text,
        title: /^[A-Za-z0-9 |\-\u2013]{1,60}$/.test(document.title) ? document.title : null };
})()"#;

fn empty_claims_text(text: &str) -> bool {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    ["er zijn geen teruggaven bekend", "you have no refunds", "no refunds are known", "no refunds found"]
        .iter()
        .any(|sentence| text.contains(sentence))
}

fn claims_route(page: &Value) -> bool {
    page["location"].as_str().and_then(|s| tauri::Url::parse(s).ok()).is_some_and(|u| {
        u.origin().ascii_serialization() == "https://www.ns.nl"
            && matches!(u.path(), "/mijnns" | "/mijnns/")
            && u.fragment().is_some_and(|h| h == "/claims" || h.starts_with("/claims?") || h.starts_with("/claims/"))
    })
}

fn empty_claims_page(page: &Value) -> bool {
    // A detail route cannot prove the entire list is empty.
    claims_route(page)
        && page["location"]
            .as_str()
            .and_then(|s| tauri::Url::parse(s).ok())
            .is_some_and(|u| u.fragment().is_some_and(|h| h.split('?').next() == Some("/claims")))
        && page["empty_visible"] == true
        && page["error_visible"] == false
}

fn customer_api_unavailable(url: &str, status: i64) -> bool {
    (500..600).contains(&status)
        && tauri::Url::parse(url)
            .ok()
            .map(|u| u.host_str() == Some("gateway.apiportal.ns.nl") && u.path().split('/').any(|s| s == "omni-customer-api"))
            .unwrap_or(false)
}

#[cfg(test)]
fn claims_page_summary(page: &Value) -> String {
    let location = page["location"]
        .as_str()
        .and_then(|s| tauri::Url::parse(s).ok())
        .map(|u| {
            // Short alphabetic route names are public; ids and encoded values are redacted.
            let route = |s: &str| {
                s.split('/')
                    .map(|part| if part.len() <= 20 && part.bytes().all(|b| b.is_ascii_alphabetic()) { part } else { ":id" })
                    .collect::<Vec<_>>()
                    .join("/")
            };
            let origin = match u.host_str() {
                Some("www.ns.nl" | "login.ns.nl" | "loginapi.ns.nl") => u.origin().ascii_serialization(),
                _ => "[other origin]".into(),
            };
            let hash = u.fragment().map(|h| format!("#{}", route(h.split('?').next().unwrap_or("")))).unwrap_or_default();
            format!("{origin}{}{hash}", route(u.path()))
        })
        .unwrap_or_else(|| "unavailable".into());
    let flag = |key: &str| page[key].as_bool().map(|b| b.to_string()).unwrap_or_else(|| "unknown".into());
    let title = page["title"]
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= 60 && s.chars().all(|c| c.is_ascii_alphanumeric() || " |-\u{2013}".contains(c)))
        .unwrap_or("unavailable");
    format!("location={location}; claims_text_visible={}; error_element_visible={}; teruggaven_heading_visible={}; empty_list_text_visible={}; title={title}",
        flag("claims_visible"), flag("error_visible"), flag("heading_visible"), flag("empty_visible"))
}

/// Path of an NS API URL with ids, numbers and long tokens replaced by ":id" and no query
/// string, for diagnostics (never contains personal data).
pub fn anon_path(url: &str) -> String {
    // Diagnostics may already contain a sanitised relative path.
    let parsed =
        if url.starts_with('/') { tauri::Url::parse("https://www.ns.nl").and_then(|base| base.join(url)) } else { tauri::Url::parse(url) };
    let Ok(parsed) = parsed else { return "[invalid path]".into() };
    parsed
        .path()
        .split('/')
        .map(|seg| match seg {
            ""
            | "mijnns-claim-api"
            | "mijnns-transaction-api"
            | "mijnns-card-coupling-api"
            | "mijnns-invoice-api"
            | "mijnns-payment-api"
            | "omni-customer-api"
            | "api"
            | "gtbv"
            | "claims"
            | "transactions"
            | "couplings"
            | "customers"
            | "me"
            | "details"
            | "v1"
            | "v2"
            | "v3" => seg,
            _ => ":id",
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Structure without payloads or object keys: field counts and value types, arrays as their
/// length plus the shape of the first element. Strings show only "str" and whether they look like
/// a date. For diagnosing an unrecognised NS response without logging personal data.
pub fn json_shape(v: &Value, depth: usize) -> String {
    if depth > 4 {
        return "…".into();
    }
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(_) => "num".into(),
        Value::String(x) => {
            let date = x.len() >= 10 && x.as_bytes().get(4) == Some(&b'-') && x.as_bytes().get(7) == Some(&b'-');
            if date {
                "date-str".into()
            } else {
                "str".into()
            }
        }
        Value::Array(a) => match a.first() {
            Some(f) => format!("[{}× {}]", a.len(), json_shape(f, depth + 1)),
            None => "[]".into(),
        },
        Value::Object(o) => {
            format!("object({} fields)", o.len())
        }
    }
}

/// Unknown or negated status text never proves approval or payment.
fn remote_status(raw: &str) -> ClaimState {
    let text = raw.to_lowercase().replace(['_', '-'], " ");
    let words: Vec<&str> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |wanted: &[&str]| words.iter().any(|w| wanted.contains(w));
    let rejection = ["afgewezen", "rejected", "reject", "denied"];
    let negated_rejection = words.windows(2).any(|w| ["not", "niet", "geen"].contains(&w[0]) && rejection.contains(&w[1]));
    if has(&rejection) && !negated_rejection {
        return ClaimState::Rejected;
    }
    // A negative qualifier may refer to any of the outcome words in the description.
    // Preserve only explicit, unambiguous outcomes; the original text stays in raw_status.
    if has(&["not", "niet", "unpaid", "unapproved", "pending", "nog"]) {
        return ClaimState::Submitted;
    }
    if has(&["uitbetaald", "paid"]) {
        ClaimState::Paid
    } else if has(&["toegekend", "goedgekeurd", "approved", "accepted"]) {
        ClaimState::Approved
    } else {
        ClaimState::Submitted
    }
}

/// Defensive parser: the populated shape of `gtbv/claims` has not been observed yet
/// (the account had no claims on 2026-11-12). Unknown statuses map to SUBMITTED.
/// Returns `None` (= list unavailable) when the shape is not recognised or any entry cannot be
/// tied to a trip (no travel date and no transaction id): duplicate protection fails closed.
pub fn parse_remote_claims(v: &Value) -> Option<Vec<RemoteClaim>> {
    let items: Vec<Value> = match v {
        Value::Array(a) => a.clone(),
        // Only one page of a longer list: incomplete, so unavailable (duplicate check fails closed).
        Value::Object(o) if more_pages(o) => return None,
        Value::Object(o) => ["claims", "content", "items", "data", "results", "gtbvClaims"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_array).cloned())?,
        _ => return None,
    };
    let out: Vec<RemoteClaim> = items
        .iter()
        .map(|it| {
            let raw = ["status", "state", "claimStatus", "statusDescription"]
                .iter()
                .find_map(|k| it.get(*k).and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            let state = Some(remote_status(&raw));
            let date = ["travelDate", "journeyDate", "date", "departureDate", "checkInDate"]
                .iter()
                .find_map(|k| it.get(*k).and_then(Value::as_str))
                .and_then(|d| chrono::NaiveDate::parse_from_str(d.get(..10).unwrap_or(d), "%Y-%m-%d").ok());
            let tx = ["transactionId", "journeyId", "tripId"].iter().find_map(|k| it.get(*k).and_then(Value::as_str)).map(str::to_string);
            let amount_cents = parse_amount_cents(it);
            RemoteClaim {
                reference: find_reference(it),
                travel_date: date,
                origin_uic: None,
                destination_uic: None,
                transaction_id: tx,
                state,
                raw_status: if raw.is_empty() { "listed".into() } else { raw },
                amount_cents,
            }
        })
        .collect();
    out.iter().all(|r| r.travel_date.is_some() || r.transaction_id.is_some()).then_some(out)
}

/// Does a list response say there are more pages than this one? (Spring `totalPages`/`last`,
/// `hasNext`-style flags, a `next` link or cursor, or a total larger than the page.)
fn more_pages(o: &serde_json::Map<String, Value>) -> bool {
    let flag = |m: &serde_json::Map<String, Value>| -> bool {
        let total_pages = ["totalPages", "pageCount", "numberOfPages"].iter().find_map(|k| m.get(*k).and_then(Value::as_i64));
        let size = ["numberOfElements", "size", "pageSize", "count"].iter().find_map(|k| m.get(*k).and_then(Value::as_i64));
        let total = ["totalElements", "totalCount", "total", "totalItems"].iter().find_map(|k| m.get(*k).and_then(Value::as_i64));
        let next_link = |x: &Value| match x {
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Bool(b) => *b,
            _ => true,
        };
        total_pages.map(|t| t > 1).unwrap_or(false)
            || m.get("last").and_then(Value::as_bool) == Some(false)
            || ["hasNext", "hasMore", "hasNextPage", "moreResults"].iter().any(|k| m.get(*k).and_then(Value::as_bool) == Some(true))
            || ["next", "nextPage", "nextPageToken", "nextCursor", "nextLink", "cursor"]
                .iter()
                .any(|k| m.get(*k).map(next_link).unwrap_or(false))
            || matches!((total, size), (Some(t), Some(s)) if t > s)
    };
    if flag(o) {
        return true;
    }
    // nested paging objects: {"page": {...}}, {"pagination": {...}}, {"_links": {"next": ...}}, {"links": [{"rel": "next"}]}
    for k in ["page", "pagination", "paging", "pageable", "meta"] {
        if let Some(Value::Object(m)) = o.get(k) {
            if flag(m) {
                return true;
            }
        }
    }
    let link_next = |x: &Value| match x {
        Value::Object(m) => m.contains_key("next"),
        Value::Array(a) => a.iter().any(|l| l.get("rel").and_then(Value::as_str) == Some("next")),
        _ => false,
    };
    o.get("_links").map(link_next).unwrap_or(false) || o.get("links").map(link_next).unwrap_or(false)
}

/// True when a transactions request URL's `from=` date is at least `days` in the past.
fn covers_days(url: &str, days: i64) -> bool {
    let Some(q) = url.split('?').nth(1) else { return false };
    let from = q.split('&').find_map(|kv| kv.strip_prefix("from=")).unwrap_or("");
    // NS has used both "2025-05-01T00:00:00" and "20250501": compare on the digits only.
    let digits: String = from.replace("%3A", ":").replace("%2D", "-").chars().filter(char::is_ascii_digit).take(8).collect();
    let date = chrono::NaiveDate::parse_from_str(&digits, "%Y%m%d");
    date.map(|d| (Utc::now().date_naive() - d).num_days() >= days).unwrap_or(false)
}

/// The Mijn NS account e-mail shown as "Ingelogd met" (own account; stored locally for display).
pub fn account_email(b: &NsBridge) -> Option<String> {
    b.eval("sessionStorage.getItem('userinfo-emailAddress')").ok().and_then(|v| v.as_str().map(str::to_string)).filter(|s| s.contains('@'))
}

#[cfg(test)]
mod form_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn history_and_form_require_a_readable_matching_account() {
        for stored in [None, Some("a@example.org")] {
            assert!(account_binding(stored, None).is_err());
        }
        assert!(account_binding(Some("a@example.org"), Some("b@example.org")).is_err());
        assert!(account_binding(Some(" a@example.org "), Some("A@example.org")).is_ok());
        assert!(account_binding(None, Some("a@example.org")).is_ok());
    }

    #[test]
    fn iban_read_back() {
        assert!(iban_matches("NL91 ABNA 0417 1643 00", "NL91ABNA0417164300"));
        assert!(iban_matches("nl91abna0417164300", "NL91ABNA0417164300"));
        assert!(!iban_matches("NL02 RABO 0123 4567 89", "NL91ABNA0417164300"));
        assert!(!iban_matches("", "NL91ABNA0417164300"));
        // same last four characters is not enough: the whole IBAN must match
        assert!(!iban_matches("NL20 INGB 0001 6643 00", "NL91ABNA0417164300"));
        assert!(!iban_matches("NL91ABNA0417164300X", "NL91ABNA0417164300"));
        assert!(holder_matches("  j.  de VRIES ", "J. de Vries"));
        assert!(!holder_matches("", "J. de Vries"));
        assert!(!holder_matches("P. Jansen", "J. de Vries"));
        assert!(NAME_MISMATCH_MSG.contains(" / "));
        assert!(card_number_ok("3528000011112222"));
        assert!(!card_number_ok("3528ééé1") && !card_number_ok("1234567") && !card_number_ok("35280000111122x2"));
        assert!(IBAN_MISMATCH_MSG.contains(" / ") && FORM_CHANGED_MSG.contains(" / "));
    }

    #[test]
    fn form_with_messages_or_unknown_fields_is_not_sent() {
        for v in [serde_json::json!({}), serde_json::json!({"errors": [], "invalid": [], "extra": [7]})] {
            assert!(!super::form_problems(&v).is_empty());
        }
        let personal = serde_json::json!({"errors": ["Dear Jane Example, check your address"], "invalid": [], "extra": []});
        assert!(!super::form_problems(&personal).join(" ").contains("Jane"));
        assert!(form_problems(&json!({"invalid": [], "errors": [], "extra": []})).is_empty());
        assert_eq!(form_problems(&json!({"invalid": ["iban"], "errors": [], "extra": []})).len(), 1);
        assert_eq!(form_problems(&json!({"invalid": [], "errors": ["Vul je IBAN in"], "extra": ["ticketNumber"]})).len(), 2);
        assert!(!form_problems(&Value::Null).is_empty(), "no answer = not safe");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_empty_claims_requires_the_list_route_and_no_error() {
        let check = |route: &str, text: &str, error: Value| {
            empty_claims_page(&serde_json::json!({
                "location": format!("https://www.ns.nl/mijnns#{route}"),
                "empty_visible": empty_claims_text(text), "error_visible": error,
            }))
        };
        let sentence = "Er zijn geen teruggaven bekend. Wij bewaren je teruggaven voor een periode van 18 maanden.";
        assert!(check("/claims", sentence, Value::Bool(false)));
        assert!(check("/claims", "Er zijn geen\n teruggaven bekend", Value::Bool(false)));
        assert!(check("/claims", "You have no refunds.", Value::Bool(false)));
        for route in ["/overzicht", "/reishistorie", "/claims/123", "/claims-other", ""] {
            assert!(!check(route, sentence, Value::Bool(false)));
        }
        for text in ["Mijn teruggaven", "You have no journeys", "No refunds policy", "Teruggave toegekend", ""] {
            assert!(!check("/claims", text, Value::Bool(false)));
        }
        assert!(!check("/claims", sentence, Value::Bool(true)));
        assert!(!check("/claims", sentence, Value::Null));
        assert!(!empty_claims_page(&serde_json::json!({"location": "https://other.example/mijnns#/claims",
            "empty_visible": true, "error_visible": false})));
    }

    #[test]
    fn claims_diagnostic_routes_titles_and_paths_are_sanitised() {
        let summary = claims_page_summary(&serde_json::json!({
            "location": "https://www.ns.nl/mijnns#/overzicht/12345/abcdef012345/private-name",
            "heading_visible": true, "empty_visible": false, "title": "Mijn NS | Teruggaven \u{2013} NS"
        }));
        assert!(summary.contains("#/overzicht/:id/:id/:id"));
        assert!(summary.contains("teruggaven_heading_visible=true; empty_list_text_visible=false"));
        assert!(summary.contains("title=Mijn NS | Teruggaven \u{2013} NS"));
        for title in
            ["private@example.com", "Private_Name", "<secret>", "", "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijk"]
        {
            let summary = claims_page_summary(&serde_json::json!({"title": title}));
            assert!(summary.ends_with("title=unavailable"));
        }
        let p = anon_path("https://different-api.ns.nl/api/customers/Jane-Smith/private@example.com/123?token=secret");
        assert_eq!(p, "/api/customers/:id/:id/:id");
        assert_eq!(anon_path(&p), p);
    }

    #[test]
    fn missing_claims_diagnostics_are_value_free() {
        let summary = claims_page_summary(&serde_json::json!({
            "location": "https://www.ns.nl/mijnns?email=private@example.com#/claims?token=secret",
            "claims_visible": true, "error_visible": true, "error_text": "private name"
        }));
        assert_eq!(summary, "location=https://www.ns.nl/mijnns#/claims; claims_text_visible=true; error_element_visible=true; teruggaven_heading_visible=unknown; empty_list_text_visible=unknown; title=unavailable");
        let summary = claims_page_summary(&serde_json::json!({"location": "https://www.ns.nl/private-name#/claims/private-name"}));
        assert!(!summary.contains("private-name"));
        assert!(summary.contains("/:id#/claims/:id"));
        assert!(claims_page_summary(&Value::Null).contains("location=unavailable"));
        assert!(customer_api_unavailable("https://gateway.apiportal.ns.nl/omni-customer-api/customers/123", 503));
        assert!(!customer_api_unavailable("https://gateway.apiportal.ns.nl/omni-customer-api/customers/123", 400));
        assert!(!customer_api_unavailable("https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/claims", 503));
        assert!(!customer_api_unavailable("https://other.example/omni-customer-api", 503));
    }

    #[test]
    fn covers() {
        let now = chrono::Utc::now().date_naive();
        let old = (now - chrono::Duration::days(365)).format("%Y-%m-%d");
        let recent = (now - chrono::Duration::days(7)).format("%Y-%m-%d");
        assert!(super::covers_days(&format!("https://x/transactions?from={old}T00:00:00&until=x"), 300));
        assert!(!super::covers_days(&format!("https://x/transactions?from={recent}&limit=50"), 300));
        // Compact date format NS switched to (observed November 2026).
        let compact = (Utc::now().date_naive() - chrono::Duration::days(500)).format("%Y%m%d");
        assert!(super::covers_days(&format!("https://x/transactions?from={compact}&limit=50"), 450));
        let compact_recent = (Utc::now().date_naive() - chrono::Duration::days(7)).format("%Y%m%d");
        assert!(!super::covers_days(&format!("https://x/transactions?from={compact_recent}"), 300));
    }

    #[test]
    fn full_history_never_accepts_the_default_week_response() {
        let capture = |days, coupling: &str| Captured {
            url: format!(
                "https://gateway.apiportal.ns.nl/mijnns-transaction-api/couplings/{coupling}/transactions?from={}T00:00:00",
                (Utc::now().date_naive() - chrono::Duration::days(days)).format("%Y-%m-%d")
            ),
            method: "GET".into(),
            status: 200,
            body: Some("[]".into()),
            seq: 1,
        };
        let week = capture(7, "card-a");
        assert!(history_capture_matches(&week, FetchRange::Week, Some("card-a")));
        assert!(!history_capture_matches(&week, FetchRange::Year, Some("card-a")));
        assert!(!history_capture_matches(&week, FetchRange::Max, Some("card-a")));
        let year = capture(365, "card-a");
        assert!(history_capture_matches(&year, FetchRange::Year, Some("card-a")));
        assert!(!history_capture_matches(&year, FetchRange::Max, Some("card-a")));
        let full = capture(550, "card-a");
        assert!(history_capture_matches(&full, FetchRange::Max, Some("card-a")));
        assert!(!history_capture_matches(&full, FetchRange::Max, Some("card-b")));
        let mut denied = week;
        denied.status = 401;
        assert!(history_capture_matches(&denied, FetchRange::Max, Some("card-a")));
    }
}

/// Amount in a claim-list entry, in cents. The real list format has not been observed with
/// amounts yet, so only unambiguous shapes are accepted: an explicit "...InCents" key, or a
/// euro value written as a string with a decimal separator ("4,17" / "4.17"). A bare JSON number
/// under a euro-named key is ambiguous (3 = EUR 3 or 3 cents?) and is ignored.
pub fn parse_amount_cents(it: &Value) -> Option<i64> {
    for k in ["amountInCents", "amountCents", "refundAmountInCents", "paidAmountInCents"] {
        if let Some(c) = it.get(k).and_then(Value::as_i64) {
            return Some(c);
        }
    }
    for k in ["amount", "refundAmount", "compensationAmount", "paidAmount"] {
        let v = match it.get(k) {
            Some(Value::Object(o)) => o.get("amount").or_else(|| o.get("value")).cloned(),
            other => other.cloned(),
        };
        if let Some(Value::String(t)) = v {
            let t = t.replace(['€', ' ', '\u{a0}', '\u{202f}'], "").replace(',', ".");
            if t.starts_with('-') {
                continue; // a negative "payout" is not a payout
            }
            if let Some((eu, ct)) = t.split_once('.') {
                if ct.len() == 2 {
                    if let (Ok(e), Ok(c)) = (eu.parse::<i64>(), ct.parse::<i64>()) {
                        return Some(e * 100 + c);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod amount_tests {
    use super::parse_amount_cents;
    use serde_json::json;

    #[test]
    fn only_unambiguous_amounts() {
        assert_eq!(parse_amount_cents(&json!({"amountInCents": 417})), Some(417));
        assert_eq!(parse_amount_cents(&json!({"amount": "4,17"})), Some(417));
        assert_eq!(parse_amount_cents(&json!({"amount": {"amount": "€ 3.00"}})), Some(300));
        // ambiguous bare numbers are ignored (never EUR 3 -> 3 cents)
        assert_eq!(parse_amount_cents(&json!({"amount": 3})), None);
        assert_eq!(parse_amount_cents(&json!({"amount": 4.17})), None);
    }
}

#[cfg(test)]
mod claim_list_tests {
    use super::parse_remote_claims;
    use serde_json::json;

    #[test]
    fn negative_statuses_never_become_paid_or_approved() {
        use ovrefund_core::model::ClaimState;
        for raw in [
            "unpaid",
            "not paid",
            "not approved",
            "niet uitbetaald",
            "nog niet goedgekeurd",
            "payment_pending",
            "unapproved",
            "disapproved",
            "not rejected",
            "niet afgewezen",
        ] {
            let parsed = parse_remote_claims(&json!([{"transactionId": "T1", "status": raw}])).unwrap();
            assert_eq!(parsed[0].state, Some(ClaimState::Submitted), "{raw}");
        }
        for (raw, state) in
            [("Paid", ClaimState::Paid), ("Toegekend", ClaimState::Approved), ("Afgewezen - niet uitbetaald", ClaimState::Rejected)]
        {
            let parsed = parse_remote_claims(&json!([{"transactionId": "T1", "status": raw}])).unwrap();
            assert_eq!(parsed[0].state, Some(state), "{raw}");
        }
    }

    #[test]
    fn unknown_shapes_fail_closed() {
        assert_eq!(parse_remote_claims(&json!([])).map(|v| v.len()), Some(0));
        assert!(parse_remote_claims(&json!(null)).is_none());
        assert!(parse_remote_claims(&json!({"links": [], "page": 1})).is_none(), "first array is not the claims");
        assert!(parse_remote_claims(&json!([{"status": "In behandeling"}])).is_none(), "entry without date or id");
        let ok = parse_remote_claims(&json!({"claims": [{"travelDate": "2026-10-17", "status": "Toegekend"}]})).unwrap();
        assert_eq!(ok.len(), 1);
    }

    #[test]
    fn paginated_list_is_unavailable() {
        let one = json!({"travelDate": "2026-10-17", "status": "Toegekend"});
        for v in [
            json!({"content": [one.clone()], "totalPages": 3, "number": 0}),
            json!({"content": [one.clone()], "last": false}),
            json!({"claims": [one.clone()], "hasNext": true}),
            json!({"items": [one.clone()], "next": "/claims?page=2"}),
            json!({"items": [one.clone()], "_links": {"next": {"href": "x"}}}),
            json!({"items": [one.clone()], "links": [{"rel": "next", "href": "x"}]}),
            json!({"content": [one.clone()], "page": {"size": 1, "totalElements": 2, "totalPages": 2, "number": 0}}),
            json!({"content": [one.clone()], "totalElements": 7, "numberOfElements": 1}),
        ] {
            assert!(parse_remote_claims(&v).is_none(), "more pages => unavailable: {v}");
        }
        // a complete single page is fine
        for v in [
            json!({"content": [one.clone()], "totalPages": 1, "last": true, "number": 0, "totalElements": 1, "numberOfElements": 1}),
            json!({"claims": [one.clone()], "hasNext": false, "next": null}),
            json!({"content": [one.clone()], "page": {"size": 20, "totalElements": 1, "totalPages": 1, "number": 0}}),
        ] {
            assert_eq!(parse_remote_claims(&v).map(|c| c.len()), Some(1), "{v}");
        }
    }

    #[test]
    fn odd_dates_never_panic() {
        // multi-byte character across byte 10, and short strings
        for d in ["2026-10-2€x", "2026-10-17T08:00:00Z", "é", ""] {
            let _ = parse_remote_claims(&json!([{"travelDate": d, "transactionId": "T1"}]));
        }
        let v = parse_remote_claims(&json!([{"travelDate": "2026-10-17T08:00:00Z", "transactionId": "T1"}])).unwrap();
        assert_eq!(v[0].travel_date, chrono::NaiveDate::from_ymd_opt(2026, 10, 17));
    }
}

#[cfg(test)]
mod submit_proof_tests {
    use super::submit_proof;

    #[test]
    fn generic_id_alone_is_not_proof() {
        assert_eq!(submit_proof(Some(r#"{"id": 17}"#), "formulier", "formulier"), (None, false));
        assert_eq!(submit_proof(Some("not json"), "", ""), (None, false));
        assert_eq!(submit_proof(None, "", ""), (None, false));
    }

    #[test]
    fn claim_reference_or_new_confirmation_text_is_proof() {
        assert_eq!(submit_proof(Some(r#"{"claimReference": "GTBV-1"}"#), "", ""), (Some("GTBV-1".into()), true));
        assert!(!submit_proof(Some(r#"{"reference": ""}"#), "", "").1, "empty reference is no reference");
        // confirmation text that appeared after the click; generic id kept as reference then
        assert_eq!(
            submit_proof(Some(r#"{"id": "A9"}"#), "Verzenden", "Bedankt, we hebben je aanvraag ontvangen"),
            (Some("A9".into()), true)
        );
        // the same words already on the form before the click prove nothing
        assert!(!submit_proof(None, "Bedankt voor je aanvraag", "Bedankt voor je aanvraag").1);
        assert!(!submit_proof(None, "we hebben je aanvraag nodig", "we hebben je aanvraag nodig").1);
    }

    #[test]
    fn negative_page_text_is_no_confirmation() {
        for after in [
            "Je aanvraag is niet verzonden",
            "Je aanvraag is niet ontvangen. Probeer het later opnieuw.",
            "Verzenden mislukt",
            "Er ging iets fout. Bedankt voor je geduld.",
        ] {
            assert!(!submit_proof(None, "Verzenden", after).1, "{after}");
        }
        // a claim reference in NS's answer is still proof
        assert!(submit_proof(Some(r#"{"claimReference": "GTBV-1"}"#), "", "").1);
    }

    #[test]
    fn only_the_claim_post_counts_as_the_submission() {
        use super::is_claim_submit_write as w;
        let base = "https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv";
        assert!(w("POST", base));
        assert!(w("POST", &format!("{base}/claims")));
        assert!(w("POST", &format!("{base}/claims?x=1")));
        assert!(!w("PUT", &format!("{base}/claims")), "PUT may be a draft save");
        assert!(!w("GET", &format!("{base}/claims")));
        assert!(!w("POST", &format!("{base}/claims/validate")));
        assert!(!w("POST", &format!("{base}/attachments")));
        assert!(!w("POST", &format!("{base}x/claims")), "other API with the same prefix");
        assert!(!w("POST", "https://gateway.apiportal.ns.nl/mijnns-claim-api/settings"));
        assert!(!w("POST", "https://evil.example/mijnns-claim-api/gtbv/claims"));
    }

    #[cfg(test)]
    mod diag_tests {
        use crate::ns_adapters::{anon_path, json_shape};
        use serde_json::json;

        #[test]
        fn shape_and_path_never_contain_values() {
            let v = json!({"claims": [{"aanvraagnummer": "600000001", "reisdatum": "2026-10-17", "bedrag": 417, "naam": "J. de Vries"}], "total": 1});
            assert_eq!(json_shape(&json!({"private-trip 2026-11-01T12:34": "private payload"}), 0), "object(1 fields)");
            let s = json_shape(&v, 0);
            assert!(!s.contains("600000001") && !s.contains("Vries") && !s.contains("417"), "{s}");
            assert_eq!(s, "object(2 fields)");
            let p = anon_path("https://gateway.apiportal.ns.nl/mijnns-claim-api/v2/claims/600000001/details?email=a@example.com");
            assert_eq!(p, "/mijnns-claim-api/v2/claims/:id/details");
        }
    }
}

/// Exact focused field, including fields nested in NS shadow roots.
fn bank_focus_script(selector: &str) -> String {
    format!("(() => {{ if (window.top !== window || location.protocol !== 'https:' || location.hostname !== 'www.ns.nl') return false; let active = document.activeElement; while (active && active.shadowRoot && active.shadowRoot.activeElement) active = active.shadowRoot.activeElement; const field = window.__ovq('{selector}'); return !!field && active === field; }})()")
}

#[cfg(test)]
mod submission_guard_tests {
    use super::*;
    #[test]
    fn observed_write_prevents_every_retryable_early_exit() {
        assert!(matches!(guard_not_sent(SubmitOutcome::NotSent("fill failed".into()), false), SubmitOutcome::NotSent(_)));
        assert!(matches!(guard_not_sent(SubmitOutcome::NotSent("fill failed".into()), true), SubmitOutcome::Unknown(_)));
        assert!(matches!(guard_not_sent(SubmitOutcome::Unknown("timeout".into()), false), SubmitOutcome::Unknown(_)));
        assert!(!is_claim_submit_write("POST", "https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/validate"));
        assert!(is_claim_submit_write("POST", "https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/claims"));
    }
}
