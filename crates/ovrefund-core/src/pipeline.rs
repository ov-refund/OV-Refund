//! Adapter traits and the run pipeline:
//! lock → recover in-flight → fetch journeys → dedupe → evidence → policy →
//! reconcile → (opt-in) submit → notify → finish.
use crate::db::{ClaimRow, Db};
use crate::eligibility::{self, ClaimWindow, PolicyInput};
use crate::model::*;
use crate::schedule::ScheduleProvider;
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Europe::Amsterdam;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    Ok,
    LoginRequired,
    Blocked(String),
    Unknown,
}

impl AuthState {
    pub fn as_str(&self) -> String {
        match self {
            AuthState::Ok => "ok".into(),
            AuthState::LoginRequired => "login_required".into(),
            AuthState::Blocked(r) => format!("blocked: {r}"),
            AuthState::Unknown => "unknown".into(),
        }
    }
}

#[derive(Debug)]
pub enum FetchError {
    /// Session expired / login page shown. Never retried automatically.
    LoginRequired,
    /// Bot challenge, unexpected page, rate limit: stop, do not retry.
    /// `Blocked("verificatiecode")`: NS asked for a one-time code / extra check during auto-login.
    Blocked(String),
    /// The automatic login was refused (NS's login form came back with an error), e.g. after a
    /// password change. Never retried within the run.
    AutoLoginFailed,
    /// Logged in with a different NS account than the one this ledger's trips came from.
    AccountMismatch,
    Other(anyhow::Error),
}

/// `FetchError::Blocked` reason when NS asks for a verification code / extra check.
pub const BLOCKED_VERIFICATION: &str = "verificatiecode";
/// `FetchError::Blocked` reason when OV Refund no longer recognises the Mijn NS page.
pub const BLOCKED_SITE_CHANGED: &str = "NS-website gewijzigd / NS website changed";

/// Run errors and notification texts, "Nederlands / English" (the GUI and e-mails show one half).
pub const LOGIN_NEEDED_MSG: &str = "OV Refund moet opnieuw inloggen bij NS. Klik op Inloggen bij Mijn NS, of zet Automatisch inloggen aan. / OV Refund needs to log in to NS again. Click Log in to Mijn NS, or turn on automatic login.";
pub const VERIFICATION_MSG: &str =
    "NS vraagt om een extra controle. Open OV Refund en log zelf in. / NS asks for an extra check. Open OV Refund and log in yourself.";
pub const BLOCKED_MSG: &str = "NS hield de controle tegen met een extra beveiligingscontrole. Probeer het later opnieuw of log zelf in. / NS stopped the check with an extra security check. Try again later or log in yourself.";
pub const SITE_CHANGED_MSG: &str = "De NS-website is veranderd: OV Refund herkent Mijn NS niet meer. Er is niets verstuurd. Probeer het later opnieuw of wacht op een update van OV Refund. / The NS website changed: OV Refund no longer recognises Mijn NS. Nothing was sent. Try again later or wait for an OV Refund update.";
pub const AUTOLOGIN_FAILED_MSG: &str = "Automatisch inloggen bij NS lukte niet. Heb je je NS-wachtwoord gewijzigd? Vul het opnieuw in onder Instellingen → Account & NS → Automatisch inloggen bij NS. / Automatic login to NS failed. Did you change your NS password? Enter it again under Settings → Account & NS → Log in to NS automatically.";
pub const AUTOLOGIN_DISABLED_MSG: &str = "Automatisch inloggen bij NS lukte twee keer niet en staat nu uit. Vul je NS-wachtwoord opnieuw in onder Instellingen → Account & NS → Automatisch inloggen bij NS. / Automatic login to NS failed twice and is now off. Enter your NS password again under Settings → Account & NS → Log in to NS automatically.";
pub const ACCOUNT_MISMATCH_MSG: &str = "Je bent ingelogd met een ander NS-account dan eerder. Er is niets opgehaald of verstuurd. Log uit en log in met je eigen account, of kies Alles vergeten om opnieuw te beginnen. / You logged in with a different NS account than before. Nothing was fetched or sent. Log out and log in with your own account, or choose Forget everything to start over.";
/// The logged-in NS account could not be read from Mijn NS while an account is stored: a
/// submission is refused (it could go to someone else's account).
pub const ACCOUNT_UNKNOWN_MSG: &str = "OV Refund kon niet controleren met welk NS-account je bent ingelogd. Er is niets verstuurd. Probeer het later opnieuw. / OV Refund could not check which NS account you are logged in with. Nothing was sent. Try again later.";
pub const NS_NO_RESPONSE_MSG: &str = "Mijn NS reageerde niet (misschien onderhoud). OV Refund probeert het bij de volgende controle opnieuw. / Mijn NS did not respond (maybe maintenance). OV Refund will try again at the next check.";
/// Scheduled check while automatic login is off: NS is not contacted, the user is reminded.
pub const REMINDER_MSG: &str =
    "Tijd voor een controle: open OV Refund en log in bij Mijn NS. / Time for a check: open OV Refund and log in to Mijn NS.";

/// The user-facing message for a run's auth state (None when logged in or unknown).
pub fn auth_message(auth_state: &str) -> Option<&'static str> {
    Some(match auth_state {
        "login_required" => LOGIN_NEEDED_MSG,
        "autologin_failed" => AUTOLOGIN_FAILED_MSG,
        "account_mismatch" => ACCOUNT_MISMATCH_MSG,
        s if s == format!("blocked: {BLOCKED_VERIFICATION}") => VERIFICATION_MSG,
        s if s == format!("blocked: {BLOCKED_SITE_CHANGED}") => SITE_CHANGED_MSG,
        s if s.starts_with("blocked") => BLOCKED_MSG,
        _ => return None,
    })
}

/// "Nederlands / English" -> the half for the language (`nl` true = Dutch).
pub fn pick_lang(s: &str, nl: bool) -> &str {
    match s.split_once(" / ") {
        Some((a, b)) => {
            if nl {
                a
            } else {
                b
            }
        }
        None => s,
    }
}

pub trait JourneySource {
    fn id(&self) -> &str;
    fn fetch_journeys(&mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> std::result::Result<Vec<Journey>, FetchError>;
}

/// Status of a claim as reported by NS (Mijn teruggaven).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteClaim {
    pub reference: Option<String>,
    pub travel_date: Option<chrono::NaiveDate>,
    pub origin_uic: Option<String>,
    pub destination_uic: Option<String>,
    pub transaction_id: Option<String>,
    /// Mapped state: SUBMITTED / APPROVED / REJECTED / PAID
    pub state: Option<ClaimState>,
    /// Amount NS awarded/paid (cents), when NS shows it.
    #[serde(default)]
    pub amount_cents: Option<i64>,
    pub raw_status: String,
}

pub enum SubmitOutcome {
    /// NS acknowledged; reference if shown.
    Confirmed(SubmissionReceipt),
    /// Conclusively not sent (form validation failed before clicking submit, etc.).
    NotSent(String),
    /// Request may or may not have reached NS.
    Unknown(String),
}

pub trait ClaimGateway {
    fn id(&self) -> &str;
    /// True only once a real end-to-end submission has been verified for this gateway.
    fn verified_for_auto(&self) -> bool;
    /// Open and validate the claim form for this journey without submitting.
    fn preflight(&mut self, facts: &ClaimFacts) -> Result<Vec<String>>;
    fn submit(&mut self, facts: &ClaimFacts) -> SubmitOutcome;
    /// All claims NS lists for this account (None = not available).
    fn list_remote_claims(&mut self) -> Result<Option<Vec<RemoteClaim>>>;
}

pub trait Notifier {
    fn channel(&self) -> &str;
    fn notify(&mut self, summary: &RunSummary, lines: &[String]) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// Scan + assess only; submissions happen via explicit user action.
    Manual,
    /// Opted-in unattended submission of `supported_for_auto` claims.
    Automatic,
}

