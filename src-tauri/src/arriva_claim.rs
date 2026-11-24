//! Arriva claims for real trips. Manual only: a claim is made when the user presses Claim for that
//! trip in the app, never by a scheduled or automatic check.
//!
//! The flow: re-check the trip against Arriva's rules and the ledger (no claim twice for the same
//! journey), make the two attachments (the Mijn NS travel-history page with the trip and a caption
//! strip with the full card number; the Rijden de Treinen delay card of the trains), fill Arriva's
//! form in a hidden, guarded window with the traveller's own details and verify every field. Only
//! then is the claim recorded as SUBMITTING, the guard armed for exactly one final write and
//! Indienen pressed. The window shows itself only when Google asks for a CAPTCHA puzzle. The
//! outcome (HubSpot's response to the final write) makes the claim SUBMITTED; an unclear outcome
//! makes it INDETERMINATE ("check your e-mail from Arriva") and nothing is retried automatically.
//! In the demo, a dry run and developer builds the guard is never armed: Indienen is pressed and
//! the final write is blocked before the network.
use crate::arriva::{self, Bridge, FormData};
use anyhow::{anyhow, bail, Result};
use ovrefund_core::compensation::{effective_arriva_fare, ArrivaFare};
use ovrefund_core::model::{ClaimState, Confidence};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// Credential Manager entry with the contact details Arriva's form asks for (JSON).
pub const CONTACT_KEY: &str = "arriva-contact";
/// File names of the attachments (also the names Arriva receives; no personal data in them).
pub const TRAVEL_FILE: &str = "reisbewijs-mijn-ns.png";
pub const DELAY_FILE: &str = "vertragingsbewijs-rijden-de-treinen.png";
/// How long the app waits for the outcome after Indienen (a CAPTCHA puzzle can take a while).
const OUTCOME_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Contact {
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    /// Expiry date printed on the OV-chipkaart, "DD-MM-YYYY" (optional; filled when Arriva asks).
    #[serde(default)]
    pub card_valid_until: Option<String>,
    /// Credential key of the linked card the traveller saved this date for.
    #[serde(default)]
    card_expiry_key: Option<String>,
}

pub fn contact() -> Option<Contact> {
    crate::secrets::get(CONTACT_KEY).ok().flatten().and_then(|s| serde_json::from_str(&s).ok())
}

fn email_ok(e: &str) -> bool {
    let e = e.trim();
    e.len() <= 254
        && !e.contains(char::is_whitespace)
        && !e.contains(char::is_control)
        && e.split_once('@')
            .is_some_and(|(a, d)| !a.is_empty() && !d.contains('@') && d.contains('.') && !d.starts_with('.') && !d.ends_with('.'))
}

fn parse_valid_until(s: &str) -> Option<(i32, u32, u32)> {
    let d = chrono::NaiveDate::parse_from_str(s.trim(), "%d-%m-%Y").ok()?;
    use chrono::Datelike;
    Some((d.year(), d.month(), d.day()))
}

/// Checks and normalises the contact details; bilingual error naming the field.
pub fn validate_contact(c: &Contact) -> Result<Contact> {
    let first = c.first_name.trim();
    let last = c.last_name.trim();
    anyhow::ensure!(!first.is_empty() && first.len() <= 80, "Vul je voornaam in. / Enter your first name.");
    anyhow::ensure!(!last.is_empty() && last.len() <= 80, "Vul je achternaam in. / Enter your last name.");
    anyhow::ensure!(email_ok(&c.email), "Vul een geldig e-mailadres in. / Enter a valid e-mail address.");
    let until = c.card_valid_until.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(u) = until {
        anyhow::ensure!(parse_valid_until(u).is_some(), "Vul de vervaldatum in als DD-MM-JJJJ. / Enter the expiry date as DD-MM-YYYY.");
    }
    Ok(Contact {
        first_name: first.into(),
        last_name: last.into(),
        email: c.email.trim().into(),
        card_valid_until: until.map(str::to_string),
        card_expiry_key: until.and(c.card_expiry_key.clone()),
    })
}

fn journey_card_key(journey: &ovrefund_core::model::Journey) -> Option<String> {
    Some(crate::secrets::card_key(journey.coupling_id.as_deref()?, journey.card_last4.as_deref()?))
}

fn expiry_for_card(contact: &Contact, key: Option<&str>) -> Option<(i32, u32, u32)> {
    key.filter(|key| contact.card_expiry_key.as_deref() == Some(*key))?;
    contact.card_valid_until.as_deref().and_then(parse_valid_until)
}

fn expiry_card_for_settings(db: &ovrefund_core::db::Db, saved_key: Option<&str>) -> Result<Option<(String, String)>> {
    let mut stmt = db.conn.prepare(
        "SELECT DISTINCT coupling_id,card_last4 FROM journeys
         WHERE coupling_id IS NOT NULL AND coupling_id<>'' AND card_last4 IS NOT NULL",
    )?;
    let cards = stmt
        .query_map([], |r| {
            let coupling: String = r.get(0)?;
            let last4: String = r.get(1)?;
            Ok((crate::secrets::card_key(&coupling, &last4), last4))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some(key) = saved_key {
        return Ok(cards.into_iter().find(|(card, _)| card == key));
    }
    Ok(if cards.len() == 1 { cards.into_iter().next() } else { None })
}

/// What the app shows per claim while (and after) the user's Claim runs. Not persisted: the ledger
/// (claim status and history) is the record.
static STATE: once_cell::sync::Lazy<Mutex<HashMap<i64, ClaimProgress>>> = once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));
/// One Arriva claim at a time (held until its outcome is recorded).
static RUNNING: AtomicBool = AtomicBool::new(false);
struct Running;
impl Running {
    fn take() -> Option<Self> {
        (!RUNNING.swap(true, Ordering::SeqCst)).then_some(Running)
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

struct ClaimProgress {
    ui: Value,
    preview: Option<Prepared>,
}

impl ClaimProgress {
    fn new(phase: &str, message: &str) -> Self {
        Self { ui: json!({"phase": phase, "message": message, "at": chrono::Utc::now().to_rfc3339()}), preview: None }
    }
}

fn set_state(app: &AppHandle, claim_id: i64, phase: &str, message: &str) {
    STATE.lock().unwrap().insert(claim_id, ClaimProgress::new(phase, message));
    let _ = app.emit("arriva-claim-updated", claim_id);
}

/// File in a claim's attachment folder naming the trip it was made for (a short hash).
const OWNER_FILE: &str = "reis.id";

fn claims_root() -> std::path::PathBuf {
    crate::paths::data_dir().join("arriva").join("claims")
}

fn claim_dir(claim_id: i64) -> std::path::PathBuf {
    claims_root().join(claim_id.to_string())
}

/// Which trip a set of attachments belongs to (claim ids are reused after a history wipe).
fn owner_tag(j: &ovrefund_core::model::Journey) -> String {
    ovrefund_core::short_hash(&format!("{}|{}", j.source_record_id, j.check_in.to_rfc3339()))
}

/// The attachment folder of a claim, only when its files were made for that claim's trip.
fn owned_dir(db: &ovrefund_core::db::Db, claim_id: i64) -> Option<std::path::PathBuf> {
    let dir = claim_dir(claim_id);
    let tag = std::fs::read_to_string(dir.join(OWNER_FILE)).ok()?;
    let j = db.journey(db.claim(claim_id).ok()?.journey_id).ok()?;
    (tag.trim() == owner_tag(&j)).then_some(dir)
}

/// Removes the attachments (full card number) of every claim that no longer needs them: kept only
/// while the user looks at them before sending, while the claim is being sent, and while it is
/// SUBMITTING or INDETERMINATE. So they go after a final outcome (submitted, not sent, rejected), on
/// dismissal or "claimed elsewhere", on expiry, after a dry run, and for claims that no longer exist
/// or now belong to another trip (after "Geschiedenis wissen"). A preview in another process (the
/// app while a scheduled check runs) is not visible here: an open claim's fresh folder stays for
/// [`PREVIEW_GRACE`].
pub fn remove_unneeded_attachments(db: &ovrefund_core::db::Db) -> Result<()> {
    remove_unneeded_attachments_at(db, chrono::Utc::now())
}

fn remove_unneeded_attachments_at(db: &ovrefund_core::db::Db, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    let entries = match std::fs::read_dir(claims_root()) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut error = None;
    for entry in entries {
        let e = match entry {
            Ok(e) => e,
            Err(e) => {
                error.get_or_insert(e);
                continue;
            }
        };
        let fresh = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .is_some_and(|modified| now.signed_duration_since(chrono::DateTime::<chrono::Utc>::from(modified)) < PREVIEW_GRACE);
        let keep = e.file_name().to_str().and_then(|n| n.parse::<i64>().ok()).is_some_and(|id| {
            owned_dir(db, id).is_some()
                && (phase_of(id).is_some_and(|p| ACTIVE_PHASES.contains(&p.as_str()))
                    || db.claim(id).is_ok_and(|c| {
                        c.status == ClaimState::Submitting
                            || (c.status == ClaimState::Indeterminate
                                && db
                                    .journey(c.journey_id)
                                    .is_ok_and(|j| now <= ovrefund_core::eligibility::expires_at(j.check_in, Default::default())))
                            || (fresh
                                && phase_of(id).is_none()
                                && matches!(c.status, ClaimState::NeedsReview | ClaimState::Ready | ClaimState::Failed))
                    }))
        });
        if !keep {
            if let Err(e) = std::fs::remove_dir_all(e.path()) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    error.get_or_insert(e);
                }
            }
        }
    }
    error.map_or(Ok(()), |e| Err(e.into()))
}

