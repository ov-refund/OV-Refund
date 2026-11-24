//! Live Mijn NS adapters (journey source + claim gateway) on top of [`NsBridge`].
//! Flows and selectors were observed on 2026-11-12 (docs/integration-inventory.md).
use crate::ns_bridge::{Captured, NsBridge, SessionStatus};
use crate::ns_elements::{self as el, ElSpec, Resolved};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use ovrefund_core::model::*;
use ovrefund_core::ns_parse;
use ovrefund_core::pipeline::*;
use serde_json::Value;
use std::time::{Duration, Instant};

pub const GATEWAY_ID: &str = "mijnns-webview";

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
            FetchRange::Year => Some((el::PERIOD_YEAR, 300)),
            FetchRange::Max => Some((el::PERIOD_MAX, 450)),
        }
    }
}

pub struct MyNsJourneySource {
    pub bridge: NsBridge,
    pub range: FetchRange,
}

fn transaction_coupling(c: &Captured) -> Option<&str> {
    let path = c.url.split(['?', '#']).next()?;
    let id = path.strip_prefix("https://gateway.apiportal.ns.nl/mijnns-transaction-api/couplings/")?.strip_suffix("/transactions")?;
    (c.method == "GET" && !id.is_empty() && !id.contains('/') && !is_capped(&c.url)).then_some(id)
}

fn is_tx(c: &Captured) -> bool {
    transaction_coupling(c).is_some()
}

/// Since 2026-11-20 Mijn NS also loads a short "latest transactions" list (`from=…&limit=15`) next to the
/// travel-history view (`from=…&until=…`). A capped list is never the history: its early `from=` made it
/// look like the full 1.5-year view, and only 15 transactions were imported.
fn is_capped(url: &str) -> bool {
    url.split('?').nth(1).map(|q| q.split('&').any(|kv| kv.starts_with("limit="))).unwrap_or(false)
}

fn history_capture_matches(c: &Captured, range: FetchRange, coupling: Option<&str>) -> bool {
    let Some(id) = transaction_coupling(c) else { return false };
    coupling.is_none_or(|expected| id == expected)
        // Preserve authentication/HTTP failures for the caller; never accept a successful
        // short response as a completed full-history import.
        && (c.status != 200 || range.filter().map(|(_, days)| covers_days(&c.url, days)).unwrap_or(true))
}

