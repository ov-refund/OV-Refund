//! Wires DB + NS bridge + adapters + notifiers into the core pipeline.
use crate::ns_adapters::{FetchRange, MyNsJourneySource, NsClaimGateway, GATEWAY_ID};
use crate::ns_bridge::NsBridge;
use crate::{paths, secrets};
use anyhow::Result;
use ovrefund_core::db::Db;
use ovrefund_core::email::{self, SmtpConfig};
use ovrefund_core::model::RunSummary;
use ovrefund_core::pipeline::{self, Notifier, RunMode, RunOptions};
use ovrefund_core::schedule::ArchiveScheduleProvider;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

pub fn open_db() -> Result<Db> {
    Db::open(&paths::db_path())
}

/// Language for e-mails and notifications (the app's language; Dutch until the app has set it).
pub fn lang(db: &Db) -> email::Lang {
    email::Lang::from_code(&setting(db, "ui_lang", "nl"))
}

pub fn setting(db: &Db, key: &str, default: &str) -> String {
    db.get_setting(key).ok().flatten().unwrap_or_else(|| default.to_string())
}

/// The live gateway may submit unattended only after at least one user-approved,
/// NS-acknowledged submission through this same gateway.
pub fn gateway_verified(db: &Db) -> bool {
    if crate::demo::is_demo() {
        return true;
    }
    // User decision 2026-11-12: allow automatic submission before a first verified submission.
    if setting(db, "auto_unverified_override", "0") == "1" {
        return true;
    }
    gateway_verified_by_submission(db)
}

/// True once a user-approved submission went through this gateway ON THIS INSTALLATION AND NS
/// gave it an application number (strong proof: NS's answer, an exact claim-list match or an NS
/// e-mail with that number). Page text or a date-only match never counts, and neither do claims
/// taken from a backup (`submitted_here` is set only when this installation starts a submission
/// and cleared by a restore; existing installations were migrated best effort, see db.rs 0008).
pub fn gateway_verified_by_submission(db: &Db) -> bool {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM claims WHERE gateway=?1 AND authorized_mode='manual' AND submitted_here=1 AND submitted_at IS NOT NULL
             AND ns_reference IS NOT NULL AND TRIM(ns_reference) <> ''
             AND status IN ('SUBMITTED','APPROVED','REJECTED','PAID')",
            [GATEWAY_ID],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Changes whenever the user logs out ("disconnect") or erases everything (settings are wiped).
/// A run compares it before and after: if it changed, the run must not save the NS session or
/// account details again.
const SESSION_EPOCH: &str = "session_epoch";

pub fn bump_session_epoch(db: &Db) -> Result<()> {
    db.set_setting(SESSION_EPOCH, &format!("{}-{}", std::process::id(), chrono::Utc::now().timestamp_micros()))
}

fn session_epoch(db: &Db) -> Option<String> {
    db.get_setting(SESSION_EPOCH).ok().flatten()
}

pub fn auto_enabled(db: &Db) -> bool {
    setting(db, "mode", "manual") == "automatic" && db.get_setting("auto_opt_in_at").ok().flatten().is_some()
}

/// The user still has trips at another claim service that are not entered as "claimed
/// elsewhere" yet (wizard answer "yes"): automatic submission is refused until "import_done".
/// Automatic submission waits while another claim service is in use, and while the question
/// is unanswered (e.g. after "Instellen overslaan").
pub fn other_service_blocks_auto(db: &Db) -> bool {
    !matches!(setting(db, "other_service", "").as_str(), "none" | "import_done")
}

/// The refusal for [`other_service_blocks_auto`]: unanswered or still in use.
pub fn other_service_msg(db: &Db) -> &'static str {
    if setting(db, "other_service", "") == "yes" {
        OTHER_SERVICE_MSG
    } else {
        OTHER_SERVICE_UNANSWERED_MSG
    }
}

/// Shared by real and demo runs, including the check immediately before each submission.
fn auto_submission_allowed(db: &Db, verified: bool, bank_present: bool) -> bool {
    auto_enabled(db) && verified && bank_present && !other_service_blocks_auto(db)
}

/// Would a check started now submit automatically (all gates, as the run itself checks them)?
pub fn auto_allowed_now(db: &Db) -> bool {
    let verified = crate::demo::is_demo() || gateway_verified(db);
    auto_submission_allowed(db, verified, secrets::bank_details().is_some())
}

/// Automatic submission is switched on, but a gate keeps a check from submitting: (code, "NL / EN").
/// `None` when automatic submission is off or every gate is open.
pub fn auto_gate_reason(db: &Db, verified: bool, bank_present: bool) -> Option<(&'static str, &'static str)> {
    if !auto_enabled(db) {
        return None;
    }
    if other_service_blocks_auto(db) {
        return Some(if setting(db, "other_service", "") == "yes" {
            ("other_service", PAUSED_OTHER_SERVICE_REASON)
        } else {
            ("service_unanswered", PAUSED_SERVICE_UNANSWERED_REASON)
        });
    }
    if !bank_present {
        return Some(("bank", PAUSED_BANK_REASON));
    }
    if !verified {
        return Some(("unverified", PAUSED_UNVERIFIED_REASON));
    }
    None
}

pub const PAUSED_OTHER_SERVICE_REASON: &str = "Gepauzeerd: je gebruikt nog een andere claimdienst. Voeg de reizen die de dienst al claimde toe als Elders geclaimd en klik op Import klaar. / Paused: you still use another claim service. Add the trips it already claimed as Claimed elsewhere and click Import done.";
pub const PAUSED_SERVICE_UNANSWERED_REASON: &str =
    "Gepauzeerd: geef eerst aan of je een claimdienst gebruikt. / Paused: first tell us whether you use a claim service.";
pub const PAUSED_BANK_REASON: &str = "Gepauzeerd: stel eerst je rekening in. / Paused: set up your bank account first.";
pub const PAUSED_UNVERIFIED_REASON: &str = "Gepauzeerd: dien eerst zelf een claim in, of zet automatisch indienen opnieuw aan. / Paused: submit a claim yourself first, or turn automatic submission on again.";

/// Fill in why automatic submission was paused when a gate turned the run into a manual one.
fn note_gate_reason(s: &mut RunSummary, gate: Option<(&'static str, &'static str)>) {
    if s.auto_paused_reason.is_none() {
        if let Some((code, reason)) = gate {
            s.auto_paused_code = Some(code.into());
            s.auto_paused_reason = Some(reason.into());
        }
    }
}

pub const OTHER_SERVICE_MSG: &str = "Je gebruikt nog een andere claimdienst. Voeg eerst de reizen die de dienst al claimde toe als Elders geclaimd. Klik daarna op Import klaar (Instellingen → Gegevens & back-up). / You still use another claim service. First add the trips it already claimed as Claimed elsewhere. Then click Import done (Settings → Data & backup).";
pub const OTHER_SERVICE_UNANSWERED_MSG: &str = "Geef eerst aan of je een claimdienst gebruikt (Instellingen → Automatisch). / First tell us whether you use a claim service (Settings → Automatic).";

/// Automatic login is on and a login is stored.
pub fn auto_login_on(db: &Db) -> bool {
    setting(db, "auto_login", "1") == "1" && secrets::ns_login().is_some()
}