/// The attachments stored with a claim: (name "NL / EN", file, size).
fn attachments(claim_id: i64) -> Vec<Value> {
    let Some(dir) = crate::runner::open_db().ok().and_then(|db| owned_dir(&db, claim_id)) else { return vec![] };
    [
        (
            TRAVEL_FILE,
            "Reisbewijs: je reis in Mijn NS met je OV-chipkaartnummer / Travel proof: your trip in Mijn NS with your OV-chipkaart number",
        ),
        (DELAY_FILE, "Vertragingsbewijs: Rijden de Treinen open data (CC BY 4.0) / Delay proof: Rijden de Treinen open data (CC BY 4.0)"),
    ]
    .iter()
    .enumerate()
    .filter_map(|(index, (f, name))| {
        let m = std::fs::metadata(dir.join(f)).ok()?;
        if !m.is_file() {
            return None;
        }
        Some(json!({"name": name, "file": f, "kind": "png", "size": m.len(), "path": "", "index": index}))
    })
    .collect()
}

/// For the app: per claim the phase of a running or finished Claim, whether the contact details
/// are set, and whether this build can send a claim at all.
pub fn state() -> Value {
    let claims: serde_json::Map<String, Value> = STATE.lock().unwrap().iter().map(|(k, v)| (k.to_string(), v.ui.clone())).collect();
    let c = contact();
    let mut missing = vec![];
    if c.is_none() {
        missing.push("contact");
    }
    if crate::secrets::bank_details().is_none() {
        missing.push("bank");
    }
    json!({
        "claims": claims,
        "missing": missing,
        "submit_enabled": arriva::final_write_possible(),
    })
}

/// Everything a real claim needs, read from the ledger and the stored details. Refuses (bilingual,
/// "nothing was sent") whenever a fact is missing or the trip no longer qualifies.
struct Prepared {
    form: FormData,
    fare: ArrivaFare,
    journey: ovrefund_core::model::Journey,
    legs: Vec<ovrefund_core::model::Leg>,
    delay: i64,
    expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(PartialEq, serde::Deserialize)]
pub struct ReviewedTrip {
    check_in: chrono::DateTime<chrono::Utc>,
    origin: String,
    destination: String,
    delay: Option<i64>,
    refund: Option<i64>,
    vrij_basis: Option<String>,
    card_last4: Option<String>,
    iban_last4: Option<String>,
}

impl Prepared {
    fn summary(&self) -> ReviewedTrip {
        let last4 = |value: &str| value.get(value.len().saturating_sub(4)..).map(str::to_string);
        let comp = self.fare.compensation(&self.journey, self.delay);
        let refund = comp.as_ref().map(|c| c.cents);
        let vrij_basis = comp.filter(|c| !c.exact).map(|c| c.basis);
        ReviewedTrip {
            check_in: self.journey.check_in,
            origin: self.form.departure_station.clone(),
            destination: self.form.arrival_station.clone(),
            delay: Some(self.delay),
            refund,
            vrij_basis,
            card_last4: last4(&self.form.card),
            iban_last4: last4(&self.form.iban),
        }
    }

    fn check_preview(&self, current: &Self) -> Result<()> {
        anyhow::ensure!(
            self.form == current.form && self.fare == current.fare && self.legs == current.legs && self.delay == current.delay
                && self.expires_at == current.expires_at
                && self.journey.source_id == current.journey.source_id
                && self.journey.source_record_id == current.journey.source_record_id
                && self.journey.coupling_id == current.journey.coupling_id
                && self.journey.check_in == current.journey.check_in
                && self.journey.check_out == current.journey.check_out,
            "De claimgegevens zijn gewijzigd. Kies Claimen om de bijlagen opnieuw te bekijken. / The claim details changed. Choose Claim to review the attachments again."
        );
        Ok(())
    }
}

/// Refusal for a claim whose earlier attempt may have reached Arriva, without the user's check.
const UNCERTAIN_MSG: &str = "Deze claim is misschien al bij Arriva aangekomen. Kijk in je e-mail van Arriva. Staat er geen bevestiging in, vink dat dan aan bij Claimen. / This claim may already have reached Arriva. Check your e-mail from Arriva. If there is no confirmation, tick that when you press Claim.";

fn prepare(db: &ovrefund_core::db::Db, claim_id: i64, confirm_resubmit: bool) -> Result<Prepared> {
    let c = db.claim(claim_id)?;
    let j = db.journey(c.journey_id)?;
    anyhow::ensure!(j.is_arriva(), "Deze reis is geen Arriva-reis. Er is niets verstuurd. / This is not an Arriva trip. Nothing was sent.");
    anyhow::ensure!(
        matches!(c.status, ClaimState::NeedsReview | ClaimState::Failed | ClaimState::Ready),
        "Deze claim kan niet (meer) worden ingediend (status {0}). / This claim cannot be submitted (any more) (status {0}).",
        c.status.as_str()
    );
    anyhow::ensure!(confirm_resubmit || !db.was_uncertain(claim_id)?, UNCERTAIN_MSG);
    anyhow::ensure!(!db.has_external_claim(&j)?, ovrefund_core::pipeline::CLAIMED_ELSEWHERE_MSG);
    let (conf, _, sdep, exp) = db.assessment(c.journey_id)?.ok_or_else(|| anyhow!(ovrefund_core::pipeline::NOT_ASSESSED_MSG))?;
    let now = chrono::Utc::now();
    anyhow::ensure!(
        now <= exp,
        "De claimtermijn van Arriva (3 maanden) is verlopen. Er is niets verstuurd. / Arriva's claim period (3 months) has passed. Nothing was sent."
    );
    let (delay, reasons): (Option<i64>, String) =
        db.conn.query_row("SELECT estimated_arrival_delay_min, reasons_json FROM candidates WHERE journey_id=?1", [c.journey_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let first_reason = serde_json::from_str::<Vec<String>>(&reasons).ok().and_then(|r| r.into_iter().next()).unwrap_or_default();
    let not_eligible = "Deze reis voldoet niet (meer) aan de regels van Arriva. Er is niets verstuurd. / This trip does not (or no longer) meet Arriva's rules. Nothing was sent.";
    anyhow::ensure!(
        conf == Confidence::NeedsReview && first_reason.starts_with(ovrefund_core::eligibility::ARRIVA_ELIGIBLE_PREFIX),
        not_eligible
    );
    let delay = delay.ok_or_else(|| anyhow!(not_eligible))?;
    if let Some((_, nl, en)) = blocker(db, &j) {
        bail!("{nl} Er is niets verstuurd. / {en} Nothing was sent.");
    }
    let region = j.arriva_region().ok_or_else(|| anyhow!("{REGION_UNKNOWN_NL} / {REGION_UNKNOWN_EN}"))?;
    let local = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&chrono_tz::Europe::Amsterdam);
    let a = ovrefund_core::carrier_policy::ARRIVA.assess(Some(region), delay, local(j.check_in).date_naive(), local(now).date_naive());
    anyhow::ensure!(a.eligible, not_eligible);
    let option = a.form_option.ok_or_else(|| anyhow!(not_eligible))?;
    let sdep = sdep.ok_or_else(|| anyhow!(ovrefund_core::pipeline::NO_DEPARTURE_MSG))?;
    let fare = effective_arriva_fare(&j).ok_or_else(|| anyhow!("{FARE_UNKNOWN_NL} / {FARE_UNKNOWN_EN}"))?;
    let dest = j.destination.clone().ok_or_else(|| anyhow!(ovrefund_core::pipeline::TRIP_INCOMPLETE_MSG))?;
    let card: String = full_card(db, &j).ok_or_else(|| anyhow!("{CARD_UNKNOWN_NL} / {CARD_UNKNOWN_EN}"))?;
    let bank = crate::secrets::bank_details()
        .ok_or_else(|| anyhow!("Stel eerst je rekening in bij Jouw gegevens. / Set up your bank account under Your details first."))?;
    let contact = contact().ok_or_else(|| {
        anyhow!("Vul eerst je naam en e-mailadres voor Arriva in bij Jouw gegevens. / Enter your name and e-mail address for Arriva under Your details first.")
    })?;
    let contact = validate_contact(&contact)?;
    let ev = db.cached_evidence(&j.fallback_key())?.ok_or_else(|| anyhow!(not_eligible))?;
    let mut legs: Vec<ovrefund_core::model::Leg> = ev.planned.map(|r| r.legs).unwrap_or_default();
    for l in ev.taken.map(|r| r.legs).unwrap_or_default() {
        if !legs.iter().any(|p| p.train_number == l.train_number && p.dep_planned == l.dep_planned) {
            legs.push(l);
        }
    }
    anyhow::ensure!(!legs.is_empty(), not_eligible);
    let form = FormData {
        region: region.into(),
        delay_option: option.into(),
        card,
        iban: bank.iban.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase(),
        valid_until: expiry_for_card(&contact, journey_card_key(&j).as_deref()),
        departure_station: j.origin.name.clone(),
        arrival_station: dest.name,
        travel_time: local(sdep).format("%d-%m-%Y %H:%M").to_string(),
        fare: form_fare(&fare),
        first_name: contact.first_name,
        last_name: contact.last_name,
        email: contact.email,
        demo: false,
    };
    Ok(Prepared { form, fare, journey: j, legs, delay, expires_at: exp })
}

fn form_fare(fare: &ArrivaFare) -> String {
    let cents = fare.cents();
    format!("{},{:02}", cents / 100, cents % 100)
}

/// Field ids of the form checks (see arriva.rs) in plain words, for the messages.
fn field_names(ids: &[String]) -> (String, String) {
    let name = |id: &str| -> (&str, &str) {
        match id {
            "valid_until" => ("de vervaldatum van je OV-chipkaart (Jouw gegevens)", "your OV-chipkaart's expiry date (Your details)"),
            "region" => ("regio", "region"),
            "card_number" => ("OV-chipkaartnummer", "OV-chipkaart number"),
            "iban" => ("IBAN", "IBAN"),
            "attachment" => ("bijlagen", "attachments"),
            "departure_station" => ("vertrekstation", "departure station"),
            "arrival_station" => ("aankomststation", "arrival station"),
            "travel_time" => ("reisdatum en -tijd", "travel date and time"),
            "fare" => ("ritprijs", "fare"),
            "delay" => ("vertraging", "delay"),
            "first_name" => ("voornaam", "first name"),
            "last_name" => ("achternaam", "last name"),
            "email" => ("e-mailadres", "e-mail address"),
            "consent" => ("toestemming", "consent"),
            "no_form_errors" => ("foutmeldingen op het formulier", "errors on the form"),
            "no_other_required" => ("een verplicht veld dat OV Refund niet kent", "a required field OV Refund does not know"),
            "submit_button" => ("knop Indienen", "submit button"),
            _ => ("formulierveld", "form field"),
        }
    };
    let nl = ids.iter().map(|i| name(i).0).collect::<Vec<_>>().join(", ");
    let en = ids.iter().map(|i| name(i).1).collect::<Vec<_>>().join(", ");
    (nl, en)
}

/// After the attachments went to Arriva (HubSpot's file upload) a refusal must not say that nothing
/// was sent.
const UPLOADED_NL: &str = "Een of meer bijlagen kunnen al naar Arriva zijn geüpload. De claim is niet ingediend.";
const UPLOADED_EN: &str = "One or more attachments may already have been uploaded to Arriva. The claim was not submitted.";

/// The error as the user reads it: once attachments were uploaded, "nothing was sent" becomes
/// [`UPLOADED_NL`] / [`UPLOADED_EN`].
fn after_upload(e: anyhow::Error) -> anyhow::Error {
    if arriva::final_write().uploads == 0 {
        return e;
    }
    let m = format!("{e:#}");
    let (nl, en) = m.split_once(" / ").unwrap_or((&m, &m));
    let clean = |s: &str, x: &str| s.replace(x, "").trim().trim_end_matches('.').to_string();
    anyhow!("{}. {UPLOADED_NL} / {}. {UPLOADED_EN}", clean(nl, "Er is niets verstuurd."), clean(en, "Nothing was sent."))
}

/// Claim status, uncertainty and "claimed elsewhere", checked before the form opens and again right
/// before the claim is recorded as being submitted.
fn recheck(db: &ovrefund_core::db::Db, claim_id: i64, j: &ovrefund_core::model::Journey, confirm_resubmit: bool) -> Result<ClaimState> {
    let c = db.claim(claim_id)?;
    anyhow::ensure!(
        matches!(c.status, ClaimState::NeedsReview | ClaimState::Failed | ClaimState::Ready),
        "De status van deze claim is intussen veranderd. Er is niets verstuurd. / The status of this claim changed in the meantime. Nothing was sent."
    );
    anyhow::ensure!(confirm_resubmit || !db.was_uncertain(claim_id)?, UNCERTAIN_MSG);
    anyhow::ensure!(!db.has_external_claim(j)?, ovrefund_core::pipeline::CLAIMED_ELSEWHERE_MSG);
    Ok(c.status)
}

/// How long an open claim's new attachments survive a sweep by a process that did not make them.
const PREVIEW_GRACE: chrono::Duration = chrono::Duration::minutes(30);

/// Phases of a Claim in which its attachments are still needed (see [`remove_unneeded_attachments`]).
const ACTIVE_PHASES: [&str; 3] = ["preparing", "preview", "verifying"];

fn phase_of(claim_id: i64) -> Option<String> {
    STATE.lock().unwrap().get(&claim_id).and_then(|v| v.ui["phase"].as_str().map(str::to_string))
}

static QUIT_CONFIRMED: AtomicBool = AtomicBool::new(false);
pub fn quit_confirmed() -> bool {
    QUIT_CONFIRMED.load(Ordering::SeqCst)
}

/// The user chose to close the app although an Arriva claim still waits for its outcome.
#[tauri::command]
pub fn arriva_quit_anyway(app: AppHandle) {
    QUIT_CONFIRMED.store(true, Ordering::SeqCst);
    arriva::disarm_final_write();
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.close();
    }
}