pub struct RunOptions {
    pub mode: RunMode,
    pub trigger: String,
    pub lookback_days: i64,
    pub window: ClaimWindow,
    pub now: DateTime<Utc>,
    /// Pause between two automatic submissions (one at a time; every check runs again after it).
    pub auto_pause: std::time::Duration,
    /// Automatic mode only: submit just the claims whose claim window ends within this time
    /// (a "Controleer nu" in the app submits urgent claims; the rest waits for the scheduled
    /// check). `None`: every eligible claim.
    pub auto_only_due_within: Option<Duration>,
    /// Re-checked from storage right before EACH automatic submission (opt-in still given, bank
    /// account still set, ...). `false` stops automatic submission for the rest of the run.
    pub auto_still_allowed: Option<Box<dyn Fn(&Db) -> bool>>,
    /// Is the NS session this run started with still the user's (no logout or "forget
    /// everything" since)? `false`: the run never records the NS login state (a stale run must
    /// not show "logged in" after a logout). `None`: always.
    pub same_session: Option<Box<dyn Fn(&Db) -> bool>>,
    /// Runs inside the run lock after NS's claim list was read and before any submission, e.g.
    /// reading NS decision e-mails so their outcome is known before submitting. Returns lines
    /// for the summary.
    pub before_submit: Option<Box<dyn Fn(&mut Db) -> Vec<String>>>,
    /// Runs inside the run lock at the very end of every run (also after an error or an expired
    /// login), before the summary is stored and sent: every write that belongs to the run (saving
    /// the session, account details, reading NS e-mails, ...) happens here, so nothing is written
    /// after the lock is released (e.g. after a logout or "forget everything" took the lock).
    /// Skipped when the lease was lost. Returns lines for the summary.
    pub after_run: Option<Box<dyn Fn(&mut Db, &RunSummary) -> Vec<String>>>,
    /// The run's clock. `None`: `now` plus the real time since the run started. Tests pass a fake
    /// clock (claim windows and the time budget are measured with it).
    pub clock: Option<Box<dyn Fn() -> DateTime<Utc>>>,
    /// No new automatic submission starts once this much time has passed since the run started
    /// (Task Scheduler stops a check after 2 hours). The rest waits for the next check.
    pub auto_time_budget: Duration,
    /// Run-lock owner to use. The caller may already hold the lock under this owner (taken before
    /// opening the NS window, so a second process stops right away). `None`: a new owner.
    pub lock_owner: Option<String>,
    /// Is the process behind this run-lock owner still running? A claim another check is still
    /// submitting (its lease ran out, e.g. after sleep) is then not marked uncertain yet.
    /// `None`: never.
    pub owner_alive: Option<Box<dyn Fn(&str) -> bool>>,
}

/// Setting prefix (+ claim id) for a submission in progress: "<time>|<run-lock owner>". Set right
/// before the claim goes to NS and removed once its outcome is recorded.
pub const INFLIGHT_KEY_PREFIX: &str = "submitting_owner:";

/// A submission whose check is still running is left alone for at most this long (Task
/// Scheduler ends a check after 2 hours).
pub const INFLIGHT_GRACE_HOURS: i64 = 2;

/// Claims in SUBMITTING that a still running check (another run-lock owner whose process is alive,
/// see [`RunOptions::owner_alive`]) started less than [`INFLIGHT_GRACE_HOURS`] ago.
pub fn inflight_elsewhere(db: &Db, own: &str, alive: &dyn Fn(&str) -> bool, now: DateTime<Utc>) -> Result<Vec<i64>> {
    let mut out = vec![];
    for c in db.claims_in(&[ClaimState::Submitting])? {
        let Some(v) = db.get_setting(&format!("{INFLIGHT_KEY_PREFIX}{}", c.id))? else { continue };
        let Some((t, owner)) = v.split_once('|') else { continue };
        let recent =
            DateTime::parse_from_rfc3339(t).ok().is_some_and(|t| now - t.with_timezone(&Utc) < Duration::hours(INFLIGHT_GRACE_HOURS));
        if owner != own && recent && alive(owner) {
            out.push(c.id);
        }
    }
    Ok(out)
}

/// Re-read NS's claim list before a submission when the last read is older than this (it is
/// also re-read after every submission that reached NS).
pub const CLAIM_LIST_MAX_AGE_MINUTES: i64 = 5;

/// Setting with the last "stop automatic submission" request: "<time>|owner:<run-lock owner>".
/// It is written without the run lock (stopping is always safe) and names the run that held the
/// lock at that moment (also in another process): only that run stops (no new submission starts,
/// a submission in progress completes). A stop without a live run is not stored, so a stop can
/// never affect a later run (also not after the history was wiped, a backup was restored or the
/// clock changed). Older formats ("<time>" or "<time>|<run id>") stop nothing.
pub const AUTO_STOP_KEY: &str = "auto_stop_requested_at";

/// Setting with the run-lock owner of a run that is in its automatic-submission phase (from the
/// first automatic submission step until it releases the lock). The app waits for such a run
/// after a stop; any other busy run is refused right away.
pub const AUTO_PHASE_KEY: &str = "auto_phase_owner";

/// Why automatic submission was stopped (stored with the stop, shown after the run).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The user pressed Stop.
    User,
    /// The user logged out of NS.
    LoggedOut,
    /// The bank account was removed.
    BankRemoved,
    /// Automatic submission was switched off (also: the claim-service answer "yes").
    AutoOff,
    /// "Forget everything" (or the uninstaller) erases the data.
    Erased,
}

impl StopReason {
    fn code(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::LoggedOut => "logout",
            Self::BankRemoved => "bank",
            Self::AutoOff => "auto_off",
            Self::Erased => "erase",
        }
    }
    fn from_code(code: &str) -> Self {
        match code {
            "logout" => Self::LoggedOut,
            "bank" => Self::BankRemoved,
            "auto_off" => Self::AutoOff,
            "erase" => Self::Erased,
            _ => Self::User,
        }
    }
    /// The paused reason after the run ("NL / EN").
    pub fn reason(self) -> &'static str {
        match self {
            Self::User => PAUSED_STOPPED_REASON,
            Self::LoggedOut => "Gestopt: je bent uitgelogd bij NS. / Stopped: you logged out of NS.",
            Self::BankRemoved => "Gestopt: je rekeningnummer is verwijderd. / Stopped: your bank account was removed.",
            Self::AutoOff => "Gestopt: automatisch indienen is uitgezet. / Stopped: automatic submission was switched off.",
            Self::Erased => "Gestopt: je gegevens worden gewist. / Stopped: your data is being erased.",
        }
    }
    /// The run line ("NL / EN").
    pub fn line(self) -> &'static str {
        match self {
            Self::User => AUTO_STOPPED_LINE,
            other => other.reason(),
        }
    }
}

/// Ask the running check (this or another process) to stop submitting automatically (after the
/// current claim). Nothing is stored when no check holds the run lock.
pub fn request_auto_stop(db: &Db, reason: StopReason) -> Result<()> {
    request_auto_stop_with(db, &|_| false, reason)
}

/// [`request_auto_stop`] that also reaches a check whose lease ran out (e.g. after sleep) while
/// its process (`alive`) is still running: such a check renews its lease and goes on.
pub fn request_auto_stop_with(db: &Db, alive: &dyn Fn(&str) -> bool, reason: StopReason) -> Result<()> {
    let owner = match db.live_run_lock_owner()? {
        Some(o) => Some(o),
        None => db.run_lock_owner()?.filter(|o| alive(o)),
    };
    match owner {
        // The owner stays last: the run removes its own stop by that suffix.
        Some(owner) => db.set_setting(AUTO_STOP_KEY, &format!("{}|reason:{}|owner:{owner}", Utc::now().to_rfc3339(), reason.code())),
        None => Ok(()),
    }
}

/// Was a stop requested for the run holding the run lock as `owner`?
pub fn auto_stop_requested(db: &Db, owner: &str) -> Result<bool> {
    Ok(auto_stop_reason(db, owner)?.is_some())
}

/// Why a stop was requested for the run holding the run lock as `owner` (`None`: no stop).
pub fn auto_stop_reason(db: &Db, owner: &str) -> Result<Option<StopReason>> {
    let Some(v) = db.get_setting(AUTO_STOP_KEY)? else { return Ok(None) };
    let Some((head, o)) = v.split_once("|owner:") else { return Ok(None) };
    if o != owner {
        return Ok(None);
    }
    Ok(Some(head.split_once("|reason:").map(|(_, c)| StopReason::from_code(c)).unwrap_or(StopReason::User)))
}

/// Remove a stored stop request (history wiped, backup restored, automatic submission switched on).
pub fn clear_auto_stop(db: &Db) -> Result<()> {
    db.conn.execute("DELETE FROM settings WHERE key=?1", [AUTO_STOP_KEY])?;
    Ok(())
}

/// Is a run (this or another process) holding the run lock in its automatic-submission phase?
pub fn auto_phase_active(db: &Db) -> Result<bool> {
    let Some(owner) = db.live_run_lock_owner()? else { return Ok(false) };
    Ok(db.get_setting(AUTO_PHASE_KEY)?.as_deref() == Some(owner.as_str()))
}

/// Error that ends a run early because the user stopped automatic submission during the sync.
#[derive(Debug)]
struct StoppedByUser;
impl std::fmt::Display for StoppedByUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(AUTO_STOPPED_LINE)
    }
}
impl std::error::Error for StoppedByUser {}

/// "Gepauzeerd: n claim(s) met een onzekere uitkomst. / Paused: n claim(s) with an uncertain outcome."
pub fn paused_uncertain_reason(n: usize) -> String {
    if n == 1 {
        "Gepauzeerd: 1 claim met een onzekere uitkomst. / Paused: 1 claim with an uncertain outcome.".into()
    } else {
        format!("Gepauzeerd: {n} claims met een onzekere uitkomst. / Paused: {n} claims with an uncertain outcome.")
    }
}

/// Automatic submission paused because NS's claim list could not be read.
pub const PAUSED_CLAIM_LIST_REASON: &str = "Gepauzeerd: het claimoverzicht van NS was niet beschikbaar. De volgende controle probeert het opnieuw. / Paused: the NS claim list was not available. The next check tries again.";

