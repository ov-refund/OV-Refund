//! Arriva's claim form in a guarded hidden window: the developer demo (synthetic data, never sent)
//! and the shared form code of real claims (`arriva_claim`). Every request pauses before the
//! network. Reads (GET, HEAD, OPTIONS) go through to any https host, as the page loads them;
//! writes only to Google's CAPTCHA and HubSpot's file upload. Every other write fails closed.
//! In a real claim's window the file upload (the user's own attachments) is allowed only when
//! this build may send the claim at all ([`final_write_possible`]). The form's final submission
//! is let through exactly once, and only when the user's own Claim flow armed it after every field
//! was verified (`arm_final_write`); never in the demo, the form test, the view test, a dry run or
//! a developer build without explicit permission.
use anyhow::{anyhow, bail, Result};
use crossbeam_channel::bounded;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const LABEL: &str = "arriva-demo";
const PROOF_LABEL: &str = "arriva-proof";
const URL: &str = "https://klantenservice.arriva.nl/geldterug";
const RAW: &str = include_str!("../assets/arriva/real-delayed-service.json");
/// Window of a real claim (see `arriva_claim`).
pub(crate) const CLAIM_LABEL: &str = "arriva-claim";
static STATE: once_cell::sync::Lazy<Mutex<Value>> = once_cell::sync::Lazy::new(|| Mutex::new(initial()));
/// What happened to the claim window's final submission write.
#[derive(Debug, Clone, Default)]
pub(crate) struct FinalWrite {
    generation: u64,
    armed: bool,
    submit_deadline: Option<Instant>,
    claim_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    attachments: Vec<Attachment>,
    uploads_open: bool,
    upload_attempts: Vec<UploadAttempt>,
    /// The final write went to the network (allowed once by the armed guard).
    pub sent: bool,
    /// Its request id (CDP networkId), to read the response.
    pub network_id: Option<String>,
    submit_url: Option<String>,
    /// HTTP status of the response, when one came.
    pub status: Option<u16>,
    /// CDP confirmed that the response finished loading.
    pub finished: bool,
    /// The request failed, possibly after receiving response headers.
    pub failed: bool,
    /// Final writes the guard stopped (not armed).
    pub blocked: u32,
    /// Attachment uploads that went to HubSpot from the claim window.
    pub uploads: u32,
}
#[derive(Debug, Clone, PartialEq)]
struct Attachment {
    name: String,
    size: usize,
}

#[derive(Debug, Clone)]
struct UploadAttempt {
    id: String,
    files: Vec<usize>,
    status: Option<u16>,
    finished: bool,
    failed: bool,
    acknowledged: Option<Vec<Vec<String>>>,
}

impl UploadAttempt {
    fn response_complete(&self) -> bool {
        self.finished && !self.failed && self.status.is_some_and(|code| (200..300).contains(&code))
    }
}

fn upload_references(body: &str) -> Vec<Vec<String>> {
    fn collect(value: &Value, out: &mut Vec<Vec<String>>) {
        match value {
            Value::Array(items) => items.iter().for_each(|item| collect(item, out)),
            Value::Object(fields) => {
                let before = out.len();
                for key in ["files", "file", "data"] {
                    if let Some(item) = fields.get(key) {
                        collect(item, out);
                    }
                }
                if out.len() == before {
                    let alternatives = ["fileUrl", "url", "fileId", "id"].iter().filter_map(|key| fields.get(*key)
                        .and_then(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
                            .or_else(|| v.as_u64().map(|id| id.to_string())))).collect::<Vec<_>>();
                    if !alternatives.is_empty() {
                        out.push(alternatives);
                    }
                }
            }
            Value::String(url) if url.starts_with("https://") => out.push(vec![url.clone()]),
            _ => {}
        }
    }

    let mut references = Vec::new();
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if !ovrefund_core::pipeline::response_has_error(&value) {
            collect(&value, &mut references);
        }
    }
    references.sort_unstable();
    references.dedup();
    references
}

impl FinalWrite {
    fn arm(&mut self) -> bool {
        if self.sent || self.armed {
            return false;
        }
        self.armed = true;
        true
    }

    fn permit_submit(&mut self, network_id: Option<&str>) -> bool {
        if !std::mem::take(&mut self.armed) {
            self.blocked += 1;
            return false;
        }
        self.sent = true;
        self.network_id = network_id.map(str::to_string);
        true
    }

    fn permit_upload(&mut self, files: Vec<usize>, id: &str) -> bool {
        if !self.uploads_open || files.is_empty() || self.upload_attempts.iter().any(|a| a.id == id)
            || files.iter().any(|file| self.upload_attempts.iter().filter(|a| a.files.contains(file)).count() >= 2)
        {
            return false;
        }
        self.upload_attempts.push(UploadAttempt {
            id: id.to_string(), files, status: None, finished: false, failed: false, acknowledged: None,
        });
        self.uploads += 1;
        true
    }

    fn uploads_cover(&self, fields: Option<&[String]>) -> bool {
        let mut covered = std::collections::HashSet::new();
        let mut references = std::collections::HashSet::new();
        for upload in self.upload_attempts.iter().filter(|upload| upload.response_complete()) {
            let Some(files) = &upload.acknowledged else { continue };
            let matched: Option<std::collections::HashSet<_>> = files.iter()
                .map(|ids| ids.iter().find(|id| fields.is_none_or(|values| values.contains(id))))
                .collect();
            if let Some(matched) = matched.filter(|ids| files.len() == upload.files.len() && ids.len() == files.len()) {
                covered.extend(upload.files.iter().copied());
                references.extend(matched);
            }
        }
        covered.len() == self.attachments.len() && references.len() >= self.attachments.len()
    }

    fn pending_upload_responses(&self) -> impl Iterator<Item = &str> {
        self.upload_attempts.iter()
            .filter(|upload| upload.response_complete() && upload.acknowledged.is_none())
            .map(|upload| upload.id.as_str())
    }

    fn check_upload_response(&mut self, id: &str, body: Option<&str>) {
        if let Some(upload) = self.upload_attempts.iter_mut().find(|upload| upload.id == id && upload.response_complete()) {
            // CDP can temporarily fail to return a completed body. Retry until the upload step expires.
            upload.acknowledged = if upload.status == Some(204) { Some(Vec::new()) } else { body.map(upload_references) };
        }
    }

    fn attachments_ready(&self, selected: bool) -> bool {
        self.uploads_cover(None) || selected && self.upload_attempts.iter().all(|upload| {
            upload.response_complete() && upload.status == Some(204)
        })
    }

    fn request_allowed(&mut self, event: &Value, claim_window: bool, may_send: bool) -> bool {
        let request = &event["request"];
        let method = request["method"].as_str().unwrap_or("");
        let url = request["url"].as_str().unwrap_or("");
        if is_attachment_write(method, url) {
            if !claim_window || !claim_upload_allowed(method, url, may_send) || !self.uploads_open {
                return false;
            }
            let Some(id) = event["networkId"].as_str() else { return false };
            return multipart_attachments(request, &self.attachments).is_some_and(|(files, _)| self.permit_upload(files, id));
        }
        if method == "POST" && is_final_submit(url) && claim_window {
            if !may_send || !self.armed || self.timed_out()
                || self.claim_expires_at.is_some_and(|end| chrono::Utc::now() > end)
                || self.attachments.len() != 2
                || !multipart_attachments(request, &self.attachments).is_some_and(|(files, fields)| {
                    files.len() == self.attachments.len() || self.uploads_cover(Some(&fields))
                })
            {
                self.blocked += 1;
                return false;
            }
            self.uploads_open = false;
            self.submit_url = Some(url.to_string());
            return self.permit_submit(event["networkId"].as_str());
        }
        allow_request(method, url)
    }