/// Whether an Arriva Claim is running (preparing, preview or waiting for Arriva's outcome).
pub fn running() -> bool {
    RUNNING.load(Ordering::SeqCst) || STATE.lock().unwrap().values().any(|v| v.ui["phase"] == "verifying")
}

/// The full OV-chipkaart number of the trip's card (the demo's fictional card in the demo).
fn full_card(db: &ovrefund_core::db::Db, j: &ovrefund_core::model::Journey) -> Option<String> {
    if crate::demo::is_demo() {
        return Some(arriva::DEMO_CARD.to_string());
    }
    j.card_last4.as_deref().and_then(|l| crate::secrets::card_number(db, j.coupling_id.as_deref(), l)).filter(|n: &String| n.len() == 16)
}

/// Why an Arriva trip that meets Arriva's rules still cannot be claimed in the app: (code, NL, EN).
/// The app then shows "submit it yourself at Arriva" with this reason, and no Claim button.
pub fn blocker(db: &ovrefund_core::db::Db, j: &ovrefund_core::model::Journey) -> Option<(&'static str, &'static str, &'static str)> {
    if db.has_card_conflict(j).unwrap_or(true) {
        return Some((
            "card_conflict",
            "Mijn NS koppelt deze reis aan een andere kaart. Controleer de gekoppelde kaart en dien zelf in bij Arriva.",
            "Mijn NS links this trip to a different card. Check the linked card and submit directly to Arriva.",
        ));
    }
    if j.arriva_region().is_none() {
        return Some(("region", REGION_UNKNOWN_NL, REGION_UNKNOWN_EN));
    }
    if is_student_product(j) {
        return Some(("student", STUDENT_NL, STUDENT_EN));
    }
    if effective_arriva_fare(j).is_none() {
        if j.subscription.as_deref().is_some_and(|s| !s.trim().is_empty()) {
            return Some(("ns_product", NS_PRODUCT_NL, NS_PRODUCT_EN));
        }
        return Some(("fare", FARE_UNKNOWN_NL, FARE_UNKNOWN_EN));
    }
    if full_card(db, j).is_none() {
        return Some(("card", CARD_UNKNOWN_NL, CARD_UNKNOWN_EN));
    }
    None
}

fn is_student_product(j: &ovrefund_core::model::Journey) -> bool {
    [j.subscription.as_deref(), j.product_name.as_deref()].into_iter().flatten().any(|s| s.to_lowercase().contains("student"))
}

const REGION_UNKNOWN_NL: &str = "De Arriva-regio van deze reis is onbekend. Dien deze claim zelf in bij Arriva.";
const REGION_UNKNOWN_EN: &str = "The Arriva region of this trip is unknown. Submit this claim yourself at Arriva.";
const STUDENT_NL: &str = "Voor een studentenreisproduct vult OV Refund het Arriva-formulier niet in. Dien deze claim zelf in bij Arriva.";
const STUDENT_EN: &str = "OV Refund does not fill in Arriva's form for a student travel product. Submit this claim yourself at Arriva.";
const FARE_UNKNOWN_NL: &str = "De ritprijs van deze reis is onbekend en Arriva vraagt die. Dien deze claim zelf in bij Arriva.";
const FARE_UNKNOWN_EN: &str = "The fare of this trip is unknown and Arriva asks for it. Submit this claim yourself at Arriva.";
const NS_PRODUCT_NL: &str = "OV Refund kan de ritwaarde nog niet bepalen. Daarvoor zijn een herkend NS Vrij-abonnement, een bekende maandprijs en een gefactureerde reis binnen de vrije reisuren nodig; dien zelf in bij Arriva.";
const NS_PRODUCT_EN: &str = "OV Refund cannot yet determine the trip value. It needs a recognised NS Vrij subscription, a known monthly price and an invoiced trip within the free travel hours; submit yourself at Arriva.";
const CARD_UNKNOWN_NL: &str =
    "Het volledige OV-chipkaartnummer is niet bekend. Kies Controleer nu zodat OV Refund het uit Mijn NS leest, of dien deze claim zelf in bij Arriva.";
const CARD_UNKNOWN_EN: &str =
    "The full OV-chipkaart number is not known. Choose Check now so OV Refund reads it from Mijn NS, or submit this claim yourself at Arriva.";

/// Makes the two attachments under the caller's run lock. The user's travel proof uses Mijn NS
/// and timetable data. The demo uses fictional travel history.
fn make_attachments(app: &AppHandle, db: &mut ovrefund_core::db::Db, claim_id: i64, p: &Prepared) -> Result<Vec<std::path::PathBuf>> {
    let (travel, delay) = if crate::demo::is_demo() {
        arriva::demo_attachment_pngs(app)?
    } else {
        let b = crate::ns_bridge::NsBridge::open(app, false)?;
        // Strict: only this trip on its own day (other trips and account details blanked), or no travel proof at all.
        let shot = crate::ns_adapters::capture_history_trip(&b, &p.journey, true);
        let _ = b.navigate("about:blank");
        b.hide();
        let shot = shot?;
        let local = p.journey.check_in.with_timezone(&chrono_tz::Europe::Amsterdam);
        let caption = crate::proof_cards::Caption {
            card_number: Some(p.form.card.clone()),
            date: local.format("%d-%m-%Y").to_string(),
            route: format!("{} \u{2013} {}", p.form.departure_station, p.form.arrival_station),
            demo: false,
        };
        let travel = arriva::travel_proof_png(app, &shot.png, &caption)?;
        let (delay, months) = arriva::delay_cards(app, &p.legs)?;
        for (month, at) in &months {
            db.set_setting(&ovrefund_core::archive::retrieved_key(month), &at.to_rfc3339())?;
        }
        (travel, delay)
    };
    let dir = claim_dir(claim_id);
    // Write the owner marker before storing any attachment containing personal data.
    remove_attachments(claim_id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(OWNER_FILE), owner_tag(&p.journey))?;
    let tp = dir.join(TRAVEL_FILE);
    let dp = dir.join(DELAY_FILE);
    let written = (|| -> Result<Vec<std::path::PathBuf>> {
        anyhow::ensure!(
            !travel.is_empty() && !delay.is_empty(),
            "De bijlagen konden niet worden gemaakt. Er is niets verstuurd. / The attachments could not be made. Nothing was sent."
        );
        std::fs::write(&tp, travel)?;
        std::fs::write(&dp, delay)?;
        Ok(vec![tp, dp])
    })();
    if written.is_err() {
        remove_attachments(claim_id);
    }
    written
}