/// Why automatic submission ended early (code "stopped"): the user pressed Stop.
pub const PAUSED_STOPPED_REASON: &str = "Gestopt op jouw verzoek. / Stopped at your request.";
/// Why automatic submission ended early (code "budget"): the time budget ran out.
pub const PAUSED_BUDGET_REASON: &str = "Gestopt: de controle duurde te lang. De rest volgt bij de volgende controle. / Stopped: the check took too long. The rest follows at the next check.";

/// Record that automatic submission was stopped (run line and paused reason, see [`StopReason`]).
fn note_stopped(db: &Db, owner: &str, s: &mut RunSummary, lines: &mut Vec<String>) {
    let reason = auto_stop_reason(db, owner).ok().flatten().unwrap_or(StopReason::User);
    lines.push(reason.line().into());
    s.auto_paused_reason = Some(reason.reason().into());
    s.auto_paused_code = Some("stopped".into());
}

/// Run line when the user stopped automatic submission.
pub const AUTO_STOPPED_LINE: &str = "Automatisch indienen is op jouw verzoek gestopt. / Automatic submission stopped at your request.";
/// Run line when NS's claim list could not be read again during automatic submission.
pub const CLAIM_LIST_LOST_LINE: &str = "Het claimoverzicht van NS was niet meer beschikbaar. Automatisch indienen is gestopt. / The NS claim list was no longer available. Automatic submission stopped.";

/// Run line when the time budget ran out with `n` claims still in the queue.
pub fn time_budget_line(n: usize) -> String {
    if n == 1 {
        "Tijdslimiet bereikt: 1 claim wacht op de volgende controle. / Time limit reached: 1 claim waits for the next check.".into()
    } else {
        format!(
            "Tijdslimiet bereikt: {n} claims wachten op de volgende controle. / Time limit reached: {n} claims wait for the next check."
        )
    }
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            mode: RunMode::Manual,
            trigger: "manual".into(),
            lookback_days: 100,
            window: ClaimWindow::default(),
            now: Utc::now(),
            auto_pause: std::time::Duration::from_secs(60),
            auto_only_due_within: None,
            auto_still_allowed: None,
            same_session: None,
            before_submit: None,
            after_run: None,
            clock: None,
            auto_time_budget: Duration::minutes(90),
            lock_owner: None,
            owner_alive: None,
        }
    }
}

/// Claims an automatic run would consider: READY claims and FAILED ones (conclusively not sent)
/// with < 3 attempts, with automatic-grade evidence and an open claim window. Order: claims whose
/// window ends within [`URGENT_DAYS`] (soonest first, also retries), then READY, then FAILED. An
/// earlier attempt that may have reached NS is never retried unattended. (All other gates —
/// consent, bank account, NS claim list, nothing unresolved — apply on top.)
pub fn auto_queue(db: &Db, now: DateTime<Utc>) -> Result<Vec<(DateTime<Utc>, ClaimRow)>> {
    let mut queue: Vec<(DateTime<Utc>, ClaimRow)> = vec![];
    for c in db.claims_in(&[ClaimState::Ready, ClaimState::Failed])? {
        if c.status == ClaimState::Failed && c.attempts >= 3 {
            continue;
        }
        if db.was_uncertain(c.id)? {
            continue;
        }
        if let Some((conf, _, _, exp)) = db.assessment(c.journey_id)? {
            if conf == Confidence::SupportedForAuto && exp >= now {
                queue.push((exp, c));
            }
        }
    }
    // Urgent claims first (a retry near its deadline is not starved by new claims); otherwise
    // READY first: a retry that keeps failing never holds up the others.
    let urgent = now + Duration::days(URGENT_DAYS);
    queue.sort_by_key(|(exp, c)| (*exp > urgent, *exp > urgent && c.status == ClaimState::Failed, *exp));
    Ok(queue)
}

/// A claim whose window ends within this many days goes first in an automatic check.
pub const URGENT_DAYS: i64 = 4;

/// Setting prefix (+ claim id): the last automatic attempt of this claim was refused by the final
/// check before sending (nothing reached NS), e.g. evidence no longer strong enough. Such a claim
/// is not a retryable failure: no extra check before its deadline is planned for it. Removed when
/// a later attempt passes the final check.
pub const AUTO_REFUSED_KEY_PREFIX: &str = "auto_refused:";

/// Was the last automatic attempt of this claim refused by the final check?
pub fn auto_refused(db: &Db, claim_id: i64) -> Result<bool> {
    Ok(db.get_setting(&format!("{AUTO_REFUSED_KEY_PREFIX}{claim_id}"))?.is_some())
}

/// A submission refused by the final check before sending: nothing reached NS. Its text is the
/// bilingual reason.
#[derive(Debug)]
pub struct Refusal(pub String);
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Refusal {}

/// `Err(Refusal(msg))` unless `ok`.
fn refuse_unless(ok: bool, msg: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Refusal(msg.to_string()).into())
    }
}

/// When to run one extra check before a claim deadline: the earliest deadline among
/// `deadlines` that falls before the next regular check + 3 days gets a check at 10:00 (Dutch
/// time) 4 days before its last day (never within the next hour). Deadlines within the next hour
/// are left out (too close for an extra check); they never take the check away from the others.
pub fn deadline_run_at(now: DateTime<Utc>, next_regular: DateTime<Utc>, deadlines: &[DateTime<Utc>]) -> Option<DateTime<Utc>> {
    let d = deadlines.iter().copied().filter(|d| *d < next_regular + Duration::days(3) && *d > now + Duration::hours(1)).min()?;
    let at = local_ten_am(d.with_timezone(&Amsterdam).date_naive() - Duration::days(4)).max(now + Duration::hours(1));
    (at < d).then_some(at)
}

/// 10:00 Dutch time on `day`, as UTC (DST-safe: 10:00 always exists and is never ambiguous).
pub fn local_ten_am(day: chrono::NaiveDate) -> DateTime<Utc> {
    use chrono::TimeZone;
    let t = day.and_hms_opt(10, 0, 0).unwrap_or_default();
    Amsterdam.from_local_datetime(&t).earliest().map(|x| x.with_timezone(&Utc)).unwrap_or_else(|| Utc.from_utc_datetime(&t))
}

pub fn local_hhmm(t: DateTime<Utc>) -> String {
    t.with_timezone(&Amsterdam).format("%H:%M").to_string()
}

/// Build the exact facts for the NS form from stored evidence.
pub fn build_facts(j: &Journey, band: DelayBand, sched_dep: DateTime<Utc>) -> Option<ClaimFacts> {
    Some(ClaimFacts {
        source_record_id: j.source_record_id.clone(),
        coupling_id: j.coupling_id.clone()?,
        check_in: j.check_in,
        check_out: j.check_out?,
        origin_name: j.origin.name.clone(),
        destination_name: j.destination.as_ref()?.name.clone(),
        scheduled_departure_local: local_hhmm(sched_dep),
        delay_band: band,
        reached_destination: true,
    })
}

/// Assess one journey and move its ledger entry accordingly. Never touches
/// claims that are in flight or done.
pub fn assess_and_track(
    db: &mut Db,
    sched: &dyn ScheduleProvider,
    gateway_id: &str,
    jid: i64,
    j: &Journey,
    opts: &RunOptions,
) -> Result<Assessment> {
    let existing = db.claim_for_journey(jid)?;
    let already = existing.as_ref().map(|c| c.status.blocks_new_submission()).unwrap_or(false) || db.has_external_claim(j)?;
    let ev = sched.evidence(db, j)?;
    db.save_evidence(jid, &ev)?;
    let mut a =
        eligibility::assess(&PolicyInput { journey: j, evidence: &ev, now: opts.now, window: opts.window, already_claimed: already });
    // An exact refund (from the trip's own fare) below NS's EUR 2.30 minimum is never paid out.
    if a.confidence != Confidence::NotClaimableOrOutOfScope
        && crate::compensation::expected(j, a.delay_band, None).map(|c| c.exact && c.cents == 0).unwrap_or(false)
    {
        a.confidence = Confidence::NotClaimableOrOutOfScope;
        a.reasons.insert(0, eligibility::BELOW_MINIMUM_REASON.into());
    }
    db.save_assessment(jid, &a)?;
    if already {
        if existing.is_none() || existing.as_ref().map(|c| !c.status.blocks_new_submission()).unwrap_or(false) {
            // external claim recorded by user → ledger entry
            let c = db.ensure_claim(jid, gateway_id, ClaimState::Discovered)?;
            if !c.status.blocks_new_submission() {
                db.mark_external(c.id, "import")?;
            }
        }
        return Ok(a);
    }
    // Evidence not yet available: track the journey, but don't open a claim yet. An open
    // review/ready entry created earlier goes back to CANDIDATE (re-assessed when data arrives).
    if ev.status == EvidenceStatus::Pending {
        if let Some(c) = existing.filter(|c| matches!(c.status, ClaimState::NeedsReview | ClaimState::Ready)) {
            db.transition(c.id, Some(c.status), ClaimState::Candidate, "policy", "Wacht op treintijden / Awaiting timetable")?;
        }
        return Ok(a);
    }
    let target = match a.confidence {
        Confidence::SupportedForAuto => Some(ClaimState::Ready),
        Confidence::NeedsReview => Some(ClaimState::NeedsReview),
        Confidence::NotClaimableOrOutOfScope => None,
    };
    match (existing, target) {
        (None, Some(t)) => {
            let c = db.ensure_claim(jid, gateway_id, ClaimState::Candidate)?;
            db.transition(c.id, Some(ClaimState::Candidate), t, "policy", &a.reasons.join("; "))?;
        }
        (Some(c), Some(t)) if c.status != t && matches!(c.status, ClaimState::Candidate | ClaimState::NeedsReview | ClaimState::Ready) => {
            // Don't downgrade a user-approved READY to NEEDS_REVIEW silently if evidence is just pending.
            if !(c.status == ClaimState::Ready && t == ClaimState::NeedsReview && ev.status == EvidenceStatus::Pending) {
                db.transition(c.id, Some(c.status), t, "policy", &a.reasons.join("; "))?;
            }
        }
        (Some(c), None) if matches!(c.status, ClaimState::Candidate | ClaimState::NeedsReview | ClaimState::Ready | ClaimState::Failed) => {
            db.transition(
                c.id,
                Some(c.status),
                ClaimState::Dismissed,
                "policy",
                &crate::db::bilingual_detail("Niet meer te claimen: ", "No longer claimable: ", &a.reasons.join("; ")),
            )?;
        }
        _ => {}
    }
    Ok(a)
}