/// One NS account per Windows user: true when trips are stored for another NS account than
/// `email` (case-insensitive). Without trips or a stored account, any account is fine.
pub fn account_mismatch(db: &Db, email: &str) -> bool {
    let Some(stored) = db.get_setting("ns_account_email").ok().flatten().filter(|s| !s.trim().is_empty()) else { return false };
    let trips = db.conn.query_row("SELECT COUNT(*) FROM journeys", [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    trips > 0 && !stored.trim().eq_ignore_ascii_case(email.trim())
}

/// E-mail is only attempted with a stored (app) password; otherwise every run would fail loudly.
fn mail_password_set() -> bool {
    secrets::get(secrets::SMTP_PASSWORD).ok().flatten().map(|p| !p.is_empty()).unwrap_or(false)
}

/// Setting with [`SmtpConfig::password_fingerprint`] of the mail server + user name the stored
/// app password was entered for. Never in a backup.
pub const SMTP_PW_FOR: &str = "smtp_password_for";

/// Bind a stored app password that has no fingerprint yet (installations before this check) to
/// the mail settings it is used with now. Call before the settings change.
pub fn bind_legacy_smtp_password(db: &Db) {
    if db.get_setting(SMTP_PW_FOR).ok().flatten().is_none() && mail_password_set() {
        let _ = db.set_setting(SMTP_PW_FOR, &smtp_config(db).password_fingerprint());
    }
}

/// Is the stored app password (if any) the one entered for the current mail server and user name?
pub fn smtp_password_matches(db: &Db) -> bool {
    bind_legacy_smtp_password(db);
    db.get_setting(SMTP_PW_FOR).ok().flatten().as_deref() == Some(smtp_config(db).password_fingerprint().as_str())
}

/// An app password is stored for the current mail server and user name.
pub fn smtp_password_usable(db: &Db) -> bool {
    mail_password_set() && smtp_password_matches(db)
}

/// The stored app password, only for the mail server and user name it was entered for; a
/// changed server or user name needs the password again ([`email::SMTP_REENTER_MSG`]).
pub fn smtp_password(db: &Db) -> Result<Option<String>> {
    let pw = secrets::get(secrets::SMTP_PASSWORD)?.filter(|p| !p.is_empty());
    if pw.is_some() {
        anyhow::ensure!(smtp_password_matches(db), email::SMTP_REENTER_MSG);
    }
    Ok(pw)
}

pub fn smtp_config(db: &Db) -> SmtpConfig {
    db.get_setting("smtp").ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub struct ToastNotifier {
    pub app: AppHandle,
    pub only_if_relevant: bool,
}

impl Notifier for ToastNotifier {
    fn channel(&self) -> &str {
        "windows"
    }
    fn notify(&mut self, s: &RunSummary, _lines: &[String]) -> Result<()> {
        if self.only_if_relevant && !email::summary_is_relevant(s) {
            return Ok(());
        }
        let db = open_db().ok();
        let lang = db.as_ref().map(lang).unwrap_or_default();
        let (title, mut body) = email::toast_text(s, lang);
        // Two failed automatic logins in a row: say that it is switched off now.
        if s.auth_state == "autologin_failed"
            && db.as_ref().map(|db| setting(db, crate::ns_bridge::AUTO_LOGIN_DISABLED, "0") == "1").unwrap_or(false)
        {
            body = ovrefund_core::pipeline::pick_lang(ovrefund_core::pipeline::AUTOLOGIN_DISABLED_MSG, lang == email::Lang::Nl).to_string();
        }
        self.app.notification().builder().title(title).body(body).show()?;
        Ok(())
    }
}

pub struct EmailNotifier {
    pub cfg: SmtpConfig,
    /// Scheduled runs: only mail when something is worth telling.
    pub only_if_relevant: bool,
}

impl Notifier for EmailNotifier {
    fn channel(&self) -> &str {
        "email"
    }
    fn notify(&mut self, s: &RunSummary, lines: &[String]) -> Result<()> {
        if self.only_if_relevant && !email::summary_is_relevant(s) {
            return Ok(());
        }
        let db = open_db().ok();
        let lang = db.as_ref().map(lang).unwrap_or_default();
        let (subject, body) = email::render_summary(s, lines, lang);
        let totals = db.as_ref().and_then(|db| email::report_from_db(db).ok());
        let html = email::render_html(s, lines, totals.as_ref(), crate::demo::is_demo(), lang);
        let pw = match db.as_ref() {
            Some(db) => smtp_password(db)?,
            None => anyhow::bail!("De gegevens van OV Refund konden niet worden geopend. / OV Refund's data could not be opened."),
        };
        email::send_rich(&self.cfg, pw.as_deref(), &subject, &body, Some(&html))
    }
}

/// One complete run. Blocking: call from a worker thread (never the UI thread).
pub(crate) fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let mut code = 0u32;
                let alive = process_image_matches(h) && GetExitCodeProcess(h, &mut code).is_ok() && code == 259; // STILL_ACTIVE
                let _ = CloseHandle(h);
                alive
            }
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        true
    }
}

/// Process id in a run-lock owner string ("pid<pid>-...").
pub fn lock_owner_pid(owner: &str) -> Option<u32> {
    owner.strip_prefix("pid").and_then(|s| s.split('-').next()).and_then(|p| p.parse::<u32>().ok())
}

/// Ask the running check (this or another process) to stop submitting automatically. Also
/// reaches a check whose lease ran out (e.g. after sleep) while its process still runs.
pub fn request_auto_stop(db: &Db, reason: pipeline::StopReason) -> Result<()> {
    pipeline::request_auto_stop_with(db, &|owner: &str| lock_owner_pid(owner).is_some_and(process_alive), reason)
}

/// Stop a process (the uninstaller's wipe after a check did not finish in time). Never this process.
pub fn kill_process(pid: u32) -> bool {
    if pid == std::process::id() {
        return false;
    }
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE};
        match OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let ok = process_image_matches(h) && TerminateProcess(h, 1).is_ok();
                let _ = CloseHandle(h);
                ok
            }
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// A run lock left by a process that no longer exists (crash, kill) is released.
pub fn recover_stale_lock(db: &Db) -> Result<()> {
    if let Some(owner) = db.run_lock_owner()? {
        let pid = lock_owner_pid(&owner);
        if pid.map(|p| p != std::process::id() && !process_alive(p)).unwrap_or(false) {
            log::warn!("releasing stale run lock held by {owner}");
            db.break_stale_lock(&owner)?;
        }
    }
    Ok(())
}

/// After each run that contacted NS: plan (or replace, or remove) the one extra check before a
/// claim deadline. Only while weekly checks and automatic submission are on; the claims are those
/// an automatic run would submit ([`pipeline::auto_queue`]), all other gates apply when it runs.
/// Runs under the run lock. A plan that races with withdrawing consent in the app (which removes
/// the task without the lock) is harmless: the extra check applies every gate again when it runs.
pub fn plan_deadline_run() {
    let r = (|| -> Result<()> {
        let db = open_db()?;
        let owner = format!("pid{}-plan-{}", std::process::id(), chrono::Utc::now().timestamp_millis());
        if !db.acquire_run_lock(&owner, chrono::Duration::minutes(2))? {
            log::info!("extra check not planned now: another check or change is running");
            return Ok(());
        }
        let r = plan_deadline_locked(&db);
        let _ = db.release_run_lock(&owner);
        r
    })();
    if let Err(e) = r {
        log::warn!("planning the extra check failed: {e:#}");
    }
}

/// [`plan_deadline_run`] for a caller that holds the run lock: a check plans from its after-run
/// step, before it releases its lock (an action waiting for that lock never finds it taken again).
fn plan_deadline_locked(db: &Db) -> Result<()> {
    let st = crate::scheduler::status();
    let verified = crate::demo::is_demo() || gateway_verified(db);
    let allowed = st.installed && auto_submission_allowed(db, verified, secrets::bank_details().is_some());
    let now = chrono::Utc::now();
    let interval = st.interval_days.unwrap_or(7).clamp(1, 31) as i64;
    let next_regular = st
        .next_run
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or(now + chrono::Duration::days(interval));
    match deadline_plan(db, now, allowed, next_regular)? {
        DeadlinePlan::Remove => crate::scheduler::remove_deadline(),
        DeadlinePlan::Keep => Ok(()),
        DeadlinePlan::Install { at, deadline, retry } => {
            log::info!("extra check planned before a claim deadline{}", if retry { " (again)" } else { "" });
            crate::scheduler::install_deadline(at, deadline)?;
            db.set_setting(DEADLINE_PLANNED_KEY, &planned_value(deadline, at, now, retry))
        }
    }
}

/// Trigger of the extra check before a claim deadline (`--run-once --trigger deadline`).
pub const DEADLINE_TRIGGER: &str = "deadline";

/// Plan the extra check from a run's after-run step (under the run's lock) and note that it was
/// done.
fn plan_in_run(db: &Db, planned: &AtomicBool) {
    if let Err(e) = plan_deadline_locked(db) {
        log::warn!("planning the extra check failed: {e:#}");
    }
    planned.store(true, Ordering::SeqCst);
}

/// "<deadline>|<run time>[|retry]" of the last extra check that was planned (both RFC 3339): the
/// time the extra check runs, not the time it was planned; "retry" when it was planned again
/// after an extra check that failed or did not run. Cleared when the extra check is removed.
pub const DEADLINE_PLANNED_KEY: &str = "deadline_run_planned";

/// Value for [`DEADLINE_PLANNED_KEY`]: the deadline and when the extra check runs (never earlier
/// than now), and whether it is a retry.
pub fn planned_value(
    deadline: chrono::DateTime<chrono::Utc>,
    at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
    retry: bool,
) -> String {
    format!("{}|{}{}", deadline.to_rfc3339(), at.max(now).to_rfc3339(), if retry { "|retry" } else { "" })
}

/// A stored [`DEADLINE_PLANNED_KEY`] value: (deadline, run time, retry).
fn parse_planned(v: &str) -> Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>, bool)> {
    let mut parts = v.split('|');
    let d = chrono::DateTime::parse_from_rfc3339(parts.next()?).ok()?.with_timezone(&chrono::Utc);
    let t = chrono::DateTime::parse_from_rfc3339(parts.next()?).ok()?.with_timezone(&chrono::Utc);
    Some((d, t, parts.next() == Some("retry")))
}