/// Step 1 of the user's Claim: every check that needs no network (claim, trip, Arriva's rules,
/// deadline, contact details, card number, IBAN) and both attachments, which the app then shows
/// ("Klopt, ga verder" / "Annuleren"). The proof uses Mijn NS and timetable data. Arriva's form
/// stays closed until the user has reviewed the attachments and consented.
pub fn make_preview(app: &AppHandle, claim_id: i64, reviewed: &ReviewedTrip) -> Result<Vec<Value>> {
    let _op = Running::take().ok_or_else(|| {
        anyhow!("Er loopt al een Arriva-claim. Wacht tot die klaar is. / An Arriva claim is already running. Wait until it is done.")
    })?;
    set_state(app, claim_id, "preparing", "Bijlagen maken… / Making the attachments…");
    // Hold the same lease as a check through proof creation and publication of the preview.
    let mut _lease = None;
    let mut made_files = false;
    let r = (|| -> Result<Prepared> {
        _lease = Some(crate::commands::LedgerLock::take_with(10, BUSY_MSG)?);
        let mut db = crate::runner::open_db()?;
        // Preparing a local preview is allowed before the user's re-submission confirmation.
        let p = prepare(&db, claim_id, true)?;
        anyhow::ensure!(
            crate::demo::is_demo() || p.summary() == *reviewed,
            "De claimgegevens zijn gewijzigd. Kies Claimen om ze opnieuw te bekijken. / The claim details changed. Choose Claim to review them again."
        );
        make_attachments(app, &mut db, claim_id, &p)?;
        made_files = true;
        recheck(&db, claim_id, &p.journey, true)?;
        Ok(p)
    })();
    match r {
        Ok(p) => {
            let mut progress = ClaimProgress::new("preview", "Bekijk de bijlagen. / Check the attachments.");
            progress.preview = Some(p);
            STATE.lock().unwrap().insert(claim_id, progress);
            let _ = app.emit("arriva-claim-updated", claim_id);
            Ok(attachments(claim_id))
        }
        Err(e) => {
            if made_files {
                remove_attachments(claim_id);
            }
            set_state(app, claim_id, "failed", &ovrefund_core::redact::redact(&format!("{e:#}")));
            Err(e)
        }
    }
}

/// The user cancelled the preview, or the Claim stopped before [`submit`] took over: while the
/// claim is still in its preview, the phase is reset and the attachments are deleted (nothing was
/// sent). In any other phase nothing changes (a claim being sent keeps its files).
pub fn cancel_preview(app: &AppHandle, claim_id: i64) -> bool {
    let reset = reset_preview(claim_id);
    if reset {
        let _ = app.emit("arriva-claim-updated", claim_id);
    }
    reset
}

/// Clear active previews when the main window closes without sending them.
pub fn cancel_open_previews() {
    let mut state = STATE.lock().unwrap();
    state.retain(|claim_id, progress| {
        if progress.ui["phase"] == "preview" {
            remove_attachments(*claim_id);
            return false;
        }
        true
    });
}

/// [`cancel_preview`] without the event: whether the claim was in its preview (and was reset).
fn reset_preview(claim_id: i64) -> bool {
    let mut state = STATE.lock().unwrap();
    if state.get(&claim_id).and_then(|v| v.ui["phase"].as_str()) != Some("preview") {
        return false;
    }
    state.remove(&claim_id);
    remove_attachments(claim_id);
    true
}

/// Refusal of Claim while a check or another action of the app is running.
const BUSY_MSG: &str =
    "Er loopt al een controle of claim. Probeer het zo opnieuw. / A check or claim is already running. Please try again shortly.";

/// Refusal of a Claim that stopped before the form opened, while the attachments were shown.
const STOPPED_BEFORE_FORM: &str = "Er is niets naar Arriva verstuurd. Kies Claimen om nieuwe bijlagen te maken. / Nothing was sent to Arriva. Choose Claim to make a new preview.";

/// Appends [`STOPPED_BEFORE_FORM`] to an "NL / EN" refusal.
fn stopped_before_form(msg: &str) -> String {
    let (nl, en) = msg.split_once(" / ").unwrap_or((msg, msg));
    let (snl, sen) = STOPPED_BEFORE_FORM.split_once(" / ").unwrap_or((STOPPED_BEFORE_FORM, STOPPED_BEFORE_FORM));
    format!("{nl} {snl} / {en} {sen}")
}

/// The first step of [`submit`]: one Arriva claim at a time, and only after the preview. When
/// another claim is running, a claim still in its preview is reset (its attachments deleted), so
/// it never stays "in progress".
fn start_submit(claim_id: i64) -> Result<(Running, Prepared)> {
    let Some(op) = Running::take() else {
        let msg = "Er loopt al een Arriva-claim; probeer het zo opnieuw. / An Arriva claim is already running; please try again shortly.";
        if reset_preview(claim_id) {
            bail!(stopped_before_form(msg));
        }
        bail!(msg);
    };
    let mut state = STATE.lock().unwrap();
    let progress = state.get_mut(&claim_id).filter(|p| p.ui["phase"] == "preview");
    let preview = progress.and_then(|p| {
        let preview = p.preview.take()?;
        *p = ClaimProgress::new("preparing", "Formulier voorbereiden? / Preparing the form?");
        Some(preview)
    });
    let preview = preview.ok_or_else(|| {
        anyhow!("Bekijk eerst de bijlagen: kies Claimen opnieuw. Er is niets verstuurd. / Check the attachments first: choose Claim again. Nothing was sent.")
    })?;
    Ok((op, preview))
}

fn remove_attachments(claim_id: i64) {
    let _ = std::fs::remove_dir_all(claim_dir(claim_id));
}

/// Step 2, after the user confirmed the attachments: Arriva's form is filled in a hidden, guarded
/// window with the traveller's details and the attachments, every field is verified, the claim is
/// recorded as SUBMITTING and Indienen pressed. Returns once Indienen was pressed (or the claim was
/// refused); the outcome is awaited in the background (holding `busy`) and reported through the
/// ledger and `arriva-claim-updated`. `confirm_resubmit`: the user checked their e-mail from Arriva
/// after an uncertain earlier attempt and found no confirmation.
///
/// A build or run that cannot send (dry run, test or developer build) stops before the form: the
/// user's attachments never leave the computer there. The demo goes through the whole flow with its
/// fictional attachments, but nothing is uploaded and the final write is blocked.
pub fn submit<G: Send + 'static>(app: &AppHandle, claim_id: i64, confirm_resubmit: bool, busy: G) -> Result<()> {
    let started = start_submit(claim_id);
    let _ = app.emit("arriva-claim-updated", claim_id);
    let (op, preview) = started?;
    let demo = crate::demo::is_demo();
    let r = (|| -> Result<Option<(Bridge, bool, crate::commands::LedgerLock)>> {
        let lease = crate::commands::LedgerLock::take_with(30, BUSY_MSG)?;
        let mut db = crate::runner::open_db()?;
        let p = prepare(&db, claim_id, confirm_resubmit)?;
        preview.check_preview(&p)?;
        let files: Vec<std::path::PathBuf> = owned_dir(&db, claim_id)
            .map(|d| vec![d.join(TRAVEL_FILE), d.join(DELAY_FILE)])
            .filter(|f| f.iter().all(|f| std::fs::metadata(f).is_ok_and(|m| m.len() > 0)))
            .ok_or_else(|| anyhow!("De bijlagen ontbreken. Kies Claimen opnieuw. Er is niets verstuurd. / The attachments are missing. Choose Claim again. Nothing was sent."))?;
        recheck(&db, claim_id, &p.journey, confirm_resubmit)?;
        let send = arriva::final_write_possible();
        if !send && !demo {
            // Dry run / test or developer build: everything local was checked; the form (and with it
            // the upload of the user's attachments) is never opened.
            return Ok(None);
        }
        let b = Bridge::open_claim(app)?;
        (|| -> Result<()> {
            arriva::populate(&b, &files, &p.form)?;
            let unverified = arriva::final_check(&b, &p.form)?;
            if !unverified.is_empty() {
                let (nl, en) = field_names(&unverified);
                bail!("Niet alle velden van Arriva konden worden gecontroleerd: {nl}. Er is niets verstuurd. / Not all of Arriva's fields could be verified: {en}. Nothing was sent.");
            }
            Ok(())
        })()
        .map_err(after_upload)?;
        // Every field verified. Checked once more (no claim twice for the same journey), then the
        // claim is recorded as being submitted, with its in-flight marker in the same transaction,
        // before anything can be sent.
        let status = recheck(&db, claim_id, &p.journey, confirm_resubmit).map_err(after_upload)?;
        if status != ClaimState::Ready {
            db.transition(claim_id, Some(status), ClaimState::Ready, "user", "Claimen bij Arriva / Claim at Arriva")
                .map_err(after_upload)?;
        }
        let now = chrono::Utc::now();
        let begun = db.transition_and_set_checked(
            claim_id,
            Some(ClaimState::Ready),
            ClaimState::Submitting,
            "user",
            "Indienen bij Arriva (alle velden gecontroleerd) / Submitting to Arriva (all fields verified)",
            &format!("{}{claim_id}", ovrefund_core::pipeline::INFLIGHT_KEY_PREFIX),
            &format!("{}|{}", now.to_rfc3339(), lease.0),
            |db| {
                anyhow::ensure!(db.live_run_lock_owner()?.as_deref() == Some(lease.0.as_str()), ovrefund_core::db::LOCK_LOST_MSG);
                preview.check_preview(&prepare(db, claim_id, confirm_resubmit)?)?;
                db.conn.execute(
                    "UPDATE claims SET gateway='arriva-hubspot-v1', authorized_mode='manual', authorized_at=?2,
                     attempts=attempts+1, submitted_here=1 WHERE id=?1",
                    rusqlite::params![claim_id, now.to_rfc3339()],
                )?;
                Ok(())
            },
        );
        if let Err(e) = &begun {
            ovrefund_core::pipeline::restore_unsent(&mut db, claim_id, status, e);
        }
        begun.map_err(after_upload)?;
        set_state(app, claim_id, "verifying", "Verificatie bij Arriva… / Verifying at Arriva…");
        let shown = (|| -> Result<()> {
            b.window.hide()?;
            if demo {
                b.captcha_only(
                    "DEMO - Verificatie bij Arriva… Verzending blijft geblokkeerd.",
                    "DEMO - Verifying at Arriva… Submission stays blocked.",
                )?;
            } else {
                b.captcha_only("Verificatie bij Arriva…", "Verifying at Arriva…")?;
            }
            Ok(())
        })();
        match shown.and_then(|_| {
            if send {
                arriva::arm_final_write(OUTCOME_WAIT, p.expires_at)?;
            }
            arriva::press_submit(&b)
        }) {
            Ok(_) => Ok(Some((b, send, lease))),
            Err(e) => {
                let f = arriva::disarm_final_write();
                let sent = possibly_sent(&f);
                if sent {
                    Ok(Some((b, send, lease)))
                } else {
                    record(app, claim_id, interrupted_outcome(&f, &e.to_string()));
                    Err(after_upload(e))
                }
            }
        }
    })();
    match r {
        Ok(Some((b, send, lease))) => {
            let app = app.clone();
            std::thread::spawn(move || {
                // The claim stays protected (one Arriva claim at a time, the app busy) until its
                // outcome is recorded.
                let _op = op;
                let _busy = busy;
                let _lease = lease;
                let outcome = wait_outcome(&b, send);
                arriva::disarm_final_write();
                if let Some(w) = app.get_webview_window(arriva::CLAIM_LABEL) {
                    let _ = w.close();
                }
                record(&app, claim_id, outcome);
            });
            Ok(())
        }
        Ok(None) => {
            remove_attachments(claim_id);
            set_state(
                app,
                claim_id,
                "dry_run",
                "Proefrun: alles op je computer gecontroleerd en beide bijlagen gemaakt (en weer gewist). In deze versie opent OV Refund het formulier van Arriva niet: er is niets geüpload of ingediend. / Dry run: everything checked on your computer and both attachments made (and deleted again). In this version OV Refund does not open Arriva's form: nothing was uploaded or submitted.",
            );
            Ok(())
        }
        Err(e) => {
            arriva::disarm_final_write();
            if let Some(w) = app.get_webview_window(arriva::CLAIM_LABEL) {
                let _ = w.close();
            }
            // An outcome recorded as uncertain keeps its own message.
            if phase_of(claim_id).as_deref() != Some("uncertain") {
                set_state(app, claim_id, "failed", &ovrefund_core::redact::redact(&format!("{e:#}")));
            }
            // After the phase left "preview"/"verifying": kept only while the claim is in flight or
            // uncertain; otherwise deleted.
            if let Ok(db) = crate::runner::open_db() {
                let _ = remove_unneeded_attachments(&db);
            }
            Err(e)
        }
    }
}