/// Choose `label` in NS's travel-history period filter. The filter is found by the element
/// resolver; the option by its text (case, spacing and "1,5"/"1.5" ignored), and only when exactly
/// one option has that text.
fn select_period(b: &NsBridge, label: &str, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        let r = (|| -> Result<()> {
            let s = el::resolve(b, &el::PERIOD_FILTER, None)?;
            let opts = b.eval(&format!("(s => s && !s.disabled ? [...s.options].map(o => String(o.text)) : null)({})", s.expr))?;
            let opts: Vec<String> = opts
                .as_array()
                .ok_or_else(|| anyhow!("period filter not ready"))?
                .iter()
                .map(|o| o.as_str().unwrap_or("").to_string())
                .collect();
            let i = el::period_option_index(&opts, label).ok_or_else(|| anyhow!("period option not found or not unique"))?;
            let want = serde_json::to_string(&opts[i])?;
            let set = b.eval(&format!(
                "(s => {{ const o = s && s.options[{i}]; if (!o || s.disabled || window.__ovr.norm(o.text) !== window.__ovr.norm({want})) return false; \
                 s.selectedIndex = {i}; s.dispatchEvent(new Event('change', {{bubbles: true}})); return s.selectedIndex === {i}; }})({})",
                s.expr
            ))?;
            anyhow::ensure!(set == Value::Bool(true), "period option could not be selected");
            Ok(())
        })();
        if r.is_ok() || start.elapsed() >= timeout {
            return r;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Press "Tonen" in the period filter once it is enabled.
fn press_show(b: &NsBridge) -> Result<()> {
    let mut last = anyhow!("not tried");
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(200));
        match el::resolve(b, &el::SHOW_BUTTON, None) {
            Ok(btn) => {
                if b.eval(&format!("(e => !!e && !window.__ovr.disabled(e))({})", btn.expr))?.as_bool() != Some(true) {
                    last = anyhow!("Tonen is disabled");
                } else {
                    match b.click(&btn.expr) {
                        Ok(()) => return Ok(()),
                        Err(e) => last = e,
                    }
                }
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Wait for the travel-history response of the current view; for longer ranges select the period
/// in the official filter and press "Tonen", as a user would.
fn load_view(b: &NsBridge, since: u64, range: FetchRange, coupling: Option<&str>, hash: &str) -> Option<Captured> {
    let matches = |c: &Captured| history_capture_matches(c, FetchRange::Week, coupling);
    // After an automatic login NS may land on its dashboard (since 2026-11-20 with a capped "latest
    // transactions" list) instead of the travel history: go back to exactly the page that was
    // opened (`hash`, with its `?card=` for another card).
    let want = serde_json::to_string(hash).unwrap_or_default();
    let on_history = |b: &NsBridge| {
        b.eval(&format!("(w => {{ const [r, q] = w.split('?'); return location.hash.split('?')[0] === r && (!q || location.hash.includes(q)); }})({want})"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let mut since = since;
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(1500));
        if on_history(b) {
            break;
        }
        log::info!("Mijn NS opened another page than the travel history; going back to the travel history");
        since = b.mark();
        let _ = b.eval(&format!("location.hash = {want}; true"));
    }
    // NS does not always load a list view by itself (sometimes only its capped "latest
    // transactions" list): for a longer period the filter below is used anyway.
    let first = wait_capture(b, since, Duration::from_secs(if range.filter().is_some() { 20 } else { 45 }), matches);
    ovrefund_core::progress::report(15, "fetch");
    let Some((label, _)) = range.filter() else {
        if first.is_some() {
            return first;
        }
        // No week view loaded by itself: ask for it with the filter ("Laatste 7 dagen").
        let m2 = b.mark();
        if let Err(e) = select_period(b, "Laatste 7 dagen", Duration::from_secs(20)).and_then(|_| press_show(b)) {
            log::warn!("week view not loaded and the period filter could not be used ({e})");
            return None;
        }
        return wait_capture(b, m2, Duration::from_secs(45), matches);
    };
    if let Some(first) = first.filter(|c| history_capture_matches(c, range, coupling)) {
        return Some(first);
    }
    let m2 = b.mark();
    if let Err(e) = select_period(b, label, Duration::from_secs(20)) {
        // Only the filter's own option labels and the page route: no personal data.
        let seen = b
            .eval("(() => { const s = document.querySelector('#filter-period-control'); return JSON.stringify({route: location.hash.split('?')[0], select: !!s, disabled: s ? s.disabled : null, options: s ? [...s.options].map(o => o.text.trim()) : [], selects: [...document.querySelectorAll('select')].map(e => e.id || e.name || '?')}); })()")
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        log::warn!("requested history period filter unavailable ({e}); import not completed; filter: {seen}");
        return None;
    }
    if let Err(e) = press_show(b) {
        log::warn!("requested history period could not be applied ({e}); import not completed");
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
        check_account(b, true).map_err(|e| match e.to_string().as_str() {
            ACCOUNT_MISMATCH_MSG => FetchError::AccountMismatch,
            ACCOUNT_UNKNOWN_MSG => FetchError::AccountUnreadable,
            _ => FetchError::Other(e),
        })?;
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
        let Some(first) = load_view(b, mark, self.range, None, "#/reishistorie") else {
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
        // Full card numbers -> Windows Credential Manager only (for the "view" button and the Arriva
        // travel proof), per linked card. Never in the DB/logs.
        for body in b
            .captured_since(mark)
            .iter()
            .filter(|c| c.url.contains("/mijnns-card-coupling-api/") && c.status == 200)
            .filter_map(|c| c.body.clone())
        {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                for c in v.as_array().into_iter().flatten() {
                    let Some(coupling) = c["couplingId"].as_str() else { continue };
                    for t in c["tokens"].as_array().into_iter().flatten() {
                        if let Some(n) = t["cardNumber"].as_str().filter(|n| card_number_ok(n)) {
                            let _ = crate::secrets::store_card_number(coupling, n);
                        }
                    }
                }
            }
        }
        // Per response: the card and the first day it covers (for the completeness check below).
        let mut covered: Vec<(String, chrono::NaiveDate)> = vec![];
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
            for reason in rep.skipped {
                log::warn!("skipped transaction: {reason}");
            }
            out.extend(rep.journeys);
            if let (Some(id), Some(from)) = (coupling.as_ref(), query_date(&c.url, "from")) {
                covered.push((id.clone(), from));
            }
            Ok(coupling)
        };
        let done = handle(&first)?;
        ovrefund_core::progress::report(22, "fetch");
        couplings.retain(|c| Some(c) != done.as_ref());
        let total = couplings.len();
        for (index, coupling) in couplings.into_iter().enumerate() {
            let m = b.mark();
            let hash = history_hash(Some(&coupling));
            b.navigate(&format!("https://www.ns.nl/mijnns{hash}")).map_err(FetchError::Other)?;
            b.reload().map_err(FetchError::Other)?;
            if let Some(c) = load_view(b, m, self.range, Some(&coupling), &hash) {
                handle(&c)?;
            } else {
                return Err(FetchError::Other(anyhow::anyhow!(NS_NO_RESPONSE_MSG)));
            }
            ovrefund_core::progress::report(ovrefund_core::progress::within(22, 34, index + 1, total), "fetch");
        }
        if self.range != FetchRange::Week {
            if let Ok(db) = crate::runner::open_db() {
                incomplete_import(&db, &out, &covered).map_err(FetchError::Other)?;
            }
        }
        Ok(out)
    }
}

/// A full import that returned clearly fewer journeys than the ledger already holds for the same
/// card and period is incomplete: the history is not marked as read and the check reports that
/// Mijn NS did not answer. Logs counts only (no card, account or trip data).
fn incomplete_import(db: &ovrefund_core::db::Db, out: &[Journey], covered: &[(String, chrono::NaiveDate)]) -> Result<()> {
    let end = Utc::now() - chrono::Duration::days(2);
    for (coupling, from) in covered {
        let Some(start) = from.succ_opt().and_then(|d| d.and_hms_opt(0, 0, 0)).map(|d| d.and_utc()) else { continue };
        if start >= end {
            continue;
        }
        let known = db.journey_count_for_coupling(coupling, start, end)?;
        let read =
            out.iter().filter(|j| j.coupling_id.as_deref() == Some(coupling.as_str()) && j.check_in >= start && j.check_in <= end).count();
        if ovrefund_core::pipeline::import_incomplete(read, known) {
            log::warn!("full history import incomplete: {read} journeys read, {known} already in the ledger for this card and period; not accepted");
            bail!(NS_NO_RESPONSE_MSG);
        }
        log::info!("full history import: {read} journeys read, {known} already in the ledger for this card and period");
    }
    Ok(())
}

#[derive(Clone, PartialEq)]
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
/// Is the resolved radio button / checkbox selected right now?
fn is_checked(b: &NsBridge, e: &Resolved) -> bool {
    b.eval(&format!("(e => !!e && !!window.__ovr && window.__ovr.checked(e))({})", e.expr)).ok().and_then(|v| v.as_bool()) == Some(true)
}

/// A claim-form element, waiting briefly for the page to render it.
fn form_el(b: &NsBridge, spec: &'static ElSpec) -> Result<Resolved> {
    el::resolve_wait(b, spec, None, Duration::from_secs(5))
}

/// Click a radio choice or checkbox in NS's form. NS draws a styled control over a visually hidden
/// input, so the input itself may not be clickable: fall back to its label, then check it really
/// is selected. An already ticked checkbox is left as it is (a click would untick it).
pub fn click_choice(b: &NsBridge, spec: &'static ElSpec) -> Result<()> {
    let choice = el::resolve_wait(b, spec, None, Duration::from_secs(10))?;
    if spec.kind == el::Kind::Checkbox && is_checked(b, &choice) {
        return Ok(());
    }
    let input = &choice.expr;
    if b.click(input).is_err() {
        let label = format!("(i => i && ((i.labels && i.labels[0]) || i.closest('label') || (i.id && i.getRootNode().querySelector('label[for=\"' + i.id + '\"]'))))({input})");
        b.click(&label)?;
    }
    b.wait_until("choice selected", Duration::from_secs(5), |b| Ok(is_checked(b, &choice)))
}

fn open_claim_form(b: &NsBridge, f: &ClaimFacts) -> Result<()> {
    let row = locate_trip_row(b, f)?;
    b.click(&row).context("trip row in the travel history")?;
    std::thread::sleep(Duration::from_millis(1200));
    // The link is searched only in the opened row's own container (then its page component).
    let scopes = [format!("({row}).parentElement"), format!("({row}).closest('ng-component')")];
    let mut link = None;
    b.wait_until("refund link in trip details", Duration::from_secs(15), |b| {
        link = scopes.iter().find_map(|s| el::resolve(b, &el::CLAIM_LINK, Some(s)).ok());
        Ok(link.is_some())
    })?;
    let link = link.ok_or_else(|| anyhow!("refund link in the trip details not found"))?;
    b.click(&link.expr).context("refund link in the trip details")?;
    b.wait_until("claim form for this trip", Duration::from_secs(30), |b| current_claim_route(b, f))
        .map_err(|_| anyhow::anyhow!("the form that opened is not for this trip"))?;
    Ok(())
}

pub(crate) fn current_claim_route(b: &NsBridge, f: &ClaimFacts) -> Result<bool> {
    let parts = b.eval(
        "(() => { if (location.origin !== 'https://www.ns.nl' || !/^\\/mijnns\\/?$/.test(location.pathname)) return null; \
         try { return location.hash.split('/').map(decodeURIComponent); } catch { return null; } })()",
    )?;
    Ok(claim_route_matches(&parts, f))
}

fn claim_route_matches(parts: &Value, f: &ClaimFacts) -> bool {
    parts.as_array().is_some_and(|parts| {
        parts.len() == 6
            && parts[0] == "#"
            && parts[1] == "reishistorie"
            && parts[2] == "geld-terug-bij-vertraging"
            && !f.coupling_id.is_empty()
            && parts[3] == f.coupling_id
            && !f.source_record_id.is_empty()
            && parts[4] == f.source_record_id
            && parts[5].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).is_some_and(|t| t.timestamp() == f.check_in.timestamp())
    })
}

/// Travel proof for a provider claim: one unedited screenshot of the Mijn NS travel-history page,
/// from the top of the page (NS header and logo, the period filter) down to just below this trip,
/// which is expanded to show its details. The list is first narrowed with NS's own period filter:
/// the trip's day ("Kies handmatig"), else the shortest preset that contains the trip. At a fixed
/// desktop width (1440 CSS px). When the page would be taller than `MAX_PAGE` the visible browser
/// viewport around the trip is captured instead. Uses the logged-in NS window.
///
/// `strict` (a travel proof that will be sent): the list must be narrowed to the trip's day only
/// (no longer list as a fallback) and the expanded row must be this transaction (its id in the
/// page, else its check-out time and destination). Otherwise the capture fails with
/// [`TRIP_SHOT_FAILED_MSG`]. The developer `history-shot` keeps the fallbacks for diagnostics.
pub fn capture_trip_row(b: &NsBridge, j: &Journey, strict: bool) -> Result<RowShot> {
    let (check_in, origin_name) = (j.check_in, j.origin.name.as_str());
    let local = check_in.with_timezone(&chrono_tz::Europe::Amsterdam);
    b.cdp(
        "Emulation.setDeviceMetricsOverride",
        serde_json::json!({"width": 1440, "height": 1000, "deviceScaleFactor": 1, "mobile": false}),
        Duration::from_secs(10),
    )?;
    let r = (|| -> Result<RowShot> {
        open_history(b, j.coupling_id.as_deref())?;
        let filter = match set_day_filter(b, local.date_naive()) {
            Ok(n) if n <= 1 => "day filter (only the trip's day listed)".to_string(),
            Ok(n) if strict => {
                log::warn!("travel proof: the day filter listed {n} days");
                bail!(TRIP_SHOT_FAILED_MSG)
            }
            Ok(n) => format!("day filter ({n} days listed)"),
            Err(why) => {
                if strict {
                    // Never a longer period instead: the screenshot would show other trips.
                    log::warn!("travel proof: day filter failed");
                    bail!(TRIP_SHOT_FAILED_MSG);
                }
                // Close a calendar that may still be open, then use the shortest preset with the trip.
                for t in ["keyDown", "keyUp"] {
                    let _ = b.cdp(
                        "Input.dispatchKeyEvent",
                        serde_json::json!({"type": t, "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27}),
                        Duration::from_secs(5),
                    );
                }
                std::thread::sleep(Duration::from_millis(300));
                let label = preset_for(local.date_naive(), Utc::now().with_timezone(&chrono_tz::Europe::Amsterdam).date_naive());
                choose_period(b, label)?;
                format!("preset \"{label}\" (day filter failed: {})", why.lines().next().unwrap_or(""))
            }
        };
        let row = find_trip_row(b, check_in, origin_name).map_err(|e| if strict { anyhow!(TRIP_SHOT_FAILED_MSG) } else { e })?;
        b.click(&row).context("trip row in the travel history")?;
        std::thread::sleep(Duration::from_millis(1200));
        let check = row_is_trip(b, &row, j)?;
        if strict && !check.starts_with("verified") {
            log::warn!("travel proof: the expanded row could not be confirmed as this trip ({check})");
            bail!(TRIP_SHOT_FAILED_MSG);
        }
        let (png, capture) = capture_page(b, &row)?;
        Ok(RowShot { png, filter: format!("{filter}; row {check}"), capture })
    })();
    let _ = b.cdp("Emulation.clearDeviceMetricsOverride", serde_json::json!({}), Duration::from_secs(10));
    r
}

/// The travel proof of a claim that will be sent could not be made for exactly this trip.
pub const TRIP_SHOT_FAILED_MSG: &str = "Schermafbeelding van je reis lukte niet: Mijn NS kon de lijst niet op alleen jouw reis en reisdag zetten. Er is niets naar Arriva gestuurd; probeer het later opnieuw of dien de claim zelf in. / Couldn't capture your trip: Mijn NS could not narrow the list to only your trip and its day. Nothing was sent to Arriva; try again later or submit the claim yourself.";

/// Whether the expanded row is `j`: its transaction id anywhere in the row (attributes, links),
/// else its check-out time and destination in the row's text (also inside shadow roots).
/// Returns "verified by …" or why not (no trip data in the text).
fn row_is_trip(b: &NsBridge, row: &str, j: &Journey) -> Result<String> {
    let out = j.check_out.map(|t| t.with_timezone(&chrono_tz::Europe::Amsterdam).format("%H:%M").to_string());
    let dest = j.destination.as_ref().map(|d| d.name.to_lowercase().split_whitespace().collect::<String>());
    let v = b.eval(&format!(
        "(row => {{ const box = row.parentElement || row; const attrs = [], text = [];          const walk = n => {{ for (const c of n.childNodes) {{ if (c.nodeType === 3) text.push(c.textContent); else if (c.nodeType === 1) {{ for (const a of c.attributes) attrs.push(a.value); if (c.shadowRoot) walk(c.shadowRoot); walk(c); }} }} }};          walk(box); const t = text.join(' ').toLowerCase().replace(/\\s+/g, ''); const id = {id}; const out = {out}, dest = {dest};          return {{ id: !!id && attrs.some(a => a.includes(id)), out: !!out && t.includes(out), dest: !!dest && t.includes(dest) }}; }})({row})",
        id = serde_json::to_string(&j.source_record_id)?,
        out = serde_json::to_string(&out)?,
        dest = serde_json::to_string(&dest)?,
    ))?;
    Ok(if v["id"] == true {
        "verified by transaction id".into()
    } else if v["out"] == true && v["dest"] == true {
        "verified by check-out time and destination".into()
    } else {
        format!("not verified (check-out time {}, destination {})", v["out"] == true, v["dest"] == true)
    })
}

/// Tallest page (CSS px) captured whole; above this the viewport around the trip is captured.
const MAX_PAGE: f64 = 6000.0;

/// Page script: what is covered in the travel proof, in page coordinates (CSS px). The page stays
/// as NS shows it; covered are the other trips in the list and every visible text or form value
/// outside the trip and its day heading that is not a known generic NS label (so the account
/// e-mail, names, card pickers and anything new NS adds are covered: unknown means covered).
/// Text at opacity 0 counts as visible (it can fade in); CSS-generated text (`::before`/`::after`)
/// that is not a generic label is covered with its element, and so is a visible custom element
/// that shows nothing readable (likely a closed shadow root). `fail` (fail closed, no screenshot):
/// a visible frame (`iframe`), an open dialog or popover (`top-layer`) outside the trip, or
/// generated text / a closed component that overlaps the trip (`pseudo`, `closed-shadow`).
const MASK_MEASURE: &str = r#"
const box = row.parentElement || row;
const page = r => ({x: r.left + scrollX, y: r.top + scrollY, w: r.width, h: r.height});
const keep = [page(box.getBoundingClientRect())];
const list = document.querySelector('mns-travel-history-list');
const items = list ? [...list.querySelectorAll('nes-heading, [tabindex]')] : [];
const i = items.indexOf(row);
let dayHeading = null;
for (let k = i - 1; k >= 0; k--) if (items[k].tagName.toLowerCase() === 'nes-heading') {
  dayHeading = items[k]; keep.push(page(dayHeading.getBoundingClientRect())); break;
}
const within = (r, k) => r.x >= k.x - 2 && r.y >= k.y - 2 && r.x + r.w <= k.x + k.w + 2 && r.y + r.h <= k.y + k.h + 2;
const overlaps = (r, k) => r.x < k.x + k.w && k.x < r.x + r.w && r.y < k.y + k.h && k.y < r.y + r.h;

const hide = [];
let shared = false, fail = null;
for (const it of items) {
  if (it === row || it.tagName.toLowerCase() === 'nes-heading' || row.contains(it) || it.contains(row)) continue;
  const r = page((it.parentElement && it.parentElement !== box ? it.parentElement : it).getBoundingClientRect());
  if (!r.w || !r.h) continue;
  if (overlaps(r, keep[0])) shared = true; else hide.push(r);
}
const ALLOW = new Set(%ALLOW%);
const MONTHS = 'januari|februari|maart|april|mei|juni|juli|augustus|september|oktober|november|december';
const norm = s => String(s || '').replace(/\s+/g, ' ').trim().toLowerCase();
const allowed = s => { const t = norm(s); return !t || ALLOW.has(t)
  || /^(maandag|dinsdag|woensdag|donderdag|vrijdag|zaterdag|zondag|vandaag|gisteren)$/.test(t)
  || new RegExp('^\\d{1,2} (' + MONTHS + ')( \\d{4})?$').test(t) || /^\d{1,2}-\d{1,2}-\d{4}$/.test(t)
  || /^\d{1,2} (jan|feb|mrt|apr|mei|jun|jul|aug|sep|okt|nov|dec)\.? \d{4}$/.test(t)
  || t === 'bekijk hier je gemaakte reizen en betalingen.'
  || /^[\s|•·:,.()\/-]+$/.test(t); };
const up = e => e.parentElement || (e.getRootNode && e.getRootNode().host) || null;
const inKeep = (el, r) => {
  for (let e = el; e; e = up(e)) {
    if ((e === box && within(r, keep[0])) || (e === dayHeading && within(r, keep[1]))) return true;
  }
  return false;
};
// Opacity 0 counts as shown (it can fade in before the screenshot). Visibility is inherited, so the
// element's own value decides; display:none on any ancestor hides it.
const shown = el => { const v = getComputedStyle(el).visibility; if (v === 'hidden' || v === 'collapse') return false;
  for (let e = el; e && e.nodeType === 1; e = up(e)) if (getComputedStyle(e).display === 'none') return false; return true; };
const W = Math.max(document.documentElement.scrollWidth, innerWidth), H = Math.max(document.documentElement.scrollHeight, innerHeight);
const add = (el, r, why) => {
  if (r.w <= 0 || r.h <= 0 || r.x + r.w <= 0 || r.y + r.h <= 0 || r.x >= W || r.y >= H || inKeep(el, r)) return;
  if (keep.some(k => overlaps(r, k))) { fail = fail || why; return; }
  hide.push(r);
};
const cover = (el, why) => add(el, page(el.getBoundingClientRect()), why);
// CSS-generated text that is not a generic label (icon glyphs, quotes and empty strings are not text).
const unescape = s => s.replace(/\\([0-9a-fA-F]{1,6})\s?/g, (_, h) => { try { return String.fromCodePoint(parseInt(h, 16)); } catch (e) { return ''; } }).replace(/\\(.)/g, '$1');
const generated = el => { for (const p of ['::before', '::after']) { let cs; try { cs = getComputedStyle(el, p); } catch (e) { continue; }
  const c = cs && cs.content; if (!c || c === 'none' || c === 'normal' || cs.display === 'none') continue;
  if (/\b(attr|counters?)\(/.test(c)) return true;
  const t = (c.match(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g) || []).map(q => unescape(q.slice(1, -1))).join('');
  if (/[\p{L}\p{N}@]/u.test(t) && !allowed(t)) return true; } return false; };
const lightText = el => norm(el.textContent) !== '';
const openShadowBelow = el => { for (const c of el.children || []) if (c.shadowRoot || openShadowBelow(c)) return true; return false; };
const walk = n => {
  for (const c of n.childNodes || []) {
    if (c.nodeType === 3) {
      if (!c.data.trim() || allowed(c.data)) continue;
      const el = c.parentElement || (c.parentNode && c.parentNode.host); if (el && !shown(el)) continue;
      const rg = document.createRange(); rg.selectNodeContents(c);
      for (const r of rg.getClientRects()) add(el, page(r), 'overlapping-text');
    } else if (c.nodeType === 1) {
      if (c.id === 'ovrefund-mask') continue;
      const t = c.tagName.toLowerCase();
      if ((t === 'input' || t === 'select' || t === 'textarea') && !/^(checkbox|radio|button|submit|hidden)$/.test(c.type || '')) {
        const v = t === 'select' ? ((c.options && c.options[c.selectedIndex]) || {}).text : (c.value || c.placeholder);
        if (v && shown(c)) cover(c, 'overlapping-input');
      }
      const frame = /^(iframe|frame|object|embed)$/.test(t);
      let top = t === 'dialog' && (c.open || (c.hasAttribute && c.hasAttribute('open')));
      if (!top) { try { top = !!(c.matches && c.matches(':popover-open')); } catch (e) { top = false; } }
      const custom = t.includes('-') && !c.shadowRoot;
      const gen = generated(c);
      if ((frame || top || custom || gen) && shown(c)) {
        const r = page(c.getBoundingClientRect());
        // Frames (other content, unreadable here) and open dialogs or popovers: no screenshot.
        if (frame && r.w > 0 && r.h > 0) fail = fail || 'iframe';
        if (top && r.w > 0 && r.h > 0) fail = fail || 'top-layer';
        if (gen) cover(c, 'pseudo');
        // A custom element without an open shadow root that shows something but has no readable
        // text: likely a closed shadow root. Covered.
        if (custom && r.w > 40 && r.h > 20 && !lightText(c) && !openShadowBelow(c)) cover(c, 'closed-shadow');
      }
      if (c.shadowRoot) walk(c.shadowRoot);
      walk(c);
    }
  }
};
for (const e of [document.documentElement, document.body]) if (e && generated(e) && shown(e)) cover(e, 'pseudo');
walk(document.body);
return {w: W, h: H, keep, hide, heading: !!dayHeading, shared, fail};
"#;

/// Generic Mijn NS labels that may stay visible in the travel proof (compared lower-case, with
/// collapsed spaces). Anything else outside the trip is covered.
const MASK_ALLOW: &[&str] = &[
    "direct naar hoofdinhoud",
    "hoofdnavigatie",
    "ga naar de homepage van ns.nl",
    "reizen",
    "open submenu",
    "producten",
    "dagje uit",
    "service en contact",
    "mijn ns",
    "ingelogd met:",
    "zoeken",
    "zakelijk",
    "ns international",
    "in english",
    "startpagina",
    "mijn reizen",
    "betaaloverzicht",
    "gegevens en voorkeuren",
    "uitloggen",
    "reishistorie",
    "periode",
    "tonen",
    "download overzicht",
    "laatste 7 dagen",
    "deze maand",
    "vorige maand",
    "afgelopen jaar",
    "afgelopen 1,5 jaar",
    "kies handmatig",
    "van",
    "tot",
    "t/m",
    "tot en met",
    "begindatum",
    "einddatum",
    "startdatum",
    "filter",
    "filters",
    "ns",
    "hulp nodig?",
    "geld terug bij vertraging",
    "terug",
    "reishistorie & betalingen",
    "kaart",
    "geld terug bij vergeten uitchecken",
    "teruggaven",
    "declaratieoverzicht",
    "mijn laatste reis ontbreekt",
    "staat je vraag er niet tussen? bekijk",
    "meer vragen en antwoorden",
    "disclaimer",
    "privacy",
    "cookies",
    "bedrijfsinformatie",
    "voorwaarden en folders",
];

/// Page script that lays grey blocks over the page (`R`: rectangles in page coordinates).
const MASK_APPLY: &str = r##"
document.getElementById('ovrefund-mask')?.remove();
const m = document.createElement('div'); m.id = 'ovrefund-mask';
m.style.cssText = 'position:absolute;left:0;top:0;width:0;height:0;z-index:2147483647;pointer-events:none';
for (const r of R) {
  const d = document.createElement('div');
  d.style.cssText = `position:absolute;left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;background:#d6d6d6;display:flex;align-items:center;justify-content:center;overflow:hidden;font:13px Arial,sans-serif;color:#7a7a7a`;
  if (r.h >= 28 && r.w >= 80) d.textContent = 'verborgen';
  m.appendChild(d);
}
document.documentElement.appendChild(m);
return m.childElementCount === R.length;
"##;

const MASK_VERIFY: &str = r#"
const root = document.getElementById('ovrefund-mask');
if (!root || root.children.length !== R.length) return false;
const visible = s => s.display !== 'none' && s.opacity === '1' && s.visibility === 'visible';
if (!visible(getComputedStyle(root))) return false;
return R.every((r, i) => {
  const block = root.children[i], style = getComputedStyle(block), actual = block.getBoundingClientRect();
  return visible(style) && style.backgroundColor === 'rgb(214, 214, 214)'
    && Math.abs(actual.left + scrollX - r.x) < 0.5 && Math.abs(actual.top + scrollY - r.y) < 0.5
    && Math.abs(actual.width - r.w) < 0.5 && Math.abs(actual.height - r.h) < 0.5;
});
"#;

/// A rectangle in page coordinates (CSS px).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// The blocks to draw: each covered area with a 2 px margin, clipped to the page, empty ones
/// dropped and overlapping ones merged into one block.
fn mask_blocks(w: f64, h: f64, hide: &[Rect]) -> Vec<Rect> {
    let mut blocks: Vec<Rect> = vec![];
    for r in hide.iter().filter(|r| r.w > 0.0 && r.h > 0.0) {
        let x0 = (r.x - 2.0).max(0.0);
        let y0 = (r.y - 2.0).max(0.0);
        let x1 = (r.x + r.w + 2.0).min(w);
        let y1 = (r.y + r.h + 2.0).min(h);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        blocks.push(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 });
    }
    loop {
        let mut merged = false;
        'outer: for i in 0..blocks.len() {
            for k in i + 1..blocks.len() {
                let (a, b) = (blocks[i], blocks[k]);
                if a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h {
                    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
                    let (x1, y1) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
                    blocks[i] = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
                    blocks.remove(k);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            break;
        }
    }
    blocks
}

/// A drawn mask: the trip's rectangle it was made for and the blocks on the page.
struct Mask {
    keep: Rect,
    blocks: Vec<Rect>,
}

/// What [`MASK_MEASURE`] found on the page now: (page width, page height, trip, areas to cover).
/// Fails (no screenshot) when the trip could not be measured, shares its block with another trip,
/// or the page has something that cannot be covered safely (see `fail` in [`MASK_MEASURE`]).
fn measure_mask(b: &NsBridge, row: &str) -> Result<(f64, f64, Rect, Vec<Rect>, bool)> {
    let script = MASK_MEASURE.replace("%ALLOW%", &serde_json::to_string(MASK_ALLOW)?);
    let m = b.eval(&format!("(() => {{ const row = ({row}); {script} }})()"))?;
    mask_measurement(&m)
}

fn mask_measurement(m: &Value) -> Result<(f64, f64, Rect, Vec<Rect>, bool)> {
    let keep: Vec<Rect> = serde_json::from_value(m["keep"].clone()).map_err(|_| anyhow!(TRIP_SHOT_FAILED_MSG))?;
    let trip = keep.first().copied().filter(|r| r.w > 0.0 && r.h > 0.0).ok_or_else(|| anyhow!(TRIP_SHOT_FAILED_MSG))?;
    anyhow::ensure!(m["shared"] == false, TRIP_SHOT_FAILED_MSG);
    if m["fail"].is_string() {
        log::warn!("travel proof not taken: the page has content that cannot be covered");
        bail!(TRIP_SHOT_FAILED_MSG);
    }
    anyhow::ensure!(m["fail"].is_null(), TRIP_SHOT_FAILED_MSG);
    let dim = |key| m[key].as_f64().filter(|n| *n > 0.0).ok_or_else(|| anyhow!(TRIP_SHOT_FAILED_MSG));
    let hide = serde_json::from_value(m["hide"].clone()).map_err(|_| anyhow!(TRIP_SHOT_FAILED_MSG))?;
    Ok((dim("w")?, dim("h")?, trip, hide, m["heading"] == true))
}

/// Draws the grey blocks over the areas to cover.
fn draw_mask(b: &NsBridge, w: f64, h: f64, keep: Rect, hide: &[Rect]) -> Result<Mask> {
    let blocks = mask_blocks(w, h, hide);
    let applied = b.eval(&format!("(() => {{ const R = {}; {MASK_APPLY} }})()", serde_json::to_string(&blocks)?))?;
    anyhow::ensure!(applied == true, TRIP_SHOT_FAILED_MSG);
    Ok(Mask { keep, blocks })
}

/// Covers the other trips and every non-generic text outside the trip with grey blocks.
fn apply_mask(b: &NsBridge, row: &str) -> Result<(String, Mask)> {
    let (w, h, keep, hide, heading) = measure_mask(b, row)?;
    let mask = draw_mask(b, w, h, keep, &hide)?;
    Ok((format!("masked ({} blocks, heading {heading})", mask.blocks.len()), mask))
}

/// Right before the screenshot: the page is measured again. When something new is uncovered or the
/// trip moved more than 2 px, the mask is drawn once more and checked again; if it still differs,
/// no screenshot is taken.
fn mask_is_drawn(b: &NsBridge, mask: &Mask) -> Result<bool> {
    let blocks = serde_json::to_string(&mask.blocks)?;
    Ok(b.eval(&format!("(() => {{ const R = {blocks}; {MASK_VERIFY} }})()"))? == true)
}

fn confirm_mask(b: &NsBridge, row: &str, mask: Mask) -> Result<Mask> {
    let (w, h, keep, hide, _) = measure_mask(b, row)?;
    if mask_still_covers(&mask, w, h, keep, &hide) && mask_is_drawn(b, &mask)? {
        return Ok(mask);
    }
    log::info!("travel proof: the page changed after masking; masking again");
    let mask = draw_mask(b, w, h, keep, &hide)?;
    std::thread::sleep(Duration::from_millis(200));
    let (w, h, keep, hide, _) = measure_mask(b, row)?;
    anyhow::ensure!(mask_still_covers(&mask, w, h, keep, &hide) && mask_is_drawn(b, &mask)?, TRIP_SHOT_FAILED_MSG);
    Ok(mask)
}

/// Whether a drawn mask still fits the page: the trip has not moved more than 2 px, and every area
/// to cover now (clipped to the page, as the blocks are) lies inside a drawn block.
fn mask_still_covers(mask: &Mask, w: f64, h: f64, keep: Rect, hide: &[Rect]) -> bool {
    let k = mask.keep;
    if (keep.x - k.x).abs() > 2.0 || (keep.y - k.y).abs() > 2.0 || (keep.w - k.w).abs() > 2.0 || (keep.h - k.h).abs() > 2.0 {
        return false;
    }
    hide.iter().filter(|r| r.w > 0.0 && r.h > 0.0).all(|r| {
        let (x0, y0, x1, y1) = (r.x.max(0.0), r.y.max(0.0), (r.x + r.w).min(w), (r.y + r.h).min(h));
        x1 <= x0
            || y1 <= y0
            || mask.blocks.iter().any(|b| x0 >= b.x - 0.5 && y0 >= b.y - 0.5 && x1 <= b.x + b.w + 0.5 && y1 <= b.y + b.h + 0.5)
    })
}

/// Rejects the captured image if the mask or the measured page moved during capture.
fn ensure_capture_mask(b: &NsBridge, row: &str, mask: &Mask) -> Result<()> {
    let (w, h, keep, hide, _) = measure_mask(b, row)?;
    anyhow::ensure!(mask_still_covers(mask, w, h, keep, &hide) && mask_is_drawn(b, mask)?, TRIP_SHOT_FAILED_MSG);
    Ok(())
}

/// Removes the mask again (the NS window stays in use).
fn remove_mask(b: &NsBridge) {
    let _ = b.eval("(() => { document.getElementById('ovrefund-mask')?.remove(); return true; })()");
}

/// The page from y=0 to just below the expanded trip (all scroll positions at the top), full width,
/// with the other trips and personal details covered (see [`apply_mask`]).
fn capture_page(b: &NsBridge, row: &str) -> Result<(Vec<u8>, String)> {
    let measure = format!(
        "(() => {{ const row = ({row}); const box = row.parentElement || row; scrollTo(0, 0); \
         for (let n = box.parentElement; n; n = n.parentElement) if (n.scrollTop) n.scrollTop = 0; \
         const r = box.getBoundingClientRect();          let bar = 0; for (const e of document.querySelectorAll('body *')) {{ const st = getComputedStyle(e); if ((st.position === 'fixed' || st.position === 'sticky') && e.getBoundingClientRect().bottom >= innerHeight - 2 && e.getBoundingClientRect().height < 300) bar = Math.max(bar, e.getBoundingClientRect().height); }}          return {{bottom: r.bottom + scrollY, bar, page: document.documentElement.scrollHeight}}; }})()"
    );
    let m = b.eval(&measure)?;
    let bottom = m["bottom"].as_f64().unwrap_or(0.0);
    anyhow::ensure!(bottom > 0.0, "De reis in het reisoverzicht heeft geen afmeting. / The trip in the travel history has no size.");
    // NS's "Download overzicht" bar sticks to the bottom of the window: leave room for it below the trip.
    let bar = m["bar"].as_f64().unwrap_or(0.0).clamp(0.0, 300.0);
    let (png, how) = if bottom + 32.0 + bar <= MAX_PAGE {
        let height = (bottom + 32.0 + bar).ceil().max(1000.0);
        // A viewport as tall as the page part: also works when the page scrolls inside an element.
        b.cdp(
            "Emulation.setDeviceMetricsOverride",
            serde_json::json!({"width": 1440, "height": height, "deviceScaleFactor": 1, "mobile": false}),
            Duration::from_secs(10),
        )?;
        std::thread::sleep(Duration::from_millis(700));
        let bottom2 = b.eval(&measure)?["bottom"].as_f64().unwrap_or(bottom);
        let bar2 = b.eval(&measure)?["bar"].as_f64().unwrap_or(bar).clamp(0.0, 300.0);
        let clip = (bottom2 + 32.0 + bar2).ceil().min(height);
        std::thread::sleep(Duration::from_millis(300));
        let masked = apply_mask(b, row);
        let r = masked.and_then(|(how, mask)| {
            std::thread::sleep(Duration::from_millis(200));
            let mask = confirm_mask(b, row, mask)?;
            let image = b.cdp(
                "Page.captureScreenshot",
                serde_json::json!({"format": "png", "clip": {"x": 0, "y": 0, "width": 1440, "height": clip, "scale": 1}}),
                Duration::from_secs(30),
            )?;
            ensure_capture_mask(b, row, &mask)?;
            Ok((image, how))
        });
        remove_mask(b);
        let (r, how) = r?;
        (r, format!("page top to trip, 1440x{clip} CSS px, {how}"))
    } else {
        b.eval(&format!("(() => {{ const row = ({row}); {PAGE_VIEW} }})()"))?;
        std::thread::sleep(Duration::from_millis(400));
        let r = apply_mask(b, row).and_then(|(how, mask)| {
            std::thread::sleep(Duration::from_millis(200));
            let mask = confirm_mask(b, row, mask)?;
            let image =
                b.cdp("Page.captureScreenshot", serde_json::json!({"format": "png", "fromSurface": true}), Duration::from_secs(30))?;
            ensure_capture_mask(b, row, &mask)?;
            Ok((image, how))
        });
        remove_mask(b);
        let (r, how) = r?;
        (r, format!("viewport 1440x1000 (page to the trip is {bottom:.0} CSS px, over {MAX_PAGE:.0}), {how}"))
    };
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(png["data"].as_str().unwrap_or(""))?;
    anyhow::ensure!(bytes.starts_with(b"\x89PNG"), "Schermafbeelding van de reis is leeg. / Trip screenshot is empty.");
    Ok((bytes, how))
}

/// Page script for the viewport fallback: scrolls so the trip's day heading and expanded row sit
/// in view (heading near the top third).
const PAGE_VIEW: &str = r#"
const box = row.parentElement || row;
const list = document.querySelector('mns-travel-history-list');
const items = list ? [...list.querySelectorAll('nes-heading, [tabindex]')] : [];
const i = items.indexOf(row);
let head = null;
for (let k = i - 1; k >= 0; k--) if (items[k].tagName.toLowerCase() === 'nes-heading') { head = items[k]; break; }
const top = (head || box).getBoundingClientRect().top + scrollY;
scrollTo(0, Math.max(0, top - innerHeight * 0.3));
return true;
"#;

/// A travel-history screenshot (pixels as captured; only the rectangle is chosen).
pub struct RowShot {
    pub png: Vec<u8>,
    /// How the list was narrowed (day filter or preset).
    pub filter: String,
    /// What was captured (page top to trip, or the viewport).
    pub capture: String,
}

/// Travel proof for any journey, whatever the carrier (Arriva trips are in the Mijn NS history too):
/// checks the session and the account like the claim route, then captures the trip's page.
pub fn capture_history_trip(b: &NsBridge, j: &Journey, strict: bool) -> Result<RowShot> {
    match b.ensure_session(Duration::from_secs(45))? {
        SessionStatus::LoggedIn => {}
        SessionStatus::LoginRequired => bail!(ovrefund_core::pipeline::LOGIN_NEEDED_MSG),
        SessionStatus::AutoLoginFailed => bail!(ovrefund_core::pipeline::AUTOLOGIN_FAILED_MSG),
        SessionStatus::Blocked(r) => {
            bail!("{}", ovrefund_core::pipeline::auth_message(&format!("blocked: {r}")).unwrap_or(ovrefund_core::pipeline::BLOCKED_MSG))
        }
    }
    check_account(b, false)?;
    capture_trip_row(b, j, strict)
}

fn history_hash(coupling: Option<&str>) -> String {
    match coupling.filter(|id| !id.is_empty()) {
        Some(id) => {
            let mut url = tauri::Url::parse("https://www.ns.nl/mijnns").expect("valid Mijn NS URL");
            url.query_pairs_mut().append_pair("card", id);
            format!("#/reishistorie?{}", url.query().unwrap_or_default())
        }
        None => "#/reishistorie".into(),
    }
}

/// The chosen card's travel-history page with its period filter loaded.
fn open_history(b: &NsBridge, coupling: Option<&str>) -> Result<()> {
    b.dismiss_cookie_banner();
    let hash = serde_json::to_string(&history_hash(coupling))?;
    b.eval(&format!("location.hash = {hash}; true"))?;
    b.wait_until("travel history", Duration::from_secs(30), |b| Ok(el::resolve(b, &el::PERIOD_FILTER, None).is_ok()))
}

/// Chooses a preset in NS's period filter and presses Tonen.
fn choose_period(b: &NsBridge, label: &str) -> Result<()> {
    select_period(b, label, Duration::from_secs(10))
        .map_err(|_| anyhow!("Periode {label:?} niet gevonden in het filter. / Period {label:?} not found in the filter."))?;
    std::thread::sleep(Duration::from_millis(800));
    press_show(b).context("period filter (Tonen)")?;
    std::thread::sleep(Duration::from_millis(1200));
    Ok(())
}

/// The shortest preset of NS's period filter that contains `day` (seen from `today`).
pub fn preset_for(day: chrono::NaiveDate, today: chrono::NaiveDate) -> &'static str {
    use chrono::Datelike;
    let age = (today - day).num_days();
    let prev = today.with_day(1).and_then(|d| d.pred_opt()).unwrap_or(today);
    if age <= 6 {
        "Laatste 7 dagen"
    } else if (day.year(), day.month()) == (today.year(), today.month()) {
        "Deze maand"
    } else if (day.year(), day.month()) == (prev.year(), prev.month()) {
        "Vorige maand"
    } else if age <= 360 {
        "Afgelopen jaar"
    } else {
        "Afgelopen 1,5 jaar"
    }
}

/// Dutch month abbreviations as in NS's date buttons ("van 13 nov 2026").
const MONTHS_NL_SHORT: [&str; 12] = ["jan", "feb", "mrt", "apr", "mei", "jun", "jul", "aug", "sep", "okt", "nov", "dec"];

/// Page helpers for the date picker: a deep list of visible elements, and an element description
/// for diagnostics (tag, id, classes, role, aria-label, short own text).
const PICKER_JS: &str = r#"
window.__ovDeep = (root = document, out = []) => { for (const e of root.querySelectorAll('*')) { out.push(e); if (e.shadowRoot) window.__ovDeep(e.shadowRoot, out); } return out; };
window.__ovVisible = e => { if (!e.getClientRects().length) return false; const r = e.getBoundingClientRect(), s = getComputedStyle(e); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
window.__ovOwn = e => [...e.childNodes].filter(n => n.nodeType === 3).map(n => n.textContent).join(' ').replace(/\s+/g, ' ').trim();
window.__ovText = e => (e.innerText ?? e.textContent ?? '').replace(/\s+/g, ' ').trim();
window.__ovDescribe = e => `${e.tagName.toLowerCase()}${e.id ? '#' + e.id : ''}${typeof e.className === 'string' && e.className.trim() ? '.' + e.className.trim().split(/\s+/).slice(0, 4).join('.') : ''}${e.getAttribute('role') ? '[role=' + e.getAttribute('role') + ']' : ''}${e.getAttribute('aria-label') ? '[aria-label="' + e.getAttribute('aria-label').slice(0, 40) + '"]' : ''}${e.disabled || e.getAttribute('aria-disabled') === 'true' ? '[disabled]' : ''} "${window.__ovOwn(e).slice(0, 25)}"`;
true
"#;

/// Sets NS's period filter to `day` - `day` with "Kies handmatig" and the two calendars (trusted
/// clicks: open the date button, go to the month, pick the day), then presses Tonen. Returns the
/// number of day headings listed. On failure returns why, with a short outline of the calendar
/// elements found (no personal data: calendar labels and day numbers only).
fn set_day_filter(b: &NsBridge, day: chrono::NaiveDate) -> std::result::Result<usize, String> {
    use chrono::Datelike;
    let e = |x: anyhow::Error| format!("{x:#}");
    b.eval(PICKER_JS).map_err(e)?;
    select_period(b, "Kies handmatig", Duration::from_secs(10)).map_err(|x| format!("no \"Kies handmatig\" option ({x:#})"))?;
    b.wait_until("date pickers", Duration::from_secs(10), |b| {
        Ok(b.eval("!!document.querySelector('date-picker[formcontrolname=startDate] button.dateButton') && !!document.querySelector('date-picker[formcontrolname=endDate] button.dateButton')")?
            .as_bool()
            .unwrap_or(false))
    })
    .map_err(|_| "date buttons did not appear".to_string())?;
    let (d, m0, y) = (day.day(), day.month0() as usize, day.year());
    for which in ["startDate", "endDate"] {
        let button = format!("document.querySelector('date-picker[formcontrolname={which}] button.dateButton')");
        b.eval("window.__ovBefore = new Set(window.__ovDeep()); true").map_err(e)?;
        // The filter row animates open after "Kies handmatig": retry while the button is still moving.
        let mut clicked = Ok(());
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(700));
            clicked = b.click(&button);
            if clicked.is_ok() {
                break;
            }
        }
        clicked.map_err(|x| format!("{which}: date button not clickable ({x:#})"))?;
        std::thread::sleep(Duration::from_millis(700));
        // The calendar: elements that appeared with the click (or visible ones inside the date picker / an overlay).
        let scope = format!(
            "(() => {{ const picker = document.querySelector('date-picker[formcontrolname={which}]'); \
             const fresh = window.__ovDeep().filter(x => !window.__ovBefore.has(x)); \
             const inside = [...window.__ovDeep(picker), ...[...document.querySelectorAll('.cdk-overlay-container, [class*=overlay], [class*=popup], [class*=calendar]')].flatMap(o => window.__ovDeep(o))]; \
             window.__ovCal = [...new Set([...fresh, ...inside])].filter(window.__ovVisible); return window.__ovCal.length }})()"
        );
        let n = b.eval(&scope).map_err(e)?.as_u64().unwrap_or(0);
        let outline = || {
            b.eval("window.__ovCal.slice(0, 80).map(window.__ovDescribe).join('\\n')")
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        };
        if n == 0 {
            return Err(format!("{which}: no calendar appeared after clicking the date button"));
        }
        // Go to the trip's month: read "<month> <year>" (or month/year selects), then previous/next.
        let months = serde_json::to_string(&MONTHS_NL).unwrap_or_default();
        let shorts = serde_json::to_string(&MONTHS_NL_SHORT).unwrap_or_default();
        let target = y * 12 + m0 as i32;
        let mut reached = false;
        for _ in 0..30 {
            let cur = b
                .eval(&format!(
                    "(() => {{ const M = {months}, S = {shorts}; const idx = t => {{ t = t.toLowerCase().replace('.', ''); let i = M.findIndex(m => t.startsWith(m)); if (i < 0) i = S.findIndex(m => t.startsWith(m)); return i; }}; \
                     for (const s of window.__ovCal.filter(x => x.tagName === 'SELECT')) {{ const o = s.options[s.selectedIndex]; if (o && idx(o.text.trim()) >= 0) {{ const ys = window.__ovCal.find(x => x.tagName === 'SELECT' && x !== s && /^\\d{{4}}$/.test((x.options[x.selectedIndex]?.text || '').trim())); if (ys) return idx(o.text.trim()) + 12 * Number(ys.options[ys.selectedIndex].text.trim()); }} }} \
                     for (const x of window.__ovCal) {{ const t = window.__ovText(x); const m = /^([a-z]+)\\.?\\s+(\\d{{4}})$/i.exec(t); if (m && idx(m[1]) >= 0 && x.children.length <= 3) return idx(m[1]) + 12 * Number(m[2]); }} return null; }})()"
                ))
                .map_err(e)?;
            let Some(cur) = cur.as_i64() else {
                return Err(format!("{which}: month header not found in the calendar\n{}", outline()));
            };
            if cur as i32 == target {
                reached = true;
                break;
            }
            let dir = if (cur as i32) > target { "prev" } else { "next" };
            let nav = format!(
                "(() => {{ const want = {dir:?} === 'prev' ? /vorige|previous|prev|eerder|terug|chevron-left|arrow-left|left/i : /volgende|next|later|verder|chevron-right|arrow-right|right/i; \
                 window.__ovTarget = window.__ovCal.find(x => (x.tagName === 'BUTTON' || x.getAttribute('role') === 'button' || x.tagName === 'A') && want.test([x.getAttribute('aria-label'), x.title, typeof x.className === 'string' ? x.className : '', x.innerHTML.slice(0, 200)].join(' '))); return !!window.__ovTarget }})()"
            );
            if b.eval(&nav).map_err(e)? != true {
                return Err(format!("{which}: no {dir} month button\n{}", outline()));
            }
            b.click("window.__ovTarget").map_err(|x| format!("{which}: {dir} month button not clickable ({x:#})"))?;
            std::thread::sleep(Duration::from_millis(400));
            b.eval(&scope).map_err(e)?;
        }
        if !reached {
            return Err(format!("{which}: could not reach the trip's month\n{}", outline()));
        }
        // The day cell: own text = the day number, enabled, not a day of a neighbouring month.
        let pick = format!(
            "(() => {{ const other = /disabled|other|outside|adjacent|muted|prev-month|next-month|not-current|inactive/i; \
             const cells = window.__ovCal.filter(x => window.__ovText(x) === '{d}' && !x.disabled && x.getAttribute('aria-disabled') !== 'true' && !other.test(typeof x.className === 'string' ? x.className : '')); \
             const clickable = cells.filter(x => x.tagName === 'BUTTON' || ['gridcell', 'button', 'option'].includes(x.getAttribute('role')) || x.tagName === 'TD' || x.tagName === 'A'); \
             window.__ovTarget = (clickable.length ? clickable : cells).sort((a, b) => a.contains(b) ? 1 : b.contains(a) ? -1 : 0)[0] || null; return !!window.__ovTarget }})()"
        );
        if b.eval(&pick).map_err(e)? != true {
            return Err(format!("{which}: day {d} not found in the calendar\n{}", outline()));
        }
        b.click("window.__ovTarget").map_err(|x| format!("{which}: day {d} not clickable ({x:#})"))?;
        std::thread::sleep(Duration::from_millis(600));
        let label = b.eval(&format!("({button})?.innerText || ''")).map_err(e)?;
        let label = label.as_str().unwrap_or("").to_lowercase();
        let ok = label.contains(&format!("{d} {}", MONTHS_NL_SHORT[m0])) || label.contains(&format!("{d} {}", MONTHS_NL[m0]));
        if !(ok && label.contains(&y.to_string())) {
            return Err(format!("{which}: the date button shows {label:?} after choosing the day"));
        }
    }
    press_show(b).map_err(|x| format!("Tonen not clickable ({x:#})"))?;
    std::thread::sleep(Duration::from_millis(1500));
    let want = format!("{d} {} {y}", MONTHS_NL[m0]);
    let heads = b
        .eval(&format!(
            "(async () => {{ {DAY_MATCH_JS} for (let t = 0; t < 30; t++) {{ const h = [...document.querySelectorAll('mns-travel-history-list nes-heading')].map(x => x.innerText); if (h.some(x => window.__ovDay(x, {want:?}))) return h.length; await new Promise(r => setTimeout(r, 500)); }} return -1 }})()"
        ))
        .map_err(e)?;
    match heads.as_i64() {
        Some(n) if n >= 1 => Ok(n as usize),
        _ => Err("the trip's day is not listed after Tonen".into()),
    }
}

/// Opens the travel history (with the period filter for older trips) and returns a JS expression
/// for the row of the trip that departed at `check_in` from `origin_name`.
fn locate_trip_row(b: &NsBridge, facts: &ClaimFacts) -> Result<String> {
    open_history(b, Some(&facts.coupling_id))?;
    let amsterdam = chrono_tz::Europe::Amsterdam;
    let day = facts.check_in.with_timezone(&amsterdam).date_naive();
    let today = Utc::now().with_timezone(&amsterdam).date_naive();
    let age = (today - day).num_days();
    // The default week can be empty even though an older trip is present.
    if age > 6 {
        let label = if age > 360 { el::PERIOD_MAX } else { el::PERIOD_YEAR };
        select_period(b, label, Duration::from_secs(20)).context("period filter")?;
        std::thread::sleep(Duration::from_millis(800));
        press_show(b).context("period filter (Tonen)")?;
    }
    find_trip_row(b, facts.check_in, &facts.origin_name)
}

/// Page helper: whether an NS day heading ("1 november 2026", possibly with a weekday or comma) is
/// exactly the day `w` ("1 november 2026"); "11 november 2026" is not "1 november 2026".
const DAY_MATCH_JS: &str = "window.__ovDay = (h, w) => (' ' + String(h).toLowerCase().replace(/[,.]/g, ' ').replace(/\\s+/g, ' ').trim() + ' ').includes(' ' + String(w).toLowerCase().trim() + ' ');";

/// In the listed travel history: a JS expression for the row of the trip that departed at `check_in`
/// from `origin_name` (day heading + departure "HH:MMStation" in the route element's shadow DOM).
/// Exactly one row must match.
fn find_trip_row(b: &NsBridge, check_in: DateTime<Utc>, origin_name: &str) -> Result<String> {
    use chrono::{Datelike, Timelike};
    let local = check_in.with_timezone(&chrono_tz::Europe::Amsterdam);
    let date_txt = format!("{} {} {}", local.day(), MONTHS_NL[local.month0() as usize], local.year());
    let dep_txt = format!("{:02}:{:02}{}", local.hour(), local.minute(), origin_name);
    // Find the row: heading (date) + departure "HH:MMStation" in the route element's shadow DOM.
    let find = format!(
        "(async () => {{ {DAY_MATCH_JS} const want = {date}, dep = {dep};           for (let t = 0; t < 60; t++) {{             const l = document.querySelector('mns-travel-history-list'); let head = ''; const hits = []; let n = 0;             if (l) for (const el of l.querySelectorAll('nes-heading, [tabindex]')) {{               if (el.tagName.toLowerCase() === 'nes-heading') {{ head = el.innerText; continue; }}               const d = el.querySelector('nes-route[data-test-id=journeyDeparture]');               const txt = d && d.shadowRoot ? d.shadowRoot.textContent.trim() : '';               if (window.__ovDay(head, want) && txt.replace(/\\s/g, '') === dep.replace(/\\s/g, '')) hits.push(n); n++;             }}             if (hits.length > 1) return -2; if (hits.length === 1) return hits[0]; await new Promise(r => setTimeout(r, 500)); }} return -1; }})()",
        // JSON string literals: a station name with quotes or backslashes cannot break the script.
        date = serde_json::to_string(&date_txt.to_lowercase())?,
        dep = serde_json::to_string(&dep_txt)?
    );
    let idx = b.eval(&find)?.as_i64().unwrap_or(-1);
    anyhow::ensure!(
        idx != -2,
        "Meer dan één reis op die dag vertrekt op die tijd van dat station in je reishistorie. / More than one trip on that day departs at that time from that station in your travel history."
    );
    anyhow::ensure!(idx >= 0, "De reis staat niet in je reishistorie in Mijn NS. / The trip is not in your Mijn NS travel history.");
    Ok(format!("document.querySelectorAll('mns-travel-history-list [tabindex]')[{idx}]"))
}

impl NsClaimGateway {
    pub(crate) fn ensure_claim_session(&self) -> Result<()> {
        let b = &self.bridge;
        let failed = match b.ensure_session(Duration::from_secs(45))? {
            SessionStatus::LoggedIn => None,
            SessionStatus::LoginRequired => Some("login_required".into()),
            SessionStatus::AutoLoginFailed => Some("autologin_failed".into()),
            SessionStatus::Blocked(reason) => Some(format!("blocked: {reason}")),
        };
        if let Some(state) = failed {
            return Err(ClaimSessionFailure(state).into());
        }
        check_account(b, false).map_err(ClaimSessionFailure::from_account_error)
    }

    /// Open the journey-linked claim form and fill everything except the declaration.
    /// Returns human-readable notes. Never submits.
    pub fn fill_form(&mut self, f: &ClaimFacts) -> Result<Vec<String>> {
        self.ensure_claim_session()?;
        let b = &self.bridge;
        open_claim_form(b, f)?;
        // Every element below is found by the resolver (observed id first, then label/question
        // text, then ARIA role/name; unique matches only). The checks stay the same whichever
        // strategy matched.
        b.wait_until("claim form", Duration::from_secs(45), |b| {
            el::ensure_resolver(b)?;
            Ok(el::resolve(b, &el::DEPARTURE, None).is_ok() && el::resolve(b, &el::CHECK_IN, None).is_ok())
        })
        .map_err(|e| {
            // Name the element that is missing (no page text, no values).
            let why = [&el::DEPARTURE, &el::CHECK_IN].iter().find_map(|s| el::resolve(b, s, None).err()).map(|e| e.to_string());
            anyhow!("{e}: {}", why.unwrap_or_default())
        })?;
        let mut notes = vec![];
        // 1. Verify the prefilled check-in/out belong to this journey.
        let (ci, co) = (form_el(b, &el::CHECK_IN)?, form_el(b, &el::CHECK_OUT)?);
        let sel = b.eval(&format!(
            "((ci, co) => ({{ci: ci && ci.value, co: co && co.value, cit: ci && ci.options[ci.selectedIndex] && ci.options[ci.selectedIndex].text, cot: co && co.options[co.selectedIndex] && co.options[co.selectedIndex].text}}))({}, {})",
            ci.expr, co.expr
        ))?;
        // UTC on purpose: NS's prefilled check-in/check-out option values are the transaction's UTC
        // timestamps (the same `checkinUtc` that is in the form's route), not local times. `f.check_in`
        // and `f.check_out` are the UTC timestamps from NS's travel history, so formatting them in
        // UTC compares like with like (the live form test passed with this comparison).
        let ci_ok = timestamp_matches(&sel["ci"], f.check_in);
        let co_ok = timestamp_matches(&sel["co"], f.check_out);
        if !ci_ok || !co_ok {
            bail!("De tijden in het NS-formulier horen niet bij deze reis. Er is niets verstuurd. / The times in NS's form do not match this trip. Nothing was sent.");
        }
        let (cit, cot) = (sel["cit"].as_str().unwrap_or(""), sel["cot"].as_str().unwrap_or(""));
        notes.push(format!(
            "Inchecken en uitchecken ingevuld door NS: {cit} – {cot} / Check-in and check-out prefilled by NS: {cit} – {cot}"
        ));
        // 2. Reached NS destination: yes / no
        let arrival: &'static ElSpec = if f.reached_destination { &el::ARRIVAL_YES } else { &el::ARRIVAL_NO };
        click_choice(b, arrival).context("answer: NS destination reached")?;
        // 3. Scheduled departure (HH:MM, Europe/Amsterdam)
        let t = &f.scheduled_departure_local;
        anyhow::ensure!(t.len() == 5 && chrono::NaiveTime::parse_from_str(t, "%H:%M").is_ok(), "invalid scheduled departure");
        let dep = form_el(b, &el::DEPARTURE)?;
        b.click(&dep.expr).context("scheduled departure field")?;
        let set = b.eval(&format!(
            "(i => {{ if (!i) return null; const s = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; s.call(i, '{t}'); i.dispatchEvent(new Event('input', {{bubbles:true}})); i.dispatchEvent(new Event('change', {{bubbles:true}})); i.dispatchEvent(new Event('blur', {{bubbles:true}})); return i.value }})({})",
            dep.expr
        ))?;
        anyhow::ensure!(set.as_str() == Some(t.as_str()), "could not set scheduled departure");
        notes.push(format!("Geplande vertrektijd {t} / Scheduled departure {t}"));
        // 4. Delay band
        let band: &'static ElSpec = match f.delay_band {
            DelayBand::Min30To59 => &el::DELAY_30,
            DelayBand::Min60Plus => &el::DELAY_60,
        };
        click_choice(b, band).context("answer: delay")?;
        notes.push(format!("Vertraging: {} / Delay: {}", f.delay_band.label_nl(), f.delay_band.label_en()));
        // 5. Bank details: always the stored name + IBAN (NS may prefill an older account), typed
        //    via trusted input after clearing the field; never logged. The IBAN is read back.
        let mut bank_fields: Option<(Resolved, Resolved)> = None;
        if let Some(bank) = self.bank.clone() {
            let fields = match (form_el(b, &el::NAME), form_el(b, &el::IBAN)) {
                (Ok(n), Ok(i)) => (n, i),
                _ => bail!(FORM_CHANGED_MSG),
            };
            for (field, val) in [(&fields.0, bank.name.as_str()), (&fields.1, bank.iban.as_str())] {
                let cleared = b.eval(&format!(
                    "(i => {{ if (!i) return false; const s = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; s.call(i, ''); i.dispatchEvent(new Event('input', {{bubbles: true}})); return i.value === '' }})({})",
                    field.expr
                ))?;
                // The field must exist and be empty before typing (no leftover of an older account).
                if cleared.as_bool() != Some(true) {
                    bail!(FORM_CHANGED_MSG);
                }
                b.click(&field.expr)?;
                // Verify the top-level NS origin and the exact focused field immediately before typing.
                let url = b.window.url()?;
                if url.scheme() != "https" || url.host_str() != Some("www.ns.nl") {
                    bail!(FORM_CHANGED_MSG);
                }
                let focused_ok = b.eval(&bank_focus_script(&field.expr))?.as_bool().unwrap_or(false);
                if !focused_ok {
                    bail!(FORM_CHANGED_MSG);
                }
                b.cdp("Input.insertText", serde_json::json!({ "text": val }), Duration::from_secs(10))?;
            }
            let shown = b.eval(&format!(
                "((n, i) => ({{ iban: (i || {{}}).value || '', name: (n || {{}}).value || '' }}))({}, {})",
                fields.0.expr, fields.1.expr
            ))?;
            if !iban_matches(shown["iban"].as_str().unwrap_or(""), &bank.iban) {
                bail!(IBAN_MISMATCH_MSG);
            }
            if !holder_matches(shown["name"].as_str().unwrap_or(""), &bank.name) {
                bail!(NAME_MISMATCH_MSG);
            }
            notes.push(
                "Rekeninggegevens ingevuld vanuit Windows Referentiebeheer / Bank details entered from Windows Credential Manager".into(),
            );
            bank_fields = Some(fields);
        } else {
            notes.push("Rekeninggegevens zijn NIET ingesteld / Bank details are NOT set up".into());
        }
        // 6. Read back both answers, the departure time, bank fields and validation messages
        //    (elements found again, so a re-rendered form is checked as it is now).
        let (arrival_el, band_el, dep) = (form_el(b, arrival)?, form_el(b, band)?, form_el(b, &el::DEPARTURE)?);
        anyhow::ensure!(is_checked(b, &arrival_el), "destination answer not selected");
        anyhow::ensure!(is_checked(b, &band_el), "delay radio not selected");
        let (name_expr, iban_expr) = match &bank_fields {
            Some((n, i)) => (n.expr.clone(), i.expr.clone()),
            None => {
                let opt = |s: &'static ElSpec| el::resolve(b, s, None).map(|r| r.expr).unwrap_or_else(|_| "null".into());
                (opt(&el::NAME), opt(&el::IBAN))
            }
        };
        let st = b.eval(&format!("((dep, n, i) => ({{ dep: (dep || {{}}).value, nameFilled: !!(n || {{}}).value, ibanFilled: !!(i || {{}}).value, errors: [...document.querySelectorAll('[role=alert], .error, nes-message-inline[type=error]')].map(e => e.innerText.trim()).filter(Boolean).slice(0,5) }}))({}, {name_expr}, {iban_expr})", dep.expr))?;
        anyhow::ensure!(st["dep"].as_str() == Some(t.as_str()), "departure field lost its value");
        // Make page drift visible in the form test and the claim's notes (static names only).
        let mut found = vec![&arrival_el, &band_el, &dep];
        if let Some((n, i)) = &bank_fields {
            found.extend([n, i]);
        }
        for r in found {
            if let Some(via) = r.via {
                notes.push(format!(
                    "Veld '{0}' gevonden via {via} (id ontbreekt of is niet eenduidig) / Field '{0}' found via {via} (id missing or not unique)",
                    r.key
                ));
            }
        }
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
fn check_account(b: &NsBridge, bind_import: bool) -> Result<()> {
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
    result?;
    // Bind before the importer hands any trips to the ledger. A crash after inserting trips
    // must not leave their account unknown until the end-of-run session save.
    if bind_import && stored.is_none() {
        if let Some(email) = email {
            db.set_setting("ns_account_email", &email)?;
        }
    }
    Ok(())
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
/// and visible required fields OV Refund does not know (also inside shadow roots). `__KNOWN__` is
/// replaced by the element expressions of the fields OV Refund filled or checked
/// ([`el::FORM_FIELDS`]); every other visible required field counts as unknown.
const FORM_CHECK_JS: &str = r#"(() => {
  const known = [__KNOWN__].filter(Boolean);
  const all = [];
  const walk = (r) => { for (const el of r.querySelectorAll('*')) { all.push(el); if (el.shadowRoot) walk(el.shadowRoot); } };
  walk(document);
  const vis = (el) => !!(el.offsetWidth || el.offsetHeight || (el.getClientRects && el.getClientRects().length));
  const invalid = all.filter((el) => el.getAttribute && el.getAttribute('aria-invalid') === 'true' && vis(el)).map((el) => el.id || el.name || el.tagName.toLowerCase());
  const errors = all.filter((el) => el.matches && el.matches('[role=alert], .error, nes-message-inline[type=error]') && vis(el)).map((e) => (e.innerText || '').trim()).filter(Boolean);
  const extra = all.filter((el) => el.matches && el.matches('input, select, textarea') && el.type !== 'hidden' && (el.required || (el.getAttribute && el.getAttribute('aria-required') === 'true')) && vis(el) && !known.includes(el)).map((el) => el.id || el.name || el.type);
  return { invalid, errors: errors.slice(0, 5), extra };
})()"#;

/// The pre-submit form check with the given known-field expressions.
pub fn form_check_js(known: &[String]) -> String {
    FORM_CHECK_JS.replace("__KNOWN__", &known.join(", "))
}

/// Run the pre-submit check on the open form (`Null` when it could not run = not safe).
/// Fields the resolver cannot find uniquely are not "known": if NS marks them required they show
/// up as unknown required fields and nothing is sent.
pub fn form_check(b: &NsBridge) -> Value {
    let known: Vec<String> = el::FORM_FIELDS.iter().filter_map(|s| el::resolve(b, s, None).ok()).map(|r| r.expr).collect();
    b.eval(&form_check_js(&known)).unwrap_or(Value::Null)
}

/// Problems found by [`form_check`] (empty = safe to press "Verzenden").
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
    ["applicationNumber", "aanvraagnummer", "reference", "referenceNumber", "claimReference", "caseNumber", "claimId"].iter().find_map(
        |key| {
            let value = v.get(*key)?;
            value
                .as_str()
                .map(str::trim)
                .map(str::to_string)
                .or_else(|| value.as_i64().map(|n| n.to_string()))
                .filter(|reference| !reference.is_empty() && reference.len() <= 80)
        },
    )
}

/// Is a 2xx answer to the claim write proof that NS received the claim? Returns
/// (reference to store, proven). Proof is a claim-specific reference field in the response, or
/// confirmation text that appeared on the page after the click (not already on the form).
/// A generic `id`/`requestId` is never stored as an NS application number.
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
    let page_after = page_after.to_lowercase();
    if NEGATIVE_PAGE_WORDS.iter().any(|word| page_after.contains(word))
        || v.as_ref().is_some_and(ovrefund_core::pipeline::response_has_error)
    {
        return (None, false);
    }
    if v.is_none() && body.is_some_and(|text| ovrefund_core::pipeline::negative_acknowledgement(&text.to_lowercase())) {
        return (None, false);
    }
    let specific = v.as_ref().and_then(find_reference);
    let before = page_before.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    let after = page_after.split_whitespace().collect::<Vec<_>>().join(" ");
    let confirmation_page = [
        "we hebben je aanvraag ontvangen",
        "je aanvraag is ontvangen",
        "je aanvraag is verzonden",
        "uw aanvraag is ontvangen",
        "uw aanvraag is verzonden",
    ]
    .iter()
    .any(|phrase| after.contains(phrase) && !before.contains(phrase));
    let proven = specific.is_some() || confirmation_page;
    (specific, proven)
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
        self.submit_checked(facts, &|| Ok(()))
    }

    fn submit_checked(&mut self, facts: &ClaimFacts, check: &dyn Fn() -> Result<()>) -> SubmitOutcome {
        let mark = self.bridge.mark();
        let outcome = self.submit_inner(facts, mark, check);
        guard_not_sent(outcome, self.bridge.claim_write_since(mark))
    }

    fn list_remote_claims(&mut self) -> Result<Option<Vec<RemoteClaim>>> {
        self.list_remote_claims_inner(false)
    }

    fn list_remote_claims_for(&mut self, mode: RunMode) -> Result<Option<Vec<RemoteClaim>>> {
        self.list_remote_claims_inner(mode == RunMode::Automatic)
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

#[derive(Debug)]
struct ClaimSessionFailure(String);

impl ClaimSessionFailure {
    fn from_account_error(error: anyhow::Error) -> anyhow::Error {
        match error.to_string().as_str() {
            ACCOUNT_MISMATCH_MSG => Self("account_mismatch".into()).into(),
            ACCOUNT_UNKNOWN_MSG => Self("account_unreadable".into()).into(),
            _ => error,
        }
    }
}

impl std::fmt::Display for ClaimSessionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(ovrefund_core::pipeline::auth_message(&self.0).unwrap_or(ovrefund_core::pipeline::LOGIN_NEEDED_MSG))
    }
}
impl std::error::Error for ClaimSessionFailure {}

/// A captured claim write always makes an otherwise retryable result uncertain.
fn guard_not_sent(outcome: SubmitOutcome, wrote: bool) -> SubmitOutcome {
    match outcome {
        SubmitOutcome::NotSent(_) | SubmitOutcome::Deferred(_) | SubmitOutcome::SessionUnavailable { .. } if wrote => SubmitOutcome::Unknown("Tijdens het invullen ging er mogelijk een claim naar NS. Kijk in Mijn teruggaven. / A claim may have been sent to NS while the form was being filled in. Check Mijn teruggaven.".into()),
        other => other,
    }
}

impl NsClaimGateway {
    fn submit_inner(&mut self, facts: &ClaimFacts, submission_start: u64, check: &dyn Fn() -> Result<()>) -> SubmitOutcome {
        // Always the account stored NOW (it may have been changed or removed since the run started).
        self.bank = crate::secrets::bank_details();
        let Some(bank) = self.bank.clone() else {
            return SubmitOutcome::Deferred(
                "Stel eerst je rekening in bij Jouw gegevens. / Set up your bank account under Your details first.".into(),
            );
        };
        let notes = match self.fill_form(facts) {
            Ok(n) => n,
            // Bilingual reasons ("NL / EN") are shown to the user as they are.
            Err(e) => {
                return if let Some(auth) = e.downcast_ref::<ClaimSessionFailure>() {
                    SubmitOutcome::SessionUnavailable { state: auth.0.clone(), reason: auth.to_string() }
                } else {
                    SubmitOutcome::NotSent(form_error(&e))
                };
            }
        };
        if notes.iter().any(|n| n.contains("cannot be submitted")) {
            return SubmitOutcome::NotSent("naam of IBAN ontbreekt in het NS-formulier / name or IBAN missing in NS's form".into());
        }
        let b = &self.bridge;
        // Truthfulness declaration: only reached for user-authorized or opted-in, evidence-supported claims.
        if let Err(e) = click_choice(b, &el::DECLARATION) {
            let d = ovrefund_core::redact::redact(&e.to_string()).replace(" / ", "/");
            return SubmitOutcome::NotSent(format!(
                "de verklaring in het NS-formulier kon niet worden aangevinkt ({d}) / the declaration in NS's form could not be ticked ({d})"
            ));
        }
        let declaration = match el::resolve(b, &el::DECLARATION, None) {
            Ok(d) if is_checked(b, &d) => d,
            _ => {
                return SubmitOutcome::NotSent(
                    "de verklaring in het NS-formulier is niet aangevinkt / the declaration in NS's form is not ticked".into(),
                )
            }
        };
        let main_text = |b: &NsBridge| {
            b.eval("document.querySelector('main') ? document.querySelector('main').innerText.slice(0, 600) : ''")
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        };
        // The form's own text: confirmation words already on it are no proof of anything.
        let page_before = main_text(b);
        // Point of no return: the click below may reach NS.
        // Before the click: if the button isn't there (or not uniquely), nothing can have been sent.
        let Ok(button) = el::resolve(b, &el::SUBMIT, None) else {
            return SubmitOutcome::NotSent(
                "de knop Verzenden ontbreekt of is niet eenduidig / the Verzenden button is missing or ambiguous".into(),
            );
        };
        // It must be the submit button of the form that holds the declaration, and enabled.
        let own = b.eval(&format!("(window.__ovr ? window.__ovr.submitOk({}, {}) : false)", button.expr, declaration.expr));
        if own.ok().and_then(|v| v.as_bool()) != Some(true) {
            return SubmitOutcome::NotSent(
                "de knop Verzenden hoort niet bij het NS-formulier of is uitgeschakeld / the Verzenden button does not belong to NS's form or is disabled"
                    .into(),
            );
        }
        // Validation messages, invalid fields or required fields OV Refund does not know: the form
        // changed or wants more. Nothing is clicked.
        let mark = b.mark();
        if let Err(e) = b.click_after(
            &button.expr,
            &|| {
                let problems = form_problems(&form_check(b));
                if !problems.is_empty() {
                    log::warn!("NS form not submitted: validation failed ({} field checks)", problems.len());
                    bail!(FORM_CHANGED_MSG);
                }
                anyhow::ensure!(
                    current_claim_route(b, facts).unwrap_or(false)
                        && final_form_values(b, facts, &bank, &declaration).unwrap_or(false)
                        && b.eval(&format!("(window.__ovr ? window.__ovr.submitOk({}, {}) : false)", button.expr, declaration.expr))
                            .is_ok_and(|value| value == true),
                    FORM_CHANGED_MSG
                );
                Ok(())
            },
            &|| {
                anyhow::ensure!(self.bank == crate::secrets::bank_details(), BankChanged);
                check()?;
                if b.claim_write_since(submission_start) {
                    bail!(WriteDuringPreflight);
                }
                Ok(())
            },
        ) {
            return submit_click_failure(&e);
        }
        let write = wait_capture(b, mark, Duration::from_secs(60), |c| is_claim_submit_write(&c.method, &c.url));
        std::thread::sleep(Duration::from_secs(3));
        classify_submission(write.as_ref(), b.claim_write_count_since(submission_start), &page_before, &main_text(b))
    }
}

pub(crate) fn final_form_values(b: &NsBridge, facts: &ClaimFacts, bank: &BankDetails, declaration: &Resolved) -> Result<bool> {
    let arrival = if facts.reached_destination { &el::ARRIVAL_YES } else { &el::ARRIVAL_NO };
    let band = match facts.delay_band {
        DelayBand::Min30To59 => &el::DELAY_30,
        DelayBand::Min60Plus => &el::DELAY_60,
    };
    let fields = [arrival, band, &el::DEPARTURE, &el::CHECK_IN, &el::CHECK_OUT, &el::NAME, &el::IBAN]
        .iter()
        .map(|spec| form_el(b, spec).map(|resolved| resolved.expr))
        .collect::<Result<Vec<_>>>()?;
    let shown = b.eval(&format!(
        "((a,b,d,ci,co,n,i,decl)=>({{arrival:window.__ovr.checked(a),band:window.__ovr.checked(b),declaration:window.__ovr.checked(decl),departure:d?.value,ci:ci?.value,co:co?.value,name:n?.value,iban:i?.value}}))({},{})",
        fields.join(","), declaration.expr
    ))?;
    Ok(reviewed_values_match(&shown, facts, bank))
}

fn reviewed_values_match(shown: &Value, facts: &ClaimFacts, bank: &BankDetails) -> bool {
    shown["arrival"] == true
        && shown["band"] == true
        && shown["declaration"] == true
        && timestamp_matches(&shown["ci"], facts.check_in)
        && timestamp_matches(&shown["co"], facts.check_out)
        && shown["departure"].as_str() == Some(facts.scheduled_departure_local.as_str())
        && iban_matches(shown["iban"].as_str().unwrap_or(""), &bank.iban)
        && holder_matches(shown["name"].as_str().unwrap_or(""), &bank.name)
}

/// NS's form value for a check-in or check-out: a bare timestamp, or (as seen live) a longer value
/// that contains the trip's UTC time "YYYY-MM-DDTHH:MM:SS". Either must name exactly this second.
fn timestamp_matches(value: &Value, expected: DateTime<Utc>) -> bool {
    value.as_str().is_some_and(|raw| {
        let raw = raw.trim();
        DateTime::parse_from_rfc3339(raw)
            .map(|t| t.timestamp())
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f").map(|t| t.and_utc().timestamp()))
            .map(|seconds| seconds == expected.timestamp())
            .unwrap_or_else(|_| embedded_utc_time(raw, &expected.format("%Y-%m-%dT%H:%M:%S").to_string()))
    })
}

/// `raw` contains `utc` as a UTC time: optional fraction, then "Z" or no zone at all; a
/// following offset ("+0100", "-05:00") means another moment and does not count.
fn embedded_utc_time(raw: &str, utc: &str) -> bool {
    raw.match_indices(utc).any(|(i, m)| {
        if raw[..i].ends_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
        let rest = &raw[i + m.len()..];
        let rest = if let Some(fraction) = rest.strip_prefix('.') {
            let suffix = fraction.trim_start_matches(|c: char| c.is_ascii_digit());
            if suffix.len() == fraction.len() {
                return false;
            }
            suffix
        } else {
            rest
        };
        let suffix = rest.strip_prefix(['Z', 'z']).unwrap_or(rest);
        !suffix.starts_with(|c: char| c.is_ascii_alphanumeric())
            && !suffix.strip_prefix(['+', '-']).is_some_and(|s| s.starts_with(|c: char| c.is_ascii_digit()))
    })
}

#[derive(Debug)]
struct BankChanged;

impl std::fmt::Display for BankChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Je opgeslagen rekening is gewijzigd. Controleer je rekening bij Jouw gegevens. / Your saved bank account changed. Check your bank account under Your details.")
    }
}
impl std::error::Error for BankChanged {}