/// Submit one READY claim (manual approval or automatic). Journals SUBMITTING first.
pub fn submit_claim(
    db: &mut Db,
    gw: &mut dyn ClaimGateway,
    claim_id: i64,
    mode: AuthorizedMode,
    facts_override: Option<ClaimFacts>,
) -> Result<ClaimRow> {
    submit_claim_inner(db, gw, claim_id, mode, facts_override, None, Utc::now())
}

/// [`submit_claim`] with an explicit current time (tests use fixed dates).
pub fn submit_claim_at(
    db: &mut Db,
    gw: &mut dyn ClaimGateway,
    claim_id: i64,
    mode: AuthorizedMode,
    facts_override: Option<ClaimFacts>,
    now: DateTime<Utc>,
) -> Result<ClaimRow> {
    submit_claim_inner(db, gw, claim_id, mode, facts_override, None, now)
}

/// Production submissions keep the exact lease acquired by their caller.
pub fn submit_claim_under_lock(
    db: &mut Db,
    gw: &mut dyn ClaimGateway,
    claim_id: i64,
    mode: AuthorizedMode,
    facts_override: Option<ClaimFacts>,
    owner: &str,
    now: DateTime<Utc>,
) -> Result<ClaimRow> {
    submit_claim_inner(db, gw, claim_id, mode, facts_override, Some(owner), now)
}

fn submit_claim_inner(
    db: &mut Db,
    gw: &mut dyn ClaimGateway,
    claim_id: i64,
    mode: AuthorizedMode,
    facts_override: Option<ClaimFacts>,
    owner: Option<&str>,
    now: DateTime<Utc>,
) -> Result<ClaimRow> {
    let c = db.claim(claim_id)?;
    let j = db.journey(c.journey_id)?;
    // The final boundary checks current evidence and external imports in every mode, including
    // a manual send from a review dialog opened before the user added an external claim.
    refuse_unless(!db.has_external_claim(&j)?, CLAIMED_ELSEWHERE_MSG)?;
    let Some((conf, band, sdep, exp)) = db.assessment(c.journey_id)? else { return Err(Refusal(NOT_ASSESSED_MSG.into()).into()) };
    refuse_unless(
        conf != Confidence::NotClaimableOrOutOfScope,
        "Deze reis is niet claimbaar. Er is niets verstuurd. / This trip is not claimable. Nothing was sent.",
    )?;
    refuse_unless(
        now <= exp && now <= eligibility::expires_at(j.check_in, ClaimWindow::default()),
        "De claimtermijn is verlopen. Er is niets verstuurd. / The claim window has expired. Nothing was sent.",
    )?;
    refuse_unless(
        now - j.check_out.unwrap_or(j.check_in).max(j.check_in) >= Duration::hours(ClaimWindow::default().min_age_hours),
        "NS accepteert claims pas 24 uur na de reis. Er is niets verstuurd. / NS accepts claims only 24 hours after travel. Nothing was sent.",
    )?;
    let Some(band) = band else { return Err(Refusal(NO_DELAY_BAND_MSG.into()).into()) };
    let Some(sdep) = sdep else { return Err(Refusal(NO_DEPARTURE_MSG.into()).into()) };
    let Some(facts) = build_facts(&j, band, sdep) else { return Err(Refusal(TRIP_INCOMPLETE_MSG.into()).into()) };
    refuse_unless(
        facts_override.as_ref().map(|f| f == &facts).unwrap_or(true),
        "De reisgegevens zijn gewijzigd. Bekijk de claim opnieuw. / The trip details changed. Review the claim again.",
    )?;
    if mode == AuthorizedMode::Automatic {
        refuse_unless(conf == Confidence::SupportedForAuto, AUTO_NEEDS_EVIDENCE_MSG)?;
        refuse_unless(gw.verified_for_auto(), AUTO_NOT_VERIFIED_MSG)?;
        refuse_unless(!db.was_uncertain(claim_id)?, AUTO_UNCERTAIN_MSG)?;
    }
    if let Some(owner) = owner {
        db.renew_run_lock(owner, Duration::minutes(30))?;
    }
    // Which check is submitting this claim (see [`inflight_elsewhere`]); none without an owner.
    let inflight_key = format!("{INFLIGHT_KEY_PREFIX}{claim_id}");
    match owner {
        Some(o) => db.set_setting(&inflight_key, &format!("{}|{o}", Utc::now().to_rfc3339()))?,
        None => clear_setting(db, &inflight_key),
    }
    // Once more right before sending: a "claimed elsewhere" entry added meanwhile (it is stored
    // without the run lock) is never sent.
    if db.has_external_claim(&j)? {
        clear_setting(db, &inflight_key);
        return Err(Refusal(CLAIMED_ELSEWHERE_MSG.into()).into());
    }
    // Passed the final check: an earlier refusal of this claim is history.
    clear_setting(db, &format!("{AUTO_REFUSED_KEY_PREFIX}{claim_id}"));
    if let Err(e) = db.begin_submission(claim_id, &facts, mode) {
        clear_setting(db, &inflight_key);
        return Err(e);
    }
    let outcome = gw.submit(&facts);
    // Record what happened first: the outcome is a fact about NS, whatever happened to the lock.
    let recorded = record_outcome(db, claim_id, outcome);
    clear_setting(db, &inflight_key);
    if let Some(owner) = owner {
        if let Err(e) = db.renew_run_lock(owner, Duration::minutes(30)) {
            log::warn!("renewing the run lock after a submission failed: {e:#}");
        }
    }
    recorded
}

/// Remove a setting (best effort).
fn clear_setting(db: &Db, key: &str) {
    let _ = db.conn.execute("DELETE FROM settings WHERE key=?1", [key]);
}

/// Journal the gateway's answer for a claim in SUBMITTING.
fn record_outcome(db: &mut Db, claim_id: i64, outcome: SubmitOutcome) -> Result<ClaimRow> {
    match outcome {
        SubmitOutcome::Confirmed(r) => db.complete_submission(claim_id, &r),
        // History notes are "NL / EN"; a bilingual reason is split over the two halves.
        SubmitOutcome::NotSent(reason) => db.transition(
            claim_id,
            Some(ClaimState::Submitting),
            ClaimState::Failed,
            "gateway",
            &crate::db::bilingual_detail("Niet verzonden: ", "Not sent: ", &reason),
        ),
        SubmitOutcome::Unknown(reason) => db.transition(
            claim_id,
            Some(ClaimState::Submitting),
            ClaimState::Indeterminate,
            "gateway",
            &crate::db::bilingual_detail("Uitkomst onzeker: ", "Outcome uncertain: ", &reason),
        ),
    }
}

/// Refusals of a submission without the needed facts, and of an automatic submission (run errors
/// as "claim #N: NL / EN").
pub const CLAIMED_ELSEWHERE_MSG: &str =
    "Deze reis is al elders geclaimd. Er is niets verstuurd. / This trip is already claimed elsewhere. Nothing was sent.";
pub const NOT_ASSESSED_MSG: &str =
    "Deze reis is nog niet gecontroleerd. Er is niets verstuurd. / This trip has not been checked yet. Nothing was sent.";