/// The final write may have reached Arriva: it went out. No other write can carry the claim: the
/// guard lets through only Google's CAPTCHA calls, the attachment upload and this one final write.
fn possibly_sent(f: &arriva::FinalWrite) -> bool {
    f.sent
}

fn interrupted_outcome(f: &arriva::FinalWrite, why: &str) -> Outcome {
    if possibly_sent(f) {
        Outcome::Uncertain(why.into())
    } else {
        Outcome::NotSent(why.into())
    }
}

enum Outcome {
    /// HubSpot acknowledged the final write, with any confirmation reference.
    Sent(Option<String>),
    /// The final write went out but its result is unclear.
    Uncertain(String),
    /// No final submission was released by the guard.
    NotSent(String),
    /// Dry run (demo, developer build): everything verified, Indienen pressed, the final write blocked.
    DryRunBlocked,
}

/// Waits for the final write's response, showing the CAPTCHA-only window while Google shows a
/// puzzle. Never presses anything.
fn wait_outcome(b: &Bridge, send: bool) -> Outcome {
    let until = Instant::now() + OUTCOME_WAIT;
    let app = b.window.app_handle().clone();
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let f = arriva::final_write();
        if response_complete(&f) {
            break;
        }
        if !send && f.blocked > 0 {
            return Outcome::DryRunBlocked;
        }
        let closed = app.get_webview_window(arriva::CLAIM_LABEL).is_none();
        if closed || f.timed_out() || Instant::now() >= until {
            let write = arriva::disarm_final_write();
            let why = if closed {
                "het Arriva-venster is gesloten / the Arriva window was closed"
            } else if !write.sent && write.blocked > 0 {
                "de verzendcontrole hield het formulier tegen. Dien de claim zo nodig zelf in bij Arriva / the submission check blocked the form. Submit it yourself at Arriva if needed"
            } else {
                "geen antwoord van Arriva / no answer from Arriva"
            };
            return interrupted_outcome(&write, why);
        }
        let challenge = b.eval(&format!("(()=>{{{}return shown();}})()", arriva::CHALLENGE_VISIBLE)).map(|v| v == true).unwrap_or(false);
        let shown = b.window.is_visible().unwrap_or(false);
        if challenge && !shown {
            let presentation = if send {
                b.captcha_only("Los de puzzel op om je claim te versturen.", "Solve the puzzle to send your claim.")
            } else {
                b.captcha_only(
                    "DEMO - Los de puzzel op. Verzending blijft geblokkeerd.",
                    "DEMO - Solve the puzzle. Submission stays blocked.",
                )
            };
            let _ = presentation.and_then(|_| b.show());
        } else if !challenge && shown {
            let _ = b.window.hide();
        }
        if !possibly_sent(&f) {
            if let Ok(v) = arriva::page_outcome(b) {
                if v["captchaError"] == true {
                    return interrupted_outcome(
                        &arriva::disarm_final_write(),
                        "Arriva kon de CAPTCHA niet valideren / Arriva could not validate the CAPTCHA",
                    );
                }
            }
        }
    }
    let f = arriva::disarm_final_write();
    let body = if f.finished && !f.failed && f.status.is_some_and(|code| (200..300).contains(&code)) {
        f.network_id.as_deref().and_then(|id| b.response_text(id))
    } else {
        None
    };
    match response_outcome(&f, body.as_deref()) {
        Outcome::Sent(None) => {
            std::thread::sleep(Duration::from_millis(1500));
            let reference = arriva::page_outcome(b)
                .ok()
                .filter(|v| v["thanks"] == true && v["captchaError"] != true)
                .and_then(|v| v["reference"].as_str().map(str::to_string));
            Outcome::Sent(reference)
        }
        outcome => outcome,
    }
}

fn response_outcome(f: &arriva::FinalWrite, body: Option<&str>) -> Outcome {
    if !f.sent || !response_complete(f) || f.failed {
        return interrupted_outcome(f, "de verzending werd onderbroken / the submission was interrupted");
    }
    let Some(code) = f.status else {
        return Outcome::Uncertain("geen antwoord van Arriva / no answer from Arriva".into());
    };
    if !(200..300).contains(&code) {
        return Outcome::Uncertain(format!("Arriva antwoordde met status {code} / Arriva answered with status {code}"));
    }
    let body = body.unwrap_or_default().trim();
    if code == 204 && body.is_empty() {
        return Outcome::Sent(None);
    }
    if let Ok(json) = serde_json::from_str::<Value>(body) {
        if !ovrefund_core::pipeline::response_has_error(&json) {
            let reference = response_reference(&json);
            if reference.is_some()
                || ["success", "accepted"].iter().any(|key| json[*key] == true)
                || json["inlineMessage"].as_str().is_some_and(arriva_acknowledgement)
            {
                return Outcome::Sent(reference);
            }
        }
    } else if arriva_acknowledgement(body) {
        return Outcome::Sent(None);
    }
    Outcome::Uncertain("geen duidelijke ontvangstbevestiging van Arriva / no clear acknowledgement from Arriva".into())
}

fn arriva_acknowledgement(text: &str) -> bool {
    let text = text.to_lowercase();
    let visible = text.trim_start_matches("<p>").trim_start();
    !ovrefund_core::pipeline::negative_acknowledgement(&text)
        && [
            "bedankt voor je aanvraag",
            "bedankt voor uw aanvraag",
            "we hebben je aanvraag ontvangen",
            "thank you for your submission",
            "we received your submission",
        ]
        .iter()
        .any(|ack| visible.starts_with(ack))
}

fn response_complete(f: &arriva::FinalWrite) -> bool {
    f.sent && (f.failed || (f.finished && f.status.is_some()))
}

/// A confirmation reference in HubSpot's submit response, when it has one.
fn response_reference(v: &Value) -> Option<String> {
    for k in ["submissionGuid", "submissionId", "conversionId"] {
        if let Some(s) = v[k].as_str().map(str::trim).filter(|s| !s.is_empty() && s.len() <= 80) {
            return Some(s.to_string());
        }
    }
    None
}