#[derive(Debug, PartialEq)]
pub enum DeadlinePlan {
    /// No extra check needed (or not allowed): remove a planned one.
    Remove,
    /// The planned extra check has not run yet, or there is no time left to try again: leave it.
    Keep,
    Install {
        at: chrono::DateTime<chrono::Utc>,
        deadline: chrono::DateTime<chrono::Utc>,
        /// Planned again after an extra check that failed or did not run.
        retry: bool,
    },
}

/// What to do with the extra check before a claim deadline. `allowed`: the regular check is
/// installed and automatic submission is allowed. One rule: a planned extra check is kept while
/// its time has not come; once that time has passed and its claim is still waiting to be
/// submitted (whatever the reason: login expired, NS claim list unavailable, time limit, check
/// missed), it is planned again: first an hour later, after that at 10:00 the next day, as long
/// as that is before the deadline. Claims an automatic check never submits are left out: flagged
/// "NS may already have a claim around this date", or refused by the final check before sending
/// (not a retryable failure; see [`pipeline::auto_refused`]).
pub fn deadline_plan(
    db: &Db,
    now: chrono::DateTime<chrono::Utc>,
    allowed: bool,
    next_regular: chrono::DateTime<chrono::Utc>,
) -> Result<DeadlinePlan> {
    if !allowed {
        return Ok(DeadlinePlan::Remove);
    }
    let mut deadlines = vec![];
    for (exp, c) in pipeline::auto_queue(db, now)? {
        if !db.is_ns_claim_near(&db.journey(c.journey_id)?)? && !pipeline::auto_refused(db, c.id)? {
            deadlines.push(exp);
        }
    }
    let Some(at) = pipeline::deadline_run_at(now, next_regular, &deadlines) else {
        return Ok(DeadlinePlan::Remove);
    };
    let deadline = deadlines.iter().copied().filter(|d| *d > at).min().unwrap_or(at);
    let planned = db.get_setting(DEADLINE_PLANNED_KEY)?.as_deref().and_then(parse_planned);
    if let Some((_, t, retry)) = planned.filter(|(d, ..)| *d == deadline) {
        // Not run yet.
        if t > now {
            return Ok(DeadlinePlan::Keep);
        }
        // Its time has passed and its claim still waits: an hour later; after a retry at 10:00
        // the next day.
        let again = if retry {
            pipeline::local_ten_am(now.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive() + chrono::Duration::days(1))
                .max(now + chrono::Duration::hours(1))
        } else {
            now + chrono::Duration::hours(1)
        };
        return Ok(if again < deadline { DeadlinePlan::Install { at: again, deadline, retry: true } } else { DeadlinePlan::Keep });
    }
    Ok(DeadlinePlan::Install { at, deadline, retry: false })
}

/// Should a scheduled check (`--run-once`) be skipped? The task fires at logon and on the
/// interval: skip when a background check completed successfully less than 12 hours ago. While
/// automatic submission is allowed (`auto_allowed`, see [`auto_allowed_now`]) only an automatic
/// background check counts (checks from the app submit only urgent claims and must not starve
/// the scheduled submissions); otherwise no check submits and any finished check counts. The
/// extra check before a deadline (trigger `deadline`) is never skipped. Returns the time of that
/// last check.
pub fn skip_scheduled_run(
    db: &Db,
    trigger: &str,
    now: chrono::DateTime<chrono::Utc>,
    auto_allowed: bool,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if trigger != "cli" {
        return None;
    }
    let last = if auto_allowed { db.last_scheduled_ok_run_at("automatic") } else { db.last_completed_run_at() };
    last.ok().flatten().filter(|t| now - *t < chrono::Duration::hours(12))
}

/// A check started in the app submits automatically only claims whose claim window ends within
/// this many days; everything else waits for the scheduled check.
pub const GUI_AUTO_DUE_DAYS: i64 = 14;

/// Which claims a run may submit automatically (when automatic submission is allowed at all):
/// every eligible claim in a scheduled check, only urgent ones in a check from the app.
pub fn auto_due_window(scheduled: bool) -> Option<chrono::Duration> {
    (!scheduled).then(|| chrono::Duration::days(GUI_AUTO_DUE_DAYS))
}

/// Demo sandbox run: example data, simulated NS, no network, no e-mail.
fn run_demo(app: &AppHandle, trigger: &str, scheduled: bool, planned: Arc<AtomicBool>) -> Result<RunSummary> {
    let mut db = open_db()?;
    if db.source_status("mijnns")?.map(|s| s.0) != Some(true) {
        // not "logged in" in the demo yet
        db.upsert_source("mijnns", "mijnns", false, "login_required")?;
    }
    let logged_in = db.source_status("mijnns")?.and_then(|s| s.1).as_deref() == Some("ok");
    let mut source = crate::demo::DemoSource;
    let mut sched = crate::demo::DemoSchedule(ArchiveScheduleProvider::new());
    let mut gateway = crate::demo::DemoGateway;
    let mut toast = ToastNotifier { app: app.clone(), only_if_relevant: true };
    // E-mail works in the demo (to your own address via your own mailbox, demo credentials).
    let smtp = smtp_config(&db);
    let mut mail = EmailNotifier { cfg: smtp.clone(), only_if_relevant: false };
    let mut notifiers: Vec<&mut dyn Notifier> = vec![];
    if setting(&db, "notify_windows", "1") == "1" {
        notifiers.push(&mut toast);
    }
    if setting(&db, "notify_email", "0") == "1" && smtp.is_configured() && mail_password_set() {
        notifiers.push(&mut mail);
    }
    let opts = RunOptions {
        mode: if trigger != SCAN_TRIGGER && auto_submission_allowed(&db, true, secrets::bank_details().is_some()) {
            RunMode::Automatic
        } else {
            RunMode::Manual
        },
        auto_only_due_within: auto_due_window(scheduled),
        // Simulated NS: a short pause keeps the demo quick while showing the one-at-a-time flow.
        auto_pause: std::time::Duration::from_secs(5),
        auto_still_allowed: Some(Box::new(|db| auto_submission_allowed(db, true, secrets::bank_details().is_some()))),
        trigger: format!("demo-{trigger}"),
        lookback_days: 550,
        after_run: Some(Box::new(move |db: &mut Db, _: &RunSummary| {
            plan_in_run(db, &planned);
            vec![]
        })),
        ..Default::default()
    };
    if !logged_in {
        anyhow::bail!(
            "DEMO: log eerst (gesimuleerd) in via ‘Inloggen bij Mijn NS’. / DEMO: first log in (simulated) via ‘Log in to Mijn NS’."
        );
    }
    let gate = if trigger != SCAN_TRIGGER { auto_gate_reason(&db, true, secrets::bank_details().is_some()) } else { None };
    let mut s = pipeline::run(
        pipeline::RunContext { db: &mut db, source: &mut source, schedule: &mut sched, gateway: Some(&mut gateway), notifiers },
        &opts,
    )?;
    note_gate_reason(&mut s, gate);
    Ok(s)
}

/// One check, with rough progress (0-100 %, stage) sent to the GUI as "scan-progress" events.
pub fn run(app: &AppHandle, trigger: &str, scheduled: bool, force_full: bool) -> Result<RunSummary> {
    use tauri::Emitter;
    let a = app.clone();
    ovrefund_core::progress::set_sink(Some(Box::new(move |progress| {
        if progress.month_done {
            let _ = a.emit("scan-month-done", progress);
        }
        // An automatic submission reached NS: the app shows the new state right away.
        if progress.claim_done {
            let _ = a.emit("claim-submitted", progress);
        }
        let _ = a.emit("scan-progress", progress);
    })));
    ovrefund_core::progress::report(2, "login");
    // The run plans the extra check itself, under its own lock (see `plan_in_run`).
    let planned = Arc::new(AtomicBool::new(false));
    let r = run_inner(app, trigger, scheduled, force_full, planned.clone());
    // Not after a reminder (NS was not contacted, nothing changed).
    if plans_deadline_after(&r) && !planned.load(Ordering::SeqCst) {
        plan_deadline_run();
    }
    ovrefund_core::progress::report(100, "done");
    ovrefund_core::progress::set_sink(None);
    r.map(|(s, _)| s)
}

/// Plan the extra check before a deadline only after a check that contacted NS.
fn plans_deadline_after(r: &Result<(RunSummary, Contacted)>) -> bool {
    matches!(r, Ok((_, Contacted::Ns)))
}

/// Whether a run contacted NS or only sent a reminder (automatic login off).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Contacted {
    Ns,
    ReminderOnly,
}