pub const NO_DELAY_BAND_MSG: &str = "De vertraging van deze reis is niet duidelijk genoeg. Er is niets verstuurd. / The delay of this trip is not clear enough. Nothing was sent.";
pub const NO_DEPARTURE_MSG: &str = "De geplande vertrektijd van deze reis is onbekend. Er is niets verstuurd. / The scheduled departure of this trip is unknown. Nothing was sent.";
pub const TRIP_INCOMPLETE_MSG: &str = "Bij deze reis ontbreekt de uitcheck, het eindstation of de gekoppelde kaart. Er is niets verstuurd. / This trip has no check-out, destination or linked card. Nothing was sent.";
pub const AUTO_NEEDS_EVIDENCE_MSG: &str = "Niet automatisch ingediend: het bewijs voor de vertraging is niet sterk genoeg. Beoordeel deze reis zelf. / Not submitted automatically: the evidence for the delay is not strong enough. Review this trip yourself.";
pub const AUTO_NOT_VERIFIED_MSG: &str = "Niet automatisch ingediend: automatisch indienen staat uit. Dien deze reis zelf in. / Not submitted automatically: automatic submission is off. Submit this trip yourself.";
pub const AUTO_UNCERTAIN_MSG: &str = "Niet automatisch ingediend: een eerdere indiening van deze claim is misschien al bij NS aangekomen. Kijk in Mijn teruggaven. / Not submitted automatically: an earlier submission of this claim may already have reached NS. Check Mijn teruggaven.";

/// Setting with the outcome of the last NS claim-list read: "ok" or "unavailable".
pub const CLAIM_LIST_STATUS_KEY: &str = "claim_list_status";

/// Bilingual refusal for re-sending a claim whose earlier submission may have reached NS.
pub const RESUBMIT_RISK_MSG: &str = "Deze claim is misschien al bij NS aangekomen (de uitkomst was onzeker). Opnieuw indienen kan een dubbele claim geven. Kijk eerst in Mijn NS → Mijn teruggaven. Bevestig daarna dat je toch opnieuw wilt indienen. / This claim may already have reached NS (the outcome was uncertain). Submitting again can create a duplicate claim. Check Mijn NS → Mijn teruggaven first. Then confirm that you want to submit it again.";

/// Run error when the Mijn NS account has no personal OV-chipkaart (no travel history to read).
/// "Nederlands / English": the GUI shows one half, e-mails pick the half for their language.
pub const NO_CARD_ERROR: &str = "Er is geen persoonlijke OV-chipkaart aan je Mijn NS-account gekoppeld. Koppel je kaart in Mijn NS (https://www.ns.nl/mijnns, Gegevens en voorkeuren). OVpay/bankpas en anonieme kaarten worden niet ondersteund. / No personal OV-chipkaart is linked to your Mijn NS account. Link your card in Mijn NS (https://www.ns.nl/mijnns, Gegevens en voorkeuren). OVpay/bank cards and anonymous cards are not supported.";

/// Bilingual refusal for submitting a trip NS may already have a claim for (see
/// [`Db::ns_claim_near_keys`]) without the user's explicit acknowledgement.
pub const NEAR_CLAIM_MSG: &str = "NS heeft rond deze datum mogelijk al een claim voor deze reis (in Mijn teruggaven of in een NS-mail). Kijk eerst in Mijn NS → Mijn teruggaven. Bevestig daarna dat het om een andere reis gaat. / NS may already have a claim for this trip around this date (in Mijn teruggaven or in an NS e-mail). Check Mijn NS → Mijn teruggaven first. Then confirm that it is a different trip.";

/// Move a claim the user reviewed to READY for a manual submission. A claim whose earlier
/// submission may have reached NS ([`Db::was_uncertain`]) is refused unless the user explicitly
/// confirmed the resubmission (`confirm_resubmit`); a trip flagged as "NS may already have a
/// claim around this date" ([`Db::is_ns_claim_near`]) needs `confirm_near`.
pub fn approve_for_manual_submission(db: &mut Db, claim_id: i64, confirm_resubmit: bool, confirm_near: bool) -> Result<()> {
    let c = db.claim(claim_id)?;
    if !matches!(c.status, ClaimState::Ready | ClaimState::NeedsReview | ClaimState::Failed | ClaimState::Candidate) {
        anyhow::bail!(
            "Deze claim kan niet (meer) worden ingediend (status {0}). / This claim cannot be submitted (any more) (status {0}).",
            c.status.as_str()
        );
    }
    if db.was_uncertain(claim_id)? && !confirm_resubmit {
        anyhow::bail!(RESUBMIT_RISK_MSG);
    }
    if !confirm_near && db.is_ns_claim_near(&db.journey(c.journey_id)?)? {
        anyhow::bail!(NEAR_CLAIM_MSG);
    }
    if c.status != ClaimState::Ready {
        let detail = if confirm_resubmit {
            "Door jou bekeken en goedgekeurd (opnieuw indienen bevestigd) / Reviewed and approved by you (resubmission confirmed)"
        } else {
            "Door jou bekeken en goedgekeurd / Reviewed and approved by you"
        };
        db.transition(c.id, Some(c.status), ClaimState::Ready, "user", detail)?;
    }
    Ok(())
}

/// Duplicate guard for claims that are still OPEN locally (READY / NEEDS_REVIEW / FAILED /
/// CANDIDATE), used in every run mode: a claim NS lists for exactly this trip (transaction id or
/// NS reference) is marked EXTERNALLY_CLAIMED. A claim NS lists only around the same date changes
/// nothing, but the trip's natural key ([`Journey::natural_key`]) is returned: such a trip is
/// never submitted automatically and the GUI asks for an acknowledgement before a manual one.
pub fn reconcile_open(db: &mut Db, remote: &[RemoteClaim]) -> Result<(Vec<String>, Vec<String>)> {
    let mut lines = vec![];
    let mut near = vec![];
    for c in db.claims_in(&[ClaimState::Ready, ClaimState::NeedsReview, ClaimState::Failed, ClaimState::Candidate])? {
        let j = db.journey(c.journey_id)?;
        let day = j.check_in.with_timezone(&Amsterdam).format("%d-%m-%Y");
        match match_remote_kind(db, remote, &j, c.ns_reference.as_deref())? {
            Some((r, MatchKind::Exact)) => {
                let raw = r.raw_status.replace(" / ", "/");
                let detail = if db.was_uncertain(c.id)? {
                    format!("Staat in Mijn teruggaven ({raw}); mogelijk de eerdere onzekere indiening van OV Refund / Listed in Mijn teruggaven ({raw}); possibly OV Refund's earlier uncertain submission")
                } else {
                    format!("Staat in Mijn teruggaven ({raw}) / Listed in Mijn teruggaven ({raw})")
                };
                db.mark_external_detail(c.id, "reconcile", &detail)?;
                if let Some(rf) = &r.reference {
                    db.set_ns_reference(c.id, rf)?;
                }
                lines.push(format!("{day}: already listed at NS — will not be submitted again"));
            }
            _ => {
                if remote_claim_near(db, remote, &j)? {
                    near.push(j.natural_key());
                }
            }
        }
    }
    Ok((lines, near))
}

/// Match NS's claim list against local SUBMITTED / INDETERMINATE claims.
/// How a remote NS claim was linked to a local journey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// transaction id or NS reference: certain
    Exact,
    /// only the travel date (unambiguous on both sides): plausible, never proof of submission
    Date,
}

/// Find the NS-side claim for a local journey, in strict priority: transaction id, then NS
/// reference, then travel date — the date only when it is unambiguous on BOTH sides
/// (exactly one NS journey locally and exactly one remote claim around that day).
pub fn match_remote<'a>(db: &Db, remote: &'a [RemoteClaim], j: &Journey, reference: Option<&str>) -> Result<Option<&'a RemoteClaim>> {
    Ok(match_remote_kind(db, remote, j, reference)?.map(|(r, _)| r))
}

pub fn match_remote_kind<'a>(
    db: &Db,
    remote: &'a [RemoteClaim],
    j: &Journey,
    reference: Option<&str>,
) -> Result<Option<(&'a RemoteClaim, MatchKind)>> {
    if let Some(r) = remote.iter().find(|r| r.transaction_id.as_deref() == Some(j.source_record_id.as_str())) {
        return Ok(Some((r, MatchKind::Exact)));
    }
    if let Some(rf) = reference {
        if let Some(r) = remote.iter().find(|r| r.reference.as_deref().is_some_and(|x| same_reference(x, rf))) {
            return Ok(Some((r, MatchKind::Exact)));
        }
    }
    let date = j.check_in.with_timezone(&Amsterdam).date_naive();
    let same_day: Vec<&RemoteClaim> = remote.iter().filter(|r| r.travel_date == Some(date)).collect();
    if same_day.len() != 1 {
        return Ok(None);
    }
    let day = date.to_string();
    // Count ALL local journeys around that day (not only those with a local claim): a trip the
    // user filed elsewhere has no local claim but could be the one NS lists.
    let local_same_day: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM journeys jj
         WHERE substr(jj.check_in_utc,1,10) BETWEEN date(?1,'-1 day') AND date(?1,'+1 day')",
        [&day],
        |r| r.get(0),
    )?;
    Ok(if local_same_day == 1 { Some((same_day[0], MatchKind::Date)) } else { None })
}