fn record(app: &AppHandle, claim_id: i64, outcome: Outcome) {
    let r = (|| -> Result<(&'static str, String)> {
        let mut db = crate::runner::open_db()?;
        let result = record_outcome(&mut db, claim_id, &outcome).or_else(|error| {
            log::warn!("recording Arriva submission failed: {}", ovrefund_core::redact::redact(&error.to_string()));
            record_outcome(
                &mut db,
                claim_id,
                &Outcome::Uncertain("de uitkomst kon niet worden opgeslagen / the outcome could not be saved".into()),
            )
        });
        if result.is_err() {
            // The window can no longer send. Leave the claim recoverable if the journal write failed.
            let key = format!("{}{claim_id}", ovrefund_core::pipeline::INFLIGHT_KEY_PREFIX);
            let _ = db.conn.execute("DELETE FROM settings WHERE key=?1", [key]);
        }
        result
    })();
    match r {
        Ok((phase, message)) => set_state(app, claim_id, phase, &message),
        Err(e) => {
            log::warn!("recording the Arriva outcome failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
            set_state(app, claim_id, "uncertain", "Onzeker of Arriva je claim heeft ontvangen. Kijk in je e-mail van Arriva. / Not sure whether Arriva received your claim. Check your e-mail from Arriva.");
        }
    }
    // After the outcome's phase is set (while "verifying" they count as in use): attachments stay
    // only while the outcome is uncertain.
    if let Ok(db) = crate::runner::open_db() {
        let _ = remove_unneeded_attachments(&db);
    }
}

fn record_outcome(db: &mut ovrefund_core::db::Db, claim_id: i64, outcome: &Outcome) -> Result<(&'static str, String)> {
    match outcome {
        Outcome::DryRunBlocked => {
            // The demo (the only flow that presses Indienen without an armed guard): the claim goes
            // back to "Niet verzonden" and can be tried again.
            if db.claim(claim_id)?.status == ClaimState::Submitting {
                db.transition(
                    claim_id,
                    Some(ClaimState::Submitting),
                    ClaimState::Failed,
                    "gateway",
                    "Niet verzonden (demo): verzending geblokkeerd / Not sent (demo): submission blocked",
                )?;
            }
            Ok((
                "dry_run",
                "Niet verzonden (demo): alles gecontroleerd en Indienen ingedrukt; de verzending is geblokkeerd en er is niets geüpload. / Not sent (demo): everything verified and Indienen pressed; the submission was blocked and nothing was uploaded."
                    .to_string(),
            ))
        }
        Outcome::Sent(reference) => {
            db.complete_submission(
                claim_id,
                &ovrefund_core::model::SubmissionReceipt {
                    reference: reference.clone(),
                    confirmed: true,
                    detail: "Arriva heeft de claim ontvangen / Arriva received the claim".into(),
                },
            )?;
            Ok((
                "submitted",
                "Verstuurd: Arriva heeft je claim ontvangen. Bewaar een eventuele bevestigingsmail. / Sent: Arriva received your claim. Keep the confirmation e-mail if you get one.".to_string(),
            ))
        }
        Outcome::Uncertain(why) => {
            db.transition(
                claim_id,
                Some(ClaimState::Submitting),
                ClaimState::Indeterminate,
                "gateway",
                &ovrefund_core::db::bilingual_detail("Uitkomst onzeker: ", "Outcome uncertain: ", why),
            )?;
            Ok((
                "uncertain",
                "Onzeker of Arriva je claim heeft ontvangen. Kijk in je e-mail van Arriva; OV Refund probeert het niet opnieuw. / Not sure whether Arriva received your claim. Check your e-mail from Arriva; OV Refund does not try again."
                    .to_string(),
            ))
        }
        Outcome::NotSent(why) => {
            if db.claim(claim_id)?.status == ClaimState::Submitting {
                db.transition(
                    claim_id,
                    Some(ClaimState::Submitting),
                    ClaimState::Failed,
                    "gateway",
                    &ovrefund_core::db::bilingual_detail("Niet ingediend: ", "Not submitted: ", why),
                )?;
            }
            let (nl, en) = why.split_once(" / ").unwrap_or((why, why));
            let uploaded = arriva::final_write().uploads > 0;
            let (unl, uen) = if uploaded { (format!(" {UPLOADED_NL}"), format!(" {UPLOADED_EN}")) } else { (String::new(), String::new()) };
            Ok((
                "failed",
                format!("Niet ingediend: {nl}.{unl} Je kunt het opnieuw proberen. / Not submitted: {en}.{uen} You can try again."),
            ))
        }
    }
}

type CmdResult<T> = Result<T, String>;
fn err(e: anyhow::Error) -> String {
    let m = ovrefund_core::redact::redact(&format!("{e:#}"));
    if m.contains(" / ") {
        m
    } else {
        "De actie lukte niet. Probeer het opnieuw. / The action failed. Please try again.".into()
    }
}

#[tauri::command]
pub fn arriva_state() -> Value {
    state()
}

#[tauri::command]
pub fn arriva_contact() -> Value {
    let saved = contact();
    let c = saved.as_ref().cloned().unwrap_or_default();
    let card = crate::runner::open_db().ok().and_then(|db| expiry_card_for_settings(&db, c.card_expiry_key.as_deref()).ok().flatten());
    let expiry = card.as_ref().and_then(|(key, _)| saved_expiry_for_settings(&c, key));
    json!({"first_name": c.first_name, "last_name": c.last_name, "email": c.email,
        "card_valid_until": expiry, "expiry_card_key": card.as_ref().map(|(key, _)| key),
        "expiry_card_last4": card.as_ref().map(|(_, last4)| last4), "set": saved.is_some()})
}

fn saved_expiry_for_settings<'a>(contact: &'a Contact, card_key: &str) -> Option<&'a str> {
    expiry_for_card(contact, Some(card_key))?;
    contact.card_valid_until.as_deref()
}

fn confirm_expiry_card(expected_key: Option<&str>, displayed_key: Option<&str>) -> Result<()> {
    anyhow::ensure!(
        expected_key.is_some() && expected_key == displayed_key,
        "De kaart voor de vervaldatum is gewijzigd of onbekend. Open Gegevens voor Arriva opnieuw en controleer de kaart, of laat de vervaldatum leeg. / The card for the expiry date changed or is unknown. Reopen Details for Arriva and check the card, or leave the expiry date blank."
    );
    Ok(())
}

#[tauri::command]
pub fn set_arriva_contact(
    first_name: String,
    last_name: String,
    email: String,
    card_valid_until: Option<String>,
    expiry_card_key: Option<String>,
) -> CmdResult<Value> {
    let _lease = crate::commands::LedgerLock::take_with(2, BUSY_MSG).map_err(err)?;
    // A date in the past is accepted: the trip may have been made on a card that has expired since.
    let mut c = validate_contact(&Contact { first_name, last_name, email, card_valid_until, card_expiry_key: None }).map_err(err)?;
    if c.card_valid_until.is_some() {
        let db = crate::runner::open_db().map_err(err)?;
        let saved = contact();
        let card = expiry_card_for_settings(&db, saved.as_ref().and_then(|c| c.card_expiry_key.as_deref())).map_err(err)?;
        confirm_expiry_card(card.as_ref().map(|(key, _)| key.as_str()), expiry_card_key.as_deref()).map_err(err)?;
        c.card_expiry_key = card.map(|(key, _)| key);
    }
    crate::secrets::set(CONTACT_KEY, &serde_json::to_string(&c).map_err(|e| err(e.into()))?).map_err(err)?;
    Ok(arriva_contact())
}

#[tauri::command]
pub fn clear_arriva_contact() -> CmdResult<()> {
    let _lease = crate::commands::LedgerLock::take_with(2, BUSY_MSG).map_err(err)?;
    crate::secrets::delete(CONTACT_KEY).map_err(err)
}