fn run_inner(
    app: &AppHandle,
    trigger: &str,
    scheduled: bool,
    force_full: bool,
    planned: Arc<AtomicBool>,
) -> Result<(RunSummary, Contacted)> {
    if crate::demo::is_demo() {
        return run_demo(app, trigger, scheduled, planned).map(|s| (s, Contacted::Ns));
    }
    // The check's time budget counts from here (also the wait for the run lock, see below).
    let started = std::time::Instant::now();
    let mut db = open_db()?;
    recover_stale_lock(&db)?;
    let epoch = match session_epoch(&db) {
        Some(e) => e,
        None => {
            bump_session_epoch(&db)?;
            session_epoch(&db).unwrap_or_default()
        }
    };
    // Scheduled check without automatic login: NS is not contacted at all (it would only show
    // its login page); the user gets a reminder when there is something to do.
    if scheduled && !auto_login_on(&db) {
        return remind_instead_of_check(app, &db, trigger == DEADLINE_TRIGGER).map(|s| (s, Contacted::ReminderOnly));
    }
    // Another check (or a change in the app) holds the lock: stop before opening the NS window
    // or tidying the browser profile that run is using. The lock is taken here (atomically) and
    // handed to the run, so a second process started at the same moment stops too. A scheduled
    // check waits a while for a check or change in the app (see `wait_for_run_lock`).
    let owner = run_owner(scheduled);
    wait_for_run_lock(
        || {
            recover_stale_lock(&db)?;
            db.acquire_run_lock(&owner, chrono::Duration::minutes(30))
        },
        || db.live_run_lock_owner(),
        scheduled,
        SCHEDULED_LOCK_WAIT,
        std::time::Duration::from_secs(5),
    )?;
    let _held = HeldLock(owner.clone());
    // A logout or "forget everything" that waited for another check is finished first.
    crate::commands::finish_pending_before(app)?;
    crate::ns_bridge::reset_auto_login_attempt();
    let bridge = NsBridge::open(app, false)?;
    let verified = gateway_verified(&db);
    let gate = if trigger != SCAN_TRIGGER { auto_gate_reason(&db, verified, secrets::bank_details().is_some()) } else { None };
    // Automatic submission also needs a bank account (otherwise every attempt would fail), and
    // no unrecorded claims at another service.
    // A check from the app ("Controleer nu") submits only urgent claims (see GUI_AUTO_DUE_DAYS).
    // `ov-refund scan` is a diagnostic: it never submits.
    let mode = if trigger != SCAN_TRIGGER && auto_submission_allowed(&db, verified, secrets::bank_details().is_some()) {
        RunMode::Automatic
    } else {
        RunMode::Manual
    };
    // Incremental: the full history once, then only what changed since the last successful sync.
    let last_sync = db.source_status("mijnns")?.and_then(|s| s.2).and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok());
    let enough = db.conn.query_row("SELECT COUNT(*) FROM journeys", [], |r| r.get::<_, i64>(0)).unwrap_or(0) > 20;
    let range = match last_sync {
        _ if force_full || !enough => FetchRange::Max,
        None => FetchRange::Max,
        // The year view also refreshes prices of trips NS has invoiced since; do it every 3 days.
        Some(t)
            if chrono::Utc::now().signed_duration_since(t) < chrono::Duration::days(6)
                && db
                    .get_setting("last_year_fetch")?
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(&v).ok())
                    .map(|y| chrono::Utc::now().signed_duration_since(y) < chrono::Duration::days(3))
                    .unwrap_or(false) =>
        {
            FetchRange::Week
        }
        Some(_) => FetchRange::Year,
    };
    log::info!("fetch range {range:?}");
    let mut source = MyNsJourneySource { bridge: bridge.clone(), range };
    let mut gateway = NsClaimGateway { bridge: bridge.clone(), verified, bank: secrets::bank_details() };
    let mut sched = ArchiveScheduleProvider::new();
    prune_webview_profile();
    // Clean up leftovers from older versions (downloaded archive files, old webview folder).
    let _ = std::fs::remove_dir_all(paths::data_dir().join("archive-cache"));
    let _ = std::fs::remove_dir_all(paths::data_dir().join("webview"));
    // Only notify when something is worth telling (also for scheduled runs); the app shows the rest.
    let mut toast = ToastNotifier { app: app.clone(), only_if_relevant: true };
    let smtp = smtp_config(&db);
    let mut mail = EmailNotifier { cfg: smtp.clone(), only_if_relevant: true };
    let mut notifiers: Vec<&mut dyn Notifier> = vec![];
    if setting(&db, "notify_windows", "1") == "1" {
        notifiers.push(&mut toast);
    }
    if setting(&db, "notify_email", "0") == "1" && smtp.is_configured() && mail_password_set() && scheduled {
        notifiers.push(&mut mail);
    }
    let mail_synced = Arc::new(AtomicBool::new(false));
    let opts = RunOptions {
        mode,
        trigger: trigger.to_string(),
        lookback_days: setting(&db, "lookback_days", "550").parse().unwrap_or(550),
        auto_only_due_within: auto_due_window(scheduled),
        // Counted from the start of this check, including the wait for the run lock.
        auto_time_budget: auto_time_budget(started.elapsed()),
        // Re-read from storage before EACH automatic submission: the user may have switched
        // automatic mode off, removed the bank account or logged out while this run was busy.
        auto_still_allowed: Some(Box::new({
            let epoch = epoch.clone();
            move |db: &Db| {
                auto_submission_allowed(db, gateway_verified(db), secrets::bank_details().is_some())
                    && session_epoch(db).as_deref() == Some(epoch.as_str())
            }
        })),
        // Logged out or "forget everything" since this run started: the run never records the
        // NS login state (the app must not show "logged in" after a logout).
        same_session: Some(Box::new({
            let epoch = epoch.clone();
            move |db: &Db| session_epoch(db).as_deref() == Some(epoch.as_str())
        })),
        // NS decision e-mails are read inside the run lock, before anything is submitted.
        before_submit: Some(Box::new({
            let ran = mail_synced.clone();
            move |db: &mut Db| {
                if setting(db, "read_ns_mail", "0") != "1" {
                    return vec![];
                }
                ran.store(true, Ordering::SeqCst);
                if let Err(e) = sync_ns_mail(db) {
                    log::warn!("ns mail sync failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                }
                vec![]
            }
        })),
        // Every write that belongs to this run happens here, still inside the run lock, so a
        // logout / "forget everything" / uninstall (which take the same lock and change the
        // session epoch) can never be followed by this run writing the session or data back.
        after_run: Some(Box::new({
            let ran = mail_synced.clone();
            let bridge = bridge.clone();
            let epoch = epoch.clone();
            let app = app.clone();
            move |db: &mut Db, s: &RunSummary| {
                bridge.hide();
                // Logged out or "forget everything" since this run started: write nothing back.
                if session_epoch(db).as_deref() != Some(epoch.as_str()) {
                    log::warn!("data was erased or the user logged out during the run; session and settings are not saved");
                    let _ = bridge.navigate("about:blank");
                    return vec![];
                }
                let ok = s.auth_state == "ok";
                if ok && range != FetchRange::Week {
                    let _ = db.set_setting("last_year_fetch", &chrono::Utc::now().to_rfc3339());
                }
                // Only after a normal check: never record the account of a mismatching login.
                if ok {
                    if let Some(email) = crate::ns_adapters::account_email(&bridge).filter(|e| !account_mismatch(db, e)) {
                        let _ = db.set_setting("ns_account_email", &email);
                    }
                }
                // Keep the (possibly refreshed) session for the next run.
                if ok {
                    if let Err(e) = crate::session_store::save(&bridge) {
                        log::warn!("session save failed: {e:#}");
                    }
                }
                let _ = bridge.navigate("about:blank");
                // Runs that stopped before the claim step (e.g. login expired) still read the NS e-mails.
                if !ran.load(Ordering::SeqCst) && setting(db, "read_ns_mail", "0") == "1" {
                    if let Err(e) = sync_ns_mail(db) {
                        log::warn!("ns mail sync failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                    }
                }
                // Leanest footprint: drop NS's cached website files after every check (cookies/login
                // stay). The next check re-downloads Mijn NS's app (~2-3 MB compressed).
                let _ = bridge.cdp("Network.clearBrowserCache", serde_json::json!({}), std::time::Duration::from_secs(10));
                if scheduled {
                    notify_update_once(&app, db);
                }
                plan_in_run(db, &planned);
                vec![]
            }
        })),
        lock_owner: Some(owner),
        // A claim another check is still submitting (lease lost during sleep) is left alone.
        owner_alive: Some(Box::new(|owner: &str| lock_owner_pid(owner).is_some_and(process_alive))),
        ..Default::default()
    };
    let s = pipeline::run(
        pipeline::RunContext { db: &mut db, source: &mut source, schedule: &mut sched, gateway: Some(&mut gateway), notifiers },
        &opts,
    );
    // Nothing is written after the run lock was released (see `after_run`).
    bridge.hide();
    s.map(|mut s| {
        note_gate_reason(&mut s, gate);
        (s, Contacted::Ns)
    })
}

/// Releases a run lock taken before the run when the run itself never started (e.g. the NS
/// window could not be opened). The run releases it as usual; releasing twice is harmless.
struct HeldLock(String);
impl Drop for HeldLock {
    fn drop(&mut self) {
        if let Ok(db) = open_db() {
            let _ = db.release_run_lock(&self.0);
        }
    }
}

/// Trigger of `ov-refund scan` (a diagnostic): always manual mode, never submits.
pub const SCAN_TRIGGER: &str = "scan";

/// Run-lock owner of a check: scheduled checks ("-sched-") are told apart from checks started in
/// the app ("-run-"), so a scheduled check never waits behind another scheduled check.
fn run_owner(scheduled: bool) -> String {
    format!("pid{}-{}-{}", std::process::id(), if scheduled { "sched" } else { "run" }, chrono::Utc::now().timestamp_millis())
}

/// Is this run-lock owner a scheduled check (see [`run_owner`])?
fn is_scheduled_owner(owner: &str) -> bool {
    owner.contains("-sched-")
}

/// How long a scheduled check waits for a check or change in the app (the app holds the lock for
/// a change at most a few minutes; a check from the app usually ends within this time).
pub const SCHEDULED_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Take the run lock (`try_take`). A check started in the app is refused right away when the lock
/// is busy. A scheduled check (`scheduled`) waits up to `max`, trying every `step`, so a deadline
/// or weekly check is not lost to a short check or change in the app; it stops right away when
/// the holder is another scheduled check (that check does the same work).
fn wait_for_run_lock(
    mut try_take: impl FnMut() -> Result<bool>,
    holder: impl Fn() -> Result<Option<String>>,
    scheduled: bool,
    max: std::time::Duration,
    step: std::time::Duration,
) -> Result<()> {
    let start = std::time::Instant::now();
    let mut logged = false;
    loop {
        if try_take()? {
            return Ok(());
        }
        if !scheduled || holder()?.is_some_and(|h| is_scheduled_owner(&h)) || start.elapsed() >= max {
            anyhow::bail!(RUN_BUSY_MSG);
        }
        if !logged {
            log::info!("scheduled check: waiting for a check or change in the app to finish");
            logged = true;
        }
        std::thread::sleep(step);
    }
}

/// Refusal when another check holds the run lock.
pub const RUN_BUSY_MSG: &str =
    "Er loopt al een controle. Probeer het zo opnieuw. / Another OV Refund check is already running. Please try again shortly.";

/// Scheduled check while automatic login is off: no NS contact. Reminds the user (Windows
/// notification, and e-mail when set up) when there is something the user can act on (see
/// [`ACTIONABLE_SQL`]) or the last check is older than max(interval, 7) days; at most once per
/// interval (minimum 20 hours; the task also fires at every logon), and by e-mail at most once
/// per max(2 × interval, 7) days. The extra check before a deadline (`deadline`) reminds with its
/// own text whenever there is something to act on (see [`reminder_kind`]).
fn remind_instead_of_check(app: &AppHandle, db: &Db, deadline: bool) -> Result<RunSummary> {
    let open: i64 = db.conn.query_row(ACTIONABLE_SQL, [], |r| r.get(0))?;
    let now = chrono::Utc::now();
    let interval = setting(db, "interval_days", "7").parse::<i64>().unwrap_or(7).clamp(1, 31);
    let p = ReminderPolicy::new(interval);
    let stale = db.last_completed_run_at()?.map(|t| now - t > p.stale_after).unwrap_or(true);
    let since = |key: &str, d: chrono::Duration| -> Result<bool> {
        Ok(db
            .get_setting(key)?
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| now.signed_duration_since(t) < d)
            .unwrap_or(false))
    };
    let reminded_recently = since("last_reminder_at", p.throttle)?;
    let mailed_recently = since("last_reminder_email_at", p.email_throttle)?;
    let summary = RunSummary { auth_state: "not_checked".into(), mode: "manual".into(), ..Default::default() };
    let Some(kind) = reminder_kind(deadline, open, stale, reminded_recently, mailed_recently) else {
        log::info!("scheduled check skipped (automatic login off); no reminder needed");
        return Ok(summary);
    };
    let l = lang(db);
    let nl = l == email::Lang::Nl;
    let (text, title, may_mail) = match kind {
        ReminderKind::Deadline => (
            ovrefund_core::pipeline::pick_lang(DEADLINE_REMINDER_MSG, nl).to_string(),
            l.t("OV Refund: een claim verloopt binnenkort", "OV Refund: a claim expires soon").to_string(),
            true,
        ),
        ReminderKind::Regular { mail } => (
            ovrefund_core::pipeline::pick_lang(ovrefund_core::pipeline::REMINDER_MSG, nl).to_string(),
            l.t("OV Refund: tijd voor een controle", "OV Refund: time for a check").to_string(),
            mail,
        ),
    };
    if setting(db, "notify_windows", "1") == "1" {
        let _ = app.notification().builder().title(&title).body(&text).show();
    }
    let smtp = smtp_config(db);
    if setting(db, "notify_email", "0") == "1" && smtp.is_configured() && mail_password_set() && may_mail {
        match smtp_password(db).and_then(|pw| email::send(&smtp, pw.as_deref(), &title, &text)) {
            Ok(()) => db.set_setting("last_reminder_email_at", &now.to_rfc3339())?,
            Err(e) => log::warn!("reminder e-mail failed: {e:#}"),
        }
    }
    db.set_setting("last_reminder_at", &now.to_rfc3339())?;
    log::info!("scheduled check skipped (automatic login off); reminder sent");
    Ok(summary)
}

/// Reminder of the extra check before a deadline when automatic login is off.
pub const DEADLINE_REMINDER_MSG: &str =
    "Een claim verloopt binnenkort. Open OV Refund en klik op Controleer nu. / A claim expires soon. Open OV Refund and click Check now.";

/// Which reminder a scheduled check without automatic login sends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReminderKind {
    /// The regular reminder; `mail`: also by e-mail (not mailed recently).
    Regular { mail: bool },
    /// The extra check before a deadline: its own text, not held back by the throttle.
    Deadline,
}

/// The reminder to send, if any. The extra check before a deadline (`deadline`) reminds whenever
/// there is something to act on; otherwise see [`reminder_due`].
pub fn reminder_kind(
    deadline: bool,
    open_or_pending: i64,
    last_check_stale: bool,
    reminded_recently: bool,
    mailed_recently: bool,
) -> Option<ReminderKind> {
    if deadline && open_or_pending > 0 {
        return Some(ReminderKind::Deadline);
    }
    reminder_due(open_or_pending, last_check_stale, reminded_recently).then_some(ReminderKind::Regular { mail: !mailed_recently })
}

/// Time for automatic submissions in a check, counted from its start (Task Scheduler stops a
/// check after 2 hours): 90 minutes minus the time already spent, e.g. waiting for the run lock.
pub fn auto_time_budget(spent: std::time::Duration) -> chrono::Duration {
    let spent = chrono::Duration::from_std(spent).unwrap_or(chrono::Duration::MAX);
    (chrono::Duration::minutes(90) - spent).max(chrono::Duration::zero())
}

/// Claims the user can act on, for the reminder: claimable, to review, not sent (under the retry
/// limit of 3 attempts), and trips still waiting for timetable data whose claim is open.
pub const ACTIONABLE_SQL: &str =
    "SELECT (SELECT COUNT(*) FROM claims WHERE status IN ('READY','NEEDS_REVIEW') OR (status='FAILED' AND attempts < 3))
      + (SELECT COUNT(*) FROM schedules s LEFT JOIN claims c ON c.journey_id = s.journey_id
         WHERE s.status = 'pending' AND (c.id IS NULL OR c.status IN ('CANDIDATE','READY','NEEDS_REVIEW','FAILED')))";

/// Timing of the reminder for a check interval of `interval_days`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReminderPolicy {
    /// The last check counts as old after max(interval, 7) days.
    pub stale_after: chrono::Duration,
    /// At most one reminder per interval (minimum 20 hours).
    pub throttle: chrono::Duration,
    /// At most one reminder e-mail per max(2 × interval, 7) days.
    pub email_throttle: chrono::Duration,
}