    fn network_event(&mut self, event: &str, data: &Value) {
        let Some(id) = data["requestId"].as_str() else { return };
        let response = &data["response"];
        if self.sent && self.network_id.as_deref() == Some(id) {
            match event {
                "Network.loadingFailed" => self.failed = true,
                "Network.loadingFinished" => self.finished = true,
                "Network.responseReceived" if response["url"].as_str() == self.submit_url.as_deref() => {
                    self.status = response["status"].as_u64().and_then(|code| code.try_into().ok());
                }
                "Network.responseReceived" => self.failed = true,
                _ => {}
            }
        }
        if let Some(upload) = self.upload_attempts.iter_mut().find(|a| a.id == id) {
            match event {
                "Network.loadingFailed" => upload.failed = true,
                "Network.loadingFinished" => upload.finished = true,
                "Network.responseReceived" => {
                    if response["url"].as_str().is_some_and(is_upload) {
                        upload.status = response["status"].as_u64().and_then(|code| code.try_into().ok());
                    } else {
                        upload.failed = true;
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn timed_out(&self) -> bool {
        self.submit_deadline.is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn disarm(&mut self) -> Self {
        self.uploads_open = false;
        self.armed = false;
        self.clone()
    }
}

static FINAL_WRITE: Mutex<Option<FinalWrite>> = Mutex::new(None);
static CLAIM_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Whether a final write may be armed at all: never in the demo, a dry run, a devtools test with its
/// own scratch folder, or a developer build (unless the maintainer explicitly allows it for an
/// approved live test with OVREFUND_ARRIVA_ALLOW_SUBMIT=1).
pub(crate) fn may_arm(devtools: bool, demo: bool, dry_run: bool, test_scratch: bool, allow_env: bool) -> bool {
    !demo && !dry_run && !test_scratch && (!devtools || allow_env)
}
pub(crate) fn final_write_possible() -> bool {
    !cfg!(test) && !crate::arriva_claim::quit_confirmed() && may_arm(
        cfg!(feature = "devtools"),
        crate::demo::is_demo(),
        std::env::var_os("OVREFUND_DRY_RUN").is_some(),
        SCRATCH_OVERRIDE.get().is_some(),
        std::env::var("OVREFUND_ARRIVA_ALLOW_SUBMIT").as_deref() == Ok("1"),
    )
}
/// Arms the guard of the claim window for one final write. Only the user's Claim flow calls this,
/// after every field was verified and the claim was recorded as SUBMITTING.
pub(crate) fn arm_final_write(wait: Duration, claim_expires_at: chrono::DateTime<chrono::Utc>) -> Result<()> {
    if !final_write_possible() {
        bail!("Verzenden is in deze omgeving uitgeschakeld. / Submitting is disabled in this environment.");
    }
    let mut guard = FINAL_WRITE.lock().unwrap();
    let guard = guard.get_or_insert_with(FinalWrite::default);
    anyhow::ensure!(guard.arm(), "Arriva-verzending is al gestart. / Arriva submission has already started.");
    guard.submit_deadline = Some(Instant::now() + wait);
    guard.claim_expires_at = Some(claim_expires_at);
    Ok(())
}
pub(crate) fn disarm_final_write() -> FinalWrite {
    FINAL_WRITE.lock().unwrap().get_or_insert_with(FinalWrite::default).disarm()
}
pub(crate) fn reset_final_write() {
    let generation = CLAIM_GENERATION.fetch_add(1, Ordering::Relaxed);
    *FINAL_WRITE.lock().unwrap() = Some(FinalWrite { generation, ..FinalWrite::default() });
}
pub(crate) fn final_write() -> FinalWrite {
    FINAL_WRITE.lock().unwrap().clone().unwrap_or_default()
}
static OP: Mutex<()> = Mutex::new(());
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Claim-host writes the guard stopped (all of them fail before reaching the network).
static BLOCKED: AtomicUsize = AtomicUsize::new(0);
/// Attachment uploads the guard stopped in the demo / form-test window (never sent anywhere).
static DEMO_UPLOADS_BLOCKED: AtomicUsize = AtomicUsize::new(0);
/// Separate scratch folder for the devtools `arriva-form-test` (removed afterwards).
static SCRATCH_OVERRIDE: OnceLock<std::path::PathBuf> = OnceLock::new();
fn available() -> bool {
    cfg!(feature = "devtools") && crate::demo::is_demo()
}
pub fn enabled_for_check() -> bool {
    // On by default in the developer demo; OVREFUND_ARRIVA_DEMO=0 leaves Arriva out.
    available() && (ENABLED.load(Ordering::SeqCst) || std::env::var("OVREFUND_ARRIVA_DEMO").as_deref() != Ok("0"))
}
/// Travel day of the demo's real-path trip (and of its timetable record in the demo): always 10
/// days before today (Amsterdam), so it stays within Arriva's 3-month claim period. The delay card
/// still shows the public record of train 732284 on [`RDT_DATE`].
pub fn demo_real_date() -> chrono::NaiveDate {
    chrono::Utc::now().with_timezone(&chrono_tz::Europe::Amsterdam).date_naive() - chrono::Duration::days(10)
}
/// Amsterdam local time on `date` as RFC 3339.
fn amsterdam(date: chrono::NaiveDate, h: u32, m: u32) -> String {
    use chrono::TimeZone;
    let t = date.and_hms_opt(h, m, 0).expect("valid time");
    chrono_tz::Europe::Amsterdam.from_local_datetime(&t).earliest().expect("a time outside the DST change").to_rfc3339()
}
pub fn demo_journey() -> ovrefund_core::model::Journey {
    let date = demo_real_date();
    let next = date.succ_opt().expect("valid date");
    demo_trip(
        "DEMO-ARRIVA-732284",
        &amsterdam(date, 23, 25),
        &amsterdam(next, 0, 49),
        ("8400644", "Venlo"),
        ("8400470", "Nijmegen"),
        1240,
    )
}
/// The demo's Arriva trips: one claimable (real public RDT record, attachments, real form), one
/// already tested and one with too little delay. The last two are fictional and never sent anywhere.
pub fn demo_journeys() -> Vec<ovrefund_core::model::Journey> {
    vec![
        demo_journey(),
        demo_trip(
            "DEMO-ARRIVA-TESTED",
            "2026-10-21T08:12:00+02:00",
            "2026-10-21T08:58:00+02:00",
            ("8400526", "Roermond"),
            ("8400644", "Venlo"),
            680,
        ),
        demo_trip(
            "DEMO-ARRIVA-SHORT",
            "2026-11-03T17:40:00+01:00",
            "2026-11-03T18:21:00+01:00",
            ("8400307", "Heerlen"),
            ("8400424", "Maastricht"),
            590,
        ),
    ]
}
fn demo_trip(id: &str, check_in: &str, check_out: &str, from: (&str, &str), to: (&str, &str), fare: i64) -> ovrefund_core::model::Journey {
    use ovrefund_core::model::{Journey, StationRef};
    let t = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc);
    Journey {
        source_id: "demo".into(),
        source_record_id: id.into(),
        coupling_id: None,
        card_last4: Some("1234".into()),
        check_in: t(check_in),
        check_out: Some(t(check_out)),
        origin: StationRef { uic: from.0.into(), name: from.1.into() },
        destination: Some(StationRef { uic: to.0.into(), name: to.1.into() }),
        carrier_code: Some("ARR".into()),
        carrier_name: Some("Arriva".into()),
        product_name: Some("Fictieve NS Flex-reis".into()),
        travel_class: Some(2),
        fare_cents: Some(fare),
        subscription: None,
        subscription_fee_cents: None,
        invoiced: Some(true),
        payload_sha256: if id == "DEMO-ARRIVA-732284" {
            "arriva-demo-synthetic-passenger-real-service-732284".into()
        } else {
            format!("arriva-demo-fictional-{}", id.to_lowercase())
        },
    }
}
fn initial() -> Value {
    json!({"available":available(),"demo_only":true,"phase":"idle","steps":[
 {"id":"guard","label":"Verzending blokkeren","status":"pending","detail":"Alle definitieve aanvragen worden voor netwerkverkeer geblokkeerd."},
 {"id":"evidence","label":"Bijlagen maken","status":"pending","detail":"Reisbewijs (demogegevens) en vertragingsbewijs (Rijden de Treinen open data)."},
 {"id":"form","label":"Arriva-formulier invullen","status":"pending","detail":"Officieel formulier, synthetische gegevens."},
 {"id":"captcha","label":"CAPTCHA en controle","status":"pending","detail":"Alleen in het officiële formulier; de claim wordt niet ingediend."}],
 "trip":{"from":"Venlo","to":"Nijmegen","train_number":"732284","date":"2026-10-29","delay_minutes":24,"fare_cents":1240,"check_in":"2026-10-29T23:25:00+01:00","check_out":"2026-10-30T00:49:00+01:00","region":"Limburg"},
 "demo_trips":DEMO_TRIPS.iter().map(|t| demo_trip_status(t.0, t.2)).collect::<Vec<_>>(),
 "policy":crate::carrier_policy::ARRIVA.to_json(),
 "attachments":[],"logs":[],"window_open":false,"verified_fields":0,"unverified":[],"captcha":null})
}
/// Demo trips: id, region (Arriva's train concession of the line), recorded arrival delay, fixed demo
/// state. 732284 is a real Maaslijn train (its delay is replaced by the downloaded record's value);
/// the other two are fictional.
const DEMO_TRIPS: &[(&str, &str, i64, Option<&str>)] = &[
    ("DEMO-ARRIVA-732284", "Limburg", 24, None),
    ("DEMO-ARRIVA-TESTED", "Limburg", 31, Some("tested")),
    ("DEMO-ARRIVA-SHORT", "Limburg", 9, None),
];
/// A demo trip with Arriva's rules applied (carrier policy): status, refund band, form choice, deadline.
fn demo_trip_status(id: &str, delay: i64) -> Value {
    demo_trip_status_on(id, delay, chrono::Local::now().date_naive())
}
fn demo_trip_status_on(id: &str, delay: i64, today: chrono::NaiveDate) -> Value {
    let policy = &crate::carrier_policy::ARRIVA;
    let (_, region, _, fixed) = DEMO_TRIPS.iter().find(|t| t.0 == id).copied().unwrap_or((id, "", delay, None));
    let j = demo_journeys().into_iter().find(|j| j.source_record_id == id);
    let applies = j.as_ref().is_some_and(|j| policy.matches(j.carrier_code.as_deref(), j.carrier_name.as_deref()));
    let travel = j.map(|j| j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive()).unwrap_or(today);
    let a = policy.assess(Some(region).filter(|r| !r.is_empty()), delay, travel, today);
    let status = match fixed {
        Some(f) => f,
        None if a.eligible && applies => "claimable",
        None => "not_eligible",
    };
    json!({"id":id,"status":status,"delay_minutes":delay,"train_number":(id == "DEMO-ARRIVA-732284").then_some(RDT_TRAIN),
        "real_service":id == "DEMO-ARRIVA-732284","region":a.region,"eligible":a.eligible && applies,"min_delay":a.min_delay,
        "refund_percent":a.refund_percent,"form_option":a.form_option,"deadline":a.deadline.map(|d| d.format("%d-%m-%Y").to_string()),
        "reason_nl":a.reason_nl,"reason_en":a.reason_en})
}
fn note(step: &str, status: &str, message: &str) {
    let mut s = STATE.lock().unwrap();
    if let Some(steps) = s["steps"].as_array_mut() {
        for t in steps {
            if t["id"] == step {
                t["status"] = json!(status);
                t["detail"] = json!(message);
            }
        }
    }
    s["logs"].as_array_mut().unwrap().push(json!({"at":chrono::Utc::now().to_rfc3339(),"step":step,"message":message}));
}
fn snapshot(app: &AppHandle) -> Value {
    let mut s = STATE.lock().unwrap().clone();
    s["available"] = json!(available());
    s["enabled"] = json!(enabled_for_check());
    s["window_open"] = json!(app.get_webview_window(LABEL).is_some());
    s["window_visible"] = json!(app.get_webview_window(LABEL).and_then(|w| w.is_visible().ok()).unwrap_or(false));
    if s["window_open"] == false && matches!(s["phase"].as_str(), Some("ready" | "verifying" | "captcha_attention")) {
        s["phase"] = json!("window_closed");
        s["last_error"] = json!("Het Arriva-venster is gesloten. Bereid de demo opnieuw voor.");
    }
    s
}
fn require() -> Result<()> {
    if !available() {
        bail!("Arriva is uitsluitend beschikbaar in de ontwikkelaarsdemo. / Arriva is only available in the developer demo.")
    }
    Ok(())
}
fn scratch() -> std::path::PathBuf {
    if let Some(p) = SCRATCH_OVERRIDE.get() {
        return p.clone();
    }
    if crate::demo::is_demo() {
        return std::env::temp_dir().join(format!("OVRefund-arriva-demo{}", crate::demo::instance_suffix()));
    }
    // Real claims: the browser profile of the Arriva window, removed by "forget everything".
    crate::paths::data_dir().join("arriva").join("work")
}
fn navigation(url: &tauri::Url) -> bool {
    url.as_str() == "about:blank" || (url.scheme() == "https" && url.host_str() == Some("klantenservice.arriva.nl"))
}
// Reads (GET/HEAD/OPTIONS) are allowed to any https host (and about/data/blob URLs); writes only
// to the narrow allowlist below, which deliberately also stops alternate or changed submission URLs.
fn allow_request(method: &str, url: &str) -> bool {
    let Ok(u) = tauri::Url::parse(url) else { return false };
    if !matches!(u.scheme(), "https" | "about" | "data" | "blob") {
        return false;
    }
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        return true;
    }
    if u.scheme() != "https" {
        return false;
    }
    let h = u.host_str().unwrap_or("");
    let p = u.path();
    (matches!(h, "google.com" | "www.google.com" | "recaptcha.net" | "www.recaptcha.net" | "recaptcha.google.com")
        && (p.starts_with("/recaptcha/") || p.starts_with("/recaptcha/enterprise/")))
}
/// A write to Google's reCAPTCHA (never carries the claim). Test only: [`allow_request`] lets no other
/// write through, so the claim window sends nothing but these, the upload and the one final write.
#[cfg(test)]
fn is_captcha(url: &str) -> bool {
    tauri::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && matches!(
                u.host_str(),
                Some("google.com" | "www.google.com" | "recaptcha.net" | "www.recaptcha.net" | "recaptcha.google.com")
            )
            && u.path().starts_with("/recaptcha/")
    })
}
/// HubSpot's attachment upload (a write that carries the attachments), by its exact path.
fn is_upload(url: &str) -> bool {
    tauri::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https" && u.port().is_none() && u.username().is_empty() && u.password().is_none()
            && matches!(u.host_str(), Some("forms-eu1.hsforms.com" | "forms.hsforms.com" | "api-eu1.hubspot.com"))
            && u.path() == "/submissions-files/v1/upload/files"
    })
}
fn is_attachment_write(method: &str, url: &str) -> bool {
    method == "POST" && is_upload(url)
}
/// Whether the claim window may upload the user's attachments: only when this build may send the
/// claim (not in a dry run, a test build or a developer build without explicit permission).
pub(crate) fn claim_upload_allowed(method: &str, url: &str, may_send: bool) -> bool {
    !is_attachment_write(method, url) || may_send
}
/// The form's final submission request (HubSpot "submit"), as opposed to validation or upload calls.
fn is_final_submit(url: &str) -> bool {
    tauri::Url::parse(url).is_ok_and(|u| {
        if u.scheme() != "https" || u.port().is_some() || !u.username().is_empty() || u.password().is_some()
            || u.host_str() != Some("forms-eu1.hsforms.com")
        {
            return false;
        }
        let Some(path) = u.path().strip_prefix("/submissions/v3/public/submit/formsnext/multipart/") else { return false };
        path.split_once('/').is_some_and(|(portal, form)| {
            !portal.is_empty() && portal.bytes().all(|b| b.is_ascii_digit())
                && !form.is_empty() && form.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    })
}
/// CDP can omit the blob bytes, while still exposing the multipart headers and delimiters.
fn multipart_attachments(request: &Value, expected: &[Attachment]) -> Option<(Vec<usize>, Vec<String>)> {
    use base64::Engine;
    let content_type = request["headers"].as_object()?.iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))?.1.as_str()?;
    let (mime, params) = content_type.split_once(';')?;
    if !mime.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    let boundary = params.split(';').find_map(|param| param.trim().strip_prefix("boundary="))?.trim_matches('"');
    if boundary.is_empty() || boundary.len() > 70 || boundary.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let delimiter = format!("--{boundary}");
    let next = format!("\r\n{delimiter}");
    let encoded = request["postDataEntries"].as_array().filter(|entries| !entries.is_empty());
    let mut omitted = Vec::new();
    let bytes = if let Some(entries) = encoded {
        let mut bytes = Vec::new();
        for entry in entries {
            if let Some(data) = entry["bytes"].as_str() {
                base64::engine::general_purpose::STANDARD.decode_vec(data, &mut bytes).ok()?;
            } else {
                omitted.push(bytes.len());
            }
        }
        bytes
    } else {
        request["postData"].as_str()?.as_bytes().to_vec()
    };
    let mut rest = bytes.strip_prefix(delimiter.as_bytes())?;
    let mut files = Vec::new();
    let mut fields = Vec::new();
    while let Some(part) = rest.strip_prefix(b"\r\n") {
        let header_end = find_bytes(part, b"\r\n\r\n")?;
        let header = std::str::from_utf8(&part[..header_end]).ok()?;
        let body = &part[header_end + 4..];
        let end = find_bytes(body, next.as_bytes())?;
        let disposition = header.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("Content-Disposition").then_some(value)
        })?;
        if let Some(filename) = disposition.split(';').find_map(|s| s.trim().strip_prefix("filename=")) {
            let filename = filename.strip_prefix('"')?.strip_suffix('"')?;
            let index = expected.iter().position(|file| {
                let omitted_blob = end == 0 && omitted.contains(&(bytes.len() - body.len()));
                file.name == filename && (end == file.size || omitted_blob || end == 0 && encoded.is_none())
            })?;
            if files.contains(&index) {
                return None;
            }
            files.push(index);
        } else if let Ok(value) = std::str::from_utf8(&body[..end]) {
            if let Ok(items) = serde_json::from_str::<Vec<String>>(value) {
                fields.extend(items);
            } else {
                fields.push(value.trim().to_string());
            }
        }
        rest = &body[end + next.len()..];
    }
    matches!(rest, b"--" | b"--\r\n").then_some((files, fields))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[derive(Clone)]