fn submit_click_failure(e: &anyhow::Error) -> SubmitOutcome {
    if let Some(refused) = e.downcast_ref::<crate::ns_bridge::ClickRefused>() {
        if refused.0.is::<BankChanged>() {
            return SubmitOutcome::Deferred(refused.to_string());
        }
        return SubmitOutcome::NotSent(refused.to_string());
    }
    if e.is::<crate::ns_bridge::ClickNotDispatched>() {
        return SubmitOutcome::NotSent(e.to_string());
    }
    SubmitOutcome::Unknown(
        "Verzenden bij NS is onderbroken. Kijk in Mijn teruggaven. / Sending to NS was interrupted. Check Mijn teruggaven.".into(),
    )
}

fn classify_submission(write: Option<&Captured>, observed_writes: usize, page_before: &str, page_after: &str) -> SubmitOutcome {
    if observed_writes > 1 {
        return SubmitOutcome::Unknown(
            "Er gingen meerdere verzoeken naar NS. Kijk in Mijn teruggaven. / Multiple requests went to NS. Check Mijn teruggaven.".into(),
        );
    }
    match write {
        Some(c) if (200..300).contains(&c.status) => {
            // A 2xx alone is not proof (the write endpoint was never observed live): require a
            // claim reference or NS's confirmation text, otherwise let the claim list settle it.
            let (reference, proven) = submit_proof(c.body.as_deref(), page_before, page_after);
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

impl NsClaimGateway {
    /// NS's claim list. `strict` (automatic mode): only a captured claims API response counts;
    /// NS's "no refunds" page text alone leaves the list unknown (`None`), so no unattended
    /// submission relies on page text for duplicate protection.
    fn list_remote_claims_inner(&mut self, strict: bool) -> Result<Option<Vec<RemoteClaim>>> {
        let b = &self.bridge;
        let mark = b.mark();
        // The guard limits metadata-only diagnostics to this operation, including error exits.
        let diagnostics = crate::ns_bridge::ClaimsDiagnostics::start();
        let on_mijnns = b.eval("location.origin === 'https://www.ns.nl' && /^\\/mijnns\\/?$/.test(location.pathname)")?;
        if on_mijnns == true {
            if b.eval("location.hash === '#/claims'")? == true {
                b.reload()?;
            } else {
                b.eval("location.hash = '#/claims'; true")?;
            }
        } else {
            b.navigate("https://www.ns.nl/mijnns#/claims")?;
        }
        let started = Instant::now();
        let mut reapplied = false;
        let mut empty_since = None;
        let mut empty_page = false;
        // How long the empty-list text must stay before it is accepted; in strict mode the API
        // response gets more time (the text alone is never enough there).
        let settle = Duration::from_secs(if strict { 10 } else { 2 });
        let found = loop {
            if let Some(c) = b.captured_since(mark).into_iter().find(is_claims_list_response) {
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
                if since.elapsed() >= settle {
                    if let Some(c) = b.captured_since(mark).into_iter().find(is_claims_list_response) {
                        break Some(c);
                    }
                    empty_page = true;
                    break None;
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
        if found.is_none() && !empty_page {
            let captures = b.captured_since(mark);
            if captures.iter().any(|c| customer_api_unavailable(&c.url, c.status)) {
                log::warn!("NS customer service API unavailable (5xx); Mijn teruggaven may not load");
            }
            log::warn!("claims list: no response; {} API response(s) observed", captures.len());
        }
        decide_claims_list(found.as_ref().map(|c| (c.status, c.body.as_deref())), empty_page, strict)
    }
}

fn is_claims_list_response(response: &crate::ns_bridge::Captured) -> bool {
    response.method == "GET"
        && tauri::Url::parse(&response.url).is_ok_and(|url| {
            url.origin().ascii_serialization() == "https://gateway.apiportal.ns.nl" && url.path() == "/mijnns-claim-api/gtbv/claims"
        })
}

/// The claim list from what the claims page produced (pure, so the decision is tested).
/// `api`: the captured claims API response (HTTP status, body). `empty_page`: NS's "no refunds"
/// text was shown on the list route without such a response. `strict` (automatic mode): only a
/// captured 2xx response whose list is recognised counts, so an empty list needs a 2xx response
/// with an empty array; page text alone is "list unknown" (`None`). Not strict (the app's own
/// check, which never submits on its own): the page text counts as an empty list, so the app can
/// show that NS lists no claims.
fn decide_claims_list(api: Option<(i64, Option<&str>)>, empty_page: bool, strict: bool) -> Result<Option<Vec<RemoteClaim>>> {
    let Some((status, body)) = api else {
        if empty_page && !strict {
            log::info!("claims list: empty (page text)");
            return Ok(Some(vec![]));
        }
        if empty_page {
            log::info!("claims list: empty page text without a claims response; list unknown for automatic submission");
        }
        return Ok(None);
    };
    if !(200..300).contains(&status) {
        bail!("Mijn teruggaven gaf een fout (HTTP {0}). / Mijn teruggaven returned an error (HTTP {0}).", status);
    }
    // Unknown or unparseable shape = "list unavailable" (never "NS has no claims").
    let Some(v) = body.and_then(|b| serde_json::from_str::<Value>(b).ok()) else {
        log::warn!("claims list body missing or not JSON");
        return Ok(None);
    };
    let parsed = parse_remote_claims(&v);
    if parsed.is_none() {
        log::warn!("claims list shape not recognised; duplicate check unavailable; shape: {}", json_shape(&v, 0));
    }
    Ok(parsed)
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
    let items = match v {
        Value::Array(a) => a,
        Value::Object(o) => {
            let list_keys = ["claims", "content", "items", "data", "results", "gtbvClaims"];
            let mut lists = list_keys.iter().filter_map(|key| o.get(*key)?.as_array());
            let items = lists.next()?;
            if lists.any(|other| items != other)
                || o.iter().any(|(key, value)| {
                    !(list_keys.contains(&key.as_str()) && value.is_array()) && ovrefund_core::pipeline::response_error_field(key, value)
                })
            {
                return None;
            }
            if more_pages(o, items.len() as i64) {
                return None;
            }
            items
        }
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
            let state = remote_status(&raw);
            let date = ["travelDate", "journeyDate", "date", "departureDate", "checkInDate"]
                .iter()
                .find_map(|k| it.get(*k).and_then(Value::as_str))
                .and_then(|d| chrono::NaiveDate::parse_from_str(d.get(..10).unwrap_or(d), "%Y-%m-%d").ok());
            let tx = ["transactionId", "journeyId", "tripId"]
                .iter()
                .find_map(|k| it.get(*k).and_then(Value::as_str).map(str::trim).filter(|id| !id.is_empty()))
                .map(str::to_string);
            let amount_cents = parse_amount_cents(it);
            RemoteClaim {
                reference: find_reference(it),
                travel_date: date,
                origin_uic: None,
                destination_uic: None,
                transaction_id: tx,
                state: Some(state),
                raw_status: state.as_str().into(),
                amount_cents,
            }
        })
        .collect();
    out.iter().all(|r| r.travel_date.is_some() || r.transaction_id.is_some()).then_some(out)
}

/// Does a list response say there are more pages than this one? (Spring `totalPages`/`last`,
/// `hasNext`-style flags, a `next` link or cursor, or a total larger than the page.)
fn more_pages(o: &serde_json::Map<String, Value>, entries: i64) -> bool {
    let flag = |m: &serde_json::Map<String, Value>| -> bool {
        let total_pages = ["totalPages", "pageCount", "numberOfPages"].iter().find_map(|k| m.get(*k).and_then(Value::as_i64));
        let total = ["totalElements", "totalCount", "total", "totalItems"]
            .iter()
            .filter_map(|k| m.get(*k).and_then(Value::as_i64))
            .any(|total| total > entries);
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
            || total
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
    covers_days_on(url, days, Utc::now().with_timezone(&chrono_tz::Europe::Amsterdam).date_naive())
}

/// A date query parameter of a travel-history request. NS has used both "2025-05-01T00:00:00" and
/// "20250501": read on the digits only.
fn query_date(url: &str, key: &str) -> Option<chrono::NaiveDate> {
    let q = url.split('?').nth(1)?;
    let v = q.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))?;
    let digits: String = v.replace("%3A", ":").replace("%2D", "-").chars().filter(char::is_ascii_digit).take(8).collect();
    chrono::NaiveDate::parse_from_str(&digits, "%Y%m%d").ok()
}

/// A full (year / 1.5-year) travel-history response: `from=` at least `days` before `today`
/// (Amsterdam), `until=` present and not before yesterday, and no paging-style parameter (a page
/// or a capped list is never the whole history).
fn covers_days_on(url: &str, days: i64, today: chrono::NaiveDate) -> bool {
    let Some(q) = url.split('?').nth(1) else { return false };
    let paged = q.split('&').any(|kv| {
        let k = kv.split('=').next().unwrap_or("").to_ascii_lowercase();
        matches!(k.as_str(), "limit" | "size" | "pagesize" | "max" | "offset" | "page")
    });
    let from_ok = query_date(url, "from").is_some_and(|d| (today - d).num_days() >= days);
    let until_ok = query_date(url, "until").is_some_and(|d| (today - d).num_days() <= 1);
    !paged && from_ok && until_ok
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
    fn form_times_match_bare_and_embedded_values_to_the_second() {
        let t: DateTime<Utc> = "2026-10-17T12:53:00Z".parse().unwrap();
        for ok in [
            "2026-10-17T12:53:00Z",
            "2026-10-17T14:53:00+02:00",
            "2026-10-17T12:53:00",
            "  2026-10-17T12:53:00.000Z ",
            "trip-123|2026-10-17T12:53:00Z|8400319",
            "{\"checkIn\":\"2026-10-17T12:53:00.000Z\"}",
            "trip:2026-10-17T12:53:00-destination",
        ] {
            assert!(timestamp_matches(&json!(ok), t), "{ok}");
        }
        for bad in [
            "2026-10-17T12:53:01Z",
            "2026-10-17T13:53:00+02:00",
            "trip-123|2026-10-17T12:54:00Z",
            "2026-10-17T12:53:00+0100",
            "x|2026-10-17T12:53:00.000-05:00",
            "12026-10-17T12:53:00Z",
            "x|2026-10-17T12:53:000Z",
            "x|2026-10-17T12:53:00.Z",
            "x|2026-10-17T12:53:00Z01",
            "x|2026-10-17T12:53:00z+01:00",
            "",
            "12:53",
        ] {
            assert!(!timestamp_matches(&json!(bad), t), "{bad}");
        }
        assert!(!timestamp_matches(&json!(null), t));
    }

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

    #[test]
    fn form_check_knows_fields_by_element_not_by_id() {
        let js = form_check_js(&["(window.__ovr ? window.__ovr.pick({}, 1, document) : null)".into()]);
        assert!(js.contains("const known = [(window.__ovr ? window.__ovr.pick({}, 1, document) : null)].filter(Boolean);"));
        assert!(js.contains("!known.includes(el)") && !js.contains("__KNOWN__"));
        // nothing resolved: every visible required field is unknown
        assert!(form_check_js(&[]).contains("const known = [].filter(Boolean);"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn travel_proof_mask_blocks_are_padded_clipped_and_merged() {
        use super::{mask_blocks, Rect};
        let a = Rect { x: 100.0, y: 60.0, w: 200.0, h: 20.0 };
        let b = Rect { x: 290.0, y: 62.0, w: 100.0, h: 20.0 };
        let far = Rect { x: 100.0, y: 900.0, w: 800.0, h: 80.0 };
        let edge = Rect { x: 1400.0, y: 0.0, w: 200.0, h: 10.0 };
        let empty = Rect { x: 5.0, y: 5.0, w: 0.0, h: 10.0 };
        let blocks = mask_blocks(1440.0, 1000.0, &[a, b, far, edge, empty]);
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        // a and b overlap: one block around both, 2 px margin
        assert!(blocks.contains(&Rect { x: 98.0, y: 58.0, w: 294.0, h: 26.0 }), "{blocks:?}");
        assert!(blocks.contains(&Rect { x: 98.0, y: 898.0, w: 804.0, h: 84.0 }), "{blocks:?}");
        // clipped to the page
        assert!(blocks.contains(&Rect { x: 1398.0, y: 0.0, w: 42.0, h: 12.0 }), "{blocks:?}");
        assert!(mask_blocks(1440.0, 1000.0, &[]).is_empty());
    }

    #[test]
    fn ns_claim_form_route_checks_exact_trip_card_and_instant() {
        let f = ClaimFacts {
            source_record_id: "FAKE-1".into(),
            coupling_id: "test-card".into(),
            check_in: "2026-10-25T00:30:00Z".parse().unwrap(),
            check_out: "2026-10-25T01:30:00Z".parse().unwrap(),
            origin_name: "Test A".into(),
            destination_name: "Test B".into(),
            scheduled_departure_local: "02:35".into(),
            delay_band: DelayBand::Min30To59,
            reached_destination: true,
        };
        let route = |card, id, time| serde_json::json!(["#", "reishistorie", "geld-terug-bij-vertraging", card, id, time]);
        assert!(claim_route_matches(&route("test-card", "FAKE-1", "2026-10-25T02:30:00.123+02:00"), &f));
        assert!(!claim_route_matches(&route("test-card", "FAKE-10", "2026-10-25T00:30:00Z"), &f));
        assert!(!claim_route_matches(&route("other-card", "FAKE-1", "2026-10-25T00:30:00Z"), &f));
        assert!(!claim_route_matches(&route("test-card", "FAKE-1", "2026-10-25T02:30:00+01:00"), &f));
    }

    #[test]
    fn travel_proof_mask_allows_only_generic_labels() {
        for l in super::MASK_ALLOW {
            assert_eq!(*l, l.to_lowercase(), "allow-list entries are lower case");
            assert!(!l.contains('@'), "{l}");
            assert!(!l.chars().any(|c| c.is_ascii_digit()) || *l == "afgelopen 1,5 jaar" || *l == "laatste 7 dagen", "{l}");
        }
        let js = super::MASK_MEASURE.replace("%ALLOW%", &serde_json::to_string(super::MASK_ALLOW).unwrap());
        assert!(js.contains("\"ingelogd met:\"") && !js.contains("%ALLOW%"));
    }

    #[test]
    fn travel_proof_mask_is_checked_again_before_the_screenshot() {
        use super::{mask_measurement, mask_still_covers, Mask, Rect};
        let keep = Rect { x: 20.0, y: 250.0, w: 800.0, h: 100.0 };
        let email = Rect { x: 300.0, y: 10.0, w: 160.0, h: 20.0 };
        let mask = Mask { keep, blocks: super::mask_blocks(1440.0, 2000.0, &[email]) };
        // Nothing changed, or a text moved by less than the margin: still covered.
        assert!(mask_still_covers(&mask, 1440.0, 2000.0, keep, &[email]));
        assert!(mask_still_covers(&mask, 1440.0, 2000.0, keep, &[Rect { x: 301.5, ..email }]));
        // Something new appeared, a covered text grew, or the trip moved: not covered any more.
        assert!(!mask_still_covers(&mask, 1440.0, 2000.0, keep, &[email, Rect { x: 900.0, y: 900.0, w: 50.0, h: 20.0 }]));
        assert!(!mask_still_covers(&mask, 1440.0, 2000.0, keep, &[Rect { w: 200.0, ..email }]));
        assert!(!mask_still_covers(&mask, 1440.0, 2000.0, Rect { y: 253.0, ..keep }, &[email]));
        assert!(mask_still_covers(&mask, 1440.0, 2000.0, Rect { y: 251.5, ..keep }, &[email]));
        // Areas past the page edge count as clipped, as the drawn blocks are.
        let wide = Rect { x: 1400.0, y: 500.0, w: 200.0, h: 20.0 };
        let m2 = Mask { keep, blocks: super::mask_blocks(1440.0, 2000.0, &[wide]) };
        assert!(mask_still_covers(&m2, 1440.0, 2000.0, keep, &[wide]));
        // A measurement that asks to fail closed never gives a mask.
        let m = |fail: Value, shared: bool| serde_json::json!({"w": 1440, "h": 2000, "keep": [{"x": 20, "y": 250, "w": 800, "h": 100}], "hide": [], "heading": true, "shared": shared, "fail": fail});
        assert!(mask_measurement(&m(Value::Null, false)).is_ok());
        let mut malformed = m(Value::Null, false);
        malformed["hide"] = serde_json::json!([{"x": 10, "w": 40, "h": 20}]);
        assert!(mask_measurement(&malformed).is_err());
        for bad in [m(Value::from("iframe"), false), m(Value::from("top-layer"), false), m(Value::Null, true), m(Value::Bool(true), false)]
        {
            assert_eq!(mask_measurement(&bad).err().unwrap().to_string(), TRIP_SHOT_FAILED_MSG);
        }
    }

    /// Runs the real mask script against a fake Mijn NS page (with shadow roots, text ranges and
    /// computed styles) in Node.js when it is installed (CI has it); skipped otherwise.
    #[test]
    fn travel_proof_mask_script_in_a_fake_dom() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let script = super::MASK_MEASURE.replace("%ALLOW%", &serde_json::to_string(super::MASK_ALLOW).unwrap());
        let program = format!(
            "{}\nconst MEASURE = {};\nconst VERIFY = {};\nrunScenarios();",
            include_str!("ns_mask_fake_dom.js"),
            serde_json::to_string(&script).unwrap(),
            serde_json::to_string(MASK_VERIFY).unwrap()
        );
        let child = Command::new("node").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let Ok(mut child) = child else {
            eprintln!("node not found: fake-DOM mask test skipped");
            return;
        };
        child.stdin.take().unwrap().write_all(program.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "node failed: {}\n{stdout}", String::from_utf8_lossy(&out.stderr));
        let v: Value = serde_json::from_str(stdout.trim()).expect("scenario JSON");
        let base = &v["base"];
        assert_eq!(base["fail"], Value::Null, "{base}");
        assert_eq!(base["shared"], false);
        assert_eq!(base["heading"], true);
        // Page coordinates (the page is scrolled by 50 px).
        assert_eq!(serde_json::from_value::<Rect>(base["keep"][0].clone()).unwrap(), Rect { x: 20.0, y: 250.0, w: 800.0, h: 100.0 });
        assert_eq!(base["email"], true, "the account e-mail inside the header's shadow root is covered");
        assert_eq!(base["nav_touched"], false, "generic navigation labels stay visible");
        assert_eq!(base["select"], true, "the card picker showing a name is covered");
        assert_eq!(base["next"], true, "the next trip is covered");
        assert_eq!(base["trip_touched"], false, "the trip and its day heading are never covered");
        assert_eq!(base["faded"], true, "text at opacity 0 is covered (it may fade in)");
        assert_eq!(base["hidden"], false, "display:none text is not covered");
        assert_eq!(base["generated"], true, "CSS-generated text is covered");
        assert_eq!(base["icon"], false, "an icon glyph is not text");
        assert_eq!(v["shared"]["shared"], true, "two trips in one box: no screenshot");
        assert_eq!(v["iframe"]["fail"], "iframe");
        assert_eq!(v["tiny_frame"]["fail"], "iframe");
        assert_eq!(v["overlap"]["fail"], "overlapping-text");
        assert_eq!(v["drawn"], true);
        assert_eq!(v["missing_overlay"], false);
        assert_eq!(v["transparent_overlay"], false);
        assert_eq!(v["dialog"]["fail"], "top-layer");
        assert_eq!(v["popover"]["fail"], "top-layer");
        assert_eq!(v["closed_shadow"]["fail"], Value::Null);
        assert_eq!(v["closed_shadow"]["closed"], true, "a closed-shadow component is covered");
        assert_eq!(v["closed_shadow"]["trip_touched"], false);
        assert_eq!(v["pseudo_over_trip"]["fail"], "pseudo", "generated text on a box around the trip: no screenshot");
    }
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
    fn empty_claims_page_text_alone_never_proves_an_empty_list_for_automatic_submission() {
        // Page text without a claims response: an empty list for the app, unknown when automatic.
        assert_eq!(decide_claims_list(None, true, false).unwrap(), Some(vec![]));
        assert_eq!(decide_claims_list(None, true, true).unwrap(), None);
        // No response and no empty text: unknown in both modes.
        assert_eq!(decide_claims_list(None, false, false).unwrap(), None);
        assert_eq!(decide_claims_list(None, false, true).unwrap(), None);
        // A captured 2xx response with an empty array is an empty list in both modes.
        for strict in [false, true] {
            assert_eq!(decide_claims_list(Some((200, Some("[]"))), false, strict).unwrap(), Some(vec![]));
            assert_eq!(decide_claims_list(Some((200, Some(r#"{"claims": []}"#))), true, strict).unwrap(), Some(vec![]));
            // Missing, non-JSON or unrecognised bodies stay unknown, also with the empty text.
            assert_eq!(decide_claims_list(Some((200, None)), true, strict).unwrap(), None);
            assert_eq!(decide_claims_list(Some((200, Some("<html>"))), true, strict).unwrap(), None);
            assert_eq!(decide_claims_list(Some((200, Some(r#"{"other": 1}"#))), true, strict).unwrap(), None);
            // An error status is an error, never an empty list.
            assert!(decide_claims_list(Some((500, Some("[]"))), true, strict).is_err());
            assert!(decide_claims_list(Some((404, Some("[]"))), false, strict).is_err());
        }
        let listed = decide_claims_list(Some((200, Some(r#"[{"status": "INGEDIEND", "travelDate": "2026-03-01"}]"#))), false, true)
            .unwrap()
            .unwrap();
        assert_eq!(listed.len(), 1);
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
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let day = |n: i64, f: &str| (today - chrono::Duration::days(n)).format(f).to_string();
        let ok = |q: String, days| super::covers_days_on(&format!("https://x/transactions?{q}"), days, today);
        let (old, recent, until) = (day(365, "%Y-%m-%d"), day(7, "%Y-%m-%d"), day(0, "%Y-%m-%d"));
        assert!(ok(format!("from={old}T00:00:00&until={until}T23:59:59"), 300));
        assert!(!ok(format!("from={recent}&until={until}"), 300));
        // Compact date format NS switched to (observed November 2026); until= yesterday is fine.
        let (compact, yesterday) = (day(500, "%Y%m%d"), day(1, "%Y%m%d"));
        assert!(ok(format!("from={compact}&until={yesterday}"), 450));
        // No until=, an old until= or any paging parameter: never the whole history.
        assert!(!ok(format!("from={compact}"), 450));
        assert!(!ok(format!("from={compact}&until={}", day(2, "%Y%m%d")), 450));
        for p in ["limit=50", "size=100", "pageSize=100", "max=500", "offset=0", "page=1"] {
            assert!(!ok(format!("from={compact}&until={yesterday}&{p}"), 450), "{p}");
        }
        assert!(!ok(format!("from={}&until={yesterday}", day(7, "%Y%m%d")), 300));
    }

    #[test]
    fn day_heading_must_match_exactly() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let program = format!(
            "const window = {{}};\n{}\nconsole.log(JSON.stringify([['1 november 2026', true], ['Zondag, 1 november 2026', true], ['11 november 2026', false], ['21 november 2026', false], ['1 november 2025', false], ['1 november 20261', false]].map(([h, want]) => window.__ovDay(h, '1 november 2026') === want)));",
            super::DAY_MATCH_JS
        );
        let Ok(mut child) = Command::new("node").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() else {
            eprintln!("node not found: day heading test skipped");
            return;
        };
        child.stdin.take().unwrap().write_all(program.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[true,true,true,true,true,true]");
    }

    #[test]
    fn full_history_never_accepts_the_default_week_response() {
        let capture = |days, coupling: &str| {
            Captured {
            url: format!(
                "https://gateway.apiportal.ns.nl/mijnns-transaction-api/couplings/{coupling}/transactions?from={}T00:00:00&until={}T23:59:59",
                (Utc::now().date_naive() - chrono::Duration::days(days)).format("%Y-%m-%d"),
                Utc::now().with_timezone(&chrono_tz::Europe::Amsterdam).date_naive().format("%Y-%m-%d")
            ),
            method: "GET".into(),
            status: 200,
            body: Some("[]".into()),
            seq: 1,
        }
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
        assert!(!history_capture_matches(&full, FetchRange::Max, Some("card")));
        let mut changed_host = full.clone();
        changed_host.url = changed_host.url.replace("gateway.apiportal.ns.nl", "example.invalid");
        assert!(!history_capture_matches(&changed_host, FetchRange::Max, Some("card-a")));
        // The "latest transactions" list with an early from= is never the full history.
        let mut capped = capture(538, "card-a");
        capped.url.push_str("&limit=15");
        assert!(!history_capture_matches(&capped, FetchRange::Max, Some("card-a")));
        assert!(!history_capture_matches(&capped, FetchRange::Week, Some("card-a")));
        // A full range without until= is not accepted.
        let mut open = capture(568, "card-a");
        open.url = open.url.split("&until=").next().unwrap().to_string();
        assert!(!history_capture_matches(&open, FetchRange::Max, Some("card-a")));
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
        if let Some(c) = it.get(k).and_then(Value::as_i64).filter(|c| *c >= 0) {
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
            if let Some((eu, ct)) = t.split_once('.') {
                if !eu.is_empty() && ct.len() == 2 && eu.bytes().chain(ct.bytes()).all(|b| b.is_ascii_digit()) {
                    if let (Ok(e), Ok(c)) = (eu.parse::<i64>(), ct.parse::<i64>()) {
                        return e.checked_mul(100)?.checked_add(c);
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
        assert_eq!(parse_amount_cents(&json!({"amountInCents": -1})), None);
        assert_eq!(parse_amount_cents(&json!({"amount": "1.-1"})), None);
        assert_eq!(parse_amount_cents(&json!({"amount": "92233720368547758.99"})), None);
    }
}

#[cfg(test)]
mod claim_list_tests {
    use super::parse_remote_claims;
    use ovrefund_core::model::ClaimState;
    use serde_json::json;

    #[test]
    fn negative_statuses_never_become_paid_or_approved() {
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
        assert!(parse_remote_claims(&json!({"claims": [], "success": false, "errors": ["fixture"]})).is_none());
        assert!(parse_remote_claims(&json!({"status": "failed", "claims": []})).is_none());
        assert!(parse_remote_claims(&json!({"claims": [], "meta": {"success": false}})).is_none());
        assert!(parse_remote_claims(&json!({"claims": [], "meta": [{"success": false}]})).is_none());
        assert!(parse_remote_claims(&json!({"claims": [], "detail": "Failed to load claims"})).is_none());
        assert_eq!(parse_remote_claims(&json!({"claims": [], "requestId": "fixture-error-id"})).map(|v| v.len()), Some(0));
        assert!(parse_remote_claims(&json!({"claims": [], "content": [{"transactionId": "fixture-trip", "status": "Paid"}]})).is_none());
        assert_eq!(parse_remote_claims(&json!({"claims": [], "items": []})).map(|v| v.len()), Some(0));
        let rejected = json!([{"transactionId": "fixture-trip", "status": "rejected"}]);
        assert_eq!(parse_remote_claims(&json!({"claims": rejected, "items": rejected})).unwrap()[0].state, Some(ClaimState::Rejected));
        assert_eq!(parse_remote_claims(&json!({"claims": [], "meta": {"success": true}})).map(|v| v.len()), Some(0));
        let list = parse_remote_claims(&json!({"status": "ok", "claims": [
            {"transactionId": "test-trip", "status": "rejected", "requestId": "test-request"}
        ]}))
        .unwrap();
        assert_eq!(list[0].state, Some(ovrefund_core::model::ClaimState::Rejected));
        assert_eq!(list[0].reference, None);
        assert!(parse_remote_claims(&json!([{"status": "In behandeling"}])).is_none(), "entry without date or id");
        assert!(parse_remote_claims(&json!([{"transactionId": " "}])).is_none(), "blank id is not trip evidence");
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
            json!({"content": [], "totalElements": 1}),
            json!({"content": [one.clone()], "page": {"size": 20, "totalElements": 2}}),
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
        // A confirmation page can prove receipt without proving the application number.
        assert_eq!(submit_proof(Some(r#"{"id": "A9"}"#), "Verzenden", "Bedankt, we hebben je aanvraag ontvangen"), (None, true));
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

/// Exact focused field (given as an element expression), including fields nested in NS shadow roots.
fn bank_focus_script(field_expr: &str) -> String {
    format!("(() => {{ if (window.top !== window || location.protocol !== 'https:' || location.hostname !== 'www.ns.nl') return false; let active = document.activeElement; while (active && active.shadowRoot && active.shadowRoot.activeElement) active = active.shadowRoot.activeElement; const field = {field_expr}; return !!field && active === field; }})()")
}

#[cfg(test)]
mod submission_guard_tests {
    use super::*;
    #[test]
    fn an_unreadable_account_is_a_session_failure_before_form_preparation() {
        let error = account_binding(Some("fixture@example.invalid"), None).unwrap_err();
        let failure = ClaimSessionFailure::from_account_error(error);
        assert_eq!(failure.downcast_ref::<ClaimSessionFailure>().unwrap().0, "account_unreadable");
        assert_eq!(failure.to_string(), ACCOUNT_UNKNOWN_MSG);
    }

    #[test]
    fn only_proven_pre_dispatch_refusal_can_be_retried_after_click_call() {
        let blocked = anyhow::Error::new(crate::ns_bridge::ClickNotDispatched);
        assert!(matches!(guard_not_sent(submit_click_failure(&blocked), false), SubmitOutcome::NotSent(_)));
        assert!(matches!(guard_not_sent(submit_click_failure(&blocked), true), SubmitOutcome::Unknown(_)));
        let before_press = anyhow::Error::msg("fixture transport timeout").context(crate::ns_bridge::ClickNotDispatched);
        assert!(matches!(submit_click_failure(&before_press), SubmitOutcome::NotSent(_)));
        let bank_changed = anyhow::Error::new(crate::ns_bridge::ClickRefused(BankChanged.into()));
        assert!(matches!(guard_not_sent(submit_click_failure(&bank_changed), false), SubmitOutcome::Deferred(_)));
        assert!(matches!(guard_not_sent(submit_click_failure(&bank_changed), true), SubmitOutcome::Unknown(_)));
        let stopped = anyhow::Error::new(crate::ns_bridge::ClickRefused(anyhow::Error::msg("stopped")));
        assert!(matches!(guard_not_sent(submit_click_failure(&stopped), false), SubmitOutcome::NotSent(_)));
        assert!(matches!(guard_not_sent(submit_click_failure(&stopped), true), SubmitOutcome::Unknown(_)));
        for error in ["mousePressed timed out", "mouseReleased timed out", "window closed"] {
            assert!(matches!(submit_click_failure(&anyhow::Error::msg(error)), SubmitOutcome::Unknown(_)));
        }
    }

    #[test]
    fn a_form_rerender_cannot_swap_the_reviewed_values() {
        let facts = ClaimFacts {
            source_record_id: "test-trip".into(),
            coupling_id: "test-card".into(),
            check_in: "2026-10-25T00:30:00Z".parse().unwrap(),
            check_out: "2026-10-25T02:29:00Z".parse().unwrap(),
            origin_name: "Test origin".into(),
            destination_name: "Test destination".into(),
            scheduled_departure_local: "02:45".into(),
            delay_band: DelayBand::Min30To59,
            reached_destination: true,
        };
        let bank = BankDetails { name: "Test Fictíef".into(), iban: "NL00TEST0000000000".into() };
        let reviewed = serde_json::json!({
            "arrival":true, "band":true, "declaration":true, "departure":"02:45",
            "ci":"2026-10-25T02:30:00+02:00", "co":"2026-10-25T02:29:00.000",
            "name":" TEST  FICTÍEF ", "iban":"nl00 test 0000 0000 00"
        });
        assert!(reviewed_values_match(&reviewed, &facts, &bank));
        for (field, value) in [
            ("arrival", Value::Bool(false)),
            ("band", Value::Bool(false)),
            ("declaration", Value::Bool(false)),
            ("departure", Value::from("02:47")),
            ("ci", Value::from("2026-10-25T02:30:00+01:00")),
            ("co", Value::from("2026-10-25T02:28:00Z")),
            ("co", Value::from("2026-10-25T02:29:00+0100")),
            ("name", Value::from("Test Fictief")),
            ("iban", Value::from("NL00TEST0001000000")),
        ] {
            let mut rerendered = reviewed.clone();
            rerendered[field] = value;
            assert!(!reviewed_values_match(&rerendered, &facts, &bank), "{field}");
        }
    }

    #[test]
    fn click_response_and_page_evidence_must_agree() {
        let response = |status, body: &str| Captured {
            url: "https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/claims".into(),
            method: "POST".into(),
            status,
            body: Some(body.into()),
            seq: 1,
        };
        let proof = response(201, r#"{"claimReference":"fixture-error-42"}"#);
        let contradictory = response(201, r#"{"claimReference":"fixture-ref","detail":"The request was not submitted"}"#);
        assert!(matches!(classify_submission(Some(&contradictory), 1, "", ""), SubmitOutcome::Unknown(_)));
        assert!(matches!(classify_submission(Some(&proof), 1, "", ""), SubmitOutcome::Confirmed(_)));
        assert!(matches!(classify_submission(Some(&proof), 2, "", ""), SubmitOutcome::Unknown(_)));
        assert!(matches!(classify_submission(Some(&proof), 1, "", "Verzenden mislukt"), SubmitOutcome::Unknown(_)));
        for status in [400, 422, 500] {
            assert!(matches!(classify_submission(Some(&response(status, "{}")), 1, "", ""), SubmitOutcome::Unknown(_)));
        }
        for body in ["{}", r#"{"id":"fixture-ref"}"#, r#"{"claimId":"fixture-ref","success":false}"#, "unexpected content"] {
            assert!(matches!(classify_submission(Some(&response(200, body)), 1, "", "Bedankt voor je geduld"), SubmitOutcome::Unknown(_)));
        }
        assert!(matches!(classify_submission(None, 1, "", "Je aanvraag is ontvangen"), SubmitOutcome::Unknown(_)));
        assert!(matches!(classify_submission(None, 0, "", ""), SubmitOutcome::Unknown(_)));
    }

    #[test]
    fn observed_write_prevents_every_retryable_early_exit() {
        assert!(matches!(guard_not_sent(SubmitOutcome::NotSent("fill failed".into()), false), SubmitOutcome::NotSent(_)));
        assert!(matches!(guard_not_sent(SubmitOutcome::NotSent("fill failed".into()), true), SubmitOutcome::Unknown(_)));
        assert!(matches!(guard_not_sent(SubmitOutcome::Unknown("timeout".into()), false), SubmitOutcome::Unknown(_)));
        assert!(!is_claim_submit_write("POST", "https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/validate"));
        assert!(is_claim_submit_write("POST", "https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/claims"));
    }
}

#[cfg(test)]
mod history_period_tests {
    use super::preset_for;
    use chrono::NaiveDate;
    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    #[test]
    fn claim_list_accepts_only_its_get_response() {
        let mut response = crate::ns_bridge::Captured {
            method: "GET".into(),
            url: "https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/claims?page=0".into(),
            status: 200,
            body: Some("[]".into()),
            seq: 1,
        };
        assert!(super::is_claims_list_response(&response));
        response.method = "POST".into();
        assert!(!super::is_claims_list_response(&response));
        response.method = "GET".into();
        response.url = "https://gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/claims/other".into();
        assert!(!super::is_claims_list_response(&response));
    }

    #[test]
    fn history_route_selects_the_card_without_changing_the_route_or_query() {
        use super::history_hash;
        assert_eq!(history_hash(None), "#/reishistorie");
        assert_eq!(history_hash(Some("fake-card-2")), "#/reishistorie?card=fake-card-2");
        assert_eq!(history_hash(Some("a&b#c")), "#/reishistorie?card=a%26b%23c");
    }

    #[test]
    fn shortest_preset_that_contains_the_trip() {
        let today = d("2026-11-20");
        assert_eq!(preset_for(d("2026-11-15"), today), "Laatste 7 dagen");
        assert_eq!(preset_for(d("2026-11-02"), today), "Deze maand");
        assert_eq!(preset_for(d("2026-10-01"), today), "Vorige maand");
        assert_eq!(preset_for(d("2026-08-31"), today), "Afgelopen jaar");
        assert_eq!(preset_for(d("2025-10-01"), today), "Afgelopen 1,5 jaar");
        // Early in a month the last week reaches into the previous month.
        assert_eq!(preset_for(d("2026-12-30"), d("2027-01-03")), "Laatste 7 dagen");
        assert_eq!(preset_for(d("2026-12-20"), d("2027-01-03")), "Vorige maand");
    }
}