/// Does NS list a claim around this journey's travel date that is not already tied to ANOTHER
/// local trip by transaction id or reference? (conservative duplicate guard)
pub fn remote_claim_near(db: &Db, remote: &[RemoteClaim], j: &Journey) -> Result<bool> {
    let date = j.check_in.with_timezone(&Amsterdam).date_naive();
    for r in remote.iter().filter(|r| r.travel_date.map(|d| (d - date).num_days().abs() <= 1).unwrap_or(false)) {
        let by_transaction: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM journeys jj WHERE jj.source_record_id <> ?2 AND ?1 IS NOT NULL AND jj.source_record_id = ?1",
            rusqlite::params![r.transaction_id, j.source_record_id],
            |x| x.get(0),
        )?;
        let by_reference = match r.reference.as_deref() {
            Some(rf) => {
                let mut st = db.conn.prepare(
                    "SELECT cl.ns_reference FROM claims cl JOIN journeys jj ON jj.id = cl.journey_id
                     WHERE jj.source_record_id <> ?1 AND cl.ns_reference IS NOT NULL",
                )?;
                let refs = st.query_map([&j.source_record_id], |x| x.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                refs.iter().any(|ours| same_reference(ours, rf))
            }
            None => false,
        };
        if by_transaction == 0 && !by_reference {
            // Live-test aid: a claim this installation filed around that day whose reference NS
            // lists in another form keeps the other trip waiting for the user (fail-safe).
            let ours_same_days: i64 = db.conn.query_row(
                "SELECT COUNT(*) FROM claims cl JOIN journeys jj ON jj.id = cl.journey_id
                 WHERE cl.submitted_here = 1 AND jj.source_record_id <> ?2
                   AND cl.status IN ('SUBMITTED','INDETERMINATE','APPROVED','REJECTED','PAID')
                   AND substr(jj.check_in_utc,1,10) BETWEEN date(?1,'-2 day') AND date(?1,'+2 day')",
                rusqlite::params![date.to_string(), j.source_record_id],
                |x| x.get(0),
            )?;
            if ours_same_days > 0 {
                log::info!(
                    "claim list: an NS claim around {date} is not tied to a claim OV Refund filed (reference {}, transaction id {}); the trip is held for the user",
                    if r.reference.is_some() { "not recognised" } else { "missing" },
                    if r.transaction_id.is_some() { "not ours" } else { "missing" },
                );
            }
            return Ok(true);
        }
    }
    Ok(false)
}

/// NS references compared without case, spaces or punctuation ("NS-123 456" = "ns123456"). An
/// empty reference never matches.
pub fn same_reference(a: &str, b: &str) -> bool {
    let n = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_uppercase).collect::<String>();
    let (a, b) = (n(a), n(b));
    !a.is_empty() && a == b
}

pub fn reconcile(db: &mut Db, remote: &[RemoteClaim]) -> Result<(usize, Vec<String>)> {
    let mut approved_new = 0;
    let mut lines = vec![];
    for c in db.claims_in(&[ClaimState::Submitted, ClaimState::Indeterminate, ClaimState::Approved])? {
        let j = db.journey(c.journey_id)?;
        let date = j.check_in.with_timezone(&Amsterdam).date_naive();
        let m = match_remote_kind(db, remote, &j, c.ns_reference.as_deref())?;
        match (c.status, m) {
            // A date-only match is not proof that OUR submission reached NS: leave it for the user.
            (ClaimState::Indeterminate, Some((_, MatchKind::Date))) => {}
            (ClaimState::Indeterminate, Some((r, MatchKind::Exact))) => {
                let raw = r.raw_status.replace(" / ", "/");
                db.transition(
                    c.id,
                    Some(c.status),
                    ClaimState::Submitted,
                    "reconcile",
                    &format!("Gevonden bij NS ({raw}) / Found at NS ({raw})"),
                )?;
                if let Some(rf) = &r.reference {
                    db.set_ns_reference(c.id, rf)?;
                }
                lines.push(format!("{}: submission confirmed by NS claim list", date));
            }
            // Likewise for every other claim: a date-only match changes nothing (no reference, no
            // amount, no status); only a transaction id or NS reference is proof.
            (_, Some((_, MatchKind::Date))) => {}
            (_, Some((r, MatchKind::Exact))) => {
                if let Some(rf) = &r.reference {
                    db.set_ns_reference(c.id, rf)?;
                }
                // Never overwrite an amount from NS's e-mail or the user with the list's figure.
                if let (Some(a), Some(ClaimState::Approved | ClaimState::Paid), None) = (r.amount_cents, r.state, db.paid_cents(c.id)?) {
                    db.set_paid_amount(c.id, Some(a))?;
                }
                let cur = db.claim(c.id)?.status;
                if let Some(to) = r.state {
                    if to != cur && cur.can_transition_to(to) {
                        db.transition(
                            c.id,
                            Some(cur),
                            to,
                            "reconcile",
                            &crate::db::bilingual_detail("Status bij NS: ", "Status at NS: ", &r.raw_status.replace(" / ", "/")),
                        )?;
                        if to == ClaimState::Approved || to == ClaimState::Paid {
                            approved_new += 1;
                        }
                        lines.push(format!("{}: NS status {}", date, r.raw_status));
                    }
                }
            }
            // A failed lookup or missing entry never downgrades a submitted claim.
            _ => {}
        }
    }
    Ok((approved_new, lines))
}

/// Apply NS's claim list: statuses of submitted claims, and every mode's duplicate guard for
/// open claims (exact matches become "claimed elsewhere", date-only matches are flagged).
fn apply_claim_list(db: &mut Db, remote: &[RemoteClaim], s: &mut RunSummary, lines: &mut Vec<String>) -> Result<()> {
    let (n, l) = reconcile(db, remote)?;
    s.approved_since_last += n;
    lines.extend(l);
    // Every mode: trips NS already lists are never offered/submitted again.
    let (l, near) = reconcile_open(db, remote)?;
    lines.extend(l);
    db.set_ns_claim_near_keys(&near)?;
    Ok(())
}

/// The pause before automatic submission `k` (0-based) of `total`, in slices of at most one
/// second: keeps the lease, shows a countdown and returns `false` as soon as the user asks to stop.
fn pause_between_submissions(db: &Db, owner: &str, opts: &RunOptions, k: usize, total: usize) -> Result<bool> {
    let start = std::time::Instant::now();
    loop {
        db.renew_run_lock(owner, Duration::minutes(30))?;
        if auto_stop_requested(db, owner)? {
            return Ok(false);
        }
        let done = start.elapsed();
        let left = opts.auto_pause.saturating_sub(done);
        if left.is_zero() {
            return Ok(true);
        }
        // The ring keeps moving through the pause (the first half of claim k's share).
        let frac = done.as_secs_f64() / opts.auto_pause.as_secs_f64();
        let pct = 92.0 + 7.0 * ((k as f64 + 0.5 * frac) / total.max(1) as f64);
        crate::progress::pause(pct.round() as u8, k + 1, total, left.as_secs_f64().ceil() as u64);
        std::thread::sleep(left.min(std::time::Duration::from_secs(1)));
    }
}

pub struct RunContext<'a> {
    pub db: &'a mut Db,
    pub source: &'a mut dyn JourneySource,
    pub schedule: &'a mut dyn ScheduleProvider,
    pub gateway: Option<&'a mut dyn ClaimGateway>,
    pub notifiers: Vec<&'a mut dyn Notifier>,
}