pub(crate) struct Bridge {
    pub(crate) window: WebviewWindow,
}
impl Bridge {
    fn demo(&self) -> bool {
        self.window.label() != CLAIM_LABEL
    }
    pub(crate) fn cdp(&self, method: &str, params: Value) -> Result<Value> {
        let (tx, rx) = bounded(1);
        let m = method.to_string();
        let p = params.to_string();
        self.window.with_webview(move |pw| {
            #[cfg(windows)]
            unsafe {
                use webview2_com::{CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR};
                let tx2 = tx.clone();
                let r = (|| -> windows::core::Result<()> {
                    let core = pw.controller().CoreWebView2()?;
                    let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |hr, j| {
                        let _ = tx2
                            .send(hr.map(|_| j).map_err(|e| anyhow!("Browsercommunicatie mislukt. / Browser communication failed. ({e})")));
                        Ok(())
                    }));
                    core.CallDevToolsProtocolMethod(
                        *CoTaskMemPWSTR::from(m.as_str()).as_ref().as_pcwstr(),
                        *CoTaskMemPWSTR::from(p.as_str()).as_ref().as_pcwstr(),
                        &done,
                    )
                })();
                if let Err(e) = r {
                    let _ = tx.send(Err(anyhow!("Browsercommunicatie mislukt. / Browser communication failed. ({e})")));
                }
            }
        })?;
        let raw =
            rx.recv_timeout(Duration::from_secs(60)).map_err(|_| anyhow!("Browser reageert niet. / Browser is not responding."))??;
        Ok(serde_json::from_str(&raw)?)
    }
    pub(crate) fn eval(&self, js: &str) -> Result<Value> {
        let v = self.cdp("Runtime.evaluate", json!({"expression":js,"awaitPromise":true,"returnByValue":true,"userGesture":true}))?;
        if let Some(ex) = v.get("exceptionDetails") {
            if !self.demo() {
                bail!("Arriva voorbereiden mislukt. / Arriva preparation failed.");
            }
            let detail = ex
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| ex.get("text").and_then(Value::as_str))
                .unwrap_or("Formulier reageert niet");
            let detail = detail.lines().next().unwrap_or(detail);
            bail!(
                "Arriva voorbereiden mislukt. / Arriva preparation failed. ({})",
                ovrefund_core::redact::redact(&detail.chars().take(180).collect::<String>())
            )
        }
        Ok(v.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }
    pub(crate) fn response_text(&self, network_id: &str) -> Option<String> {
        use base64::Engine;
        let raw = self.cdp("Network.getResponseBody", json!({"requestId": network_id})).ok()?;
        let body = raw["body"].as_str()?;
        if raw["base64Encoded"] == true {
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(body).ok()?).ok()
        } else {
            Some(body.to_string())
        }
    }

    fn open(app: &AppHandle) -> Result<Self> {
        if let Some(w) = app.get_webview_window(LABEL) {
            return Ok(Self { window: w });
        }
        let b = Self::guarded(app, LABEL, "Arriva — DEMO — verzending geblokkeerd", (1050., 850.), navigation)?;
        note("guard", "done", "Netwerkbewaking actief: onbekende schrijfacties en alle aanvragen geblokkeerd vóór verzending. / Network guard on: unknown writes and every submission blocked before sending.");
        b.window.navigate(URL.parse()?)?;
        Ok(b)
    }
    /// A fresh hidden, guarded window for a real claim, at Arriva's form (an earlier one is closed).
    pub(crate) fn open_claim(app: &AppHandle) -> Result<Self> {
        if let Some(w) = app.get_webview_window(CLAIM_LABEL) {
            let _ = w.close();
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while app.get_webview_window(CLAIM_LABEL).is_some() && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        anyhow::ensure!(
            app.get_webview_window(CLAIM_LABEL).is_none(),
            "Het vorige Arriva-venster is nog open. / The previous Arriva window is still open."
        );
        reset_final_write();
        let b = Self::guarded(app, CLAIM_LABEL, "Arriva", (1050., 850.), navigation)?;
        b.window.navigate(URL.parse()?)?;
        Ok(b)
    }
    /// A hidden window at about:blank with the write guard installed before any page loads.
    /// Navigation is limited by `nav`; new windows are refused.
    fn guarded(app: &AppHandle, label: &str, title: &str, size: (f64, f64), nav: fn(&tauri::Url) -> bool) -> Result<Self> {
        std::fs::create_dir_all(scratch())?;
        let w = WebviewWindowBuilder::new(app, label, WebviewUrl::External("about:blank".parse()?))
            .title(title)
            .inner_size(size.0, size.1)
            .visible(false)
            .data_directory(scratch().join("browser"))
            .additional_browser_args(crate::ns_bridge::BROWSER_ARGS)
            .on_navigation(nav)
            .build()?;
        let b = Self { window: w };
        let installed = (|| -> Result<()> {
            b.listeners()?;
            b.cdp("Network.enable", json!({}))?;
            b.cdp("Fetch.enable", json!({"patterns":[{"urlPattern":"*","requestStage":"Request"}]}))?;
            Ok(())
        })();
        if let Err(e) = installed {
            let _ = b.window.close();
            return Err(e);
        }
        Ok(b)
    }
    fn listeners(&self) -> Result<()> {
        let app = self.window.app_handle().clone();
        let claim_window = self.window.label() == CLAIM_LABEL;
        let (tx, rx) = bounded(1);
        self.window.with_webview(move |pw| {
            #[cfg(windows)]
            unsafe {
                let r = install(pw, app, claim_window);
                let _ = tx.send(r.map_err(|e| anyhow!("Veiligheidsbewaking mislukt. / Safety guard failed. ({e})")));
            }
        })?;
        rx.recv_timeout(Duration::from_secs(20))
            .map_err(|_| anyhow!("Veiligheidsbewaking niet geïnstalleerd. / Safety guard was not installed."))?
    }
    /// Remove the CAPTCHA-only presentation (the live document itself is never changed).
    pub(crate) fn clear_view(&self) -> Result<()> {
        self.eval("window.__ovArrivaObserver?.disconnect();document.getElementById('ov-arriva-captcha')?.remove();document.getElementById('ov-arriva-captcha-note')?.remove();true")?;
        Ok(())
    }
    /// Developer inspection only: the whole live form.
    fn show_form(&self) -> Result<()> {
        self.clear_view()?;
        self.window.set_size(tauri::LogicalSize::new(1050., 850.))?;
        self.window.set_title("Arriva DEMO - volledig formulier - verzending geblokkeerd")?;
        self.window.show()?;
        self.window.set_focus()?;
        Ok(())
    }
    /// Show only the reCAPTCHA challenge (and its badge) with a short status line; the rest of the
    /// page stays in place but invisible, so the form and the CAPTCHA keep working.
    pub(crate) fn captcha_only(&self, nl: &str, en: &str) -> Result<()> {
        let text = serde_json::to_string(&if self.demo() { format!("DEMO - {nl} / {en}") } else { format!("{nl} / {en}") })?;
        let applied = self.eval(&format!(
            r#"(() => {{
          let style=document.getElementById('ov-arriva-captcha');
          if(!style){{style=document.createElement('style');style.id='ov-arriva-captcha';document.head.append(style);}}
          style.textContent=`html,body{{background:#fff!important;overflow:hidden!important}}
            body *{{visibility:hidden!important}}
            body iframe[src*="bframe"],#ov-arriva-captcha-note,#ov-arriva-captcha-note *{{visibility:visible!important}}
            body>div:has(iframe[src*="bframe"]) div:has(> iframe[src*="bframe"]){{position:fixed!important;top:76px!important;left:50%!important;transform:translateX(-50%)!important}}`;
          scrollTo(0,0);
          let note=document.getElementById('ov-arriva-captcha-note');
          if(!note){{note=document.createElement('p');note.id='ov-arriva-captcha-note';document.body.append(note);}}
          note.style.cssText='position:fixed;top:0;left:0;right:0;margin:0;padding:12px 16px;font:14px/1.45 "Segoe UI",sans-serif;background:#fff2ce;color:#292929;border-bottom:1px solid #dfb850;z-index:2147483647;visibility:visible';
          note.textContent={text};
          const small=document.createElement('small');small.style.cssText='display:block;margin-top:4px;color:#5b5b5b';
          small.textContent='Beveiligd met reCAPTCHA (Google) / Protected by reCAPTCHA (Google)';note.append(small);
          return true;
        }})()"#
        ))?;
        anyhow::ensure!(applied == true, "Puzzelweergave kon niet worden voorbereid. / The puzzle view could not be prepared.");
        self.window.set_size(tauri::LogicalSize::new(460., 720.))?;
        self.window.set_title(if self.demo() { "Arriva DEMO - CAPTCHA - verzending geblokkeerd" } else { "Arriva - CAPTCHA" })?;
        Ok(())
    }
    pub(crate) fn show(&self) -> Result<()> {
        self.window.show()?;
        self.window.set_focus()?;
        Ok(())
    }
}
#[cfg(windows)]
unsafe fn install(pw: tauri::webview::PlatformWebview, app: AppHandle, claim_window: bool) -> windows::core::Result<()> {
    let claim_generation = claim_window.then(|| final_write().generation);
    use webview2_com::{
        CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR, DevToolsProtocolEventReceivedEventHandler,
        NewWindowRequestedEventHandler,
    };
    let core = pw.controller().CoreWebView2()?;
    let h = NewWindowRequestedEventHandler::create(Box::new(|_, args| {
        if let Some(a) = args {
            a.SetHandled(true)?
        }
        Ok(())
    }));
    let mut t = Default::default();
    core.add_NewWindowRequested(&h, &mut t)?;
    let receiver = core.GetDevToolsProtocolEventReceiver(*CoTaskMemPWSTR::from("Fetch.requestPaused").as_ref().as_pcwstr())?;
    let h = DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |sender, args| {
        let Some(args) = args else { return Ok(()) };
        let mut p = windows::core::PWSTR::null();
        args.ParameterObjectAsJson(&mut p)?;
        let s = CoTaskMemPWSTR::from(p).to_string();
        let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
        let Some(rid) = v["requestId"].as_str() else { return Ok(()) };
        let method = v["request"]["method"].as_str().unwrap_or("");
        let url = v["request"]["url"].as_str().unwrap_or("");
        let final_submit = method == "POST" && is_final_submit(url);
        let upload = is_attachment_write(method, url);
        let allowed = if claim_window && (upload || final_submit) {
            FINAL_WRITE.lock().unwrap().as_mut()
                .filter(|f| Some(f.generation) == claim_generation)
                .is_some_and(|f| f.request_allowed(&v, true, final_write_possible()))
        } else {
            allow_request(method, url)
        };
        if upload && !claim_window {
            DEMO_UPLOADS_BLOCKED.fetch_add(1, Ordering::SeqCst);
        }
        let is_claim = v["request"]["url"].as_str().and_then(|u| tauri::Url::parse(u).ok()).is_some_and(|u| {
            u.host_str().is_some_and(|h| h == "klantenservice.arriva.nl" || h.ends_with(".hsforms.com") || h.ends_with(".hubspot.com"))
        });
        // Developer diagnostics: page-supplied URLs can contain personal data even in the path.
        if (!allowed || v["request"]["method"] == "POST")
            && std::env::var("OVREFUND_ARRIVA_GUARD_LOG").as_deref() == Ok("1")
        {
            let verdict = if allowed { "allowed" } else { "blocked" };
            let kind = if final_submit { "submit" } else if upload { "upload" } else { "other" };
            eprintln!("guard {verdict} {kind}");
        }
        if !allowed && is_claim && !claim_window {
            BLOCKED.fetch_add(1, Ordering::SeqCst);
            note("guard", "done", "Uitgaande schrijfactie door DEMO geblokkeerd vóór netwerkverkeer. De claim is niet ingediend en er is niets naar Arriva geüpload. / Outgoing write blocked by the DEMO before the network. The claim was not submitted and nothing was uploaded to Arriva.");
            // Only the form submission itself is the final write (not field validation calls).
            if final_submit {
                // While Indienen is being handled the outcome is reported by request_verification;
                // a later write (after a solved puzzle) makes the outcome final here.
                let mut s = STATE.lock().unwrap();
                s["final_write_blocked"] = json!(true);
                if s["phase"] != "verifying" {
                    s["phase"] = json!("submission_blocked");
                }
            }
            let _ = app.emit("arriva-demo-updated", ());
        }
        if let Some(core) = sender {
            let params =
                if allowed { json!({"requestId":rid}) } else { json!({"requestId":rid,"errorReason":"BlockedByClient"}) }.to_string();
            let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
            core.CallDevToolsProtocolMethod(
                *CoTaskMemPWSTR::from(if allowed { "Fetch.continueRequest" } else { "Fetch.failRequest" }).as_ref().as_pcwstr(),
                *CoTaskMemPWSTR::from(params.as_str()).as_ref().as_pcwstr(),
                &done,
            )?;
        }
        Ok(())
    }));
    let mut t = Default::default();
    receiver.add_DevToolsProtocolEventReceived(&h, &mut t)?;
    if claim_window {
        // The response (or failure) of the final write that went out.
        for event in ["Network.responseReceived", "Network.loadingFinished", "Network.loadingFailed"] {
            let receiver = core.GetDevToolsProtocolEventReceiver(*CoTaskMemPWSTR::from(event).as_ref().as_pcwstr())?;
            let h = DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = windows::core::PWSTR::null();
                args.ParameterObjectAsJson(&mut p)?;
                let v: Value = serde_json::from_str(&CoTaskMemPWSTR::from(p).to_string()).unwrap_or(Value::Null);
                let mut f = FINAL_WRITE.lock().unwrap();
                if let Some(f) = f.as_mut().filter(|f| Some(f.generation) == claim_generation) {
                    f.network_event(event, &v);
                }
                Ok(())
            }));
            let mut t = Default::default();
            receiver.add_DevToolsProtocolEventReceived(&h, &mut t)?;
        }
    }
    Ok(())
}
/// Renders an evidence card in a hidden, offline window and captures it as PNG (2x for sharp text).
/// `margin`: CSS pixels around `#card` (0 for a strip that must match another image's width).
fn render_png(app: &AppHandle, html: &str, margin: f64) -> Result<Vec<u8>> {
    if let Some(w) = app.get_webview_window(PROOF_LABEL) {
        let _ = w.close();
        std::thread::sleep(Duration::from_millis(300));
    }
    let b = Bridge::guarded(app, PROOF_LABEL, "OV Refund bewijs", (900., 1100.), proof_navigation)?;
    let r = (|| -> Result<Vec<u8>> {
        b.eval(&format!(
            "(async()=>{{document.open();document.write({});document.close();await document.fonts.ready;await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));return true}})()",
            serde_json::to_string(html)?
        ))?;
        let rect = b.eval(&format!("(()=>{{const r=document.getElementById('card').getBoundingClientRect();return {{x:r.x+scrollX-{m},y:r.y+scrollY-{m},width:r.width+2*{m},height:r.height+2*{m}}}}})()", m = margin))?;
        // Exactly 2 image pixels per CSS pixel whatever the display scaling (the clip scale is applied
        // on top of devicePixelRatio), so a caption strip of width w/2 matches an image of width w.
        let dpr = b.eval("devicePixelRatio")?.as_f64().filter(|d| *d > 0.).unwrap_or(1.);
        let png = b.cdp(
            "Page.captureScreenshot",
            json!({"format":"png","captureBeyondViewport":true,"clip":{"x":rect["x"],"y":rect["y"],"width":rect["width"],"height":rect["height"],"scale":2. / dpr}}),
        )?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(png["data"].as_str().unwrap_or(""))?;
        anyhow::ensure!(bytes.starts_with(b"\x89PNG"), "Geen afbeelding gemaakt. / No image was captured.");
        Ok(bytes)
    })();
    let _ = b.window.close();
    r
}
/// The demo's real train: Arriva 732284 on 29-10-2026, boarded in Venlo (VL), left in Nijmegen (NM).
const RDT_DATE: &str = "2026-10-29";
const RDT_TRAIN: &str = "732284";
const RDT_BOARD: &str = "VL";
const RDT_ALIGHT: &str = "NM";
/// The fictional demo card (also typed into the form).
pub(crate) const DEMO_CARD: &str = "3528000000001234";
const TRAVEL_FILE: &str = "DEMO-reisbewijs-Arriva-732284.png";
const DELAY_FILE: &str = "vertragingsbewijs-RDT-732284.png";
/// The delay record of the demo train, downloaded now from Rijden de Treinen open data. On failure the
/// bundled copy of the same record is used and the card says the download failed.
struct DelayRecord {
    rows: Vec<Value>,
    source: crate::proof_cards::RdtSource,
    downloaded: bool,
}
fn fetch_delay_record() -> DelayRecord {
    let date = chrono::NaiveDate::parse_from_str(RDT_DATE, "%Y-%m-%d").expect("valid date");
    let month = ovrefund_core::archive::month_key(date);
    let url = ovrefund_core::archive::month_url(&month);
    let dataset = format!("services-{month}.csv.gz");
    match ovrefund_core::archive::service_record(date, RDT_TRAIN) {
        Ok(Some((rows, m))) => DelayRecord {
            rows,
            source: crate::proof_cards::RdtSource {
                dataset,
                url: m.url,
                retrieved: m.retrieved.with_timezone(&chrono::Local).format("%d-%m-%Y %H:%M:%S").to_string(),
                file_version: m.last_modified,
            },
            downloaded: true,
        },
        other => {
            let why = match other {
                Err(e) => format!("{e:#}"),
                _ => "record niet gevonden / record not found".into(),
            };
            log::warn!("demo delay record download failed: {}", ovrefund_core::redact::redact(&why));
            DelayRecord {
                rows: serde_json::from_str(RAW).expect("bundled record"),
                source: crate::proof_cards::RdtSource {
                    dataset,
                    url,
                    retrieved:
                        "download mislukt / download failed (meegeleverde kopie van dit record getoond / bundled copy of this record shown)"
                            .into(),
                    file_version: None,
                },
                downloaded: false,
            }
        }
    }
}
/// Arrival delay at the alighting station in the record.
fn arrival_delay(rows: &[Value], station: &str) -> Option<i64> {
    rows.iter().find(|r| r["Stop:Station code"] == station).and_then(|r| r["Stop:Arrival delay"].as_str()?.parse().ok())
}
/// A travel-history image with the caption strip under it: `#card` of the strip is exactly as wide as
/// the image (2x capture), so the image pixels are placed unchanged above the strip.
pub fn travel_proof_png(app: &AppHandle, screenshot: &[u8], caption: &crate::proof_cards::Caption) -> Result<Vec<u8>> {
    anyhow::ensure!(screenshot.len() > 24 && screenshot.starts_with(b"\x89PNG"), "Geen schermafbeelding. / No screenshot.");
    let width_px = u32::from_be_bytes(screenshot[16..20].try_into()?);
    let strip = render_png(app, &crate::proof_cards::caption_strip(caption, width_px.div_ceil(2)), 0.)?;
    crate::proof_cards::stack_png(screenshot, &strip)
}
/// The two attachments of the demo claim: the travel proof (a simulated history row with demo data and
/// the caption strip) and the delay proof (the public Rijden de Treinen record of the train, fetched
/// now). Both PNG, generated locally.
fn evidence(app: &AppHandle) -> Result<Vec<std::path::PathBuf>> {
    let dir = scratch();
    std::fs::create_dir_all(&dir)?;
    let (travel, delay, rec) = demo_pngs(app)?;
    let tp = dir.join(TRAVEL_FILE);
    let dp = dir.join(DELAY_FILE);
    std::fs::write(&tp, &travel)?;
    std::fs::write(&dp, &delay)?;
    {
        let mut s = STATE.lock().unwrap();
        s["attachments"] = json!([
            {"name":"Reisbewijs: reishistorie-rij met OV-chipkaartnummer (demogegevens) / Travel proof: travel-history row with OV-chipkaart number (demo data)","file":TRAVEL_FILE,"path":tp,"kind":"png","size":travel.len()},
            {"name":"Vertragingsbewijs: Rijden de Treinen open data (CC BY 4.0) / Delay proof: Rijden de Treinen open data (CC BY 4.0)","file":DELAY_FILE,"path":dp,"kind":"png","size":delay.len()}
        ]);
        s["rdt"] = json!({"downloaded": rec.downloaded, "retrieved": rec.source.retrieved, "url": rec.source.url});
        if let Some(d) = arrival_delay(&rec.rows, RDT_ALIGHT) {
            s["trip"]["delay_minutes"] = json!(d);
            if let Some(t) = s["demo_trips"].as_array_mut().and_then(|a| a.iter_mut().find(|t| t["id"] == "DEMO-ARRIVA-732284")) {
                *t = demo_trip_status("DEMO-ARRIVA-732284", d);
            }
        }
    }
    let msg = if rec.downloaded {
        format!(
            "Twee bijlagen gemaakt: reisbewijs (demogegevens) en vertragingsbewijs uit Rijden de Treinen open data, opgehaald {0}. / Two attachments made: travel proof (demo data) and delay proof from Rijden de Treinen open data, retrieved {0}.",
            rec.source.retrieved
        )
    } else {
        "Twee bijlagen gemaakt. Download bij Rijden de Treinen mislukt: vertragingsbewijs uit de meegeleverde kopie van het record (staat op de kaart). / Two attachments made. Download from Rijden de Treinen failed: delay proof from the bundled copy of the record (stated on the card).".into()
    };
    note("evidence", "done", &msg);
    Ok(vec![tp, dp])
}
/// The demo trip's two attachments as PNG (travel proof with demo data, delay card of the real public
/// record), also used by the demo's real Claim flow (`arriva_claim`).
pub(crate) fn demo_attachment_pngs(app: &AppHandle) -> Result<(Vec<u8>, Vec<u8>)> {
    demo_pngs(app).map(|(t, d, _)| (t, d))
}
fn demo_pngs(app: &AppHandle) -> Result<(Vec<u8>, Vec<u8>, DelayRecord)> {
    let j = demo_journey();
    let tz = chrono::FixedOffset::east_opt(3600).unwrap();
    let date = j.check_in.with_timezone(&tz).format("%d-%m-%Y").to_string();
    let to = j.destination.as_ref().map(|d| d.name.clone()).unwrap_or_default();
    let trip = crate::proof_cards::TravelTrip {
        date: date.clone(),
        from: j.origin.name.clone(),
        to: to.clone(),
        check_in: j.check_in.with_timezone(&tz),
        check_out: j.check_out.unwrap_or(j.check_in).with_timezone(&tz),
        operator: j.carrier_name.clone().unwrap_or_default(),
        product: "Saldo (fictief) / pay-as-you-go (fictional)".into(),
        travel_class: j.travel_class,
        fare_cents: j.fare_cents,
        source: "Demogegevens (fictief), geen echte reis of OV-chipkaart / Demo data (fictional), not a real trip or card".into(),
        demo: true,
    };
    let row = render_png(app, &crate::proof_cards::demo_history_row(&trip), 12.)?;
    let caption = crate::proof_cards::Caption {
        card_number: Some(DEMO_CARD.into()),
        date,
        route: format!("{} \u{2013} {}", j.origin.name, to),
        demo: true,
    };
    let travel = travel_proof_png(app, &row, &caption)?;
    note(
        "evidence",
        "running",
        "Vertragingsrecord ophalen bij Rijden de Treinen (open data). / Fetching the delay record from Rijden de Treinen (open data).",
    );
    let rec = fetch_delay_record();
    let delay = render_png(app, &crate::proof_cards::delay_card(&rec.rows, RDT_BOARD, RDT_ALIGHT, &rec.source)?, 12.)?;
    Ok((travel, delay, rec))
}
const HELPERS: &str = r#"
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const visible=e=>!!e&&e.getClientRects().length>0;
const wait=async f=>{for(let i=0;i<100;i++){const x=f();if(x)return x;await sleep(200)}throw Error('Formulier niet gereed')};
const set=(e,v)=>{if(!e)throw Error('Veld ontbreekt');const p=e instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(e,v);e.dispatchEvent(new Event('input',{bubbles:true}));e.dispatchEvent(new Event('change',{bubbles:true}));e.dispatchEvent(new Event('blur',{bubbles:true}))};
const field=(n,v)=>set(document.getElementsByName(n)[0],v);
const combo=async(i,text)=>{for(let k=0;k<60;k++){const e=[...document.querySelectorAll('input[role=combobox]')].filter(visible)[i];if(e){if(e.value.trim()===text)return;if(e.getAttribute('aria-expanded')!=='true')e.click();await sleep(200);const list=document.getElementById(e.getAttribute('aria-controls'));const o=list&&[...list.querySelectorAll('[role=option]')].find(x=>x.textContent.trim()===text);if(o){o.click();await sleep(200);if([...document.querySelectorAll('input[role=combobox]')].filter(visible)[i]?.value.trim()===text)return;}}await sleep(200)}throw Error('Keuzelijst niet gereed: '+text)};
const next=async()=>{const b=await wait(()=>[...document.querySelectorAll('button')].find(x=>visible(x)&&x.textContent.trim()==='Volgende'&&!x.disabled));b.click();await sleep(700)};
const combos=()=>[...document.querySelectorAll('input[role=combobox]')].filter(visible).map(e=>e.value.trim());
const value=n=>(document.getElementsByName(n)[0]?.value??'').replace(/\s+/g,'');
const ns=x=>String(x??'').replace(/\s+/g,'');
const required=e=>!!e&&(e.required||e.getAttribute('aria-required')==='true');
const noErrors=()=>![...document.querySelectorAll('[aria-invalid=true],.hsfc-ErrorAlert')].some(visible);
const submitButton=()=>[...document.querySelectorAll('button,input[type=submit]')].find(e=>visible(e)&&/^(Indienen|Verzenden)$/.test((e.textContent||e.value||'').trim()));
const submitReady=()=>{const e=submitButton();return !!e&&!e.disabled&&e.getAttribute('aria-disabled')!=='true'};
const verify=checks=>{const unverified=Object.keys(checks).filter(k=>!checks[k]);return {verified:Object.keys(checks).length-unverified.length,unverified}};
const legalBoxes=()=>[...document.querySelectorAll('input[type=checkbox]')].filter(e=>e.name.includes('LEGAL_CONSENT'));
const consentName='LEGAL_CONSENT.subscription_type_3667673841';
const consentBox=()=>{const known=legalBoxes().filter(e=>e.name===consentName);return known.length===1?known[0]:null;};
const consentOk=(consent,demo)=>legalBoxes().every(e=>e===consent? e.checked===(demo||required(e)) : !e.checked&&!required(e));
const emptyRequired=()=>[...document.querySelectorAll('input,select,textarea')].filter(e=>visible(e)&&required(e)&&e.type!=='hidden'&&(e.type==='checkbox'||e.type==='radio'?![...document.getElementsByName(e.name)].some(x=>x.checked):!String(e.value??'').trim()));
"#;