impl ReminderPolicy {
    pub fn new(interval_days: i64) -> Self {
        let d = interval_days.clamp(1, 31);
        ReminderPolicy {
            stale_after: chrono::Duration::days(d.max(7)),
            throttle: chrono::Duration::days(d).max(chrono::Duration::hours(20)),
            email_throttle: chrono::Duration::days((2 * d).max(7)),
        }
    }
}

/// Remind when there is something to act on, or the last check is old, and no reminder went out
/// within the throttle period.
pub fn reminder_due(open_or_pending: i64, last_check_stale: bool, reminded_recently: bool) -> bool {
    (open_or_pending > 0 || last_check_stale) && !reminded_recently
}

/// Background (scheduled) runs: tell people who rarely open the app that an update is ready,
/// once per version. Installing always happens in the app, on the user's click.
fn notify_update_once(app: &AppHandle, db: &Db) {
    if !crate::updater::configured() || setting(db, "update_checks", "1") != "1" {
        return;
    }
    let Ok(info) = tauri::async_runtime::block_on(crate::updater::check(app)) else { return };
    let Some(v) = info["version"].as_str().filter(|_| info["available"] == true) else { return };
    if db.get_setting("update_notified").ok().flatten().as_deref() == Some(v) {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title(match lang(db) {
            email::Lang::Nl => format!("OV Refund {v} is beschikbaar"),
            email::Lang::En => format!("OV Refund {v} is available"),
        })
        .body(lang(db).t(
            "Open OV Refund en klik op ‘Nu bijwerken’. Je gegevens en instellingen blijven bewaard.",
            "Open OV Refund and click ‘Update now’. Your data and settings are kept.",
        ))
        .show();
    let _ = db.set_setting("update_notified", v);
}

/// Setting with the per-domain counts of NS-looking mails that were skipped (last mail sync).
pub const MAIL_SKIPPED_KEY: &str = "ns_mail_skipped";

/// Read NS delay-refund e-mails (opt-in) and apply decisions/amounts to the ledger.
/// Lines are in the app's language. The caller must hold the run lock (a check, or the GUI
/// command `sync_ns_mail`).
pub fn sync_ns_mail(db: &mut Db) -> Result<Vec<String>> {
    anyhow::ensure!(!crate::demo::is_demo(), "DEMO: in de demo wordt geen e-mail gelezen. / DEMO: e-mail is not read in the demo.");
    let cfg = smtp_config(db);
    anyhow::ensure!(cfg.is_configured(), "E-mail is nog niet ingesteld. / E-mail is not set up yet.");
    let (host, port) = ovrefund_core::ns_mail::imap_for_smtp(&cfg.host).ok_or_else(|| {
        anyhow::anyhow!(
            "NS-mails lezen werkt niet met {}. Zet ‘NS-mails lezen’ uit, of gebruik een ander e-mailadres. / Reading NS e-mails does not work with {}. Turn off ‘Read NS e-mails’, or use a different e-mail address.",
            cfg.host,
            cfg.host
        )
    })?;
    let pw = smtp_password(db)?
        .ok_or_else(|| anyhow::anyhow!("Er is geen app-wachtwoord opgeslagen. Vul het in bij ‘E-mail voor meldingen’. / No app password is stored. Enter it under ‘E-mail for notifications’."))?;
    let since = chrono::Utc::now().date_naive() - chrono::Duration::days(550);
    let fetched = ovrefund_core::ns_mail::fetch(host, port, &cfg.username, &pw, since).map_err(|e| {
        let d = ovrefund_core::redact::redact(&format!("{e:#}")).replace(" / ", "/");
        anyhow::anyhow!("NS-mails lezen lukte niet ({d}). / Reading NS e-mails failed ({d}).")
    })?;
    // Diagnostics only: NS-looking mails that were not used, counted per sender domain.
    db.set_setting(MAIL_SKIPPED_KEY, &serde_json::to_string(&fetched.skipped)?)?;
    let mails = fetched.mails;
    let names: Vec<String> = {
        let mut st = db.conn.prepare(
            "SELECT DISTINCT origin_name FROM journeys UNION SELECT DISTINCT dest_name FROM journeys WHERE dest_name IS NOT NULL",
        )?;
        let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        v
    };
    let l = lang(db);
    let mut lines =
        vec![format!("{} {}", l.count(mails.len(), ("NS-mail", "NS-mails"), ("NS e-mail", "NS e-mails")), l.t("gevonden", "found"))];
    let nl = l == email::Lang::Nl;
    for m in &mails {
        if let Some(parsed) = ovrefund_core::ns_mail::parse(&m.key, &m.subject, &m.body, m.date, &names) {
            if let Some(u) = db.apply_ns_mail(&parsed)? {
                lines.push(u.line(nl));
            }
        }
    }
    // Mails that could not be tied to a trip earlier (e.g. the trip was fetched since).
    for u in db.retry_unmatched_mails()? {
        lines.push(u.line(nl));
    }
    db.set_setting("last_mail_sync", &chrono::Utc::now().to_rfc3339())?;
    log::info!("ns mail sync: {} update(s)", lines.len());
    Ok(lines)
}

/// Keep the WebView2 profile lean: delete crash reports and caches of optional components.
/// Cookies and the NS login session (in `Default/`) are never touched.
fn prune_webview_profile() {
    let base = dirs::data_local_dir().unwrap_or_default().join("nl.ovrefund.app").join("EBWebView");
    for d in [
        "Crashpad/reports",
        "Crashpad/attachments",
        "component_crx_cache",
        "WidevineCdm",
        "Subresource Filter",
        "GrShaderCache",
        "ShaderCache",
        "BrowserMetrics",
        "Speech Recognition",
        "Default/Service Worker",
        "Default/GPUCache",
        "Default/DawnGraphiteCache",
        "Default/DawnWebGPUCache",
        "Default/Code Cache",
        "hyphen-data",
    ] {
        let p = base.join(d);
        if p.exists() {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// Verify the executable path using the open handle, so PID reuse cannot target another image.
#[cfg(windows)]
unsafe fn process_image_matches(handle: windows::Win32::Foundation::HANDLE) -> bool {
    use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_err() {
        return false;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    std::env::current_exe().ok().is_some_and(|own| own.to_string_lossy().eq_ignore_ascii_case(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn foreign_pid_is_neither_a_live_app_lease_nor_a_termination_target() {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = ChildGuard(
            Command::new("cmd.exe")
                .args(["/d", "/c", "set /p OVREFUND_TEST_INPUT="])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x08000000)
                .spawn()
                .unwrap(),
        );
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(child.0.id()));
        assert!(!kill_process(child.0.id()));
        assert!(child.0.try_wait().unwrap().is_none(), "foreign image must remain running");
        let db = Db::open_in_memory().unwrap();
        let owner = format!("pid{}-stale", child.0.id());
        db.acquire_run_lock(&owner, chrono::Duration::minutes(30)).unwrap();
        recover_stale_lock(&db).unwrap();
        assert!(db.run_lock_owner().unwrap().is_none());
    }

    #[test]
    fn app_checks_submit_only_urgent_claims() {
        assert_eq!(auto_due_window(true), None, "scheduled check: every qualifying claim");
        assert_eq!(auto_due_window(false), Some(chrono::Duration::days(GUI_AUTO_DUE_DAYS)));
        assert_eq!(GUI_AUTO_DUE_DAYS, 14);
    }

    #[test]
    fn automatic_submission_requires_all_gates() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting("mode", "automatic").unwrap();
        assert!(!auto_submission_allowed(&db, true, true));
        db.set_setting("auto_opt_in_at", "consent").unwrap();
        assert!(!auto_submission_allowed(&db, true, true), "unanswered claim-service question");
        assert_eq!(other_service_msg(&db), OTHER_SERVICE_UNANSWERED_MSG);
        for service in ["none", "yes", "import_done", ""] {
            db.set_setting("other_service", service).unwrap();
            assert_eq!(auto_submission_allowed(&db, true, true), service == "none" || service == "import_done", "{service}");
            assert!(!auto_submission_allowed(&db, false, true));
            assert!(!auto_submission_allowed(&db, true, false));
        }
        db.set_setting("mode", "manual").unwrap();
        assert!(!auto_submission_allowed(&db, true, true));
    }

    #[test]
    fn reminder_policy_follows_the_interval() {
        let p = ReminderPolicy::new(1);
        assert_eq!(p.stale_after, chrono::Duration::days(7));
        assert_eq!(p.throttle, chrono::Duration::days(1));
        assert_eq!(p.email_throttle, chrono::Duration::days(7));
        let p = ReminderPolicy::new(14);
        assert_eq!(p.stale_after, chrono::Duration::days(14));
        assert_eq!(p.throttle, chrono::Duration::days(14));
        assert_eq!(p.email_throttle, chrono::Duration::days(28));
        assert!(ReminderPolicy::new(0).throttle >= chrono::Duration::hours(20));
    }

    #[test]
    fn only_actionable_claims_count_for_the_reminder() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO journeys(id,source_id,source_record_id,fallback_key,check_in_utc,origin_uic,origin_name,payload_sha256,first_seen_at,observed_at)
                 VALUES(1,'s','a','k1','2026-10-28T19:01:00Z','1','X','x','t','t'),(2,'s','b','k2','2026-10-26T19:01:00Z','1','X','x','t','t'),
                       (3,'s','c','k3','2026-10-27T19:01:00Z','1','X','x','t','t'),(4,'s','d','k4','2026-10-28T19:01:00Z','1','X','x','t','t');
                 INSERT INTO claims(journey_id,journey_key,gateway,status,attempts,created_at,updated_at) VALUES
                   (1,'k1','g','SUBMITTED',1,'t','t'),(2,'k2','g','FAILED',3,'t','t'),(3,'k3','g','APPROVED',1,'t','t');",
            )
            .unwrap();
        let n = |db: &Db| db.conn.query_row(ACTIONABLE_SQL, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(n(&db), 0, "submitted, approved and FAILED at the retry limit are not actionable");
        db.conn.execute("INSERT INTO claims(journey_id,journey_key,gateway,status,attempts,created_at,updated_at) VALUES(4,'k4','g','READY',0,'t','t')", []).unwrap();
        assert_eq!(n(&db), 1);
    }

    #[test]
    fn reminder_only_when_useful_and_not_twice_a_day() {
        assert!(reminder_due(1, false, false));
        assert!(reminder_due(0, true, false));
        assert!(!reminder_due(0, false, false), "nothing open and checked recently");
        assert!(!reminder_due(3, true, true), "already reminded today");
    }

    #[test]
    fn one_ns_account_per_ledger() {
        let db = Db::open_in_memory().unwrap();
        assert!(!account_mismatch(&db, "a@example.com"), "nothing stored yet");
        db.set_setting("ns_account_email", "A@example.com").unwrap();
        assert!(!account_mismatch(&db, "b@example.com"), "no trips yet: any account");
        db.conn
            .execute_batch(
                "INSERT INTO journeys(source_id,source_record_id,fallback_key,check_in_utc,origin_uic,origin_name,payload_sha256,first_seen_at,observed_at)
                 VALUES('s','a','k1','2026-10-28T19:01:00Z','1','X','x','t','t');",
            )
            .unwrap();
        assert!(!account_mismatch(&db, " a@EXAMPLE.COM "));
        assert!(account_mismatch(&db, "b@example.com"));
        assert!(OTHER_SERVICE_MSG.contains(" / ") && OTHER_SERVICE_MSG.contains("Import klaar"));
        assert!(OTHER_SERVICE_UNANSWERED_MSG.contains(" / "));
    }

    fn finished_run(db: &Db, mode: &str, trigger: &str, outcome: &str) {
        let id = db.start_run(mode, trigger).unwrap();
        db.finish_run(id, outcome, &RunSummary::default()).unwrap();
    }

    fn auto_on(db: &Db) {
        db.set_setting("mode", "automatic").unwrap();
        db.set_setting("auto_opt_in_at", "consent").unwrap();
    }

    #[test]
    fn checks_from_the_app_never_skip_a_scheduled_automatic_check() {
        let db = Db::open_in_memory().unwrap();
        auto_on(&db);
        let now = chrono::Utc::now();
        // Opening the app (window-limited, urgent claims only) and a start-up check.
        finished_run(&db, "automatic", "gui", "ok");
        finished_run(&db, "manual", "gui", "ok");
        assert_eq!(skip_scheduled_run(&db, "cli", now, true), None);
        // A failed or logged-out background check does not count either.
        finished_run(&db, "automatic", "cli", "auth");
        finished_run(&db, "automatic", "cli", "error");
        assert_eq!(skip_scheduled_run(&db, "cli", now, true), None);
        // A successful automatic background check does.
        finished_run(&db, "automatic", "cli", "ok");
        assert!(skip_scheduled_run(&db, "cli", now, true).is_some());
        assert_eq!(skip_scheduled_run(&db, "cli", now + chrono::Duration::hours(13), true), None, "12 hours later");
        // The extra check before a deadline is never skipped.
        assert_eq!(skip_scheduled_run(&db, "deadline", now, true), None);
    }

    #[test]
    fn deadline_check_counts_as_a_background_check() {
        let db = Db::open_in_memory().unwrap();
        auto_on(&db);
        finished_run(&db, "automatic", "deadline", "ok");
        assert!(skip_scheduled_run(&db, "cli", chrono::Utc::now(), true).is_some());
        // Manual mode: any finished check counts (nothing is submitted in the background).
        let manual = Db::open_in_memory().unwrap();
        finished_run(&manual, "manual", "gui", "ok");
        assert!(skip_scheduled_run(&manual, "cli", chrono::Utc::now(), false).is_some());
    }

    #[test]
    fn automatic_mode_that_cannot_submit_skips_like_manual_mode() {
        // Automatic mode is on, but a gate (bank account, claim service, ...) keeps checks manual:
        // a finished check counts, so the task does not run a full check at every logon.
        let db = Db::open_in_memory().unwrap();
        auto_on(&db);
        finished_run(&db, "manual", "cli", "ok");
        let now = chrono::Utc::now();
        assert!(skip_scheduled_run(&db, "cli", now, false).is_some());
        assert_eq!(skip_scheduled_run(&db, "cli", now, true), None, "allowed: only an automatic background check counts");
    }

    #[test]
    fn gate_reason_names_the_first_closed_gate() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(auto_gate_reason(&db, true, true), None, "automatic submission is off");
        auto_on(&db);
        assert_eq!(auto_gate_reason(&db, true, true).unwrap().0, "service_unanswered");
        db.set_setting("other_service", "yes").unwrap();
        assert_eq!(auto_gate_reason(&db, true, true).unwrap().0, "other_service");
        db.set_setting("other_service", "none").unwrap();
        assert_eq!(auto_gate_reason(&db, true, false).unwrap().0, "bank");
        assert_eq!(auto_gate_reason(&db, false, true).unwrap().0, "unverified");
        assert_eq!(auto_gate_reason(&db, true, true), None);
        for r in [PAUSED_OTHER_SERVICE_REASON, PAUSED_SERVICE_UNANSWERED_REASON, PAUSED_BANK_REASON, PAUSED_UNVERIFIED_REASON] {
            assert!(r.starts_with("Gepauzeerd: ") && r.contains(" / Paused: "), "{r}");
        }
        let mut s = RunSummary::default();
        note_gate_reason(&mut s, auto_gate_reason(&db, true, false));
        assert_eq!(s.auto_paused_code.as_deref(), Some("bank"));
        // A reason the run itself found (e.g. uncertain claims) is kept.
        let mut s = RunSummary { auto_paused_code: Some("uncertain".into()), auto_paused_reason: Some("x".into()), ..Default::default() };
        note_gate_reason(&mut s, auto_gate_reason(&db, true, false));
        assert_eq!(s.auto_paused_code.as_deref(), Some("uncertain"));
    }

    #[test]
    fn scan_is_a_manual_diagnostic() {
        assert_eq!(SCAN_TRIGGER, "scan");
        assert!(RUN_BUSY_MSG.contains(" / "));
    }

    #[test]
    fn a_scheduled_check_waits_for_the_app_but_not_for_another_scheduled_check() {
        let step = std::time::Duration::from_millis(1);
        let max = std::time::Duration::from_secs(5);
        // From the app: refused right away.
        let tries = std::cell::Cell::new(0);
        let r = wait_for_run_lock(
            || {
                tries.set(tries.get() + 1);
                Ok(false)
            },
            || Ok(Some("pid1-gui-edit-3".into())),
            false,
            max,
            step,
        );
        assert_eq!(r.unwrap_err().to_string(), RUN_BUSY_MSG);
        assert_eq!(tries.get(), 1);
        // Scheduled, the app holds the lock for a while: waits and then runs.
        let tries = std::cell::Cell::new(0);
        wait_for_run_lock(
            || {
                tries.set(tries.get() + 1);
                Ok(tries.get() > 3)
            },
            || Ok(Some(run_owner(false))),
            true,
            max,
            step,
        )
        .unwrap();
        assert_eq!(tries.get(), 4);
        // Scheduled, another scheduled check holds it: stops right away.
        let tries = std::cell::Cell::new(0);
        let r = wait_for_run_lock(
            || {
                tries.set(tries.get() + 1);
                Ok(false)
            },
            || Ok(Some(run_owner(true))),
            true,
            max,
            step,
        );
        assert!(r.is_err());
        assert_eq!(tries.get(), 1);
        // Never longer than the maximum.
        let start = std::time::Instant::now();
        assert!(wait_for_run_lock(|| Ok(false), || Ok(None), true, std::time::Duration::from_millis(30), step).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert!(is_scheduled_owner(&run_owner(true)) && !is_scheduled_owner(&run_owner(false)));
        assert_eq!(lock_owner_pid(&run_owner(true)), Some(std::process::id()));
    }

    #[test]
    fn a_reminder_never_plans_an_extra_check() {
        assert!(plans_deadline_after(&Ok((RunSummary::default(), Contacted::Ns))));
        assert!(!plans_deadline_after(&Ok((RunSummary::default(), Contacted::ReminderOnly))));
        assert!(!plans_deadline_after(&Err(anyhow::anyhow!(RUN_BUSY_MSG))));
    }

    /// One READY claim with automatic-grade evidence whose claim window ends at `exp`.
    fn claim_due(db: &mut Db, exp: chrono::DateTime<chrono::Utc>) {
        use ovrefund_core::model::*;
        let at = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc);
        let j = Journey {
            source_id: "fixture".into(),
            source_record_id: "D1".into(),
            coupling_id: Some("00000000-0000-0000-0000-000000000000".into()),
            card_last4: Some("0000".into()),
            check_in: at("2026-10-28T20:01:00+01:00"),
            check_out: Some(at("2026-10-28T22:41:00+01:00")),
            origin: StationRef { uic: "8400055".into(), name: "Amersfoort Centraal".into() },
            destination: Some(StationRef { uic: "8400319".into(), name: "'s-Hertogenbosch".into() }),
            carrier_code: Some("4".into()),
            carrier_name: Some("Nederlandse Spoorwegen".into()),
            product_name: None,
            travel_class: Some(2),
            fare_cents: None,
            subscription: None,
            subscription_fee_cents: None,
            invoiced: None,
            payload_sha256: "x".into(),
        };
        let (jid, _) = db.upsert_journey(&j).unwrap();
        db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
        db.save_assessment(
            jid,
            &Assessment {
                confidence: Confidence::SupportedForAuto,
                checkout_minus_scheduled_arrival_min: Some(33),
                estimated_arrival_delay_min: Some(33),
                delay_band: Some(DelayBand::Min30To59),
                scheduled_departure: Some(j.check_in),
                reasons: vec![],
                expires_at: exp,
            },
        )
        .unwrap();
    }

    #[test]
    fn deadline_check_is_planned_again_while_its_claim_still_waits() {
        let mut db = Db::open_in_memory().unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let next = now + chrono::Duration::days(7);
        // Nothing due, or not allowed: remove.
        assert_eq!(deadline_plan(&db, now, true, next).unwrap(), DeadlinePlan::Remove);
        let deadline = now + chrono::Duration::days(2);
        claim_due(&mut db, deadline);
        assert_eq!(deadline_plan(&db, now, false, next).unwrap(), DeadlinePlan::Remove);
        // Less than 4 days left: an hour from now.
        let at = now + chrono::Duration::hours(1);
        assert_eq!(deadline_plan(&db, now, true, next).unwrap(), DeadlinePlan::Install { at, deadline, retry: false });
        db.set_setting(DEADLINE_PLANNED_KEY, &planned_value(deadline, at, now, false)).unwrap();
        // Its time has not come: kept.
        assert_eq!(deadline_plan(&db, now + chrono::Duration::minutes(30), true, next).unwrap(), DeadlinePlan::Keep);
        // Its time has passed and the claim still waits (whatever kept it: login expired, claim
        // list unavailable, time limit, a missed check): again an hour later ...
        let t = at + chrono::Duration::minutes(5);
        let retry_at = t + chrono::Duration::hours(1);
        assert_eq!(deadline_plan(&db, t, true, next).unwrap(), DeadlinePlan::Install { at: retry_at, deadline, retry: true });
        // ... and after a retry at 10:00 (Dutch time) the next day.
        db.set_setting(DEADLINE_PLANNED_KEY, &planned_value(deadline, retry_at, t, true)).unwrap();
        assert_eq!(deadline_plan(&db, retry_at - chrono::Duration::minutes(1), true, next).unwrap(), DeadlinePlan::Keep);
        let t2 = retry_at + chrono::Duration::minutes(5);
        let tomorrow = chrono::DateTime::parse_from_rfc3339("2026-11-14T10:00:00+01:00").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(deadline_plan(&db, t2, true, next).unwrap(), DeadlinePlan::Install { at: tomorrow, deadline, retry: true });
        // Never after the deadline: the plan is left as it is.
        let close = deadline - chrono::Duration::minutes(90);
        db.set_setting(DEADLINE_PLANNED_KEY, &planned_value(deadline, close - chrono::Duration::minutes(10), close, true)).unwrap();
        assert_eq!(deadline_plan(&db, close, true, next).unwrap(), DeadlinePlan::Keep);
        // A different deadline is planned right away.
        db.set_setting(DEADLINE_PLANNED_KEY, &format!("{}|{}", (deadline + chrono::Duration::days(1)).to_rfc3339(), now.to_rfc3339()))
            .unwrap();
        assert!(matches!(deadline_plan(&db, t, true, next).unwrap(), DeadlinePlan::Install { retry: false, .. }));
        // The claim no longer waits (submitted, claimed elsewhere, ...): nothing to plan.
        let c = db.claims_in(&[ovrefund_core::model::ClaimState::Ready]).unwrap()[0].clone();
        db.mark_external(c.id, "user").unwrap();
        assert_eq!(deadline_plan(&db, t, true, next).unwrap(), DeadlinePlan::Remove);
    }

    #[test]
    fn extra_check_planned_days_ahead_is_planned_again_when_its_claim_still_waits() {
        let mut db = Db::open_in_memory().unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let next = now + chrono::Duration::days(7);
        let deadline = now + chrono::Duration::days(8);
        claim_due(&mut db, deadline);
        let DeadlinePlan::Install { at, .. } = deadline_plan(&db, now, true, next).unwrap() else { panic!("expected a plan") };
        assert!(at - now > chrono::Duration::days(3), "planned days ahead");
        db.set_setting(DEADLINE_PLANNED_KEY, &planned_value(deadline, at, now, false)).unwrap();
        // A check from the app before that time leaves it alone.
        assert_eq!(deadline_plan(&db, now + chrono::Duration::days(1), true, next).unwrap(), DeadlinePlan::Keep);
        // The extra check ran but could not submit (e.g. NS's claim list was unavailable or the
        // time ran out): it is not counted as done.
        let running = at + chrono::Duration::minutes(5);
        assert_eq!(
            deadline_plan(&db, running, true, next).unwrap(),
            DeadlinePlan::Install { at: running + chrono::Duration::hours(1), deadline, retry: true }
        );
    }

    #[test]
    fn a_claim_refused_by_the_final_check_plans_no_extra_check() {
        let mut db = Db::open_in_memory().unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let next = now + chrono::Duration::days(7);
        claim_due(&mut db, now + chrono::Duration::days(2));
        let c = db.claims_in(&[ovrefund_core::model::ClaimState::Ready]).unwrap()[0].clone();
        db.set_setting(&format!("{}{}", pipeline::AUTO_REFUSED_KEY_PREFIX, c.id), &now.to_rfc3339()).unwrap();
        // Not a retryable failure: no extra check (and no daily e-mail) for it.
        assert_eq!(deadline_plan(&db, now, true, next).unwrap(), DeadlinePlan::Remove);
    }

    #[test]
    fn deadline_reminder_is_not_throttled_and_has_its_own_text() {
        // Something to act on: the extra check before a deadline always reminds, also by e-mail.
        assert_eq!(reminder_kind(true, 1, false, true, true), Some(ReminderKind::Deadline));
        // Nothing open any more: no reminder.
        assert_eq!(reminder_kind(true, 0, false, false, false), None);
        // A regular check stays throttled.
        assert_eq!(reminder_kind(false, 1, false, true, false), None);
        assert_eq!(reminder_kind(false, 1, false, false, true), Some(ReminderKind::Regular { mail: false }));
        assert_eq!(reminder_kind(false, 0, true, false, false), Some(ReminderKind::Regular { mail: true }));
        assert!(
            DEADLINE_REMINDER_MSG.starts_with("Een claim verloopt binnenkort.")
                && DEADLINE_REMINDER_MSG.contains(" / A claim expires soon.")
        );
    }

    #[test]
    fn time_budget_counts_the_wait_for_the_lock() {
        let min = |m: u64| std::time::Duration::from_secs(m * 60);
        assert_eq!(auto_time_budget(min(0)), chrono::Duration::minutes(90));
        assert_eq!(auto_time_budget(min(15)), chrono::Duration::minutes(75));
        assert_eq!(auto_time_budget(min(120)), chrono::Duration::zero());
    }

    #[test]
    fn claims_flagged_near_an_ns_claim_plan_no_extra_check() {
        let mut db = Db::open_in_memory().unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        claim_due(&mut db, now + chrono::Duration::days(2));
        let c = db.claims_in(&[ovrefund_core::model::ClaimState::Ready]).unwrap()[0].clone();
        let key = db.journey(c.journey_id).unwrap().natural_key();
        db.set_ns_claim_near_keys(&[key]).unwrap();
        assert_eq!(deadline_plan(&db, now, true, now + chrono::Duration::days(7)).unwrap(), DeadlinePlan::Remove);
    }
}