pub fn run(ctx: RunContext, opts: &RunOptions) -> Result<RunSummary> {
    let RunContext { db, source, schedule, mut gateway, mut notifiers } = ctx;
    let owner = opts.lock_owner.clone().unwrap_or_else(|| format!("pid{}-{}", std::process::id(), opts.now.timestamp_millis()));
    // The run's clock (claim windows, time budget).
    let started = std::time::Instant::now();
    let clock = || -> DateTime<Utc> {
        match &opts.clock {
            Some(c) => c(),
            None => opts.now + Duration::from_std(started.elapsed()).unwrap_or_default(),
        }
    };
    if !db.acquire_run_lock(&owner, Duration::minutes(30))? {
        anyhow::bail!(
            "Er loopt al een controle. Probeer het zo opnieuw. / Another OV Refund check is already running. Please try again shortly."
        );
    }
    let run_id = match db.start_run(
        match opts.mode {
            RunMode::Manual => "manual",
            RunMode::Automatic => "automatic",
        },
        &opts.trigger,
    ) {
        Ok(id) => id,
        Err(e) => {
            let _ = db.release_run_lock(&owner);
            return Err(e);
        }
    };
    let mut s = RunSummary { run_id, mode: format!("{:?}", opts.mode).to_lowercase(), auth_state: "unknown".into(), ..Default::default() };
    // Check the session and write its state under one writer reservation. A concurrent logout
    // either precedes this check, or writes "disconnected" after us; it can never be overwritten
    // by a run that checked the old epoch just before the logout.
    let source_id = source.id().to_string();
    let record_auth = |db: &Db, authenticated: bool, state: &str| -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate)?;
        db.renew_run_lock(&owner, Duration::minutes(30))?;
        if opts.same_session.as_ref().is_none_or(|f| f(db)) {
            db.upsert_source(&source_id, "mijnns", authenticated, state)?;
        }
        tx.commit()?;
        Ok(())
    };
    let mut lines: Vec<String> = vec![];
    let result = (|| -> Result<()> {
        db.refresh_matching_policy()?;
        // A claim another check is still submitting (its lease ran out, e.g. after sleep) stays
        // as it is; nothing is submitted while it is in progress.
        let busy_elsewhere = match &opts.owner_alive {
            Some(alive) => inflight_elsewhere(db, &owner, alive.as_ref(), Utc::now())?,
            None => vec![],
        };
        for id in db.recover_inflight_except(&busy_elsewhere)? {
            clear_setting(db, &format!("{INFLIGHT_KEY_PREFIX}{id}"));
            lines.push(format!("claim #{id}: previous submission interrupted — marked uncertain, will reconcile"));
        }
        let from = opts.now - Duration::days(opts.lookback_days);
        crate::progress::report(5, "fetch");
        let journeys = match source.fetch_journeys(from, opts.now) {
            Ok(js) => {
                s.auth_state = "ok".into();
                record_auth(db, true, "ok")?;
                js
            }
            Err(e @ (FetchError::LoginRequired | FetchError::Blocked(_) | FetchError::AutoLoginFailed | FetchError::AccountMismatch)) => {
                let state = match &e {
                    FetchError::LoginRequired => "login_required".to_string(),
                    FetchError::AutoLoginFailed => "autologin_failed".to_string(),
                    FetchError::AccountMismatch => "account_mismatch".to_string(),
                    FetchError::Blocked(r) => format!("blocked: {r}"),
                    FetchError::Other(_) => unreachable!(),
                };
                record_auth(db, false, &state)?;
                s.errors.push(auth_message(&state).unwrap_or(BLOCKED_MSG).to_string());
                s.auth_state = state;
                return Ok(());
            }
            Err(FetchError::Other(e)) => return Err(e),
        };
        // The fetch can take long: make sure the lease is still ours before writing anything.
        db.renew_run_lock(&owner, Duration::minutes(30))?;
        s.journeys_seen = journeys.len();
        for j in &journeys {
            let (_, new) = db.upsert_journey(j)?;
            s.journeys_new += new as usize;
        }
        db.mark_source_synced(source.id())?;
        // Stopped by the user (or automatic submission switched off) during the sync: end the
        // check early. Trips fetched so far are kept; the next check continues.
        let stop_now = |db: &Db| -> Result<bool> { Ok(opts.mode == RunMode::Automatic && auto_stop_requested(db, &owner)?) };
        if stop_now(db)? {
            note_stopped(db, &owner, &mut s, &mut lines);
            final_counts(db, &mut s, opts)?;
            return Ok(());
        }

        let window_start = opts.now - Duration::days(opts.lookback_days);
        let all = db.journeys_since(window_start)?;
        let to_eval: Vec<(i64, Journey)> = all
            .into_iter()
            .filter(|(id, _)| db.claim_for_journey(*id).ok().flatten().map(|c| !c.status.blocks_new_submission()).unwrap_or(true))
            .collect();
        let js: Vec<Journey> = to_eval.iter().filter(|(_, j)| j.is_ns_carrier() && j.check_out.is_some()).map(|(_, j)| j.clone()).collect();
        crate::progress::report(35, "timetable");
        let gw_id = gateway.as_ref().map(|g| g.id().to_string()).unwrap_or_else(|| "none".into());
        let prepared = schedule.prepare_incremental(db, &js, &mut |db, sched, month, index, total| {
            db.renew_run_lock(&owner, Duration::minutes(30))?;
            // Between timetable months: a stop ends the sync here.
            if stop_now(db)? {
                return Err(StoppedByUser.into());
            }
            for (jid, j) in &to_eval {
                if crate::archive::month_key(j.check_in.with_timezone(&Amsterdam).date_naive()) == month {
                    assess_and_track(db, sched, &gw_id, *jid, j, opts)?;
                }
            }
            crate::progress::timetable(crate::progress::within(35, 80, index, total), month, index, total, true);
            Ok(())
        });
        match prepared {
            Ok(_) => log::info!("timetable preparation completed"),
            Err(e) if e.downcast_ref::<StoppedByUser>().is_some() => {
                log::info!("check stopped by the user during the timetable step");
                note_stopped(db, &owner, &mut s, &mut lines);
                final_counts(db, &mut s, opts)?;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        db.renew_run_lock(&owner, Duration::minutes(30))?;
        crate::progress::report(80, "assess");
        for (k, (jid, j)) in to_eval.iter().enumerate() {
            if k % 10 == 0 {
                crate::progress::report(crate::progress::within(80, 92, k, to_eval.len()), "assess");
            }
            let a = assess_and_track(db, &*schedule, &gw_id, *jid, j, opts)?;
            // Only open claims count (a trip the user dismissed or that is already handled must
            // not keep producing "needs your review" notifications).
            let open = db
                .claim_for_journey(*jid)?
                .map(|c| matches!(c.status, ClaimState::Ready | ClaimState::NeedsReview | ClaimState::Candidate))
                .unwrap_or(true);
            if !open {
                continue;
            }
            match a.confidence {
                Confidence::SupportedForAuto => s.candidates += 1,
                Confidence::NeedsReview => {
                    if db.evidence_status(*jid)?.as_deref() == Some("pending") {
                        s.pending_evidence += 1;
                    } else {
                        s.needs_review += 1;
                    }
                }
                _ => {}
            }
        }

        if let Some(gw) = gateway.as_deref_mut() {
            db.renew_run_lock(&owner, Duration::minutes(30))?;
            let mut remote_list: Option<Vec<RemoteClaim>> = None;
            crate::progress::report(92, "claims");
            let listed = gw.list_remote_claims();
            db.renew_run_lock(&owner, Duration::minutes(30))?;
            // For the GUI: did the last check manage to read NS's claim list?
            let _ = db.set_setting(CLAIM_LIST_STATUS_KEY, if matches!(listed, Ok(Some(_))) { "ok" } else { "unavailable" });
            match listed {
                Ok(Some(remote)) => {
                    apply_claim_list(db, &remote, &mut s, &mut lines)?;
                    remote_list = Some(remote);
                }
                Ok(None) => {}
                Err(e) => s.errors.push(crate::db::bilingual_detail(
                    "Status opvragen bij NS mislukt: ",
                    "Could not look up the status at NS: ",
                    &e.to_string(),
                )),
            }
            let mut list_read_at = clock();
            // NS e-mails that could not be tied to a trip before (new trips may have arrived).
            match db.retry_unmatched_mails() {
                Ok(u) if !u.is_empty() => log::info!("{} earlier NS e-mail(s) matched now", u.len()),
                Ok(_) => {}
                Err(e) => log::warn!("retrying NS e-mails failed: {e:#}"),
            }
            if let Some(hook) = &opts.before_submit {
                lines.extend(hook(db));
                db.renew_run_lock(&owner, Duration::minutes(30))?;
            }
            // Never submit while anything is still uncertain.
            let unresolved = db.claims_in(&[ClaimState::Submitting, ClaimState::Indeterminate])?.len();
            // Never auto-submit without knowing which claims NS already has (duplicate protection).
            if opts.mode == RunMode::Automatic && remote_list.is_none() {
                lines.push("NS claim list unavailable — automatic submission skipped this run".into());
                s.auto_paused_reason = Some(PAUSED_CLAIM_LIST_REASON.into());
                s.auto_paused_code = Some("claim_list".into());
            }
            if let (RunMode::Automatic, true, 0, Some(mut remote)) = (opts.mode, gw.verified_for_auto(), unresolved, remote_list) {
                // From here until the lock is released, a stop waits for this run (see AUTO_PHASE_KEY).
                db.set_setting(AUTO_PHASE_KEY, &owner)?;
                let mut queue = auto_queue(db, clock())?;
                if let Some(w) = opts.auto_only_due_within {
                    let now = clock();
                    // Claims a later scheduled check submits (not those NS may already have).
                    for (exp, c) in &queue {
                        if *exp > now + w && !db.is_ns_claim_near(&db.journey(c.journey_id)?)? {
                            s.auto_later += 1;
                        }
                    }
                    queue.retain(|(exp, _)| *exp <= now + w);
                }
                // Every qualifying claim, one at a time, with a pause after each attempt that
                // reached the gateway. All checks below run again after the pause.
                let total = queue.len();
                let mut sent_before = false;
                // A submission reached NS since the claim list was last read.
                let mut list_stale = false;
                for (k, (_, c)) in queue.into_iter().enumerate() {
                    if sent_before && !opts.auto_pause.is_zero() && !pause_between_submissions(db, &owner, opts, k, total)? {
                        note_stopped(db, &owner, &mut s, &mut lines);
                        break;
                    }
                    // The user asked to stop (without the lock, from the app or another process).
                    if auto_stop_requested(db, &owner)? {
                        note_stopped(db, &owner, &mut s, &mut lines);
                        break;
                    }
                    // Task Scheduler ends a check after 2 hours: start nothing new late in the run.
                    if clock() - opts.now >= opts.auto_time_budget {
                        log::info!("automatic submission: time budget reached, {} claim(s) left for the next check", total - k);
                        lines.push(time_budget_line(total - k));
                        s.auto_paused_reason = Some(PAUSED_BUDGET_REASON.into());
                        s.auto_paused_code = Some("budget".into());
                        break;
                    }
                    // NS's claim list again before every further submission (a claim filed on
                    // ns.nl or by another service in the meantime is never submitted twice).
                    if list_stale || clock() - list_read_at >= Duration::minutes(CLAIM_LIST_MAX_AGE_MINUTES) {
                        db.renew_run_lock(&owner, Duration::minutes(30))?;
                        let listed = gw.list_remote_claims();
                        db.renew_run_lock(&owner, Duration::minutes(30))?;
                        let _ = db.set_setting(CLAIM_LIST_STATUS_KEY, if matches!(listed, Ok(Some(_))) { "ok" } else { "unavailable" });
                        match listed {
                            Ok(Some(r)) => {
                                apply_claim_list(db, &r, &mut s, &mut lines)?;
                                remote = r;
                                list_read_at = clock();
                                list_stale = false;
                            }
                            _ => {
                                lines.push(CLAIM_LIST_LOST_LINE.into());
                                s.auto_paused_reason = Some(PAUSED_CLAIM_LIST_REASON.into());
                                s.auto_paused_code = Some("claim_list".into());
                                break;
                            }
                        }
                    }
                    // Never continue while anything is uncertain.
                    if !db.claims_in(&[ClaimState::Submitting, ClaimState::Indeterminate])?.is_empty() {
                        break;
                    }
                    let c = db.claim(c.id)?;
                    if !matches!(c.status, ClaimState::Ready | ClaimState::Failed) {
                        continue;
                    }
                    let j = db.journey(c.journey_id)?;
                    let day = j.check_in.with_timezone(&Amsterdam).format("%d-%m-%Y");
                    // Already filed at NS (by the user on ns.nl, another service, …)? Then never
                    // submit. Only a transaction id / NS reference proves that; a date-only match
                    // is a reason for caution (below), never a state change.
                    if let Some((r, MatchKind::Exact)) = match_remote_kind(db, &remote, &j, c.ns_reference.as_deref())? {
                        db.renew_run_lock(&owner, Duration::minutes(30))?;
                        db.mark_external(c.id, "reconcile")?;
                        if let Some(rf) = &r.reference {
                            db.set_ns_reference(c.id, rf)?;
                        }
                        lines.push(format!("{day}: already listed at NS — not submitted again"));
                        continue;
                    }
                    // NS lists a claim around that day that we cannot tie to a trip (claim list or
                    // an NS e-mail matched by date only): never risk a duplicate unattended; the
                    // user decides.
                    if remote_claim_near(db, &remote, &j)?
                        || match_remote_kind(db, &remote, &j, None)?.is_some()
                        || db.is_ns_claim_near(&j)?
                    {
                        lines.push(format!("{day}: NS shows a claim around this date — not submitted automatically, please check"));
                        s.near_skipped += 1;
                        continue;
                    }
                    // Consent or bank account withdrawn (or data erased) during the run: stop.
                    db.renew_run_lock(&owner, Duration::minutes(30))?;
                    if let Some(allowed) = &opts.auto_still_allowed {
                        if !allowed(db) {
                            lines.push("automatic submission is no longer enabled — stopped".into());
                            break;
                        }
                    }
                    if auto_stop_requested(db, &owner)? {
                        note_stopped(db, &owner, &mut s, &mut lines);
                        break;
                    }
                    if c.status == ClaimState::Failed {
                        db.transition(
                            c.id,
                            Some(ClaimState::Failed),
                            ClaimState::Ready,
                            "policy",
                            "Automatisch opnieuw geprobeerd (de vorige poging is niet verzonden) / Retried automatically (the previous attempt was not sent)",
                        )?;
                    }
                    db.renew_run_lock(&owner, Duration::minutes(30))?;
                    // "Claim k of n" (with Stop) only for a claim that is really submitted now.
                    crate::progress::submission(crate::progress::within(92, 99, k, total), k + 1, total);
                    let result = submit_claim_inner(db, gw, c.id, AuthorizedMode::Automatic, None, Some(&owner), clock());
                    // Pause (and read NS's list again) only after an attempt that reached NS; a
                    // claim refused before that leaves NS untouched.
                    let reached = result.is_ok() || db.claim(c.id)?.status != ClaimState::Ready;
                    sent_before |= reached;
                    list_stale |= reached;
                    if reached {
                        crate::progress::claim_done(crate::progress::within(92, 99, k + 1, total), k + 1, total);
                    }
                    match result {
                        Ok(r) => {
                            match r.status {
                                ClaimState::Submitted => {
                                    s.submitted += 1;
                                    lines.push(format!(
                                        "submitted: {} → {} on {}",
                                        j.origin.name,
                                        j.destination.as_ref().map(|d| d.name.as_str()).unwrap_or("?"),
                                        j.check_in.with_timezone(&Amsterdam).format("%d-%m-%Y")
                                    ));
                                }
                                ClaimState::Indeterminate => {
                                    s.indeterminate += 1;
                                    break; // stop submitting until reconciled
                                }
                                // A retry that NS conclusively did not receive again: skip it,
                                // the other claims go on (it waits for the next check).
                                ClaimState::Failed if c.status == ClaimState::Failed => {
                                    s.failed += 1;
                                    lines.push(format!("{day}: retry not sent again — skipped"));
                                    continue;
                                }
                                _ => {
                                    s.failed += 1;
                                    break; // stop on anything unexpected; a human looks first
                                }
                            }
                        }
                        Err(e) => {
                            // Refused by the final check (nothing reached NS), or failed (on the
                            // way to NS, or e.g. a database error).
                            if !reached && e.downcast_ref::<Refusal>().is_some() {
                                s.refused += 1;
                                db.set_setting(&format!("{AUTO_REFUSED_KEY_PREFIX}{}", c.id), &clock().to_rfc3339())?;
                            } else {
                                s.failed += 1;
                            }
                            s.errors.push(format!("claim #{}: {e}", c.id));
                            if db.claim(c.id)?.status == ClaimState::Submitting {
                                break;
                            }
                            continue;
                        }
                    }
                }
                // Submission phase over: the app hides "Claim k of n" and Stop.
                crate::progress::report(99, "claims");
            } else if opts.mode == RunMode::Automatic && unresolved > 0 {
                lines.push(format!("{unresolved} claim(s) with uncertain outcome — automatic submission paused until reconciled"));
            }
        }
        final_counts(db, &mut s, opts)?;
        // Anything uncertain (from before, or from this run) pauses automatic submission.
        let open_uncertain = db.claims_in(&[ClaimState::Submitting, ClaimState::Indeterminate])?.len();
        if opts.mode == RunMode::Automatic && gateway.is_some() && open_uncertain > 0 {
            s.auto_paused_reason = Some(paused_uncertain_reason(open_uncertain));
            s.auto_paused_code = Some("uncertain".into());
        }
        Ok(())
    })();
    if let Err(e) = &result {
        s.errors.push(format!("{e:#}"));
    }
    let outcome = if result.is_err() {
        "error"
    } else if s.auth_state != "ok" {
        "auth"
    } else {
        "ok"
    };
    let tail = (|| -> Result<()> {
        // Every final write and notification needs the lease, even without an after-run hook.
        db.renew_run_lock(&owner, Duration::minutes(30))?;
        if let Some(hook) = &opts.after_run {
            lines.extend(hook(db, &s));
            db.renew_run_lock(&owner, Duration::minutes(30))?;
        }
        db.finish_run(run_id, outcome, &s)?;
        for n in notifiers.iter_mut() {
            let ch = n.channel().to_string();
            if db.begin_notification(run_id, &ch)? {
                let r = n.notify(&s, &lines);
                db.finish_notification(run_id, &ch, &r)?;
            }
        }
        Ok(())
    })();
    let released = db.release_run_lock(&owner);
    // Markers of this run (its submission phase, a stop meant for it) go with the lock.
    let _ = db.conn.execute("DELETE FROM settings WHERE key=?1 AND value=?2", [AUTO_PHASE_KEY, owner.as_str()]);
    let _ = db.conn.execute(
        "DELETE FROM settings WHERE key=?1 AND substr(value, -length(?2)) = ?2",
        [AUTO_STOP_KEY, format!("|owner:{owner}").as_str()],
    );
    released?;
    tail?;
    Ok(s)
}

/// The run's closing counts (also after a check that was stopped early).
fn final_counts(db: &Db, s: &mut RunSummary, opts: &RunOptions) -> Result<()> {
    s.candidates = db
        .claims_in(&[ClaimState::Ready])?
        .iter()
        .filter(|c| db.assessment(c.journey_id).ok().flatten().is_some_and(|(_, _, _, exp)| exp >= opts.now))
        .count();
    s.indeterminate = db.claims_in(&[ClaimState::Indeterminate])?.len();
    s.expiring_soon = db.expiring_soon(opts.now, Duration::days(14))?;
    Ok(())
}