/// Everything typed or chosen in Arriva's form. The demo uses synthetic data ([`demo_form`]); a real
/// claim uses the traveller's own details (`arriva_claim`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct FormData {
    /// Region exactly as in the form's list (carrier policy).
    pub region: String,
    /// Delay choice exactly as in the form's list (carrier policy).
    pub delay_option: String,
    /// Full OV-chipkaart number, digits only.
    pub card: String,
    /// IBAN without spaces.
    pub iban: String,
    /// The card's expiry date (year, month 1-12, day); filled only when known.
    pub valid_until: Option<(i32, u32, u32)>,
    pub departure_station: String,
    pub arrival_station: String,
    /// Planned departure, "DD-MM-YYYY HH:MM".
    pub travel_time: String,
    /// Fare as the form takes it: "12,40".
    pub fare: String,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    /// Demo only: the consent box is ticked for the blocked test. A real claim ticks it only when the
    /// form requires it (it is then a condition of the claim the user asked for), never otherwise.
    pub demo: bool,
}
/// The synthetic demo claim (Venlo - Nijmegen, train 732284), with the region and delay choice from
/// Arriva's rules and the trip's recorded delay.
pub(crate) fn demo_form(region: &str, delay: i64) -> Result<FormData> {
    let option = crate::carrier_policy::ARRIVA
        .form_delay_option(delay.max(0) as u32)
        .ok_or_else(|| step_error("page 2: trip", &format!("geen keuze voor +{delay} min / no form choice for +{delay} min")))?;
    Ok(FormData {
        region: region.into(),
        delay_option: option.into(),
        card: DEMO_CARD.into(),
        iban: "NL96TEST0000000000".into(),
        valid_until: Some((2030, 1, 1)),
        departure_station: "Venlo".into(),
        arrival_station: "Nijmegen".into(),
        travel_time: "29-10-2026 23:29".into(),
        fare: "12,40".into(),
        first_name: "Demo".into(),
        last_name: "Testreiziger".into(),
        email: "ovrefund-demo@example.invalid".into(),
        demo: true,
    })
}
fn current_demo_form() -> Result<FormData> {
    let (region, delay) = {
        let s = STATE.lock().unwrap();
        (s["trip"]["region"].as_str().unwrap_or_default().to_string(), s["trip"]["delay_minutes"].as_i64().unwrap_or(0))
    };
    demo_form(&region, delay)
}
fn form_js(f: &FormData) -> String {
    format!("const D={};", serde_json::to_string(f).unwrap_or_else(|_| "{}".into()))
}
/// The fields of the last page (still on screen when the CAPTCHA is requested) and the submit button.
fn final_check_js(f: &FormData) -> String {
    format!("{}{FINAL_CHECK}", form_js(f))
}
/// [`final_check_js`] without the data line (for a script that already declares `D`).
const FINAL_CHECK: &str = r#"
const consent=consentBox();
return verify({first_name:value('0-1/firstname')===ns(D.first_name),last_name:value('0-1/lastname')===ns(D.last_name),email:value('0-1/email')===ns(D.email),consent:consentOk(consent,D.demo),no_form_errors:noErrors(),no_other_required:emptyRequired().length===0,submit_button:submitReady()});
"#;
fn script(b: &Bridge, body: &str) -> Result<Value> {
    b.eval(&format!("(async()=>{{{HELPERS}{body}}})()"))
}
/// One named form stage. The page script fills its fields and returns `verify(...)`; a script error
/// or any field that does not read back as filled fails the stage with its name and the field names.
fn stage(b: &Bridge, name: &str, body: &str) -> Result<u64> {
    let v = script(b, body).map_err(|e| step_error(name, &e.to_string()))?;
    let unverified: Vec<String> =
        v["unverified"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if v["unverified"].as_array().is_none() {
        return Err(step_error(name, "geen controle-uitslag / no verification result"));
    }
    if b.demo() {
        STATE.lock().unwrap()["unverified"] = json!(unverified);
    }
    if v["expiry_required"] == true && unverified.iter().any(|s| s == "valid_until") {
        bail!("Arriva vraagt om de vervaldatum van deze OV-chipkaart. Vul die in bij Jouw gegevens als daar deze kaart staat, of dien de claim zelf in bij Arriva. / Arriva requires the expiry date of this OV-chipkaart. Enter it under Your details if that section shows this card, or submit the claim yourself at Arriva.");
    }
    if !unverified.is_empty() {
        let list = unverified.join(", ");
        return Err(step_error(name, &format!("velden niet bevestigd: {list} / fields not verified: {list}")));
    }
    Ok(v["verified"].as_u64().unwrap_or(0))
}
/// "Arriva-formulier, stap 'X': <nl> / Arriva form, step 'X': <en>" from a bilingual or plain detail.
fn step_error(name: &str, detail: &str) -> anyhow::Error {
    let (nl, en) = detail.split_once(" / ").unwrap_or((detail, detail));
    anyhow!("Arriva-formulier, stap '{name}': {nl} / Arriva form, step '{name}': {en}")
}
/// Fills the three form pages with `f` and the attachments, verifying every field before moving on.
/// Returns (stage, fields verified) per stage. Never presses Indienen.
pub(crate) fn populate(b: &Bridge, files: &[std::path::PathBuf], f: &FormData) -> Result<Vec<(&'static str, u64)>> {
    let demo = b.demo();
    if demo {
        note("form", "running", "Arriva-pagina laden; daarna kaart, reis en bijlage invullen. / Loading Arriva's page; then filling in card, trip and attachment.");
        let mut s = STATE.lock().unwrap();
        s["verified_fields"] = json!(0);
        s["unverified"] = json!([]);
    }
    // Navigation is asynchronous. Never start a long script in about:blank:
    // its execution context is destroyed when the actual page arrives.
    let until = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        if b.eval("location.hostname === 'klantenservice.arriva.nl' && !!document.querySelector('input[role=combobox]')")
            .unwrap_or(Value::Bool(false))
            == true
        {
            break;
        }
        if std::time::Instant::now() >= until {
            return Err(step_error(
                "load form",
                "Arriva-formulier kon niet worden geladen. Probeer het opnieuw. / Could not load the Arriva form. Please try again.",
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut done = vec![];
    let data = form_js(f);
    let page1 = r#"
await wait(()=>[...document.querySelectorAll('input[role=combobox]')].filter(visible).length>=3);
await combo(0,'Geld terug bij vertraging');await combo(1,D.region);await combo(2,'OV-chipkaart');
field('0-5/ovchipcard_number',D.card);field('0-5/iban',D.iban);
const picker=await wait(()=>document.querySelector("input[placeholder='DD - MM - YYYY']")?._flatpickr);
if(D.valid_until)picker.setDate(new Date(D.valid_until[0],D.valid_until[1]-1,D.valid_until[2]),true);
await sleep(300);const c=combos();const d=picker.selectedDates?.[0];
const expiry_required=required(picker.input)&&!D.valid_until;
const dateOk=D.valid_until?(!!d&&d.getFullYear()===D.valid_until[0]&&d.getMonth()===D.valid_until[1]-1&&d.getDate()===D.valid_until[2]):!d&&!expiry_required;
return {...verify({subject:c[0]==='Geld terug bij vertraging',region:c[1]===D.region,ticket:c[2]==='OV-chipkaart',card_number:value('0-5/ovchipcard_number')===ns(D.card),iban:value('0-5/iban')===ns(D.iban),valid_until:dateOk}),expiry_required};
"#;
    done.push(("page 1: ticket and card", stage(b, "page 1: ticket and card", &format!("{data}{page1}"))?));
    // The generated local files are handed to the page in one selection.
    // Demo, form test, view test and eval never upload: the field is checked locally (present, accepts
    // the files as one selection), the selection is removed again without telling the page, and the
    // guard also blocks any upload request (see `install`). The same in a build or run that may not
    // send a claim (the demo's claim flow, a dry run).
    if demo || !final_write_possible() {
        let files_js = local_files_js(files)?;
        let v = b
            .eval(&format!("(async(F)=>{{{HELPERS}{LOCAL_ATTACHMENT_CHECK}}})({files_js})"))
            .map_err(|e| step_error("page 1: attachment", &e.to_string()))?;
        let ok = v["field"] == true && v["accepts"] == true && v["selected"].as_u64() == Some(files.len() as u64);
        if !ok {
            return Err(step_error(
                "page 1: attachment",
                "bijlageveld ontbreekt of neemt de bijlagen niet aan / attachment field missing or does not accept the attachments",
            ));
        }
        let r = stage(b, "page 1: attachment", LOCAL_ATTACHMENT_NEXT)?;
        done.push(("page 1: attachment", r));
    } else {
        let (local_files, attachments) = local_files(files)?;
        let file_list = json!(attachments.iter().map(|file| (&file.name, file.size)).collect::<Vec<_>>());
        {
            let mut state = FINAL_WRITE.lock().unwrap();
            let guard = state.get_or_insert_with(FinalWrite::default);
            guard.attachments = attachments;
            guard.uploads_open = true;
        }
        let upload = (|| -> Result<u64> {
            let n = b.eval(&format!(
                "(()=>{{const input=document.querySelector('input[type=file]');if(!input)return -1;const dt=new DataTransfer();\
                 for(const f of {local_files}){{const s=atob(f.b64),u=new Uint8Array(s.length);for(let i=0;i<s.length;i++)u[i]=s.charCodeAt(i);dt.items.add(new File([u],f.name,{{type:'image/png'}}))}}\
                 input.files=dt.files;const n=input.files.length;if(n===dt.files.length)input.dispatchEvent(new Event('change',{{bubbles:true}}));return n}})()"
            ))?;
            if n.as_i64() == Some(-1) {
                bail!("Bijlageveld ontbreekt. / Attachment field is missing.");
            }
            anyhow::ensure!(n.as_u64() == Some(files.len() as u64), "Niet alle bijlagen gekozen. / Not all attachments were selected.");
            // For an inline multipart submit the files stay selected. For separate uploads,
            // wait for completion for each reviewed file, even when their requests finish separately.
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let selected = b.eval(&format!(
                    "(()=>{{const files=[...(document.querySelector('input[type=file]')?.files??[])];return files.length===2&&{file_list}.every(([name,size])=>files.some(f=>f.name===name&&f.size===size))}})()"
                ))? == true;
                for id in final_write().pending_upload_responses() {
                    let body = b.response_text(id);
                    if let Some(guard) = FINAL_WRITE.lock().unwrap().as_mut() {
                        guard.check_upload_response(id, body.as_deref());
                    }
                }
                if final_write().attachments_ready(selected) {
                    break;
                }
                if Instant::now() >= deadline {
                    bail!("Beide bijlagen konden niet worden bevestigd. Kies Claimen om het opnieuw te proberen. / Both attachments could not be verified. Choose Claim to try again.");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            stage(b, "page 1: attachment", "const r=verify({attachment:true,no_form_errors:noErrors()});if(!r.unverified.length)await next();return r;")
        })();
        done.push(("page 1: attachment", upload?));
    }
    let page2 = r#"
await wait(()=>[...document.querySelectorAll('input[role=combobox]')].some(visible));
await combo(0,'Trein');await wait(()=>document.getElementsByName('0-5/departure_station')[0]);field('0-5/departure_station',D.departure_station);field('0-5/arrival_station',D.arrival_station);field('0-5/datetime_of_travel_string',D.travel_time);
const no=[...document.getElementsByName('0-5/has_studentenreisproduct_boolean')].find(e=>e.id.endsWith('-1'));if(!no)throw Error('Studentenvraag ontbreekt');no.click();
const fare=document.querySelector('input[id$="3255747934242-input"]');set(fare,D.fare);await combo(1,D.delay_option);await sleep(300);
const c=combos();
const r=verify({transport:c[0]==='Trein',departure_station:value('0-5/departure_station')===ns(D.departure_station),arrival_station:value('0-5/arrival_station')===ns(D.arrival_station),travel_time:value('0-5/datetime_of_travel_string')===ns(D.travel_time),student_product:no.checked,fare:(fare?.value??'').trim().replace('.',',')===D.fare,delay:c[1]===D.delay_option,no_form_errors:noErrors()});
if(!r.unverified.length)await next();return r;
"#;
    done.push(("page 2: trip", stage(b, "page 2: trip", &format!("{data}{page2}"))?));
    let page3 = format!(
        r#"{data}
await wait(()=>document.getElementsByName('0-1/firstname')[0]);
field('0-1/firstname',D.first_name);field('0-1/lastname',D.last_name);field('0-1/email',D.email);
const c0=consentBox();if(c0&&c0.checked!==(D.demo||required(c0)))c0.click();
await sleep(1500);
try{{await wait(submitReady)}}catch{{}}
{FINAL_CHECK}"#
    );
    done.push(("page 3: contact", stage(b, "page 3: contact", &page3)?));
    if demo {
        STATE.lock().unwrap()["verified_fields"] = json!(done.iter().map(|(_, n)| n).sum::<u64>());
        note("form","done","Officieel Arriva-formulier ingevuld met synthetische gegevens; de twee gegenereerde bijlagen alleen lokaal in het bijlageveld gecontroleerd (niet geüpload); elk veld gecontroleerd. Toestemming aangevinkt uitsluitend voor deze geblokkeerde test. / Official Arriva form filled in with synthetic data; the two generated attachments only checked locally in the file field (not uploaded); every field checked. Consent ticked only for this blocked test.");
        note(
            "captcha",
            "ready",
            "Onzichtbare reCAPTCHA Enterprise: een zichtbare puzzel is niet gegarandeerd. Verzenden blijft geblokkeerd.",
        );
        STATE.lock().unwrap()["phase"] = json!("ready");
    }
    Ok(done)
}
/// The local attachment files as a JS array of {name, b64}.
fn local_files_js(files: &[std::path::PathBuf]) -> Result<String> {
    local_files(files).map(|(js, _)| js)
}

fn local_files(files: &[std::path::PathBuf]) -> Result<(String, Vec<Attachment>)> {
    use base64::Engine;
    let mut list = vec![];
    let mut attachments = vec![];
    for f in files {
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let bytes = std::fs::read(f)?;
        list.push(json!({"name": name, "b64": base64::engine::general_purpose::STANDARD.encode(&bytes)}));
        attachments.push(Attachment { name, size: bytes.len() });
    }
    Ok((Value::Array(list).to_string(), attachments))
}
/// Demo and form test: checks Arriva's file field locally with the files `F` (present, its `accept`
/// list takes them, they fit in one selection), without the change event that makes the page upload,
/// and clears the selection again. Nothing leaves the computer (the guard also blocks uploads).
const LOCAL_ATTACHMENT_CHECK: &str = r#"
const input=document.querySelector('input[type=file]');
if(!input)return {field:false};
const dt=new DataTransfer();
for(const f of F){const s=atob(f.b64),u=new Uint8Array(s.length);for(let i=0;i<s.length;i++)u[i]=s.charCodeAt(i);dt.items.add(new File([u],f.name,{type:'image/png'}))}
const tokens=(input.accept||'').split(',').map(x=>x.trim().toLowerCase()).filter(Boolean);
const fits=f=>!tokens.length||tokens.some(t=>t==='image/*'||t===f.type||(t.startsWith('.')&&f.name.toLowerCase().endsWith(t)));
const accepts=[...dt.files].every(fits);
input.files=dt.files;const selected=input.files.length;
input.value='';
return {field:true,accepts,selected,multiple:input.multiple,cleared:input.files.length===0};
"#;
/// Demo and form test, after [`LOCAL_ATTACHMENT_CHECK`]: nothing was uploaded, no form errors, next page.
const LOCAL_ATTACHMENT_NEXT: &str = r#"
const r=verify({attachment_not_uploaded:!/Upload voltooid|Upload complete/i.test(document.body.innerText),no_form_errors:noErrors()});if(!r.unverified.length)await next();return r;
"#;
/// Re-checks the last page right before the CAPTCHA: returns the fields that do not read back.
pub(crate) fn final_check(b: &Bridge, f: &FormData) -> Result<Vec<String>> {
    let v = script(b, &final_check_js(f))?;
    let list = v["unverified"].as_array().ok_or_else(|| anyhow!("Formulier niet te controleren. / Form could not be checked."))?;
    Ok(list.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
}
/// Presses Indienen (the only place that does) and reports within about six seconds whether Google
/// shows a challenge or Arriva reports a CAPTCHA error. Whether anything is sent is up to the guard.
pub(crate) fn press_submit(b: &Bridge) -> Result<Value> {
    script(
        b,
        &format!(
            r#"{CHALLENGE_VISIBLE}
const button=submitButton();
if(!button||button.disabled)throw Error('Laatste stap niet gereed');
button.click();
let challenge=false;for(let i=0;i<30&&!challenge;i++){{await sleep(200);challenge=shown();}}
// The rest of the page may be hidden (visibility), so read its text with textContent.
const captchaError=/Captcha kon niet gevalideerd worden/i.test(document.body.textContent);
return {{captchaError,challenge}};
"#
        ),
    )
}
/// Arriva's page after the final write: a CAPTCHA error, a confirmation text (and any reference in it).
pub(crate) fn page_outcome(b: &Bridge) -> Result<Value> {
    script(
        b,
        r#"const t=document.body.textContent||'';
const captchaError=/Captcha kon niet gevalideerd worden/i.test(t);
const thanks=/bedankt|we hebben je (aanvraag|verzoek|formulier)|ontvangen|thank you/i.test(t)&&!submitButton();
const m=t.match(/(?:referentie|kenmerk|ticketnummer|zaaknummer|aanvraagnummer)\s*(?:nummer)?\s*[:#]?\s*([A-Z0-9-]{4,})/i);
return {captchaError,thanks,reference:m?m[1]:null};"#,
    )
}
fn failure(e: anyhow::Error) {
    note("form", "error", &e.to_string());
    let mut s = STATE.lock().unwrap();
    s["phase"] = json!("error");
    s["last_error"] = json!(e.to_string());
}
#[tauri::command]
pub fn arriva_demo_state(app: AppHandle) -> Value {
    snapshot(&app)
}
#[tauri::command]
pub fn arriva_demo_enable(app: AppHandle) -> Result<Value, String> {
    require().map_err(|e| e.to_string())?;
    ENABLED.store(true, Ordering::SeqCst);
    Ok(snapshot(&app))
}
fn prepare_for_check(app: &AppHandle) -> Result<()> {
    require()?;
    let _op = OP.lock().unwrap();
    *STATE.lock().unwrap() = initial();
    STATE.lock().unwrap()["phase"] = json!("preparing");
    let p = evidence(app)?;
    // Only a trip that meets Arriva's rules (with the downloaded delay) is put into the form.
    let trip = STATE.lock().unwrap()["demo_trips"].as_array().and_then(|a| a.iter().find(|t| t["id"] == "DEMO-ARRIVA-732284").cloned());
    if let Some(t) = trip.filter(|t| t["eligible"] != true) {
        bail!("{} / {}", t["reason_nl"].as_str().unwrap_or("Geen recht."), t["reason_en"].as_str().unwrap_or("Not eligible."));
    }
    if let Some(w) = app.get_webview_window(LABEL) {
        w.close()?;
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while app.get_webview_window(LABEL).is_some() {
            if std::time::Instant::now() >= until {
                bail!("Het vorige Arriva-venster sluit nog. Probeer de controle opnieuw. / The previous Arriva window is still closing. Run the check again.");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    let b = Bridge::open(app)?;
    populate(&b, &p, &current_demo_form()?)?;
    // Keep the prepared form hidden until the user's Continue action.
    Ok(())
}
pub fn finish_check(app: &AppHandle) {
    if let Err(e) = prepare_for_check(app) {
        failure(e);
    }
    let _ = app.emit("arriva-demo-updated", snapshot(app));
}
#[tauri::command]
pub async fn arriva_demo_prepare(app: AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        require().map_err(|e| e.to_string())?;
        finish_check(&app);
        Ok(snapshot(&app))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn arriva_demo_open(app: AppHandle, view: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        require().map_err(|e| e.to_string())?;
        if !matches!(view.as_str(), "captcha" | "form") {
            return Err("Onbekende weergave. / Unknown view.".into());
        }
        let _op = OP.lock().unwrap();
        if let Some(window) = app.get_webview_window(LABEL) {
            let b = Bridge { window };
            if view == "form" {
                // Developer inspection of the whole live form ("Inspect demo").
                b.show_form().map_err(|e| e.to_string())?;
            } else if STATE.lock().unwrap()["phase"] == "ready" {
                if let Err(e) = request_verification(&b) {
                    failure(e);
                }
            } else if challenge_visible(&b).unwrap_or(false) {
                // Returning to a pending challenge: show it again, never press Indienen twice.
                let r = b.captcha_only("Los de puzzel op. Verzending blijft geblokkeerd.", "Solve the puzzle. Submission stays blocked.");
                r.and_then(|_| b.show()).map_err(|e| e.to_string())?;
            }
        } else {
            STATE.lock().unwrap()["phase"] = json!("window_closed");
            STATE.lock().unwrap()["last_error"] = json!("Het Arriva-venster is gesloten. Bereid de demo opnieuw voor.");
        }
        Ok(snapshot(&app))
    })
    .await
    .map_err(|e| e.to_string())?
}
/// True when Google's challenge frame is on screen with a real size (the invisible reCAPTCHA keeps
/// its frame far off-screen and hidden until it asks for a puzzle).
pub(crate) const CHALLENGE_VISIBLE: &str = r#"
const shown=()=>[...document.querySelectorAll('iframe')].some(e=>{
 if(!/bframe/.test(e.src))return false;
 const r=e.getBoundingClientRect(),s=getComputedStyle(e);
 return r.width>40&&r.height>40&&r.bottom>0&&r.right>0&&r.top<innerHeight&&r.left<innerWidth&&s.visibility==='visible'&&(!e.checkVisibility||e.checkVisibility({checkOpacity:true,checkVisibilityCSS:true}));
});
"#;
/// Bumped for every verification; an older challenge watcher stops when it changes.
static WATCH: AtomicUsize = AtomicUsize::new(0);
fn challenge_visible(b: &Bridge) -> Result<bool> {
    Ok(script(b, &format!("{CHALLENGE_VISIBLE}return shown();"))? == true)
}
/// Shows the CAPTCHA-only window while Google shows a challenge and hides it again when the challenge
/// is gone. Runs in the background for up to ten minutes after Indienen (or until the window closes).
fn watch_challenge(b: Bridge) {
    let generation = WATCH.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let app = b.window.app_handle().clone();
        let until = std::time::Instant::now() + Duration::from_secs(600);
        while std::time::Instant::now() < until && WATCH.load(Ordering::SeqCst) == generation {
            std::thread::sleep(Duration::from_millis(400));
            if app.get_webview_window(LABEL).is_none() {
                break;
            }
            let Ok(visible) = challenge_visible(&b) else { continue };
            let shown = b.window.is_visible().unwrap_or(false);
            if visible && !shown {
                let _ = b.captcha_only("Los de puzzel op. Verzending blijft geblokkeerd.", "Solve the puzzle. Submission stays blocked.");
                let _ = b.show();
                {
                    let mut s = STATE.lock().unwrap();
                    s["captcha"] = json!("puzzle");
                    s["captcha_window_shown"] = json!(s["captcha_window_shown"].as_u64().unwrap_or(0) + 1);
                }
                note("captcha", "attention", "Puzzel zichtbaar: los hem op in het Arriva-venster. Verzending blijft geblokkeerd. / Puzzle shown: solve it in the Arriva window. Submission stays blocked.");
                let _ = app.emit("arriva-demo-updated", ());
            } else if !visible && shown && STATE.lock().unwrap()["captcha"] == "puzzle" {
                let _ = b.window.hide();
                STATE.lock().unwrap()["captcha"] = json!("challenge_closed");
                note("captcha", "attention", "Puzzel gesloten; het Arriva-venster is weer verborgen. Verzending blijft geblokkeerd. / Puzzle closed; the Arriva window is hidden again. Submission stays blocked.");
                let _ = app.emit("arriva-demo-updated", ());
            }
        }
    });
}
/// Checks the form once more, then presses Indienen in the hidden window (the network guard blocks the
/// final write). The CAPTCHA-only window appears only if Google actually shows a challenge; otherwise
/// it never appears and the app reports the outcome. Unverified fields are reported in the app.
fn request_verification(b: &Bridge) -> Result<()> {
    let unverified = final_check(b, &current_demo_form()?)?;
    STATE.lock().unwrap()["unverified"] = json!(unverified);
    if !unverified.is_empty() {
        let list = unverified.join(", ");
        note(
            "captcha",
            "error",
            &format!(
                "Niet alle velden bevestigd: {list}. CAPTCHA niet gevraagd. / Not all fields verified: {list}. CAPTCHA not requested."
            ),
        );
        bail!("Niet alle velden konden worden gecontroleerd: {list}. / Not all fields could be verified: {list}.");
    }
    // Prepare the CAPTCHA-only view while the window is still hidden.
    b.window.hide()?;
    b.captcha_only("Verificatie bij Arriva... Verzending blijft geblokkeerd.", "Verifying at Arriva... Submission stays blocked.")?;
    {
        // The claim stays "in progress" until the outcome below is known.
        let mut s = STATE.lock().unwrap();
        s["captcha_window_shown"] = json!(0);
        s["final_write_blocked"] = json!(false);
        s["phase"] = json!("verifying");
    }
    let v = press_submit(b)?;
    let (captcha, message) = if v["challenge"] == true {
        ("puzzle", "Puzzel zichtbaar: los hem op in het Arriva-venster. Verzending blijft geblokkeerd. / Puzzle shown: solve it in the Arriva window. Submission stays blocked.")
    } else if v["captchaError"] == true {
        ("failed", "Arriva meldt: Captcha kon niet gevalideerd worden. In de demo is niets naar Arriva geüpload en is de claim niet ingediend. / Arriva reports: the CAPTCHA could not be validated. In the demo nothing was uploaded to Arriva and the claim was not submitted.")
    } else {
        ("no_puzzle", "Geen puzzel nodig: de onzichtbare CAPTCHA vroeg niets; het Arriva-venster bleef verborgen. Verzending blijft geblokkeerd. / No puzzle needed: the invisible CAPTCHA asked nothing; the Arriva window stayed hidden. Submission stays blocked.")
    };
    if captcha == "puzzle" {
        b.captcha_only("Los de puzzel op. Verzending blijft geblokkeerd.", "Solve the puzzle. Submission stays blocked.")?;
        b.show()?;
        let mut s = STATE.lock().unwrap();
        s["captcha_window_shown"] = json!(1);
    }
    STATE.lock().unwrap()["captcha"] = json!(captcha);
    note("captcha", "attention", message);
    {
        let mut s = STATE.lock().unwrap();
        let done = s["final_write_blocked"] == true;
        s["phase"] = json!(if done { "submission_blocked" } else { "captcha_attention" });
    }
    // A challenge can also come (or go) later: show and hide the window with it.
    watch_challenge(b.clone());
    Ok(())
}
/// Devtools `arriva-form-test` (maintainer canary): fills Arriva's real form with the synthetic demo
/// data and the two generated attachments (demo travel card, RDT delay card) in a hidden window, verifies every field and that the submit
/// button is present and enabled. It never presses it, so the CAPTCHA is not triggered and no claim
/// is sent; the network guard stays on (and is self-tested). Prints step names only, no personal data.
pub fn form_test_run(app: &AppHandle) -> Result<String> {
    let dir = std::env::temp_dir().join(format!("OVRefund-arriva-formtest-{}", std::process::id()));
    let _ = SCRATCH_OVERRIDE.set(dir.clone());
    println!("arriva form test (NOT submitting; network guard on; CAPTCHA not triggered)");
    let r = form_test_steps(app);
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.navigate("about:blank".parse()?);
        let _ = w.close();
    }
    // WebView2 may keep the folder open for a moment after the window closes.
    for _ in 0..20 {
        if !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    r
}
fn form_test_steps(app: &AppHandle) -> Result<String> {
    let step = |name: &str, detail: String| println!("  ok: {name}: {detail}");
    let pdf = evidence(app).map_err(|e| step_error("attachments", &e.to_string()))?;
    let kb: Vec<String> = pdf.iter().map(|p| format!("{} KB", std::fs::metadata(p).map(|m| m.len().div_ceil(1024)).unwrap_or(0))).collect();
    step("attachments", format!("travel card (demo data) and RDT delay card rendered as PNG ({})", kb.join(", ")));
    let b = Bridge::open(app).map_err(|e| step_error("network guard", &e.to_string()))?;
    step("network guard", "installed before the page loaded".into());
    let form = current_demo_form()?;
    let n = populate(&b, &pdf, &form)?;
    for (name, count) in &n {
        step(name, format!("{count} fields verified"));
    }
    // The guard must stop a write to the form backend (a made-up form id) before the network.
    let before = BLOCKED.load(Ordering::SeqCst);
    let probe = b
        .eval("fetch('https://forms-eu1.hsforms.com/submissions/v3/public/submit/0/ovrefund-guard-selftest',{method:'POST',mode:'no-cors',body:'x'}).then(()=>'sent',()=>'blocked')")
        .map_err(|e| step_error("guard self-test", &e.to_string()))?;
    if probe != "blocked" || BLOCKED.load(Ordering::SeqCst) <= before {
        return Err(step_error("guard self-test", "test-schrijfactie niet geblokkeerd / test write was not blocked"));
    }
    step("guard self-test", "a test write to the form backend was blocked before the network".into());
    // Uploads are blocked too: a test upload (no file) to HubSpot's upload endpoint must fail locally.
    let before = DEMO_UPLOADS_BLOCKED.load(Ordering::SeqCst);
    let probe = b
        .eval("fetch('https://forms-eu1.hsforms.com/submissions-files/v1/upload/files',{method:'POST',mode:'no-cors',body:'x'}).then(()=>'sent',()=>'blocked')")
        .map_err(|e| step_error("upload guard self-test", &e.to_string()))?;
    if probe != "blocked" || DEMO_UPLOADS_BLOCKED.load(Ordering::SeqCst) <= before {
        return Err(step_error("upload guard self-test", "test-upload niet geblokkeerd / test upload was not blocked"));
    }
    step("upload guard self-test", "a test upload was blocked before the network".into());
    let unverified = final_check(&b, &form).map_err(|e| step_error("final check", &e.to_string()))?;
    if !unverified.is_empty() {
        let list = unverified.join(", ");
        return Err(step_error("final check", &format!("velden niet bevestigd: {list} / fields not verified: {list}")));
    }
    step("final check", "last page complete; Indienen present and enabled (not pressed)".into());
    let total: u64 = n.iter().map(|(_, c)| c).sum();
    Ok(format!(
        "arriva form test passed: 3 pages filled, {total} fields verified, both synthetic attachments accepted by the file field (not uploaded; {} upload request(s) blocked), Indienen present and enabled; CAPTCHA not triggered, claim not submitted",
        before
    ))
}
#[tauri::command]
pub async fn arriva_demo_probe(app: AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        require().map_err(|e| e.to_string())?;
        let _op = OP.lock().unwrap();
        let r = (|| -> Result<()> {
            let w = app.get_webview_window(LABEL).ok_or_else(|| anyhow!("Bereid eerst de demo voor. / Prepare the demo first."))?;
            let b = Bridge { window: w };
            request_verification(&b)?;
            Ok(())
        })();
        if let Err(e) = r {
            failure(e);
        }
        Ok(snapshot(&app))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn arriva_demo_reset(app: AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        require().map_err(|e| e.to_string())?;
        let _op = OP.lock().unwrap();
        if let Some(w) = app.get_webview_window(LABEL) {
            w.close().map_err(|e| e.to_string())?;
        }
        *STATE.lock().unwrap() = initial();
        Ok(snapshot(&app))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn arriva_demo_attachment(app: AppHandle, index: usize) -> Result<Value, String> {
    require().map_err(|e| e.to_string())?;
    let s = snapshot(&app);
    let path = s["attachments"][index]["path"].as_str().ok_or("Bijlage ontbreekt. / Attachment is missing.")?;
    let p = std::path::Path::new(path);
    if p.parent() != Some(scratch().as_path()) || p.extension().and_then(|s| s.to_str()) != Some("png") {
        return Err("Ongeldige demobijlage. / Invalid demo attachment.".into());
    }
    std::process::Command::new("explorer.exe").arg(p).spawn().map_err(|e| e.to_string())?;
    Ok(s)
}
/// The attachment as a data URL for the in-app thumbnail and enlarged view.
#[tauri::command]
pub fn arriva_demo_attachment_data(app: AppHandle, index: usize) -> Result<String, String> {
    require().map_err(|e| e.to_string())?;
    let s = snapshot(&app);
    let path = s["attachments"][index]["path"].as_str().ok_or("Bijlage ontbreekt. / Attachment is missing.")?;
    let p = std::path::Path::new(path);
    if p.parent() != Some(scratch().as_path()) || p.extension().and_then(|s| s.to_str()) != Some("png") {
        return Err("Ongeldige demobijlage. / Invalid demo attachment.".into());
    }
    let bytes = std::fs::read(p).map_err(|e| format!("Bijlage niet te lezen. / Attachment could not be read. ({e})"))?;
    use base64::Engine;
    Ok(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}
/// Evidence cards are rendered offline: the proof window never leaves about:blank.
fn proof_navigation(url: &tauri::Url) -> bool {
    url.as_str() == "about:blank"
}
/// Navigation for the `arriva-eval` diagnostic: about:blank or the Arriva form.
fn eval_navigation(url: &tauri::Url) -> bool {
    url.as_str() == "about:blank" || navigation(url)
}
fn viewport_png(b: &Bridge, path: &std::path::Path) -> Result<()> {
    let r = b.cdp("Page.captureScreenshot", json!({"format":"png","fromSurface":true}))?;
    use base64::Engine;
    std::fs::write(path, base64::engine::general_purpose::STANDARD.decode(r["data"].as_str().unwrap_or(""))?)?;
    Ok(())
}
/// A stand-in with the DOM shape of Google's challenge container (body > div > div > iframe[src*=bframe]),
/// used only by the devtools view test when Google issues no real puzzle.
const SIMULATED_CHALLENGE: &str = r#"(() => {
  const html = `<meta name="bframe"><body style="margin:0;font:14px Segoe UI,sans-serif">
    <div style="background:#1a73e8;color:#fff;padding:20px 16px"><small>SIMULATIE / SIMULATION</small><br><b style="font-size:20px">Selecteer alle afbeeldingen met bussen</b></div>
    <div style="display:grid;grid-template-columns:repeat(3,1fr);gap:4px;padding:8px">${'<div style="height:118px;background:#d5d9de"></div>'.repeat(9)}</div>
    <div style="border-top:1px solid #ddd;padding:10px 16px;text-align:right"><span style="background:#1a73e8;color:#fff;padding:9px 18px;border-radius:2px">VERIFIËREN</span></div></body>`;
  const outer = document.createElement('div'); outer.id = 'ov-simulated-challenge';
  outer.style.cssText = 'visibility:visible;position:absolute;width:100%;top:-10000px;left:0;right:0;opacity:1';
  const overlay = document.createElement('div');
  overlay.style.cssText = 'width:100%;height:100%;position:fixed;top:0;left:0;z-index:2000000000;background-color:#fff;opacity:.05';
  const box = document.createElement('div');
  box.style.cssText = 'border:1px solid #ccc;box-shadow:rgba(0,0,0,.2) 2px 2px 3px;position:absolute;visibility:visible;z-index:2000000000;left:300px;top:10600px;background:#fff';
  const f = document.createElement('iframe'); f.width = 400; f.height = 580; f.style.border = '0';
  f.src = 'data:text/html;charset=utf-8,' + encodeURIComponent(html);
  box.append(f); outer.append(overlay, box); document.body.append(outer);
  return true;
})()"#;
/// Devtools `arriva-view-test <dir>`: fills the demo form, presses Indienen in the hidden window behind
/// the network guard (the final write is blocked; no claim is submitted) and checks that the provider window
/// stays hidden when Google asks nothing. Then injects a simulated challenge and checks that the window
/// appears with only the challenge, and hides again when it is removed. Saves the two attachments and
/// a screenshot of the simulated-challenge window.
pub fn view_test_run(app: &AppHandle, out: &std::path::Path) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("OVRefund-arriva-viewtest-{}", std::process::id()));
    let _ = SCRATCH_OVERRIDE.set(dir.clone());
    std::fs::create_dir_all(out)?;
    let r = (|| -> Result<()> {
        let files = evidence(app)?;
        for (f, name) in files.iter().zip(["attachment-1-travel.png", "attachment-2-delay.png"]) {
            std::fs::copy(f, out.join(name))?;
            println!("saved {name}");
        }
        println!("delay record: {}", STATE.lock().unwrap()["rdt"]);
        let b = Bridge::open(app)?;
        populate(&b, &files, &current_demo_form()?)?;
        println!("form filled and verified");
        // Sample the window's visibility while Indienen is pressed (real Google, no simulation).
        let seen = std::sync::Arc::new(AtomicBool::new(false));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let sampler = {
            let (w, seen, stop) = (b.window.clone(), seen.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if w.is_visible().unwrap_or(false) {
                        seen.store(true, Ordering::SeqCst);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        };
        let before = BLOCKED.load(Ordering::SeqCst);
        request_verification(&b)?;
        std::thread::sleep(Duration::from_secs(4));
        stop.store(true, Ordering::SeqCst);
        let _ = sampler.join();
        let s = STATE.lock().unwrap().clone();
        println!(
            "real path: captcha {}, phase {}, window ever visible: {}, claim writes blocked by the guard: {}",
            s["captcha"],
            s["phase"],
            seen.load(Ordering::SeqCst),
            BLOCKED.load(Ordering::SeqCst) - before
        );
        // Simulated challenge: the watcher must show the window, then hide it again.
        let wait_visible = |want: bool| {
            let until = std::time::Instant::now() + Duration::from_secs(8);
            while std::time::Instant::now() < until {
                if b.window.is_visible().unwrap_or(false) == want {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            false
        };
        b.eval(SIMULATED_CHALLENGE)?;
        let appeared = wait_visible(true);
        println!("simulated challenge: window shown {appeared}");
        std::thread::sleep(Duration::from_millis(900));
        match viewport_png(&b, &out.join("captcha-simulated-challenge.png")) {
            Ok(()) => println!("saved captcha-simulated-challenge.png"),
            Err(e) => println!("screenshot failed: {e:#}"),
        }
        b.eval("document.getElementById('ov-simulated-challenge')?.remove();true")?;
        let hidden = wait_visible(false);
        println!("simulated challenge removed: window hidden {hidden}; captcha {}", STATE.lock().unwrap()["captcha"]);
        anyhow::ensure!(
            !seen.load(Ordering::SeqCst) || s["captcha"] == "puzzle",
            "Venster getoond zonder uitdaging. / Window shown without a challenge."
        );
        anyhow::ensure!(
            appeared && hidden,
            "Venster volgde de gesimuleerde uitdaging niet. / The window did not follow the simulated challenge."
        );
        Ok(())
    })();
    WATCH.fetch_add(1, Ordering::SeqCst);
    for w in app.webview_windows().into_values() {
        if w.label() != "main" {
            let _ = w.close();
        }
    }
    for _ in 0..20 {
        if !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    r
}
/// The stacked delay cards and each downloaded month with its download time.
type DelayShot = (Vec<u8>, Vec<(String, chrono::DateTime<chrono::Utc>)>);
/// Devtools `delay-shot`: the delay proof for a matched journey of any carrier (NS trains included).
/// One Rijden de Treinen open-data card per train leg (the app's matched trains), the month files
/// downloaded now; each card marks the leg's boarding and alighting station and shows the download
/// time. Cards are stacked into one PNG. Returns the PNG and each month's download time.
pub fn delay_shot_run(app: &AppHandle, legs: &[ovrefund_core::model::Leg]) -> Result<DelayShot> {
    let dir = std::env::temp_dir().join(format!("OVRefund-delayshot-{}", std::process::id()));
    let _ = SCRATCH_OVERRIDE.set(dir.clone());
    let r = delay_cards(app, legs);
    for w in app.webview_windows().into_values() {
        if w.label() != "main" {
            let _ = w.close();
        }
    }
    for _ in 0..20 {
        if !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    r
}
/// The delay cards of `legs` (see [`delay_shot_run`]), rendered in hidden offline windows.
pub(crate) fn delay_cards(app: &AppHandle, legs: &[ovrefund_core::model::Leg]) -> Result<DelayShot> {
    use ovrefund_core::archive::{month_key, month_rows};
    use std::collections::{BTreeMap, HashSet};
    anyhow::ensure!(!legs.is_empty(), "Geen treinen bij deze reis. / No trains for this journey.");
    {
        // RDT's service date is the departure date, or the day before for a train running past midnight.
        let dates = |l: &ovrefund_core::model::Leg| {
            let d = l.dep_planned.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
            [d, d.pred_opt().unwrap_or(d)]
        };
        let mut wanted: BTreeMap<String, HashSet<(chrono::NaiveDate, String)>> = BTreeMap::new();
        for l in legs {
            for d in dates(l) {
                wanted.entry(month_key(d)).or_default().insert((d, l.train_number.clone()));
            }
        }
        let mut months = BTreeMap::new();
        for (m, w) in &wanted {
            if let Some(rows) = month_rows(m, w)? {
                months.insert(m.clone(), rows);
            }
        }
        let mut out: Option<Vec<u8>> = None;
        for l in legs {
            let found = dates(l).into_iter().find_map(|d| {
                let m = months.get(&month_key(d))?;
                let rows = m.services.get(&(d, l.train_number.clone()))?;
                crate::proof_cards::service_for_leg(rows, l).map(|service| (service, m, month_key(d)))
            });
            let (rows, m, month) = found.ok_or_else(|| {
                anyhow!(
                    "Trein {t} niet gevonden in de open data van Rijden de Treinen. / Train {t} not found in Rijden de Treinen open data.",
                    t = l.train_number
                )
            })?;
            let src = crate::proof_cards::RdtSource {
                dataset: format!("services-{month}.csv.gz"),
                url: m.url.clone(),
                retrieved: m.retrieved.with_timezone(&chrono::Local).format("%d-%m-%Y %H:%M:%S").to_string(),
                file_version: m.last_modified.clone(),
            };
            let card = render_png(app, &crate::proof_cards::delay_card(&rows, &l.from_code, &l.to_code, &src)?, 12.)?;
            out = Some(match out {
                None => card,
                Some(prev) => crate::proof_cards::stack_png(&prev, &card)?,
            });
        }
        let png = out.ok_or_else(|| anyhow!("Geen kaart gemaakt. / No card was made."))?;
        Ok((png, months.iter().map(|(m, r)| (m.clone(), r.retrieved)).collect()))
    }
}
/// Devtools GUI hook (OVREFUND_UI_SCRIPT + OVREFUND_UI_SHOTS): runs a script in the main window in
/// steps separated by `//#shot <name>` lines and saves a screenshot of the window after each step.
pub fn ui_script_run(app: &AppHandle, script: &str, out: &std::path::Path) -> Result<()> {
    let window = app.get_webview_window("main").ok_or_else(|| anyhow!("Geen hoofdvenster. / No main window."))?;
    let b = Bridge { window };
    std::fs::create_dir_all(out)?;
    let text = std::fs::read_to_string(script)?;
    let mut chunk = String::new();
    for line in text.lines().chain(std::iter::once("//#end")) {
        // `//#step` only splits (each step must finish within the 60 s browser timeout).
        let marker = line.trim().strip_prefix("//#shot ").map(str::trim);
        if marker.is_none() && !matches!(line.trim(), "//#end" | "//#step") {
            chunk.push_str(line);
            chunk.push('\n');
            continue;
        }
        if !chunk.trim().is_empty() {
            if let Err(e) = b.eval(&format!("(async()=>{{{chunk}\nreturn true}})()")) {
                println!("ui step failed: {e:#}");
            }
        }
        chunk.clear();
        if let Some(name) = marker {
            std::thread::sleep(Duration::from_millis(500));
            viewport_png(&b, &out.join(format!("{name}.png")))?;
            println!("saved {name}.png");
        }
    }
    Ok(())
}
/// Devtools diagnostic `arriva-eval <url|filled> <js file> [png] [--show]`: evaluates a page script in a
/// guarded window (Arriva form, optionally filled with the demo data, or rijdendetreinen.nl) and prints
/// the JSON result. Never submits.
pub fn eval_run(app: &AppHandle, url: &str, js_file: &str, shot: Option<&str>, show: bool) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("OVRefund-arriva-eval-{}", std::process::id()));
    let _ = SCRATCH_OVERRIDE.set(dir.clone());
    let r = (|| -> Result<()> {
        let b = if url == "filled" {
            let pdf = evidence(app)?;
            let b = Bridge::open(app)?;
            populate(&b, &pdf, &current_demo_form()?)?;
            b
        } else {
            let b = Bridge::guarded(app, PROOF_LABEL, "OV Refund diagnose", (1100., 900.), eval_navigation)?;
            b.window.navigate(url.parse()?)?;
            std::thread::sleep(Duration::from_secs(8));
            b
        };
        if show {
            b.show()?;
            std::thread::sleep(Duration::from_secs(1));
        }
        let js = std::fs::read_to_string(js_file)?;
        let v = b.eval(&js)?;
        println!("{}", serde_json::to_string_pretty(&v)?);
        if let Some(p) = shot {
            let r = b.cdp("Page.captureScreenshot", json!({"format":"png"}))?;
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(r["data"].as_str().unwrap_or(""))?;
            std::fs::write(p, bytes)?;
            println!("screenshot: {p}");
        }
        Ok(())
    })();
    for w in app.webview_windows().into_values() {
        if w.label() != "main" {
            let _ = w.close();
        }
    }
    for _ in 0..20 {
        if !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    r
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_writes_always_blocked() {
        for u in [
            "https://forms-eu1.hsforms.com/submissions/v3/public/submit/123/abc",
            "https://klantenservice.arriva.nl/unknown-submit",
            "https://api-eu1.hubspot.com/new-submit",
            "https://www.google.com.evil.test/recaptcha/api2/reload",
        ] {
            assert!(!allow_request("POST", u), "{u}");
        }
        assert!(allow_request("POST", "https://www.google.com/recaptcha/enterprise/reload"));
        assert!(allow_request("POST", "https://google.com/recaptcha/enterprise/clr"));
        assert!(allow_request("POST", "https://recaptcha.net/recaptcha/enterprise/reload"));
        assert!(!allow_request("POST", "https://google.com/not-captcha"));
        assert!(is_final_submit("https://forms-eu1.hsforms.com/submissions/v3/public/submit/formsnext/multipart/1/abc"));
        assert!(!is_final_submit("https://forms-eu1.hsforms.com/submissions/v3/public/submit/1/abc"));
        assert!(!is_final_submit("https://forms.hsforms.com/submissions/v3/public/submit/formsnext/multipart/1/abc"));
        assert!(!is_final_submit("https://forms-eu1.hsforms.com/submissions-validation/v1/validate/1/abc"));
        assert!(!is_final_submit("https://forms-eu1.hsforms.com/submissions-files/v1/upload/files"));
        assert!(!is_final_submit("http://forms-eu1.hsforms.com/submissions/v3/public/submit/1/abc"));
        assert!(!is_final_submit("https://forms-eu1.hsforms.com:444/submissions/v3/public/submit/1/abc"));
        assert!(!is_final_submit("https://other.hsforms.com/submissions/v3/public/submit/1/abc"));
        assert!(!is_final_submit("https://forms-eu1.hsforms.com/other/submit/1/abc"));
        assert!(!allow_request("POST", "https://forms-eu1.hsforms.com/submissions-files/v1/upload/files"));
        assert!(allow_request("GET", URL));
        assert!(!allow_request("POST", "https://forms-eu1.hsforms.com/form-integrations/v1/uploaded-files/submit"));
        // Every write the guard allows on its own is a CAPTCHA call: besides those, only the upload
        // (in the user's own claim) and the one armed final write ever leave the claim window.
        for (m, u) in [
            ("POST", "https://www.google.com/recaptcha/enterprise/reload"),
            ("PUT", "https://recaptcha.google.com/recaptcha/api2/userverify"),
            ("POST", "https://klantenservice.arriva.nl/_hcms/forms"),
            ("PATCH", "https://forms-eu1.hsforms.com/anything"),
            ("DELETE", "https://www.google.com/recaptcha/x"),
            ("POST", "data:text/plain,x"),
        ] {
            assert!(!allow_request(m, u) || is_captcha(u), "{m} {u}");
        }
    }
    #[test]
    fn step_errors_name_the_step_in_both_languages() {
        let e = step_error("page 2: trip", "velden niet bevestigd: fare / fields not verified: fare").to_string();
        let (nl, en) = e.split_once(" / ").unwrap();
        assert_eq!(nl, "Arriva-formulier, stap 'page 2: trip': velden niet bevestigd: fare");
        assert_eq!(en, "Arriva form, step 'page 2: trip': fields not verified: fare");
        assert!(step_error("load form", "plain").to_string().ends_with("step 'load form': plain"));
    }
    #[test]
    fn captcha_view_and_canary_never_press_submit_outside_verification() {
        // Only request_verification may click the submit button; the form test and the shared
        // final check only read it.
        let src = include_str!("arriva.rs");
        assert_eq!(src.matches(concat!("button", ".click()")).count(), 1);
        let press = src.find(concat!("fn press", "_submit(")).unwrap();
        assert!(src[press..].find(concat!("button", ".click()")).unwrap() < 600, "the click is in press_submit");
        let demo = demo_form("Limburg", 24).unwrap();
        assert!(!final_check_js(&demo).contains(".click("));
        // One data line per script: a second `const D` is a SyntaxError on the page.
        assert_eq!(final_check_js(&demo).matches("const D=").count(), 1);
        assert!(!FINAL_CHECK.contains("const D=") && !FINAL_CHECK.contains("{{"));
        // press_submit is called only by the demo verification and the user's Claim flow.
        assert_eq!(src.matches(concat!("press", "_submit(b)")).count(), 1);
        let claim = include_str!("arriva_claim.rs");
        assert_eq!(claim.matches(concat!("press", "_submit(")).count(), 1);
        assert!(!claim.contains(concat!(".click", "()")));
    }
    #[test]
    fn the_users_attachments_are_never_uploaded_by_a_build_that_cannot_send() {
        let up = "https://forms-eu1.hsforms.com/submissions-files/v1/upload/files";
        assert!(!allow_request("POST", up), "the demo, form test and eval never upload");
        assert!(!claim_upload_allowed("POST", up, false), "a real claim's attachments in a dry run / test build");
        assert!(claim_upload_allowed("POST", up, true));
        assert!(claim_upload_allowed("GET", "https://klantenservice.arriva.nl/geldterug", false));
        assert!(!may_arm(true, false, false, false, false) && !may_arm(false, false, true, false, false));
    }

    #[test]
    fn attachment_preflight_does_not_spend_an_upload() {
        let up = "https://forms-eu1.hsforms.com/submissions-files/v1/upload/files";
        assert!(allow_request("OPTIONS", up));
        assert!(!is_attachment_write("OPTIONS", up));
        assert!(is_attachment_write("POST", up));
        assert!(!allow_request("PUT", up));
    }
    #[test]
    fn final_write_is_never_armed_in_dry_runs_demo_or_tests() {
        // (devtools, demo, dry_run, test scratch, allow env)
        assert!(may_arm(false, false, false, false, false), "release build, user's Claim");
        assert!(!may_arm(false, true, false, false, false), "demo");
        assert!(!may_arm(false, false, true, false, false), "dry run / canary");
        assert!(!may_arm(false, false, false, true, false), "devtools form/view test, eval, delay-shot");
        assert!(!may_arm(true, false, false, false, false), "developer build");
        assert!(may_arm(true, false, false, false, true), "developer build, explicitly allowed live test");
        assert!(!may_arm(true, true, false, false, true) && !may_arm(true, false, true, false, true));
        assert!(arm_final_write(Duration::from_secs(1), chrono::Utc::now()).is_err());
        assert!(!final_write().armed);
        // Only arm_final_write sets the guard; the demo, form test, view test and eval never call it.
        let src = include_str!("arriva.rs");
        assert_eq!(src.matches(concat!(".armed = ", "true")).count(), 1);
        let arm = concat!("arm_final", "_write(");
        let calls =
            |s: &str| s.lines().filter(|l| l.contains(arm) && !l.contains(concat!("dis", "arm_final")) && !l.contains("fn ")).count();
        assert_eq!(calls(src), 1, "only the check above calls it in this file");
        let claim = include_str!("arriva_claim.rs");
        assert_eq!(calls(claim), 1, "one call, in the Claim flow");
    }
    #[test]
    fn disarming_preserves_uploads_and_freezes_the_submit_decision() {
        let mut guard = FinalWrite { armed: true, uploads: 2, ..FinalWrite::default() };
        assert!(guard.permit_submit(Some("request-1")));
        let outcome = guard.disarm();
        assert!(outcome.sent);
        assert_eq!(outcome.uploads, 2);
        assert!(!guard.permit_submit(Some("request-2")));
        assert_eq!(guard.network_id.as_deref(), Some("request-1"));

        let mut guard = FinalWrite { armed: true, ..FinalWrite::default() };
        assert!(!guard.disarm().sent);
        assert!(!guard.permit_submit(Some("late")));
        assert!(!guard.sent);
        assert!(!guard.permit_upload(vec![0], "late"));
    }

    const UPLOAD: &str = "https://forms-eu1.hsforms.com/submissions-files/v1/upload/files";
    const SUBMIT: &str = "https://forms-eu1.hsforms.com/submissions/v3/public/submit/formsnext/multipart/1/test-form";

    fn claim_guard() -> FinalWrite {
        FinalWrite {
            attachments: [
                Attachment { name: "test-trip.png".into(), size: 3 },
                Attachment { name: "test-delay.png".into(), size: 4 },
            ].into(),
            uploads_open: true,
            ..FinalWrite::default()
        }
    }

    fn request(method: &str, url: &str, id: &str, files: &[(&str, &str)]) -> Value {
        request_with_field(method, url, id, files, "claim data")
    }

    fn request_with_field(method: &str, url: &str, id: &str, files: &[(&str, &str)], field: &str) -> Value {
        use base64::Engine;
        let parts: String = files.iter().map(|(file, bytes)| format!(
            "--fixture-boundary\r\nContent-Disposition: form-data; name=\"attachments\"; filename=\"{file}\"\r\nContent-Type: image/png\r\n\r\n{bytes}\r\n"
        )).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!(
            "{parts}--fixture-boundary\r\nContent-Disposition: form-data; name=\"fixture\"\r\n\r\n{field}\r\n--fixture-boundary--\r\n"
        ));
        json!({"networkId":id,"request":{
            "method":method,"url":url,"headers":{"Content-Type":"multipart/form-data; boundary=fixture-boundary"},
            "postDataEntries":[{"bytes":encoded}],
        }})
    }

    fn network_response(guard: &mut FinalWrite, url: &str, id: &str, status: u16) {
        guard.network_event("Network.responseReceived", &json!({"requestId":id,"response":{"url":url,"status":status}}));
        guard.network_event("Network.loadingFinished", &json!({"requestId":id}));
        if url == UPLOAD && (200..300).contains(&status) {
            let body = match id {
                "batch" => r#"{"files":[{"fileId":"fixture-trip","url":"https://proof.example.invalid/trip"},{"fileId":"fixture-delay"}]}"#,
                "delay" | "delay-retry" => r#"{"fileId":"fixture-delay"}"#,
                _ => r#"{"fileId":"fixture-trip"}"#,
            };
            guard.check_upload_response(id, Some(body));
        }
    }

    const TRIP: (&str, &str) = ("test-trip.png", "abc");
    const DELAY: (&str, &str) = ("test-delay.png", "wxyz");

    fn submitted(guard: &mut FinalWrite) {
        if !guard.armed {
            assert!(guard.arm());
        }
        assert!(guard.request_allowed(&request("POST", SUBMIT, "final", &[TRIP, DELAY]), true, true));
        network_response(guard, SUBMIT, "final", 200);
        assert_eq!(crate::arriva_claim::tests::record_guard_outcome(guard, Some(r#"{"success":true}"#), false), "SUBMITTED");
    }

    #[test]
    fn multipart_only_and_one_batch_with_preflight() {
        let mut guard = claim_guard();
        assert!(guard.attachments_ready(true));
        assert!(!guard.attachments_ready(false));
        submitted(&mut guard);
        assert_eq!(guard.uploads, 0);

        let mut guard = claim_guard();
        assert!(guard.request_allowed(&request("OPTIONS", UPLOAD, "preflight", &[]), true, true));
        assert_eq!(guard.uploads, 0);
        assert!(guard.request_allowed(&request("POST", UPLOAD, "batch", &[TRIP, DELAY]), true, true));
        assert!(!guard.attachments_ready(true));
        network_response(&mut guard, UPLOAD, "batch", 200);
        assert!(guard.attachments_ready(false));
        assert!(guard.request_allowed(&request("OPTIONS", SUBMIT, "preflight-final", &[]), true, true));
        assert!(guard.arm());
        assert!(!guard.request_allowed(&request("POST", SUBMIT, "no-links", &[]), true, true));
        let final_without_inline_files = request_with_field(
            "POST", SUBMIT, "final", &[], r#"["fixture-trip","fixture-delay"]"#,
        );
        assert!(guard.request_allowed(&final_without_inline_files, true, true));
        network_response(&mut guard, SUBMIT, "final", 200);
        assert_eq!(crate::arriva_claim::tests::record_guard_outcome(&guard, Some(r#"{"success":true}"#), false), "SUBMITTED");
        assert_eq!(guard.uploads, 1);
    }

    #[test]
    fn per_file_upload_requires_both_acknowledgements_and_retry_stays_on_its_file() {
        for retry in [false, true] {
            let mut guard = claim_guard();
            assert!(guard.request_allowed(&request("OPTIONS", UPLOAD, "preflight", &[]), true, true));
            assert!(guard.request_allowed(&request("POST", UPLOAD, "trip", &[TRIP]), true, true));
            if retry {
                guard.network_event("Network.loadingFailed", &json!({"requestId":"trip"}));
                assert!(guard.request_allowed(&request("OPTIONS", UPLOAD, "preflight-retry", &[]), true, true));
                assert!(guard.request_allowed(&request("POST", UPLOAD, "retry", &[TRIP]), true, true));
                assert!(!guard.request_allowed(&request("POST", UPLOAD, "third", &[TRIP]), true, true));
            }
            network_response(&mut guard, UPLOAD, if retry { "retry" } else { "trip" }, 200);
            assert!(!guard.attachments_ready(true), "the first finished upload cannot advance the form");
            assert!(guard.arm());
            assert!(!guard.request_allowed(&request("POST", SUBMIT, "missing-file", &[]), true, true));
            assert!(guard.request_allowed(&request("OPTIONS", UPLOAD, "preflight-delay", &[]), true, true));
            assert!(guard.request_allowed(&request("POST", UPLOAD, "delay", &[DELAY]), true, true));
            guard.network_event("Network.responseReceived", &json!({"requestId":"delay","response":{"url":UPLOAD,"status":200}}));
            assert!(!guard.attachments_ready(true), "response headers alone cannot advance the form");
            guard.network_event("Network.loadingFinished", &json!({"requestId":"delay"}));
            assert!(!guard.attachments_ready(false), "the upload response body still needs checking");
            guard.check_upload_response("delay", None);
            assert_eq!(guard.pending_upload_responses().collect::<Vec<_>>(), vec!["delay"]);
            for body in ["{}", "[]", r#"{"fileId":"fixture-delay","success":false}"#] {
                guard.check_upload_response("delay", Some(body));
                assert!(!guard.attachments_ready(false));
            }
            assert!(!guard.attachments_ready(false), "a 2xx upload rejection cannot count");
            assert!(!guard.request_allowed(&request("POST", SUBMIT, "rejected-upload", &[]), true, true));
            assert!(guard.request_allowed(&request("POST", UPLOAD, "delay-retry", &[DELAY]), true, true));
            network_response(&mut guard, UPLOAD, "delay-retry", 200);
            assert!(guard.attachments_ready(false));
            let missing = request_with_field("POST", SUBMIT, "missing-link", &[], "fixture-trip");
            assert!(!guard.request_allowed(&missing, true, true));
            let final_request = request_with_field("POST", SUBMIT, "final", &[], r#"["fixture-trip","fixture-delay"]"#);
            assert!(guard.request_allowed(&final_request, true, true));
            network_response(&mut guard, SUBMIT, "final", 200);
            assert_eq!(crate::arriva_claim::tests::record_guard_outcome(&guard, Some(r#"{"success":true}"#), false), "SUBMITTED");
            assert_eq!(guard.uploads, if retry { 4 } else { 3 });
        }
    }

    #[test]
    fn timeout_disarms_unsent_claim_and_unanswered_request_stays_uncertain() {
        let mut guard = claim_guard();
        assert!(guard.arm());
        guard.submit_deadline = Some(Instant::now() - Duration::from_millis(1));
        assert!(!guard.request_allowed(&request("POST", SUBMIT, "late", &[TRIP, DELAY]), true, true));
        let snapshot = guard.disarm();
        assert_eq!(crate::arriva_claim::tests::record_guard_outcome(&snapshot, None, true), "FAILED");

        let mut guard = claim_guard();
        assert!(guard.arm());
        assert!(guard.request_allowed(&request("POST", SUBMIT, "unanswered", &[TRIP, DELAY]), true, true));
        let snapshot = guard.disarm();
        assert!(!guard.request_allowed(&request("POST", SUBMIT, "retry", &[TRIP, DELAY]), true, true));
        assert_eq!(crate::arriva_claim::tests::record_guard_outcome(&snapshot, None, true), "INDETERMINATE");
    }

    #[test]
    fn blocked_write_does_not_spend_the_one_final_submit() {
        let mut guard = claim_guard();
        assert!(!guard.request_allowed(&request("POST", SUBMIT, "early", &[TRIP, DELAY]), true, true));
        assert!(!guard.request_allowed(&request("POST", UPLOAD, "nonclaim", &[TRIP]), false, true));
        assert!(!guard.request_allowed(&request("POST", UPLOAD, "disabled", &[TRIP]), true, false));
        assert!(!guard.request_allowed(&request("POST", UPLOAD, "other", &[("other.png", "abc")]), true, true));
        assert!(!guard.request_allowed(&request("POST", UPLOAD, "wrong-size", &[("test-trip.png", "too big")]), true, true));
        assert!(guard.arm());
        assert!(!guard.request_allowed(&request("POST", SUBMIT, "one-file", &[TRIP]), true, true));
        assert!(!guard.request_allowed(&request("POST", SUBMIT, "wrong-file", &[TRIP, ("other.png", "abc")]), true, true));
        submitted(&mut guard);
        assert!(!guard.request_allowed(&request("POST", SUBMIT, "second", &[TRIP, DELAY]), true, true));
        assert_eq!(guard.network_id.as_deref(), Some("final"));
    }

    #[test]
    fn omitted_cdp_blobs_are_distinct_from_empty_files() {
        use base64::Engine;
        let mut guard = claim_guard();
        assert!(guard.arm());
        let mut event = request("POST", SUBMIT, "final", &[(TRIP.0, ""), (DELAY.0, "")]);
        assert!(!guard.request_allowed(&event, true, true));
        let base64 = base64::engine::general_purpose::STANDARD;
        let data = base64.decode(event["request"]["postDataEntries"][0]["bytes"].as_str().unwrap()).unwrap();
        let data = String::from_utf8(data).unwrap();
        let entries = data.split_inclusive("Content-Type: image/png\r\n\r\n").flat_map(|part| {
            let bytes = json!({"bytes":base64.encode(part)});
            if part.ends_with("Content-Type: image/png\r\n\r\n") { vec![bytes, json!({})] } else { vec![bytes] }
        }).collect::<Vec<_>>();
        event["request"]["postDataEntries"] = json!(entries);
        assert!(guard.request_allowed(&event, true, true));
        let mut legacy = request("POST", UPLOAD, "legacy", &[(TRIP.0, "wrong length")]);
        let bytes = legacy["request"].as_object_mut().unwrap().remove("postDataEntries").unwrap();
        let body = base64.decode(bytes[0]["bytes"].as_str().unwrap()).unwrap();
        legacy["request"]["postData"] = json!(String::from_utf8(body).unwrap());
        assert!(multipart_attachments(&legacy["request"], &guard.attachments).is_none());
    }

    #[test]
    fn completed_final_write_with_error_response_is_uncertain() {
        let mut guard = claim_guard();
        assert!(guard.arm());
        assert!(guard.request_allowed(&request("POST", SUBMIT, "final", &[TRIP, DELAY]), true, true));
        network_response(&mut guard, SUBMIT, "final", 200);
        for body in [Some(r#"{"inlineMessage":"CAPTCHA kon niet gevalideerd worden"}"#), Some(r#"{"errors":[{"code":"invalid"}]}"#),
            Some(r#"{"success":true,"detail":"The submission was not accepted"}"#),
            Some(r#"{"success":false,"id":"fake"}"#), Some(r#"{"message":"The form was not sent","submissionId":"fixture"}"#),
            Some(r#"{"id":"fixture"}"#), Some("{}"), Some("[]"), Some("null"), Some(""), Some("<html>unexpected page</html>"), None] {
            assert_eq!(crate::arriva_claim::tests::record_guard_outcome(&guard, body, false), "INDETERMINATE");
        }
        for body in [r#"{"errors":[],"submissionGuid":"fake-ok"}"#, r#"{"inlineMessage":"<p>We hebben je aanvraag ontvangen.</p>"}"#, "Thank you for your submission."] {
            assert_eq!(crate::arriva_claim::tests::record_guard_outcome(&guard, Some(body), false), "SUBMITTED");
        }
        guard.network_event("Network.loadingFailed", &json!({"requestId":"final"}));
        assert_eq!(crate::arriva_claim::tests::record_guard_outcome(&guard, Some(r#"{"success":true}"#), false), "INDETERMINATE");
    }

    #[test]
    fn changed_legal_checkbox_is_never_treated_as_the_consented_field() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let script = format!(
            r#"{HELPERS}
            const box=(name,required,checked)=>({{name,required,checked,getAttribute:()=>null}});
            const expected=box('LEGAL_CONSENT.subscription_type_3667673841',true,true);
            const changed=box('LEGAL_CONSENT.fixture',true,false);
            let boxes=[expected];
            global.document={{querySelectorAll:()=>boxes}};
            const assert=require('node:assert/strict');
            assert.equal(consentBox(),expected);
            assert.ok(consentOk(consentBox(),false));
            boxes=[changed];
            assert.equal(consentBox(),null);
            assert.ok(!consentOk(consentBox(),false));
            changed.required=false;
            assert.ok(consentOk(consentBox(),false));
            changed.checked=true;
            assert.ok(!consentOk(consentBox(),false));
            boxes=[expected,{{...expected}}];
            assert.ok(!consentOk(consentBox(),false));
            "#
        );
        let Ok(mut node) = Command::new("node").stdin(Stdio::piped()).spawn() else { return };
        node.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        assert!(node.wait().unwrap().success());
    }

    #[test]
    fn demo_form_data() {
        let f = demo_form("Limburg", 24).unwrap();
        assert_eq!((f.region.as_str(), f.delay_option.as_str()), ("Limburg", "15 - 30 minuten"));
        assert!(f.demo);
        assert!(form_js(&f).starts_with("const D={"));
        assert!(demo_form("Limburg", 3).is_err());
    }
    #[test]
    fn remote_navigation_restricted() {
        assert!(navigation(&URL.parse().unwrap()));
        assert!(!navigation(&"https://klantenservice.arriva.nl.evil.test/".parse().unwrap()));
        assert!(!navigation(&"http://klantenservice.arriva.nl/geldterug".parse().unwrap()));
    }
    #[test]
    fn delay_record_is_the_real_service() {
        let records: Value = serde_json::from_str(RAW).unwrap();
        assert_eq!(records[8]["Stop:Arrival delay"], "24");
        assert_eq!(records[0]["Service:Train number"], "732284");
        let rows: Vec<Value> = serde_json::from_str(RAW).unwrap();
        assert_eq!(arrival_delay(&rows, RDT_ALIGHT), Some(24));
        assert_eq!(DEMO_TRIPS[0].2, 24);
    }
    #[test]
    fn demo_trips_follow_arrivas_rules() {
        let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let today = d("2026-11-20");
        // The real-path trip is always 10 days ago (Amsterdam), at 23:25, arriving after midnight.
        let real = demo_journey();
        let local = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&chrono_tz::Europe::Amsterdam);
        assert_eq!(local(real.check_in).date_naive(), demo_real_date());
        assert_eq!(local(real.check_in).format("%H:%M").to_string(), "23:25");
        assert_eq!(local(real.check_out.unwrap()).format("%H:%M").to_string(), "00:49");
        assert_eq!(local(real.check_out.unwrap()).date_naive(), demo_real_date().succ_opt().unwrap());
        let real_today = demo_real_date() + chrono::Duration::days(10);
        let claim = demo_trip_status_on("DEMO-ARRIVA-732284", 24, real_today);
        assert_eq!(claim["status"], "claimable");
        assert_eq!(claim["region"], "Limburg");
        assert_eq!(claim["form_option"], "15 - 30 minuten");
        assert_eq!(claim["refund_percent"], 50);
        // € 6,20: half of the € 12,40 fare.
        assert_eq!(crate::carrier_policy::ARRIVA.refund_cents(Some("Limburg"), 24, real.fare_cents), Some((620, 50)));
        let short = demo_trip_status_on("DEMO-ARRIVA-SHORT", 9, today);
        assert_eq!(short["status"], "not_eligible");
        assert!(short["reason_en"].as_str().unwrap().contains("from 15 minutes"));
        assert_eq!(demo_trip_status_on("DEMO-ARRIVA-TESTED", 31, today)["status"], "tested");
        // After Arriva's three-month claim period the trip no longer qualifies.
        assert_eq!(demo_trip_status_on("DEMO-ARRIVA-732284", 24, real_today + chrono::Duration::days(100))["status"], "not_eligible");
    }
}