/// The user pressed Claim for this Arriva trip and confirmed the summary: the attachments are made
/// for the preview. Nothing leaves the computer.
#[tauri::command]
pub async fn arriva_claim_preview(app: AppHandle, claim_id: i64, reviewed: ReviewedTrip) -> CmdResult<Vec<Value>> {
    crate::commands::login_window_open()?;
    let busy = crate::commands::BusyGuard::take()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        make_preview(&app, claim_id, &reviewed).map_err(err)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The user cancelled the preview: the attachments are deleted.
#[tauri::command]
pub fn arriva_claim_cancel(app: AppHandle, claim_id: i64) {
    let _ = cancel_preview(&app, claim_id);
}

/// The user confirmed the attachments ("Klopt, ga verder"): fill Arriva's form and send it. The app
/// stays busy until the outcome is known.
#[tauri::command]
pub async fn arriva_claim(
    app: AppHandle,
    claim_id: i64,
    confirm_resubmit: Option<bool>,
    attachments_confirmed: bool,
    consent_accepted: bool,
) -> CmdResult<Value> {
    if !consent_accepted {
        return Err("Bevestig eerst de toestemming voor Arriva. / First confirm consent for Arriva.".into());
    }
    if !attachments_confirmed {
        return Err("Bevestig eerst de Arriva-bijlagen. / First confirm the Arriva attachments.".into());
    }
    // Refused before the form opens (NS login window open, a check or another action running): a
    // claim still in its preview is reset and its attachments are deleted.
    let busy = crate::commands::login_window_open()
        .and_then(|_| crate::commands::BusyGuard::take().map_err(|_| BUSY_MSG.to_string()))
        .map_err(|e| if cancel_preview(&app, claim_id) { stopped_before_form(&e) } else { e })?;
    tauri::async_runtime::spawn_blocking(move || {
        submit(&app, claim_id, confirm_resubmit.unwrap_or(false), busy).map_err(err)?;
        Ok(state())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Shows a pending CAPTCHA puzzle again (the window was hidden or behind the app). Returns whether
/// a puzzle was shown.
#[tauri::command]
pub async fn arriva_claim_show(app: AppHandle) -> CmdResult<bool> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(window) = app.get_webview_window(arriva::CLAIM_LABEL) {
            let b = Bridge { window };
            if b.eval(&format!("(()=>{{{}return shown();}})()", arriva::CHALLENGE_VISIBLE)).map(|v| v == true).unwrap_or(false) {
                b.captcha_only("Los de puzzel op om je claim te versturen.", "Solve the puzzle to send your claim.").map_err(err)?;
                b.show().map_err(err)?;
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await
    .map_err(|e| err(e.into()))?
}

fn attachment_path(claim_id: i64, index: usize) -> Result<std::path::PathBuf> {
    let f = [TRAVEL_FILE, DELAY_FILE].get(index).ok_or_else(|| anyhow!("Bijlage ontbreekt. / Attachment is missing."))?;
    let db = crate::runner::open_db()?;
    let p = owned_dir(&db, claim_id).ok_or_else(|| anyhow!("Bijlage ontbreekt. / Attachment is missing."))?.join(f);
    anyhow::ensure!(p.is_file(), "Bijlage ontbreekt. / Attachment is missing.");
    Ok(p)
}

#[tauri::command]
pub fn arriva_claim_attachments(claim_id: i64) -> Vec<Value> {
    attachments(claim_id)
}

#[tauri::command]
pub fn arriva_claim_attachment_data(claim_id: i64, index: usize) -> CmdResult<String> {
    let p = attachment_path(claim_id, index).map_err(err)?;
    let bytes = std::fs::read(p).map_err(|e| err(e.into()))?;
    use base64::Engine;
    Ok(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

#[tauri::command]
pub fn arriva_claim_attachment(claim_id: i64, index: usize) -> CmdResult<()> {
    let p = attachment_path(claim_id, index).map_err(err)?;
    std::process::Command::new("explorer.exe").arg(p).spawn().map_err(|e| err(e.into()))?;
    Ok(())
}

/// "Forget everything": the attachments of every claim and the Arriva window's browser profile.
pub fn forget() -> Result<()> {
    match std::fs::remove_dir_all(crate::paths::data_dir().join("arriva")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    STATE.lock().unwrap().clear();
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn contact_details_are_checked() {
        let ok = Contact {
            first_name: " Jan ".into(),
            last_name: "Jansen".into(),
            email: "jan@example.nl".into(),
            card_valid_until: Some("31-12-2030".into()),
            card_expiry_key: None,
        };
        let v = validate_contact(&ok).unwrap();
        assert_eq!(v.first_name, "Jan");
        assert_eq!(parse_valid_until(v.card_valid_until.as_deref().unwrap()), Some((2030, 12, 31)));
        let no_date = Contact { card_valid_until: Some("  ".into()), ..ok.clone() };
        assert_eq!(validate_contact(&no_date).unwrap().card_valid_until, None);
        for bad in [
            Contact { first_name: "".into(), ..ok.clone() },
            Contact { last_name: " ".into(), ..ok.clone() },
            Contact { email: "jan@".into(), ..ok.clone() },
            Contact { email: "sample@invalid@example.invalid".into(), ..ok.clone() },
            Contact { email: "jan example@example.org".into(), ..ok.clone() },
            Contact { card_valid_until: Some("2030-12-31".into()), ..ok.clone() },
        ] {
            let e = validate_contact(&bad).unwrap_err().to_string();
            assert!(e.contains(" / "), "{e}");
        }
        // A date in the past is accepted (the trip may have been made on a card that has expired since).
        let past = validate_contact(&Contact { card_valid_until: Some("31-01-2020".into()), ..ok.clone() }).unwrap();
        assert_eq!(past.card_valid_until.as_deref(), Some("31-01-2020"));
    }
    #[test]
    fn expiry_never_crosses_linked_cards_with_the_same_last_four_digits() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let mut first = arriva::demo_journey();
        first.coupling_id = Some("test-card-one".into());
        first.card_last4 = Some("0000".into());
        let mut second = first.clone();
        second.coupling_id = Some("test-card-two".into());
        second.source_record_id = "second-test-trip".into();
        second.check_in += chrono::Duration::days(1);
        db.upsert_journey(&first).unwrap();
        let first_key = journey_card_key(&first).unwrap();
        assert_eq!(expiry_card_for_settings(&db, None).unwrap().unwrap().0, first_key);
        let mut details = Contact { card_valid_until: Some("31-12-2030".into()), ..Contact::default() };
        assert_eq!(expiry_for_card(&details, Some(&first_key)), None, "legacy date has no binding");
        assert_eq!(saved_expiry_for_settings(&details, &first_key), None, "ask for the date again before binding it");
        assert!(confirm_expiry_card(Some(&first_key), None).is_err());
        assert!(confirm_expiry_card(Some("different-card-key"), Some(&first_key)).is_err());
        assert!(confirm_expiry_card(Some(&first_key), Some(&first_key)).is_ok());
        details.card_expiry_key = Some(first_key.clone());
        db.upsert_journey(&second).unwrap();
        assert!(expiry_card_for_settings(&db, None).unwrap().is_none());
        assert_eq!(expiry_card_for_settings(&db, Some(&first_key)).unwrap().unwrap().0, first_key);
        assert_eq!(expiry_for_card(&details, Some(&first_key)), Some((2030, 12, 31)));
        assert_eq!(expiry_for_card(&details, journey_card_key(&second).as_deref()), None);
        assert_eq!(expiry_for_card(&details, None), None);
    }

    #[test]
    fn a_free_trip_under_an_ns_vrij_subscription_uses_the_per_trip_value_in_the_form() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let mut p = test_preview();
        p.journey.card_last4 = None; // Avoid looking up a card in protected storage.
        p.journey.subscription = Some("FLEXDVR".into());
        p.journey.subscription_fee_cents = Some(4430);
        for fare in [None, Some(0)] {
            p.journey.fare_cents = fare;
            assert_eq!(blocker(&db, &p.journey).map(|b| b.0), Some("card"));
            p.fare = effective_arriva_fare(&p.journey).unwrap();
            p.form.fare = form_fare(&p.fare);
            assert_eq!(p.form.fare, "2,46");
            assert_eq!(p.summary().refund, Some(123));
            assert!(p.summary().vrij_basis.unwrap().contains("maandbedrag Dal Vrij (€ 44,30, volgens NS) ÷ 18"));
        }

        p.journey.check_in = "2026-10-13T08:00:00+02:00".parse().unwrap();
        assert_eq!(blocker(&db, &p.journey).map(|b| b.0), Some("ns_product"));
        p.journey.subscription = Some("UNKNOWN-FLEXDVR".into());
        assert_eq!(blocker(&db, &p.journey).map(|b| b.0), Some("ns_product"));
        p.journey.subscription = None;
        assert_eq!(blocker(&db, &p.journey).map(|b| b.0), Some("fare"));
        assert!(NS_PRODUCT_NL.contains("dien zelf in bij Arriva") && NS_PRODUCT_EN.contains("submit yourself at Arriva"));
    }
    #[test]
    fn an_arriva_trip_with_a_conflicting_card_is_held_until_the_card_is_confirmed() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let mut trip = arriva::demo_journey();
        trip.coupling_id = Some("test-card-one".into());
        db.upsert_journey(&trip).unwrap();
        let mut relinked = trip.clone();
        relinked.coupling_id = Some("test-card-two".into());
        db.upsert_journey(&relinked).unwrap();
        assert_eq!(blocker(&db, &trip).map(|blocker| blocker.0), Some("card_conflict"));
        db.upsert_journey(&trip).unwrap();
        assert!(!db.has_card_conflict(&trip).unwrap());
    }

    #[test]
    fn response_headers_alone_do_not_confirm_an_arriva_submission() {
        let mut write = arriva::FinalWrite::default();
        assert!(!response_complete(&write));
        write.sent = true;
        write.status = Some(200);
        assert!(!response_complete(&write));
        write.failed = true;
        assert!(response_complete(&write));
        write.failed = false;
        write.finished = true;
        assert!(response_complete(&write));
    }

    #[test]
    fn a_claim_stopped_before_the_form_leaves_no_preview_behind() {
        let _serial = ATTACHMENT_DIRS.lock().unwrap_or_else(|e| e.into_inner());
        let id = 987_654;
        let dir = claim_dir(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(TRAVEL_FILE), b"png").unwrap();
        STATE.lock().unwrap().insert(id, ClaimProgress::new("preview", ""));
        // Another Arriva claim holds the one slot: this claim's preview is reset, its files deleted.
        let other = Running::take().expect("free in the test");
        let e = start_submit(id).err().unwrap().to_string();
        assert!(e.contains("nieuwe bijlagen") && e.contains("new preview") && e.contains(" / "), "{e}");
        assert_eq!(phase_of(id), None);
        assert!(!dir.exists());
        // Nothing to reset in another phase: a claim being verified keeps its files.
        std::fs::create_dir_all(&dir).unwrap();
        STATE.lock().unwrap().insert(id, ClaimProgress::new("verifying", ""));
        let e = start_submit(id).err().unwrap().to_string();
        assert!(!e.contains("verwijderd"), "{e}");
        assert!(!reset_preview(id));
        assert!(dir.exists() && phase_of(id).as_deref() == Some("verifying"));
        drop(other);
        // With the slot free, only a claim in its preview may go on.
        assert!(start_submit(id).is_err());
        let mut progress = ClaimProgress::new("preview", "");
        progress.preview = Some(test_preview());
        STATE.lock().unwrap().insert(id, progress);
        let (_running, _preview) = start_submit(id).unwrap();
        assert!(!reset_preview(id) && dir.exists());
        STATE.lock().unwrap().remove(&id);
        remove_attachments(id);
        // The refusal before the form keeps both languages.
        let m = stopped_before_form(BUSY_MSG);
        let (nl, en) = m.split_once(" / ").unwrap();
        assert!(nl.starts_with("Er loopt al een controle") && nl.contains("nieuwe bijlagen"), "{m}");
        assert!(en.starts_with("A check or claim is already running") && en.contains("new preview"), "{m}");
    }
    fn test_preview() -> Prepared {
        let journey = arriva::demo_journey();
        let expires_at = ovrefund_core::eligibility::expires_at(journey.check_in, Default::default());
        let fare = effective_arriva_fare(&journey).unwrap();
        Prepared { form: arriva::demo_form("Limburg", 24).unwrap(), fare, journey, legs: vec![], delay: 24, expires_at }
    }

    #[test]
    fn preview_refuses_changed_trip_or_form() {
        let preview = test_preview();
        assert!(preview.check_preview(&test_preview()).is_ok());
        let mut changed = test_preview();
        changed.form.delay_option = "60+ minuten".into();
        assert!(preview.check_preview(&changed).is_err());
        let mut changed = test_preview();
        changed.journey.source_record_id = "different-test-trip".into();
        assert!(preview.check_preview(&changed).is_err());
        let mut changed = test_preview();
        changed.delay = 25;
        assert!(preview.check_preview(&changed).is_err());
        assert!(preview.summary() != changed.summary());
        changed = test_preview();
        changed.form.iban = "NL96TEST0000001234".into();
        assert!(preview.summary() != changed.summary());
    }

    pub(crate) fn record_guard_outcome(write: &arriva::FinalWrite, body: Option<&str>, timeout: bool) -> &'static str {
        let mut db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let (jid, _) = db.upsert_journey(&arriva::demo_journey()).unwrap();
        let claim = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
        db.transition(claim.id, Some(ClaimState::Ready), ClaimState::Submitting, "user", "fixture").unwrap();
        let outcome = if timeout {
            interrupted_outcome(write, "geen antwoord van Arriva / no answer from Arriva")
        } else {
            response_outcome(write, body)
        };
        record_outcome(&mut db, claim.id, &outcome).unwrap();
        let stored = db.claim(claim.id).unwrap();
        let events = db.events(claim.id).unwrap();
        assert_eq!(events.last().unwrap().to_status, stored.status.as_str());
        stored.status.as_str()
    }

    #[test]
    fn sent_outcome_records_reference_and_timestamp_together() {
        let mut db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let (jid, _) = db.upsert_journey(&arriva::demo_journey()).unwrap();
        let claim = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
        db.transition(claim.id, Some(ClaimState::Ready), ClaimState::Submitting, "user", "fixture").unwrap();
        record_outcome(&mut db, claim.id, &Outcome::Sent(Some("ref-test".into()))).unwrap();
        let stored = db.claim(claim.id).unwrap();
        assert_eq!(stored.status, ClaimState::Submitted);
        assert_eq!(stored.ns_reference.as_deref(), Some("ref-test"));
        assert!(stored.submitted_at.is_some());
    }

    #[test]
    fn hubspot_reference() {
        assert_eq!(response_reference(&json!({"submissionGuid": "abc-123", "inlineMessage": "Bedankt"})), Some("abc-123".into()));
        assert_eq!(response_reference(&json!({"inlineMessage": "Bedankt"})), None);
    }
    #[test]
    fn unverified_fields_are_named() {
        let (nl, en) = field_names(&["valid_until".into(), "consent".into()]);
        assert!(nl.contains("vervaldatum") && en.contains("expiry date") && en.contains("consent"));
    }
    /// A tripwire only: it checks the order of calls in this file's source, not behaviour (the flow
    /// needs a running app and Arriva's page). Keep it in step when [`submit`] is restructured.
    #[test]
    fn claim_flow_order_record_before_arming() {
        // The ledger says SUBMITTING (with its in-flight marker, one transaction) before the guard
        // is armed and Indienen pressed.
        let src = include_str!("arriva_claim.rs");
        let body = &src[src.find("pub fn submit<").unwrap()..src.find("enum Outcome").unwrap()];
        let submitting = body.find(concat!("db.transition", "_and_set_checked(")).unwrap();
        let arm = body.find(concat!("arm_final", "_write(")).unwrap();
        let press = body.find(concat!("press", "_submit(")).unwrap();
        assert!(submitting < arm && arm < press);
        // A build or run that cannot send (except the demo, which has no real data) stops before
        // Arriva's form opens (no upload of the user's attachments), after every local check.
        let send = body.find(concat!("let send = arriva::final_write", "_possible()")).unwrap();
        let dry = body.find(concat!("if !send && !demo {", "")).unwrap();
        let form = body.find(concat!("Bridge::open", "_claim(")).unwrap();
        let checks = body.find(concat!("prepare(&db", ", claim_id")).unwrap();
        let preview = body.find(concat!("start_", "submit(claim_id)")).unwrap();
        assert!(preview < checks && checks < send && send < dry && dry < form);
        // The demo never arms the guard.
        assert!(body.find(concat!("if send {", "")).unwrap() < arm);
        // The attachments are made (and shown) in the preview step, before submit.
        let pre = &src[src.find("pub fn make_preview(").unwrap()..src.find("pub fn cancel_preview(").unwrap()];
        assert!(pre.contains(concat!("make_", "attachments(")) && !pre.contains(concat!("Bridge::open", "_claim(")));
        assert!(pre.find(concat!("LedgerLock::take", "_with")).unwrap() < pre.find(concat!("make_", "attachments(")).unwrap());
        let make = &src[src.find("fn make_attachments(").unwrap()..src.find("pub fn make_preview(").unwrap()];
        assert!(make.contains(concat!("capture_history", "_trip(")));
    }
    #[test]
    fn errors_after_the_upload_never_say_nothing_was_sent() {
        let src = include_str!("arriva_claim.rs");
        let body = &src[src.find(concat!("Bridge::open", "_claim(app)?;")).unwrap()..src.find("enum Outcome").unwrap()];
        // Every error after the form opened goes through after_upload (or record, which says so too).
        for line in body.lines().filter(|l| l.trim_start().starts_with(concat!("return Err", "(")) || l.contains(concat!("Err(", "e)"))) {
            assert!(
                line.contains("after_upload") || line.contains("Err(e) =>") || line.contains("if let Err(e)") || line.trim() == "Err(e)",
                "{line}"
            );
        }
        let nl_en = |s: &str| s.split_once(" / ").map(|(a, b)| (a.to_string(), b.to_string())).unwrap();
        let (nl, en) = nl_en(&format!("{}. {UPLOADED_NL} / {}. {UPLOADED_EN}", "Fout. Er is niets verstuurd.", "Error. Nothing was sent."));
        assert!(nl.contains("geüpload") && en.contains("uploaded"));
    }
    /// Tests that create or remove claim attachment folders in the shared test data folder.
    pub(crate) static ATTACHMENT_DIRS: Mutex<()> = Mutex::new(());
    #[test]
    fn attachments_belong_to_the_claims_trip() {
        let _serial = ATTACHMENT_DIRS.lock().unwrap_or_else(|e| e.into_inner());
        let mut db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let mut j = crate::arriva::demo_journey();
        j.source_record_id = "owner-test".into();
        let (jid, _) = db.upsert_journey(&j).unwrap();
        let c = db.ensure_claim(jid, "fixture", ClaimState::NeedsReview).unwrap();
        let dir = claim_dir(c.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(TRAVEL_FILE), b"png").unwrap();
        // Without a marker, or with another trip's marker, the files are not this claim's.
        assert!(owned_dir(&db, c.id).is_none());
        std::fs::write(dir.join(OWNER_FILE), "something-else").unwrap();
        assert!(owned_dir(&db, c.id).is_none());
        std::fs::write(dir.join(OWNER_FILE), owner_tag(&j)).unwrap();
        assert_eq!(owned_dir(&db, c.id), Some(dir.clone()));
        // Kept while the user looks at the preview.
        STATE.lock().unwrap().insert(c.id, ClaimProgress::new("preview", ""));
        // A folder of a claim id that no longer exists is removed; this claim's stays.
        let orphan = claim_dir(c.id + 1000);
        std::fs::create_dir_all(&orphan).unwrap();
        remove_unneeded_attachments(&db).unwrap();
        assert!(!orphan.exists() && dir.exists());
        // Startup cannot see an open preview in another process.
        STATE.lock().unwrap().remove(&c.id);
        crate::commands::recover_claims_on_start_in(&mut db, "fixture-startup", &|_| false).unwrap();
        assert!(dir.exists());
        assert!(db.run_lock_owner().unwrap().is_none());
        let modified = chrono::DateTime::<chrono::Utc>::from(std::fs::metadata(&dir).unwrap().modified().unwrap());
        remove_unneeded_attachments_at(&db, modified + chrono::Duration::minutes(31)).unwrap();
        assert!(!dir.exists(), "startup grace also expires for an open claim");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OWNER_FILE), owner_tag(&j)).unwrap();
        // After the preview (cancelled, or a final outcome) a claim that is not in flight loses them.
        STATE.lock().unwrap().insert(c.id, ClaimProgress::new("failed", ""));
        remove_unneeded_attachments(&db).unwrap();
        assert!(!dir.exists());
        STATE.lock().unwrap().remove(&c.id);
        // A possibly sent claim keeps them through its claim window, while an in-flight one
        // keeps them even after the deadline.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OWNER_FILE), owner_tag(&j)).unwrap();
        db.transition(c.id, Some(ClaimState::NeedsReview), ClaimState::Ready, "user", "t").unwrap();
        db.transition(c.id, Some(ClaimState::Ready), ClaimState::Submitting, "user", "t").unwrap();
        let expired = ovrefund_core::eligibility::expires_at(j.check_in, Default::default()) + chrono::Duration::seconds(1);
        remove_unneeded_attachments_at(&db, expired).unwrap();
        assert!(dir.exists());
        let owner = "fixture-claim";
        db.acquire_run_lock(owner, chrono::Duration::minutes(10)).unwrap();
        crate::commands::recover_claims_on_start_in(&mut db, "fixture-startup", &|_| true).unwrap();
        assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitting);
        assert_eq!(db.run_lock_owner().unwrap().as_deref(), Some(owner));
        db.release_run_lock(owner).unwrap();
        crate::commands::recover_claims_on_start_in(&mut db, "fixture-startup", &|_| false).unwrap();
        assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Indeterminate);
        remove_unneeded_attachments_at(&db, j.check_in).unwrap();
        assert!(dir.exists());
        remove_unneeded_attachments_at(&db, expired).unwrap();
        assert!(!dir.exists());
    }
}
