//! Tauri commands for the local GUI (main window only — see capabilities/main.json).
use crate::ns_adapters::NsClaimGateway;
use crate::ns_bridge::{NsBridge, SessionStatus};
use crate::runner::{self, open_db, setting};
use crate::{scheduler, secrets};
use ovrefund_core::model::*;
use ovrefund_core::pipeline::{self, ClaimGateway, RunMode, StopReason};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::{AppHandle, Emitter};

// Every acquisition needs its own owner, even on concurrent calls in the same millisecond.
static LOCK_SEQUENCE: AtomicU64 = AtomicU64::new(0);
fn edit_lock_owner() -> String {
    format!("pid{}-gui-edit-{}", std::process::id(), LOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed))
}

static BUSY: AtomicBool = AtomicBool::new(false);
/// The visible NS login window is open (its watcher drives the shared "ns" window).
static LOGIN_OPEN: AtomicBool = AtomicBool::new(false);

/// RAII form of the ledger lock (for longer command bodies).
pub(crate) struct LedgerLock(pub(crate) String);
impl LedgerLock {
    fn take() -> anyhow::Result<Self> {
        Self::take_for(2)
    }
    fn take_for(minutes: i64) -> anyhow::Result<Self> {
        Self::take_with(
            minutes,
            "Er loopt al een controle. Probeer het zo opnieuw. / A check is already running. Please try again shortly.",
        )
    }
    pub(crate) fn take_with(minutes: i64, busy_msg: &'static str) -> anyhow::Result<Self> {
        let db = open_db()?;
        // A lease left by a crashed/killed run must not block the user for up to 30 minutes.
        runner::recover_stale_lock(&db)?;
        let owner = edit_lock_owner();
        anyhow::ensure!(db.acquire_run_lock(&owner, chrono::Duration::minutes(minutes))?, busy_msg);
        Ok(LedgerLock(owner))
    }
    /// Take the lock; a check in its automatic-submission phase is stopped (for `reason`) and
    /// waited for (see [`wait_while_auto_phase`]) until it has finished its current claim and
    /// released the lock.
    fn take_after_stop(minutes: i64, reason: StopReason) -> anyhow::Result<Self> {
        wait_while_auto_phase(
            || Self::take_for(minutes),
            auto_phase_now,
            || stop_auto_submission(reason),
            STOP_WAIT,
            std::time::Duration::from_millis(500),
        )
    }
}

/// Retry `attempt` when a running check is in its automatic-submission phase
/// ([`pipeline::auto_phase_active`]): `stop` is called (the check ends after its current claim)
/// and `attempt` is retried until it succeeds, up to `wait`. While waiting, `stop` is called again
/// before every retry, so a check that takes the lock in the meantime (e.g. a scheduled check that
/// was waiting) is stopped too. After `wait`: [`STOPPING_BUSY_MSG`] while a check is still
/// submitting, otherwise `attempt`'s own error. Once that check was seen in its submission phase
/// it is waited for to the end (also through its closing steps, after the phase marker is gone).
/// A check that is still syncing (or any other busy action) is neither stopped nor waited for:
/// `attempt`'s own error comes back right away.
fn wait_while_auto_phase<T>(
    mut attempt: impl FnMut() -> anyhow::Result<T>,
    auto_phase: impl Fn() -> anyhow::Result<bool>,
    mut stop: impl FnMut() -> anyhow::Result<()>,
    wait: std::time::Duration,
    step: std::time::Duration,
) -> anyhow::Result<T> {
    let start = std::time::Instant::now();
    let mut waiting = false;
    loop {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !waiting {
                    if !auto_phase()? {
                        return Err(e);
                    }
                    waiting = true;
                }
                // Again before every retry: the stop names the check holding the lock right now.
                stop()?;
                if start.elapsed() >= wait {
                    if auto_phase()? {
                        anyhow::bail!(STOPPING_BUSY_MSG);
                    }
                    return Err(e);
                }
                std::thread::sleep(step);
            }
        }
    }
}

fn auto_phase_now() -> anyhow::Result<bool> {
    pipeline::auto_phase_active(&open_db()?)
}

/// How long withdrawing consent waits for a check in its automatic-submission phase (a
/// submission in progress always completes; the check stops before the next claim).
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(120);
const STOPPING_BUSY_MSG: &str = "Automatisch indienen stopt. Een claim die al wordt verstuurd, kan nog doorgaan. De controle is nog bezig. Probeer het zo opnieuw. / Automatic submission is stopping. A claim already being sent may still go through. The check is still busy. Please try again shortly.";

/// Ask a running check (this app or a scheduled one) to stop submitting automatically. Needs no
/// lock: stopping is always safe. The claim in progress completes.
fn stop_auto_submission(reason: StopReason) -> anyhow::Result<()> {
    runner::request_auto_stop(&open_db()?, reason)
}

/// Withdraw consent for automatic submission at once, without the run lock: every check reads it
/// again right before each submission (`auto_still_allowed`) and then submits nothing more. A
/// running check is asked to stop (`reason`; the claim being sent now completes) and the extra
/// check before a deadline is removed.
fn withdraw_auto_now(db: &ovrefund_core::db::Db, reason: StopReason) -> anyhow::Result<()> {
    if runner::auto_enabled(db) {
        withdraw_auto_consent(db)?;
    }
    runner::request_auto_stop(db, reason)?;
    if let Err(e) = scheduler::remove_deadline() {
        log::warn!("removing the extra check failed: {e:#}");
    }
    Ok(())
}
impl Drop for LedgerLock {
    fn drop(&mut self) {
        if let Ok(db) = open_db() {
            let _ = db.release_run_lock(&self.0);
        }
    }
}

pub(crate) fn login_window_open() -> CmdResult<()> {
    if LOGIN_OPEN.load(Ordering::SeqCst) {
        return Err(
            "Rond eerst het inloggen af in het NS-venster (of sluit het). / Finish logging in in the NS window first (or close it).".into(),
        );
    }
    Ok(())
}

/// Short ledger edits from the GUI take the same run lock as checks, so they never interleave
/// with a (possibly scheduled, other-process) run.
fn with_ledger_lock<T>(f: impl FnOnce(&mut ovrefund_core::db::Db) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let _lock = LedgerLock::take()?;
    f(&mut open_db()?)
}

type CmdResult<T> = Result<T, String>;
fn e<E: std::fmt::Display>(err: E) -> String {
    let message = ovrefund_core::redact::redact(&format!("{err:#}"));
    if message.contains(" / ") {
        message
    } else {
        // System/SQLite errors can include local paths and have no translated UI message.
        "De actie lukte niet. Probeer het opnieuw. / The action failed. Please try again.".into()
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(e)?.map_err(e)
}

pub(crate) struct BusyGuard;
impl BusyGuard {
    pub(crate) fn take() -> CmdResult<Self> {
        if BUSY.swap(true, Ordering::SeqCst) {
            return Err(
                "Er loopt nog een andere actie. Probeer het zo opnieuw. / Another action is still running. Please try again shortly."
                    .into(),
            );
        }
        Ok(BusyGuard)
    }
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn overview() -> CmdResult<Value> {
    blocking(|| {
        let db = open_db()?;
        let count = |sql: &str| db.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
        let src = db.source_status("mijnns")?;
        let export_loc = crate::paths::export_location();
        let card_last4: Option<String> = db
            .conn
            .query_row("SELECT card_last4 FROM journeys WHERE card_last4 IS NOT NULL ORDER BY check_in_utc DESC LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .ok();
        Ok(json!({
            "journeys": count("SELECT COUNT(*) FROM journeys"),
            "claimable": count("SELECT COUNT(*) FROM claims WHERE status='READY'"),
            "review": count("SELECT COUNT(*) FROM claims WHERE status='NEEDS_REVIEW'"),
            "submitted": count("SELECT COUNT(*) FROM claims WHERE status IN ('SUBMITTED','SUBMITTING')"),
            "uncertain": count("SELECT COUNT(*) FROM claims WHERE status = 'INDETERMINATE'"),
            // Automatic NS submission waits only for uncertain NS claims; an uncertain Arriva claim
            // has its own banner (the user checks their e-mail from Arriva).
            "uncertain_ns": uncertain_by_carrier(&db).0,
            "uncertain_arriva": uncertain_by_carrier(&db).1,
            "approved": count("SELECT COUNT(*) FROM claims WHERE status IN ('APPROVED','PAID')"),
            // "logged_out" after the user logged out (disconnect), as opposed to an expired session;
            // "blocked" for any NS block (the reason is in blocked_reason).
            "auth_state": match src.as_ref().and_then(|s| s.1.clone()) {
                Some(s) if s == "disconnected" => "logged_out".to_string(),
                Some(s) if s.starts_with("blocked") => "blocked".to_string(),
                Some(s) => s,
                None => "not_connected".into(),
            },
            "blocked_reason": src.as_ref().and_then(|s| s.1.clone()).and_then(|s| s.strip_prefix("blocked: ").map(str::to_string)),
            // Automatic login was switched off after two failures; credentials were erased.
            "autologin_disabled": setting(&db, crate::ns_bridge::AUTO_LOGIN_DISABLED, "0") == "1",
            // Outcome of the last NS claim-list read: "ok" | "unavailable" | "unknown".
            "claim_list_status": setting(&db, ovrefund_core::pipeline::CLAIM_LIST_STATUS_KEY, "unknown"),
            "iban_country": secrets::bank_details().map(|b| b.iban.chars().take(2).collect::<String>().to_uppercase()),
            "bug_form_key": BUG_FORM_KEY,
            "mail_unsupported_provider": runner::smtp_config(&db).unsupported_provider(),
            "ns_mail_skipped": skipped_summary(&db),
            "wizard_step": db.get_setting("wizard_step")?,
            "other_service": db.get_setting("other_service")?,
            "subscription": if crate::demo::is_demo() {
                crate::demo::subscription_summary(&crate::demo::subscription())
            } else { db.subscription_history(chrono::Utc::now())? },
            // How automatic submission was enabled: "verified" | "override" | null.
            "auto_enabled_via": db.get_setting("auto_enabled_via")?,
            "ns_help_url": NS_HELP_URL,
            "ns_refund_url": ovrefund_core::eligibility::NS_REFUND_PAGE,
            "last_sync": src.and_then(|s| s.2),
            "mode": setting(&db, "mode", "manual"),
            "auto_opt_in_at": db.get_setting("auto_opt_in_at")?,
            "gateway_verified": runner::gateway_verified(&db),
            "gateway_verified_by_submission": runner::gateway_verified_by_submission(&db),
            "bank": secrets::bank_hint(),
            "schedule": effective_schedule(&db, scheduler::status())?,
            "runs": db.runs(10)?,
            "onboarded": setting(&db, "onboarded", "0") == "1",
            "busy": BUSY.load(Ordering::SeqCst),
            "version": env!("CARGO_PKG_VERSION"),
            // Where exports and backups go (Documents\OV Refund, or the app's own folder when
            // Documents is synced by OneDrive); "forget everything" also deletes the backup and
            // CSV files OV Refund put there.
            "export_dir": export_loc.dir.to_string_lossy(),
            "onedrive_avoided": export_loc.onedrive_avoided,
            "forget_removes_exports": true,
            // The stored mail app password belongs to other mail settings: enter it again.
            "smtp_password_mismatch": secrets::get(secrets::SMTP_PASSWORD).ok().flatten().is_some() && !runner::smtp_password_matches(&db),
            "disclaimer_accepted": db.get_setting("disclaimer_accepted")?.is_some(),
            "session_saved": crate::session_store::exists() || crate::demo::is_demo(),
            "demo": crate::demo::is_demo(),
            "demo_subscription": if crate::demo::is_demo() { Some(crate::demo::subscription()) } else { None },
            "totals": db.totals()?,
            "account": {
                "card_last4": card_last4,
                // The OV-chipkaart number is shown in full (user decision); only the IBAN is masked.
                "card_number": card_last4.as_deref().and_then(|l4| {
                    if crate::demo::is_demo() {
                        return Some("3528 0000 0000 1234".to_string());
                    }
                    let one = db.couplings_for_last4(l4).ok().filter(|c| c.len() == 1).and_then(|c| c.into_iter().next());
                    let n = secrets::card_number(&db, one.as_deref(), l4)?;
                    Some(n.chars().collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" "))
                }),
                "ns_email": db.get_setting("ns_account_email")?,
                "notify_email": runner::smtp_config(&db).to,
                "iban_last4": secrets::bank_details().map(|b| b.iban[b.iban.len().saturating_sub(4)..].to_string()),
                "bank_name": secrets::bank_details().map(|b| b.name),
                "bank_institution": secrets::bank_details().and_then(|b| secrets::nl_bank_name(&b.iban)),
            },
            "interval_days": setting(&db, "interval_days", "7").parse::<u32>().unwrap_or(7),
            "auto_login": setting(&db, "auto_login", "1") == "1" && secrets::ns_login().is_some(),
            "read_ns_mail": setting(&db, "read_ns_mail", "0") == "1",
            "wizard_done": wizard_done(&db),
            "no_card": setting(&db, "no_card", "0") == "1",
            "ns_mail": db.ns_mail_summary()?,
            "attribution": ovrefund_core::archive::ATTRIBUTION,
        }))
    })
    .await
}

/// Uncertain (INDETERMINATE) claims: (NS, Arriva).
fn uncertain_by_carrier(db: &ovrefund_core::db::Db) -> (usize, usize) {
    let ids: Vec<i64> = db
        .conn
        .prepare("SELECT journey_id FROM claims WHERE status = 'INDETERMINATE'")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .unwrap_or_default();
    let arriva = ids.iter().filter(|id| db.journey(**id).is_ok_and(|j| j.is_arriva())).count();
    (ids.len() - arriva, arriva)
}

/// "ns" | "arriva" | "other": the one classification the app uses for a trip's carrier (model.rs).
pub(crate) fn carrier_kind(j: &ovrefund_core::model::Journey) -> &'static str {
    if j.is_ns_carrier() {
        "ns"
    } else if j.is_arriva() {
        "arriva"
    } else {
        "other"
    }
}

/// Arriva's rules for one trip, for the app's texts (carrier_policy.rs is the source): region, the
/// region's minimum delay, the refund percentage for this delay, the claim deadline, and why the
/// trip cannot be claimed in the app (`blocker`), if so.
fn arriva_info(db: &ovrefund_core::db::Db, r: &ovrefund_core::db::JourneyRow, today: chrono::NaiveDate) -> Value {
    let p = &ovrefund_core::carrier_policy::ARRIVA;
    let j = &r.journey;
    let region = j.arriva_region();
    let travel = j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
    let a = p.assess(region, r.delay_min.unwrap_or(0), travel, today);
    let eligible_reason = r.reasons.first().is_some_and(|x| x.starts_with(ovrefund_core::eligibility::ARRIVA_ELIGIBLE_PREFIX));
    let blocker = if eligible_reason { crate::arriva_claim::blocker(db, j) } else { None };
    json!({
        "region": region,
        "min_delay": region.map(|_| a.min_delay),
        "refund_percent": a.refund_percent,
        "deadline": a.deadline.map(|d| d.format("%Y-%m-%d").to_string()),
        "blocker": blocker.map(|(code, nl, en)| json!({"code": code, "nl": nl, "en": en})),
    })
}

#[tauri::command]
pub async fn journeys(limit: Option<i64>) -> CmdResult<Value> {
    blocking(move || {
        let db = open_db()?;
        let today = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
        let rows = db.journey_rows(limit.unwrap_or(500).clamp(1, 10_000))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let kind = carrier_kind(&r.journey);
            let arriva = if kind == "arriva" { arriva_info(&db, &r, today) } else { Value::Null };
            let mut v = serde_json::to_value(&r)?;
            v["journey"]["carrier_kind"] = json!(kind);
            v["arriva"] = arriva;
            out.push(v);
        }
        Ok(Value::Array(out))
    })
    .await
}

#[tauri::command]
pub async fn claims() -> CmdResult<Value> {
    blocking(|| {
        let db = open_db()?;
        let mut out = vec![];
        for c in db.all_claims()? {
            let j = db.journey(c.journey_id)?;
            out.push(json!({ "claim": c, "journey": j, "events": db.events(c.id)? }));
        }
        Ok(Value::Array(out))
    })
    .await
}

#[tauri::command]
pub async fn scan_now(app: AppHandle, full: Option<bool>) -> CmdResult<Value> {
    login_window_open()?;
    let _g = BusyGuard::take()?;
    let a = app.clone();
    let full = full.unwrap_or(false);
    let r = blocking(move || Ok(serde_json::to_value(runner::run(&a, "gui", false, full)?)?)).await;
    let _ = app.emit("run-finished", &r);
    r
}

/// "Stop": a running check (in the app or scheduled) submits no further claims automatically.
/// The claim in progress completes. Takes no lock.
#[tauri::command]
pub async fn stop_auto_run() -> CmdResult<()> {
    blocking(|| stop_auto_submission(StopReason::User)).await
}

/// Show the NS login window (visible). Session cookies persist in the app's WebView2 profile.
#[tauri::command]
pub async fn open_login(app: AppHandle) -> CmdResult<()> {
    if crate::demo::is_demo() {
        // Simulated login: no NS window in the demo.
        blocking(|| {
            let db = open_db()?;
            db.upsert_source("mijnns", "mijnns", true, "ok")?;
            db.set_setting("ns_account_email", crate::demo::DEMO_EMAIL)?;
            pipeline::clear_pending_stop(&db)?;
            Ok(())
        })
        .await?;
        let _ = app.emit("login-complete", ());
        return Ok(());
    }
    login_window_open()?;
    let _g = BusyGuard::take()?;
    blocking(move || {
        // Keep scheduled checks out until the interactive login watcher has finished saving.
        let lock = LedgerLock::take_for(20)?;
        // A logout that waited for a check is finished first (never after this login).
        finish_pending_before(&app)?;
        let b = NsBridge::open(&app, true)?;
        let mark0 = b.mark();
        b.navigate(crate::ns_bridge::MIJNNS_URL)?;
        b.show();
        LOGIN_OPEN.store(true, Ordering::SeqCst);
        // Watch for a completed login on the official page, then close the window by itself.
        let app2 = app.clone();
        std::thread::spawn(move || {
            use tauri::Emitter;
            struct Done;
            impl Drop for Done {
                fn drop(&mut self) {
                    LOGIN_OPEN.store(false, Ordering::SeqCst);
                }
            }
            let _done = Done;
            let lock = lock;
            let start = std::time::Instant::now();
            let mut banner_done = false;
            while start.elapsed() < std::time::Duration::from_secs(15 * 60) {
                std::thread::sleep(std::time::Duration::from_millis(400));
                if !b.window.is_visible().unwrap_or(false) {
                    return; // user closed it; the close handler triggers a session check
                }
                let host = b.window.url().ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                if crate::ns_bridge::is_ns_login_host(&host) {
                    continue;
                }
                if !banner_done && host == "www.ns.nl" {
                    banner_done = b.dismiss_cookie_banner();
                }
                // Only hide once Mijn NS really shows a logged-in page (not on NS's password-reset
                // or account pages, which also live on www.ns.nl).
                if host == "www.ns.nl" && b.logged_in_now(mark0) {
                    b.hide();
                    finish_login(&b, mark0);
                    let _ = b.navigate("about:blank");
                    drop(lock);
                    LOGIN_OPEN.store(false, Ordering::SeqCst);
                    let _ = app2.emit("login-complete", ());
                    return;
                }
            }
            // The interactive window must not remain usable after its lease has ended.
            b.hide();
        });
        Ok(())
    })
    .await
}

/// After a successful login in the visible window: one NS account per Windows user (a different
/// account is refused and nothing is saved), then save the session, the account and whether a
/// personal OV-chipkaart is linked (from the card list Mijn NS itself loaded).
fn finish_login(b: &NsBridge, mark: u64) {
    let Ok(db) = open_db() else { return };
    let mut email = None;
    for _ in 0..10 {
        email = crate::ns_adapters::account_email(b);
        if email.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if email.as_deref().map(|e| runner::account_mismatch(&db, e)).unwrap_or(false) {
        log::warn!("login with a different NS account than the stored trips; not saved");
        if secrets::delete(secrets::NS_LOGIN).is_ok() {
            let _ = db.set_setting("auto_login", "0");
        }
        let _ = db.upsert_source("mijnns", "mijnns", false, "account_mismatch");
        // Never keep the other account's session: log it out of the app's browser profile.
        clear_ns_browser_session(b);
        return;
    }
    if let Err(e) = crate::session_store::save(b) {
        log::warn!("session save failed: {e:#}");
    }
    if let Some(email) = &email {
        let _ = db.set_setting("ns_account_email", email);
    }
    let _ = db.upsert_source("mijnns", "mijnns", true, "ok");
    // A stop that waited for a check is not carried past a successful login.
    let _ = pipeline::clear_pending_stop(&db);
    // Card check right after login (cheap: the page already asked NS for the linked cards).
    for _ in 0..12 {
        if let Some(no_card) = b
            .captured_since(mark)
            .iter()
            .filter(|c| c.url.contains("/mijnns-card-coupling-api/v2/couplings/me") && c.status == 200)
            .filter_map(|c| c.body.as_deref().and_then(|body| serde_json::from_str::<Value>(body).ok()))
            .find_map(|v| v.as_array().map(|a| a.is_empty()))
        {
            let _ = db.set_setting("no_card", if no_card { "1" } else { "0" });
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Delete NS cookies and site storage from the app's browser profile (best effort).
fn clear_ns_browser_session(b: &NsBridge) {
    let t = std::time::Duration::from_secs(10);
    if let Err(e) = b.cdp("Network.clearBrowserCookies", json!({}), t) {
        log::warn!("clearing cookies failed: {e:#}");
    }
    for origin in NS_ORIGINS {
        let _ = b.cdp("Storage.clearDataForOrigin", json!({"origin": origin, "storageTypes": "all"}), t);
    }
}

const NS_ORIGINS: [&str; 3] = ["https://www.ns.nl", "https://login.ns.nl", "https://loginapi.ns.nl"];

/// First-start wizard done? Explicit flag only (set by the wizard). Installations from before the
/// flag existed (no `wizard_done` at all, but onboarded or the disclaimer accepted) count as done.
/// Never inferred from trips, so a restored backup alone never skips the wizard.
fn wizard_done(db: &ovrefund_core::db::Db) -> bool {
    match db.get_setting("wizard_done").ok().flatten() {
        Some(v) => v == "1",
        None => setting(db, "onboarded", "0") == "1" || db.get_setting("disclaimer_accepted").ok().flatten().is_some(),
    }
}

#[tauri::command]
pub async fn check_session(app: AppHandle) -> CmdResult<String> {
    if crate::demo::is_demo() {
        return blocking(|| Ok(open_db()?.source_status("mijnns")?.and_then(|s| s.1).unwrap_or_else(|| "login_required".into()))).await;
    }
    let _g = BusyGuard::take()?;
    blocking(move || {
        // CloseRequested is emitted before the login watcher observes the hidden window.
        // Let it release its lease before checking the resulting session.
        for _ in 0..20 {
            if !LOGIN_OPEN.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _lock = LedgerLock::take_for(5)?;
        finish_pending_before(&app)?;
        let b = NsBridge::open(&app, false)?;
        let s = b.ensure_session(std::time::Duration::from_secs(40))?;
        let db = open_db()?;
        let email = if s == SessionStatus::LoggedIn { crate::ns_adapters::account_email(&b) } else { None };
        let mismatch = email.as_deref().map(|e| runner::account_mismatch(&db, e)).unwrap_or(false);
        let st = match &s {
            SessionStatus::LoggedIn if mismatch => "account_mismatch".to_string(),
            SessionStatus::LoggedIn => "ok".to_string(),
            SessionStatus::LoginRequired => "login_required".to_string(),
            SessionStatus::AutoLoginFailed => "autologin_failed".to_string(),
            SessionStatus::Blocked(r) => format!("blocked: {r}"),
        };
        db.upsert_source("mijnns", "mijnns", st == "ok", &st)?;
        if st == "ok" {
            b.hide();
            crate::session_store::save(&b)?;
            if let Some(email) = &email {
                db.set_setting("ns_account_email", email)?;
            }
        }
        b.hide();
        Ok(st)
    })
    .await
}

fn facts_for(db: &ovrefund_core::db::Db, claim_id: i64) -> anyhow::Result<ClaimFacts> {
    let c = db.claim(claim_id)?;
    let j = db.journey(c.journey_id)?;
    anyhow::ensure!(j.is_ns_carrier(), pipeline::NOT_NS_TRIP_MSG);
    let (_, band, sdep, _) = db.assessment(c.journey_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Deze reis is nog niet gecontroleerd. Klik op Controleer nu. / This trip has not been checked yet. Click Check now."
        )
    })?;
    pipeline::build_facts(
        &j,
        band.ok_or_else(|| {
            anyhow::anyhow!(
                "De vertraging van deze reis is niet duidelijk genoeg om hier in te dienen. Dien hem zo nodig zelf in op ns.nl. / The delay of this trip is not clear enough to submit here. If needed, submit it yourself on ns.nl."
            )
        })?,
        sdep.ok_or_else(|| {
            anyhow::anyhow!(
                "De geplande vertrektijd van deze reis is onbekend. Dien hem zo nodig zelf in op ns.nl. / The scheduled departure of this trip is unknown. If needed, submit it yourself on ns.nl."
            )
        })?,
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "Uitcheck, eindstation of gekoppelde kaart ontbreekt voor deze reis. Dien hem zo nodig zelf in op ns.nl. / Check-out, destination or linked card is missing for this trip. If needed, submit it yourself on ns.nl."
        )
    })
}

/// Exact facts that would be entered into the NS form (for the review panel).
#[tauri::command]
pub async fn claim_facts(claim_id: i64) -> CmdResult<Value> {
    blocking(move || {
        let db = open_db()?;
        let f = facts_for(&db, claim_id)?;
        let journey = db.journey(db.claim(claim_id)?.journey_id)?;
        Ok(json!({ "facts": f, "check_in_local": pipeline::local_hhmm(f.check_in), "check_out_local": pipeline::local_hhmm(f.check_out),
                   "date_local": f.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).format("%d-%m-%Y").to_string(),
                   "bank": secrets::bank_hint(),
                   // NS may already have a claim around this date (claim list or an NS e-mail matched
                   // by date only): submit_claim needs confirm_near.
                   "ns_claim_near": db.is_ns_claim_near(&journey)?,
                   "card_conflict": db.has_card_conflict(&journey)?,
                   // An earlier submission may have reached NS: submit_claim needs confirm_resubmit.
                   "was_uncertain": db.was_uncertain(claim_id)? }))
    })
    .await
}

/// Open and fill the real NS form in the hidden webview WITHOUT submitting.
#[tauri::command]
pub async fn preflight_claim(app: AppHandle, claim_id: i64) -> CmdResult<Vec<String>> {
    login_window_open()?;
    let _g = BusyGuard::take()?;
    blocking(move || {
        let _lock = LedgerLock::take_for(10)?;
        let db = open_db()?;
        let c = db.claim(claim_id)?;
        anyhow::ensure!(
            matches!(c.status, ClaimState::Ready | ClaimState::NeedsReview | ClaimState::Failed | ClaimState::Candidate),
            "Deze claim kan niet meer worden geprobeerd. / This claim can no longer be tried."
        );
        anyhow::ensure!(!db.has_card_conflict(&db.journey(c.journey_id)?)?, pipeline::CARD_CONFLICT_MSG);
        let f = facts_for(&db, claim_id)?;
        if crate::demo::is_demo() {
            return crate::demo::DemoGateway.preflight(&f);
        }
        let mut gw =
            NsClaimGateway { bridge: NsBridge::open(&app, false)?, verified: runner::gateway_verified(&db), bank: secrets::bank_details() };
        db.renew_run_lock(&_lock.0, chrono::Duration::minutes(30))?;
        let result = gw.preflight(&f);
        if let Err(e) = &result {
            if e.is::<crate::ns_adapters::WriteDuringPreflight>() {
                // Record first: the claim may have reached NS and must never be sent again unchecked.
                let mut db = db;
                db.record_write_during_preflight(claim_id, &f)?;
                return result;
            }
        }
        if let Err(e) = db.renew_run_lock(&_lock.0, chrono::Duration::minutes(30)) {
            log::warn!("renewing the run lock after preflight failed: {e:#}");
        }
        result
    })
    .await
}

#[derive(Deserialize)]
pub struct SubmitRequest {
    pub claim_id: i64,
    /// The exact facts the user saw and confirmed; must match what we would submit.
    pub confirmed: ClaimFacts,
    pub declaration_accepted: bool,
    /// Required (true) to re-send a claim whose earlier submission may have reached NS.
    #[serde(default)]
    pub confirm_resubmit: bool,
    /// Required (true) when NS may already have a claim around this trip's date
    /// (`claim_facts.ns_claim_near`): the user checked Mijn teruggaven and it is a different trip.
    #[serde(default)]
    pub confirm_near: bool,
    /// The user checked Mijn teruggaven themselves after the live read failed.
    #[serde(default)]
    pub confirm_claim_list_unavailable: bool,
    /// The user explicitly confirmed reaching their destination. NS's "No" flow has never been observed, so
    /// such a claim is refused here and submitted by the user in Mijn NS.
    #[serde(default)]
    pub reached_destination: bool,
}

pub const NOT_REACHED_MSG: &str = "OV Refund kan geen claim invullen waarbij je je bestemming niet hebt bereikt. Dien deze claim zelf in via Mijn NS (Geld terug bij vertraging). / OV Refund cannot fill in a claim where you did not reach your destination. Submit this claim yourself in Mijn NS (Geld terug bij vertraging).";

/// User-authorized manual submission of exactly one claim.
#[tauri::command]
pub async fn submit_claim(app: AppHandle, req: SubmitRequest) -> CmdResult<Value> {
    login_window_open()?;
    let _g = BusyGuard::take()?;
    blocking(move || {
        anyhow::ensure!(req.reached_destination, NOT_REACHED_MSG);
        anyhow::ensure!(req.declaration_accepted, "Vink eerst de verklaring aan dat de gegevens kloppen. / Please tick the declaration that the details are correct first.");
        anyhow::ensure!(
            secrets::bank_details().is_some(),
            "Stel eerst je rekening in bij Jouw gegevens. / Set up your bank account under Your details first."
        );
        let mut db = open_db()?;
        // Same run lock as scans/scheduled runs: never submit while another process is busy.
        runner::recover_stale_lock(&db)?;
        let owner = edit_lock_owner();
        anyhow::ensure!(
            db.acquire_run_lock(&owner, chrono::Duration::minutes(10))?,
            "Er loopt al een controle (misschien de automatische). Probeer het over een minuut opnieuw. / A check is already running (possibly the automatic one). Please try again in a minute."
        );
        let r = (|| -> anyhow::Result<Value> {
            let current = facts_for(&db, req.claim_id)?;
            anyhow::ensure!(!db.has_card_conflict(&db.journey(db.claim(req.claim_id)?.journey_id)?)?, pipeline::CARD_CONFLICT_MSG);
            anyhow::ensure!(current == req.confirmed, "De gegevens van deze reis zijn gewijzigd. Bekijk de claim opnieuw. / The trip details have changed. Please review the claim again.");
            let mut gw: Box<dyn ClaimGateway> = if crate::demo::is_demo() {
                Box::new(crate::demo::DemoGateway)
            } else {
                let gateway = NsClaimGateway {
                    bridge: NsBridge::open(&app, false)?,
                    verified: runner::gateway_verified(&db),
                    bank: secrets::bank_details(),
                };
                gateway.ensure_claim_session()?;
                Box::new(gateway)
            };
            let remote = match if crate::demo::is_demo() { Ok(Some(vec![])) } else { gw.list_remote_claims_for(RunMode::Automatic) } {
                Ok(remote) => remote,
                Err(e) => {
                    log::warn!("manual submission: claim list unavailable: {}", ovrefund_core::redact::redact(&e.to_string()));
                    None
                }
            };
            if let Some(refusal) = check_manual_claim_list(&mut db, &req, remote.as_deref())? {
                return Ok(refusal);
            }
            // The status before the approval: a submission refused by the final checks (nothing
            // sent) puts the claim back there (a FAILED claim stays FAILED, with the reason).
            let prev = db.claim(req.claim_id)?.status;
            pipeline::approve_for_manual_submission(&mut db, req.claim_id, req.confirm_resubmit, req.confirm_near)?;
            let sent = (|| -> anyhow::Result<Value> {
                anyhow::ensure!(
                    secrets::bank_details().is_some(),
                    "Stel eerst je rekening in bij Jouw gegevens. / Set up your bank account under Your details first."
                );
                let r = pipeline::submit_claim_under_lock(
                    &mut db,
                    gw.as_mut(),
                    req.claim_id,
                    AuthorizedMode::Manual,
                    Some(req.confirmed.clone()),
                    &owner,
                    chrono::Utc::now(),
                )?;
                Ok(json!({ "status": r.status, "reference": r.ns_reference, "events": db.events(r.id)? }))
            })();
            if let Err(e) = &sent {
                if db.claim(req.claim_id).map(|c| c.status == ClaimState::Ready).unwrap_or(false) {
                    pipeline::restore_unsent(&mut db, req.claim_id, prev, e);
                }
            }
            sent
        })();
        let _ = db.release_run_lock(&owner);
        r
    })
    .await
}

fn check_manual_claim_list(
    db: &mut ovrefund_core::db::Db,
    req: &SubmitRequest,
    remote: Option<&[pipeline::RemoteClaim]>,
) -> anyhow::Result<Option<Value>> {
    if let Some(remote) = remote {
        let (_, near) = pipeline::reconcile_open(db, remote)?;
        db.set_ns_claim_near_keys(&near)?;
    }
    let c = db.claim(req.claim_id)?;
    if c.status == ClaimState::ExternallyClaimed {
        return Ok(Some(json!({
            "status": c.status, "reference": c.ns_reference, "events": db.events(c.id)?
        })));
    }
    if remote.is_none() && !req.confirm_claim_list_unavailable {
        return Ok(Some(json!({ "needs_claim_list_confirmation": true })));
    }
    let journey = db.journey(c.journey_id)?;
    Ok((!req.confirm_near && db.is_ns_claim_near(&journey)?).then(|| json!({ "needs_near_confirmation": true })))
}

#[tauri::command]
pub async fn set_claim_status(claim_id: i64, status: String) -> CmdResult<()> {
    blocking(move || {
        let _lock = LedgerLock::take()?;
        let mut db = open_db()?;
        let to = ClaimState::parse(&status).ok_or_else(|| anyhow::anyhow!("Ongeldige status. / Invalid status."))?;
        db.set_status_by_user(claim_id, to)?;
        // Not sent / skipped / an outcome: Arriva attachments of this claim are no longer needed.
        sweep_attachments(&db);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn mark_external(claim_id: i64) -> CmdResult<()> {
    blocking(move || {
        with_ledger_lock(|db| {
            db.mark_external(claim_id, "user")?;
            sweep_attachments(db);
            Ok(())
        })
    })
    .await
}

/// Recover interrupted submissions before the first GUI snapshot.
pub(crate) fn recover_claims_on_start() -> anyhow::Result<()> {
    let mut db = open_db()?;
    runner::recover_stale_lock(&db)?;
    recover_claims_on_start_in(&mut db, &edit_lock_owner(), &|owner| {
        runner::lock_owner_pid(owner).is_some_and(runner::process_alive)
    })
}

pub(crate) fn recover_claims_on_start_in(
    db: &mut ovrefund_core::db::Db, owner: &str, owner_alive: &dyn Fn(&str) -> bool,
) -> anyhow::Result<()> {
    if !db.acquire_run_lock(owner, chrono::Duration::minutes(2))? {
        return Ok(());
    }
    let result = (|| {
        let active = pipeline::inflight_elsewhere(db, owner, owner_alive, chrono::Utc::now())?;
        db.recover_inflight_except(&active)?;
        crate::arriva_claim::remove_unneeded_attachments(db)
    })();
    let released = db.release_run_lock(owner);
    result.and(released)
}

/// Removes Arriva attachments that are no longer needed (best effort; the next sweep retries).
pub(crate) fn sweep_attachments(db: &ovrefund_core::db::Db) {
    if let Err(e) = crate::arriva_claim::remove_unneeded_attachments(db) {
        log::warn!("could not remove old Arriva attachments: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
    }
}

/// Undo "claimed elsewhere" for a claim this app never sent (mis-click / wrong import).
#[tauri::command]
pub async fn undo_external(claim_id: i64) -> CmdResult<()> {
    blocking(move || with_ledger_lock(|db| db.undo_external(claim_id, "user").map(|_| ()))).await
}

#[tauri::command]
/// Returns `{ id, matched, pending, trips }`: `trips` (`[{ date: "YYYY-MM-DD" (Dutch local
/// travel day), from, to }]`, `matched` of them) are the trips with an open claim this entry
/// matches. They are marked as claimed elsewhere right away, or (`pending`: the lock was busy)
/// at the next check; until then every submission refuses them (`has_external_claim`).
pub async fn add_external_claim(date: String, origin: Option<String>, destination: Option<String>, source: String) -> CmdResult<Value> {
    blocking(move || {
        let d = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .map_err(|_| anyhow::anyhow!("Kies een geldige datum. / Please pick a valid date."))?;
        let n = |s: Option<String>| s.filter(|x| !x.trim().is_empty());
        // Stored at once, without the run lock: every submission checks these entries right
        // before it is sent (`has_external_claim`), so a running check never submits a match.
        let db = open_db()?;
        let id = db.add_external_claim(d, n(origin).as_deref(), n(destination).as_deref(), &source, None)?;
        let trips = db.open_trips_matching_external(id)?;
        // Matching trips show as claimed elsewhere right away when the lock is free; otherwise
        // the next check updates them.
        let applied = with_ledger_lock(|db| {
            let gateway = if crate::demo::is_demo() { "demo" } else { crate::ns_adapters::GATEWAY_ID };
            db.apply_external_claims(gateway, d)
        });
        let pending = match applied {
            Ok(changed) => {
                if !changed.is_empty() {
                    log::info!("claimed elsewhere: {} matching trip(s) updated", changed.len());
                }
                false
            }
            Err(e) => {
                log::info!("claimed elsewhere: matching trips are updated by the next check ({e:#})");
                true
            }
        };
        let trips: Vec<Value> = trips
            .iter()
            .map(|j| {
                json!({
                    "date": j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive().to_string(),
                    "from": j.origin.name,
                    "to": j.destination.as_ref().map(|x| x.name.as_str()).unwrap_or_default(),
                })
            })
            .collect();
        Ok(json!({ "id": id, "matched": trips.len(), "pending": pending, "trips": trips }))
    })
    .await
}

#[tauri::command]
pub async fn external_claims() -> CmdResult<Value> {
    blocking(|| {
        let rows = open_db()?.external_claims()?;
        Ok(json!(rows
            .into_iter()
            .map(|(id, d, o, t, s)| json!({"id": id, "date": d, "origin": o, "destination": t, "source": s}))
            .collect::<Vec<_>>()))
    })
    .await
}

#[tauri::command]
pub async fn delete_external_claim(id: i64) -> CmdResult<()> {
    blocking(move || with_ledger_lock(|db| db.delete_external_claim(id))).await
}

#[tauri::command]
pub async fn get_settings() -> CmdResult<Value> {
    blocking(|| {
        let db = open_db()?;
        let smtp = runner::smtp_config(&db);
        Ok(json!({
            "mode": setting(&db, "mode", "manual"),
            "auto_opt_in_at": db.get_setting("auto_opt_in_at")?,
            "interval_days": setting(&db, "interval_days", "7").parse::<u32>().unwrap_or(7),
            "notify_windows": setting(&db, "notify_windows", "1") == "1",
            "notify_email": setting(&db, "notify_email", "0") == "1",
            "smtp": smtp,
            "smtp_password_set": secrets::get(secrets::SMTP_PASSWORD).ok().flatten().is_some(),
            // false: the stored password was entered for another server / user name (enter it again)
            "smtp_password_matches": runner::smtp_password_matches(&db),
            "bank": secrets::bank_hint(),
            "schedule": effective_schedule(&db, scheduler::status())?,
            "schedule_enabled": db.get_setting("schedule_enabled")?.map(|value| value == "1"),
        }))
    })
    .await
}

#[derive(Deserialize)]
pub struct SettingsIn {
    pub mode: String,
    pub interval_days: u32,
    pub notify_windows: bool,
    pub notify_email: bool,
    pub smtp: ovrefund_core::email::SmtpConfig,
    pub smtp_password: Option<String>,
    pub schedule_enabled: bool,
    pub auto_consent: bool,
    #[serde(default)]
    pub auto_unverified_consent: bool,
}

/// Turning automatic submission off while a check runs: consent is withdrawn right away, the rest
/// of the settings is not saved.
pub const AUTO_OFF_REST_NOT_SAVED_MSG: &str = "Automatisch indienen staat uit. De rest van je instellingen kon niet worden opgeslagen omdat er een controle loopt. Probeer het zo opnieuw. / Automatic submission is off. Your other settings could not be saved because a check is running. Try again shortly.";

/// Every check on the new settings that can refuse the save; runs before anything is changed.
fn validate_settings(s: &SettingsIn, db: &ovrefund_core::db::Db) -> anyhow::Result<()> {
    anyhow::ensure!(s.mode == "manual" || s.mode == "automatic", "Ongeldige modus. / Invalid mode.");
    if s.notify_email {
        anyhow::ensure!(!s.smtp.unsupported_provider(), ovrefund_core::email::UNSUPPORTED_PROVIDER_MSG);
    }
    if s.mode == "automatic" {
        anyhow::ensure!(
            s.auto_consent || runner::auto_enabled(db),
            "Automatisch indienen kan alleen met je uitdrukkelijke toestemming. / Automatic submission requires your explicit consent."
        );
        anyhow::ensure!(!runner::other_service_blocks_auto(db), runner::other_service_msg(db));
        if !runner::auto_enabled(db) {
            anyhow::ensure!(s.schedule_enabled, "Zet eerst Automatisch controleren aan. / Turn on automatic checks first.");
            anyhow::ensure!(
                crate::demo::is_demo() || runner::gateway_verified_by_submission(db) || s.auto_unverified_consent,
                "Bevestig eerst dat je automatisch wilt indienen vóór een door NS bevestigde claim. / First confirm automatic submission before a claim has been verified with NS."
            );
        }
        // Checked before recording the opt-in, so a refused save leaves no consent behind.
        anyhow::ensure!(
            secrets::bank_details().is_some(),
            "Stel eerst je rekening in bij Jouw gegevens voordat je automatisch indienen aanzet. / Set up your bank account under Your details before turning on automatic submission."
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn save_settings(s: SettingsIn) -> CmdResult<Value> {
    blocking(move || {
        // Everything that can refuse the save first: a refused save changes nothing.
        validate_settings(&s, &open_db()?)?;
        let _lock = if s.mode == "manual" && runner::auto_enabled(&open_db()?) {
            // Withdrawing consent never waits for a running check: stop it first (after the claim
            // in progress) and withdraw the consent right away, then wait for the lock (only for a
            // check that is submitting; a syncing check is not waited for).
            stop_auto_submission(StopReason::AutoOff)?;
            withdraw_auto_consent(&open_db()?)?;
            match LedgerLock::take_after_stop(5, StopReason::AutoOff) {
                Ok(l) => l,
                Err(e) => {
                    log::info!("settings: automatic submission switched off during a check ({e:#})");
                    // No automatic submission: no extra check before a deadline either.
                    if let Err(e) = scheduler::remove_deadline() {
                        log::warn!("removing the extra check failed: {e:#}");
                    }
                    anyhow::bail!(AUTO_OFF_REST_NOT_SAVED_MSG);
                }
            }
        } else {
            LedgerLock::take_for(5)?
        };
        let db = open_db()?;
        // Again under the lock (the claim-service answer or bank account may have changed).
        validate_settings(&s, &db)?;
        if s.mode == "manual" {
            withdraw_auto_consent(&db)?;
            if let Err(e) = scheduler::remove_deadline() {
                log::warn!("removing the extra check failed: {e:#}");
            }
        }
        save_settings_locked(&db, s, configure_schedule, |pw| secrets::set(secrets::SMTP_PASSWORD, pw))
    })
    .await
}

fn effective_schedule(db: &ovrefund_core::db::Db, mut task: scheduler::TaskStatus) -> anyhow::Result<scheduler::TaskStatus> {
    if db.get_setting("schedule_enabled")?.as_deref() == Some("0") {
        task.installed = false;
        task.next_run = None;
        task.deadline_run = None;
        task.interval_days = None;
    }
    Ok(task)
}

fn schedule_needs_install(db: &ovrefund_core::db::Db, installed: bool, days: u32) -> bool {
    !installed || setting(db, "schedule_enabled", "") == "0" || setting(db, "schedule_days", "") != days.to_string()
}

fn configure_schedule(db: &ovrefund_core::db::Db, enabled: bool, days: u32) -> anyhow::Result<String> {
    let result = if enabled {
        if schedule_needs_install(db, scheduler::status().installed, days) {
            scheduler::install(days)?
        } else {
            "unchanged".into()
        }
    } else {
        scheduler::remove()?;
        "not scheduled".into()
    };
    Ok(result)
}

pub(crate) fn save_schedule_intent(db: &ovrefund_core::db::Db, enabled: bool, days: u32) -> anyhow::Result<()> {
    let tx = db.conn.unchecked_transaction()?;
    db.set_setting("schedule_enabled", if enabled { "1" } else { "0" })?;
    db.set_setting("schedule_days", &days.to_string())?;
    db.set_setting("interval_days", &days.to_string())?;
    tx.commit()?;
    Ok(())
}

fn apply_schedule_intent<T>(
    db: &ovrefund_core::db::Db, enabled: bool, days: u32,
    configure: impl FnOnce(&ovrefund_core::db::Db, bool, u32) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if !enabled {
        save_schedule_intent(db, false, days)?;
    }
    let result = configure(db, enabled, days)?;
    if enabled {
        save_schedule_intent(db, true, days)?;
    }
    Ok(result)
}

/// Task registration and the password write must finish before new submission consent is saved.
fn save_settings_locked(
    db: &ovrefund_core::db::Db,
    s: SettingsIn,
    schedule: impl FnOnce(&ovrefund_core::db::Db, bool, u32) -> anyhow::Result<String>,
    save_password: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Value> {
    let days = s.interval_days.clamp(1, 31);
    let sched = apply_schedule_intent(db, s.schedule_enabled, days, schedule)?;

    runner::bind_legacy_smtp_password(db);
    let fingerprint = s.smtp.password_fingerprint();
    let password = s.smtp_password.filter(|p| !p.trim().is_empty());
    if let Some(pw) = &password {
        // If the following commit fails, the new secret must never be used with the old server.
        if runner::smtp_config(db).password_fingerprint() != fingerprint {
            db.set_setting(runner::SMTP_PW_FOR, "")?;
        }
        let pw = if s.smtp.host.contains("gmail") || s.smtp.host.contains("googlemail") {
            pw.split_whitespace().collect()
        } else {
            pw.clone()
        };
        if let Err(e) = save_password(&pw) {
            log::warn!("saving the e-mail password failed: {}", ovrefund_core::redact::redact(&e.to_string()));
            anyhow::bail!("Het controleschema is opgeslagen. Het e-mailwachtwoord kon niet worden opgeslagen. Controleer je instellingen en probeer opnieuw. / The check schedule was saved. The e-mail password could not be saved. Review your settings and try again.");
        }
    }
    let tx = db.conn.unchecked_transaction()?;
    db.set_setting("notify_windows", if s.notify_windows { "1" } else { "0" })?;
    db.set_setting("smtp", &serde_json::to_string(&s.smtp)?)?;
    if password.is_some() {
        db.set_setting(runner::SMTP_PW_FOR, &fingerprint)?;
    }
    let mail_password_missing = s.notify_email && !runner::smtp_password_usable(db);
    db.set_setting("notify_email", if s.notify_email && !mail_password_missing { "1" } else { "0" })?;
    db.set_setting("onboarded", "1")?;
    db.conn.execute("DELETE FROM settings WHERE key='max_auto_submissions'", [])?;
    if s.mode == "automatic" {
        if !runner::auto_enabled(db) {
            pipeline::clear_auto_stop(db)?;
            db.set_setting("auto_opt_in_at", &chrono::Utc::now().to_rfc3339())?;
        }
        if s.auto_consent && s.auto_unverified_consent {
            db.set_setting("auto_unverified_override", "1")?;
        }
        record_enabled_via(db)?;
    }
    db.set_setting("mode", &s.mode)?;
    tx.commit()?;
    Ok(json!({ "schedule": sched, "mail_password_missing": mail_password_missing }))
}

/// Back to manual withdraws the consent, including the "before first verified submission" override.
pub(crate) fn withdraw_auto_consent(db: &ovrefund_core::db::Db) -> anyhow::Result<()> {
    db.conn.execute("DELETE FROM settings WHERE key IN ('auto_opt_in_at','auto_unverified_override','auto_enabled_via')", [])?;
    db.set_setting("mode", "manual")
}

/// Record how automatic submission is enabled: "verified" (a user-approved submission was
/// confirmed by NS) or "override" (the user's explicit "before a verified submission" consent).
fn record_enabled_via(db: &ovrefund_core::db::Db) -> anyhow::Result<()> {
    if runner::gateway_verified_by_submission(db) {
        db.set_setting("auto_enabled_via", "verified")
    } else if setting(db, "auto_unverified_override", "0") == "1" {
        db.set_setting("auto_enabled_via", "override")
    } else {
        db.conn.execute("DELETE FROM settings WHERE key='auto_enabled_via'", [])?;
        Ok(())
    }
}

#[tauri::command]
pub async fn set_bank_details(name: String, iban: String) -> CmdResult<String> {
    blocking(move || {
        let _lock = LedgerLock::take()?;
        secrets::set_bank_details(&name, &iban)?;
        // A stop that waited for a check is not carried past a newly saved bank account.
        pipeline::clear_pending_stop(&open_db()?)?;
        Ok(secrets::bank_hint().unwrap_or_default())
    })
    .await
}

#[tauri::command]
pub async fn clear_bank_details() -> CmdResult<()> {
    blocking(|| {
        // At once, without the run lock: a check reads the bank account again right before each
        // submission and then submits nothing more (the claim being sent keeps its own copy).
        secrets::delete(secrets::BANK)?;
        runner::request_auto_stop(&open_db()?, StopReason::BankRemoved)?;
        // No automatic submission: no extra check before a deadline either.
        if let Err(e) = scheduler::remove_deadline() {
            log::warn!("removing the extra check failed: {e:#}");
        }
        Ok(())
    })
    .await
}

/// The app's language, so e-mails and notifications (also from scheduled runs) match it.
#[tauri::command]
pub async fn set_language(lang: String) -> CmdResult<()> {
    let code = if lang == "en" { "en" } else { "nl" };
    blocking(move || open_db()?.set_setting("ui_lang", code)).await
}

#[tauri::command]
pub async fn test_notification(app: AppHandle) -> CmdResult<()> {
    use tauri_plugin_notification::NotificationExt;
    let lang = open_db().map(|db| runner::lang(&db)).unwrap_or_default();
    app.notification().builder().title("OV Refund").body(lang.t("Meldingen werken.", "Notifications work.")).show().map_err(e)
}

#[tauri::command]
pub async fn test_email() -> CmdResult<String> {
    blocking(|| {
        let _lock = LedgerLock::take()?;
        let db = open_db()?;
        let cfg = runner::smtp_config(&db);
        anyhow::ensure!(
            cfg.is_configured(),
            "E-mail is nog niet ingesteld. Stel het in bij ‘E-mail voor meldingen’. / E-mail is not set up yet. Set it up under ‘E-mail for notifications’."
        );
        anyhow::ensure!(!cfg.unsupported_provider(), ovrefund_core::email::UNSUPPORTED_PROVIDER_MSG);
        let pw = runner::smtp_password(&db)?;
        anyhow::ensure!(pw.is_some(), "Vul eerst het app-wachtwoord in. / Enter the app password first.");
        let lang = runner::lang(&db);
        let html = ovrefund_core::email::render_welcome_html(crate::demo::is_demo(), lang);
        let (subject, text) = ovrefund_core::email::welcome_text(lang);
        ovrefund_core::email::send_rich(&cfg, pw.as_deref(), &subject, &text, Some(&html))?;
        Ok(format!("Verstuurd naar {} / Sent to {}", cfg.to, cfg.to))
    })
    .await
}

/// Log out: clears the NS session from the app's WebView2 profile. Returns `true` when done;
/// `false` when a running check still holds the lock: the password, the saved session and
/// automatic login are gone at once, the browser profile is cleared once the check is done (see
/// [`PENDING_ERASE_KEY`]; the event "erase-finished" follows).
#[tauri::command]
pub async fn disconnect(app: AppHandle) -> CmdResult<bool> {
    if crate::demo::is_demo() {
        blocking(|| open_db()?.upsert_source("mijnns", "mijnns", false, "disconnected")).await?;
        return Ok(true);
    }
    login_window_open()?;
    // At once, without waiting for a running check (see `disconnect_now`).
    blocking(|| {
        let db = open_db()?;
        disconnect_now(&db)?;
        queue_erase(&db, ERASE_LOGOUT)
    })
    .await?;
    finish_erase_now_or_later(app, ERASE_LOGOUT).await
}

/// Setting with the destructive rest of a logout ([`ERASE_LOGOUT`]) or of "forget everything"
/// ([`ERASE_FORGET`]) that waits for a running check. The protective part is done at once (see
/// [`disconnect_now`], [`forget_everything`]); the rest runs as soon as the lock is free: by a
/// waiting thread of the app ([`resume_pending_erase`], also at the next start), or by whoever
/// takes the lock first for a check or a login ([`run_pending_erase_locked`]).
pub(crate) const PENDING_ERASE_KEY: &str = "pending_erase";
const ERASE_LOGOUT: &str = "logout";
const ERASE_FORGET: &str = "forget";

/// Record the rest of an erase action; "forget everything" covers a logout.
fn queue_erase(db: &ovrefund_core::db::Db, kind: &str) -> anyhow::Result<()> {
    // One atomic write: a concurrent logout must never downgrade a queued full erase.
    db.conn.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=CASE WHEN settings.value=?3 THEN ?3 ELSE excluded.value END",
        [PENDING_ERASE_KEY, kind, ERASE_FORGET],
    )?;
    Ok(())
}

/// The waiting erase action, if any.
pub(crate) fn pending_erase(db: &ovrefund_core::db::Db) -> Option<String> {
    db.get_setting(PENDING_ERASE_KEY).ok().flatten().filter(|k| k == ERASE_LOGOUT || k == ERASE_FORGET)
}

/// Run the waiting erase action, for a caller that holds the run lock (and, in the app, the busy
/// guard). Returns what was done.
pub(crate) fn run_pending_erase_locked(app: &AppHandle) -> anyhow::Result<Option<String>> {
    let db = open_db()?;
    let Some(kind) = pending_erase(&db) else { return Ok(None) };
    if kind == ERASE_FORGET {
        finish_forget(app)?;
    } else {
        finish_logout(app)?;
        // A full erase queued while the browser cleanup ran must still be completed.
        db.conn.execute("DELETE FROM settings WHERE key=?1 AND value=?2", [PENDING_ERASE_KEY, ERASE_LOGOUT])?;
    }
    Ok(Some(kind))
}

/// Refusal for a login or check that ran a waiting "forget everything" first.
pub(crate) const ERASED_MSG: &str = "Je gegevens zijn gewist. / Your data was erased.";

/// Run a waiting erase action before a login or check (caller holds the lock). After "forget
/// everything" the caller stops ([`ERASED_MSG`]).
pub(crate) fn finish_pending_before(app: &AppHandle) -> anyhow::Result<()> {
    if run_pending_erase_locked(app)?.as_deref() == Some(ERASE_FORGET) {
        anyhow::bail!(ERASED_MSG);
    }
    Ok(())
}

/// Take the busy guard and the lock without waiting and run the waiting erase action. `false`:
/// busy (a check or another action holds them).
fn try_finish_erase(app: &AppHandle) -> anyhow::Result<bool> {
    let Ok(_g) = BusyGuard::take() else { return Ok(false) };
    let Ok(_lock) = LedgerLock::take_for(5) else { return Ok(false) };
    run_pending_erase_locked(app)?;
    Ok(true)
}

/// Finish the erase action now when nothing is running (`true`); otherwise leave it to the
/// waiting thread (`false`).
async fn finish_erase_now_or_later(app: AppHandle, kind: &'static str) -> CmdResult<bool> {
    let a = app.clone();
    if blocking(move || try_finish_erase(&a)).await? {
        return Ok(true);
    }
    resume_pending_erase_kind(app, kind.into());
    Ok(false)
}

/// One waiting thread per process.
static ERASE_WAITER: AtomicBool = AtomicBool::new(false);

/// Wait (in the background) until the lock is free and run the waiting erase action, then tell
/// the app ("erase-finished" with "logout" or "forget"). Also when someone else ran it first.
pub(crate) fn resume_pending_erase(app: AppHandle) {
    let Some(kind) = open_db().ok().and_then(|db| pending_erase(&db)) else { return };
    resume_pending_erase_kind(app, kind);
}

fn resume_pending_erase_kind(app: AppHandle, initial_kind: String) {
    if ERASE_WAITER.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        // Keep the requested kind even if another lock holder finishes before this thread starts.
        let mut kind = initial_kind;
        loop {
            match open_db().map(|db| pending_erase(&db)) {
                Ok(Some(k)) => kind = k,
                // Done (here or by whoever took the lock first).
                Ok(None) => break,
                Err(e) => log::warn!("waiting erase: {e:#}"),
            }
            match try_finish_erase(&app) {
                Ok(true) => break,
                Ok(false) => std::thread::sleep(std::time::Duration::from_secs(2)),
                Err(e) => {
                    log::warn!("waiting erase failed: {e:#}");
                    std::thread::sleep(std::time::Duration::from_secs(30));
                }
            }
        }
        ERASE_WAITER.store(false, Ordering::SeqCst);
        let _ = app.emit("erase-finished", kind);
        // An action queued between the final read and clearing ERASE_WAITER needs a waiter too.
        if open_db().ok().is_some_and(|db| pending_erase(&db).is_some()) {
            resume_pending_erase(app);
        }
    });
}

/// The rest of a logout, for a caller that holds the lock: secrets first and unconditionally (a
/// failing browser step must never leave them behind; a check that ended meanwhile may have set
/// the source), then the NS cookies and site data in the app's browser profile (best effort).
fn finish_logout(app: &AppHandle) -> anyhow::Result<()> {
    secrets::delete(secrets::NS_LOGIN)?;
    crate::session_store::clear()?;
    let db = open_db()?;
    runner::bump_session_epoch(&db)?;
    record_disconnect(&db)?;
    // Then the browser profile (best effort).
    let mut problems = vec![];
    match NsBridge::open(app, false) {
        Ok(b) => {
            let t = std::time::Duration::from_secs(10);
            if let Err(e) = b.cdp("Network.clearBrowserCookies", json!({}), t) {
                problems.push(format!("cookies: {e}"));
            }
            for origin in NS_ORIGINS {
                let _ = b.cdp("Storage.clearDataForOrigin", json!({"origin": origin, "storageTypes": "all"}), t);
            }
            let _ = b.navigate("about:blank");
            if let Err(e) = b.clear_saved_logins() {
                problems.push(format!("saved logins: {e}"));
            }
        }
        Err(e) => problems.push(format!("browser: {e}")),
    }
    if !problems.is_empty() {
        log::warn!("logout: browser cleanup incomplete: {}", problems.join("; "));
    }
    Ok(())
}

/// The part of a logout that needs no lock, done first: a run that started before this point
/// submits nothing more (its session epoch changed, see `auto_still_allowed`) and saves no
/// session or account details afterwards. The NS password and saved session are removed and
/// automatic login is off, so a scheduled check only sends a reminder.
fn disconnect_now(db: &ovrefund_core::db::Db) -> anyhow::Result<()> {
    runner::bump_session_epoch(db)?;
    secrets::delete(secrets::NS_LOGIN)?;
    crate::session_store::clear()?;
    record_disconnect(db)?;
    runner::request_auto_stop(db, StopReason::LoggedOut)
}

/// Keep the ledger's account binding across logout; only erasing the ledger releases it.
fn record_disconnect(db: &ovrefund_core::db::Db) -> anyhow::Result<()> {
    db.set_setting("auto_login", "0")?;
    db.upsert_source("mijnns", "mijnns", false, "disconnected")
}

/// Confirmation word for the two erase actions ("forget everything", "erase local history"):
/// WISSEN (Dutch UI) or ERASE (English UI), case-insensitive, surrounding spaces ignored.
fn confirm_word_ok(s: &str) -> bool {
    matches!(s.trim().to_uppercase().as_str(), "WISSEN" | "ERASE")
}
const CONFIRM_WORD_MSG: &str = "Niet gewist: typ WISSEN om te bevestigen. / Not erased: type ERASE to confirm.";

/// "Forget everything": all OV Refund data and credentials for this Windows user, then restart
/// at the setup wizard. Nothing is sent anywhere. Returns `true` when done; `false` when a
/// running check still holds the lock: automatic submission, automatic login, the scheduled
/// checks and every password and the bank account are gone at once, the data is erased once the
/// check is done (see [`PENDING_ERASE_KEY`]; the event "erase-finished" follows).
#[tauri::command]
pub async fn forget_everything(app: AppHandle, confirm: String) -> CmdResult<bool> {
    login_window_open()?;
    if crate::arriva_claim::running() {
        return Err("Wacht op de uitkomst van je Arriva-claim voordat je alles wist. / Wait for your Arriva claim result before erasing everything.".into());
    }
    if !confirm_all_word_ok(&confirm) {
        return Err("Niet gewist: typ ALLES WISSEN om te bevestigen. / Not erased: type ERASE ALL to confirm.".into());
    }
    // At once, without the run lock: no check submits anything more (consent withdrawn, session
    // epoch changed, bank account gone) or saves its session afterwards, and no scheduled check
    // starts any more. The data itself is erased under the lock (see `finish_forget`).
    blocking(|| {
        let db = open_db()?;
        runner::bump_session_epoch(&db)?;
        db.set_setting("auto_login", "0")?;
        withdraw_auto_now(&db, StopReason::Erased)?;
        // Not restored at the next start (see main.rs) while the erase waits.
        db.set_setting("schedule_enabled", "0")?;
        let _ = scheduler::remove();
        secrets::delete_all()?;
        crate::session_store::clear()?;
        queue_erase(&db, ERASE_FORGET)
    })
    .await?;
    finish_erase_now_or_later(app, ERASE_FORGET).await
}

/// The rest of "forget everything", for a caller that holds the lock (a check can never write
/// data back after the wipe). Erasing the settings also removes [`PENDING_ERASE_KEY`].
fn finish_forget(app: &AppHandle) -> anyhow::Result<()> {
    let _ = scheduler::remove();
    secrets::delete_all()?;
    crate::session_store::clear()?;
    if !crate::demo::is_demo() {
        if let Ok(b) = NsBridge::open(app, false) {
            clear_ns_browser_session(&b);
            let _ = b.clear_saved_logins();
            let _ = b.navigate("about:blank");
        }
    }
    crate::paths::remove_own_diagnostics();
    // Arriva claim attachments and the Arriva window's browser profile.
    crate::arriva_claim::forget()?;
    let mut db = open_db()?;
    db.erase_all()?;
    let _ = std::fs::remove_file(crate::paths::log_path());
    // Backups and claim CSVs OV Refund wrote (only its own files; see paths::remove_own_exports).
    let n = crate::paths::remove_own_exports();
    log::info!("forget everything: {n} export file(s) removed");
    Ok(())
}

/// Anonymised diagnostics as counts only: no trip dates or times, stations, card numbers, names,
/// IBANs, e-mail addresses, cookies or tokens (a list of trip dates would show a travel pattern).
/// Written to the app's own data folder (`<data>\diagnostics\`), so "forget everything" and the
/// uninstaller remove it with the rest. Returns the path; `open_diagnostics_folder` shows it.
#[tauri::command]
pub async fn export_diagnostics() -> CmdResult<String> {
    blocking(|| {
        let _lock = LedgerLock::take()?;
        let db = open_db()?;
        let diag = diagnostics_json(&db)?;
        let dir = crate::paths::data_dir().join("diagnostics");
        std::fs::create_dir_all(&dir)?;
        let p = dir.join(format!("ovrefund-diagnostics-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        std::fs::write(&p, ovrefund_core::redact::redact(&serde_json::to_string_pretty(&diag)?))?;
        Ok(p.to_string_lossy().to_string())
    })
    .await
}

/// Open the diagnostics folder in Explorer with the given export (or the newest one) selected.
/// Only files inside `<data>diagnostics` are accepted.
#[tauri::command]
pub async fn open_diagnostics_folder(path: Option<String>) -> CmdResult<()> {
    blocking(move || {
        let dir = crate::paths::data_dir().join("diagnostics");
        std::fs::create_dir_all(&dir)?;
        let newest = || {
            std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x == "json").unwrap_or(false)).max()
        };
        let file = match path {
            Some(p) => {
                let p = std::path::PathBuf::from(p);
                anyhow::ensure!(
                    p.parent() == Some(dir.as_path()) && p.is_file(),
                    "Dit bestand staat niet in de map met diagnosegegevens. / This file is not in the diagnostics folder."
                );
                Some(p)
            }
            None => newest(),
        };
        let mut cmd = std::process::Command::new("explorer.exe");
        match file {
            Some(f) => cmd.arg(format!("/select,{}", f.display())),
            None => cmd.arg(&dir),
        };
        cmd.spawn()?;
        Ok(())
    })
    .await
}

/// Count rows of `sql` (two text columns: group, value) into {group: {value: n}}.
fn grouped_counts(db: &ovrefund_core::db::Db, sql: &str) -> anyhow::Result<Value> {
    let mut st = db.conn.prepare(sql)?;
    let mut out = serde_json::Map::new();
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?)))? {
        let (group, value, n) = row?;
        let e = out.entry(group).or_insert_with(|| json!({}));
        e[value.unwrap_or_else(|| "none".into())] = json!(n);
    }
    Ok(Value::Object(out))
}

/// The diagnostics content (also used by tests): aggregate counts per month and per state.
fn diagnostics_json(db: &ovrefund_core::db::Db) -> anyhow::Result<Value> {
    let month = "substr(j.check_in_utc, 1, 7)";
    let mail = db.ns_mail_summary()?;
    Ok(json!({
        "app_version": env!("CARGO_PKG_VERSION"), "generated_at": chrono::Utc::now().format("%Y-%m-%d").to_string(),
        "note": "Alleen totalen en uitvoerstatus. Controleer het bestand voordat je het deelt. / Only totals and run status. Check the file before sharing it.",
        // Whitelisted fields only (free text such as error summaries can contain NS page text).
        "settings": db.all_settings()?.into_iter().filter(|(k, _)| DIAG_SETTINGS.contains(&k.as_str()))
            .map(|(k, v)| if ["auto_opt_in_at", "last_mail_sync", "last_year_fetch"].contains(&k.as_str()) {
                (format!("{k}_present"), Value::Bool(!v.is_empty()))
            } else { (k, Value::String(v)) }).collect::<serde_json::Map<_, _>>(),
        "schedule": diagnostic_schedule(&scheduler::status()),
        "runs": db.runs(50)?.into_iter().map(|r| json!({
            "mode": r["mode"], "trigger": diagnostic_trigger(&r["trigger"]),
            "outcome": r["outcome"], "auth_state": diagnostic_auth(&r["auth_state"]), "had_error": !r["error_summary"].is_null(),
            "counts": r["summary"].as_object().map(|o| o.iter().filter(|(_, v)| v.is_number()).map(|(k, v)| (k.clone(), v.clone())).collect::<serde_json::Map<_, _>>()),
        })).collect::<Vec<_>>(),
        "journeys_per_month": {
            "total": grouped_counts(db, &format!("SELECT {month}, 'all', COUNT(*) FROM journeys j GROUP BY 1"))?,
            "by_carrier": grouped_counts(db, &format!("SELECT {month}, j.carrier_code, COUNT(*) FROM journeys j GROUP BY 1, 2"))?,
            "by_confidence": grouped_counts(db, &format!("SELECT {month}, c.confidence, COUNT(*) FROM journeys j LEFT JOIN candidates c ON c.journey_id = j.id GROUP BY 1, 2"))?,
            "by_evidence": grouped_counts(db, &format!("SELECT {month}, s.status, COUNT(*) FROM journeys j LEFT JOIN schedules s ON s.journey_id = j.id GROUP BY 1, 2"))?,
            "by_claim_status": grouped_counts(db, &format!("SELECT {month}, cl.status, COUNT(*) FROM journeys j LEFT JOIN claims cl ON cl.journey_id = j.id GROUP BY 1, 2"))?,
        },
        "claims_by_status_and_mode": grouped_counts(db, "SELECT status, authorized_mode, COUNT(*) FROM claims GROUP BY 1, 2")?,
        // Which state changes happened, by whom (no times, no details).
        "claim_transitions": grouped_counts(db, "SELECT COALESCE(from_status, 'new') || ' -> ' || to_status, actor, COUNT(*) FROM claim_events GROUP BY 1, 2")?,
        "ns_mail": {"total": mail["total"], "unmatched": mail["unmatched"]},
        "ns_mail_skipped": skipped_summary(db),
    }))
}

fn diagnostic_schedule(s: &scheduler::TaskStatus) -> Value {
    json!({"installed": s.installed, "interval_days": s.interval_days, "last_result": s.last_result})
}

fn diagnostic_trigger(value: &Value) -> &str {
    match value.as_str().unwrap_or("") {
        "gui" => "gui",
        "cli" => "cli",
        "scan" => "scan",
        s if s.starts_with("demo-") => "demo",
        _ => "other",
    }
}

/// Remote block details can include personal page text. Diagnostics expose a category only.
fn diagnostic_auth(value: &Value) -> &str {
    match value.as_str().unwrap_or("") {
        "ok" => "ok",
        "login_required" => "login_required",
        "autologin_failed" => "autologin_failed",
        "account_mismatch" => "account_mismatch",
        s if s.starts_with("blocked") => "blocked",
        _ => "unknown",
    }
}

/// NS-looking mails skipped at the last mail sync, per sender domain (counts only).
fn mail_skipped(db: &ovrefund_core::db::Db) -> Value {
    db.get_setting(runner::MAIL_SKIPPED_KEY).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

#[tauri::command]
pub async fn wipe_history(confirm: String) -> CmdResult<()> {
    blocking(move || {
        anyhow::ensure!(confirm_word_ok(&confirm), CONFIRM_WORD_MSG);
        with_ledger_lock(|db| wipe_history_and_log(db, &crate::paths::log_path()))
    })
    .await
}

#[tauri::command]
pub async fn set_paid_amount(claim_id: i64, euros: Option<f64>) -> CmdResult<()> {
    blocking(move || {
        let cents = euros.map(amount_cents).transpose()?;
        anyhow::ensure!(
            cents.map(|c| (0..=100_000).contains(&c)).unwrap_or(true),
            "Het bedrag moet tussen € 0 en € 1000 liggen. / The amount must be between € 0 and € 1000."
        );
        // Same run lock as checks: never interleaves with a (scheduled) run writing the ledger.
        with_ledger_lock(|db| db.set_paid_amount(claim_id, cents))
    })
    .await
}

/// A euro amount typed on the command line ("12,50" or "12.50"), with the GUI's rules.
pub(crate) fn cli_amount_cents(s: &str) -> anyhow::Result<i64> {
    let euros = s
        .trim()
        .replace(',', ".")
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("Het bedrag moet tussen € 0 en € 1000 liggen. / The amount must be between € 0 and € 1000."))?;
    amount_cents(euros)
}

fn amount_cents(euros: f64) -> anyhow::Result<i64> {
    anyhow::ensure!(
        euros.is_finite() && (0.0..=1000.0).contains(&euros),
        "Het bedrag moet tussen € 0 en € 1000 liggen. / The amount must be between € 0 and € 1000."
    );
    Ok((euros * 100.0).round() as i64)
}

/// Automatic checking on/off with an interval.
#[tauri::command]
pub async fn set_schedule(enabled: bool, interval_days: u32) -> CmdResult<String> {
    blocking(move || {
        let _lock = LedgerLock::take_for(5)?;
        let db = open_db()?;
        let d = interval_days.clamp(1, 31);
        let result = apply_schedule_intent(&db, enabled, d, configure_schedule)?;
        db.set_setting("onboarded", "1")?;
        Ok(result)
    })
    .await
}

/// Called when the GUI opens: run a background check if the last one is older than `max_age_hours`.
#[tauri::command]
pub async fn startup_check(app: AppHandle, max_age_hours: i64) -> CmdResult<bool> {
    let due = blocking(move || {
        let db = open_db()?;
        let last = db.last_completed_run_at()?;
        Ok(crate::session_store::exists()
            && last.map(|t| chrono::Utc::now() - t > chrono::Duration::hours(max_age_hours.clamp(1, 24 * 365))).unwrap_or(true))
    })
    .await?;
    if !due {
        return Ok(false);
    }
    scan_now(app, None).await.map(|_| true)
}

/// Optional protected NS login storage (see SECURITY.md).
#[tauri::command]
pub async fn set_auto_login(app: AppHandle, enabled: bool) -> CmdResult<()> {
    login_window_open()?;
    let _g = BusyGuard::take()?;
    blocking(move || {
        let _lock = LedgerLock::take_for(5)?;
        let db = open_db()?;
        if !enabled {
            secrets::delete(secrets::NS_LOGIN)?;
        }
        db.set_setting("auto_login", if enabled { "1" } else { "0" })?;
        reset_auto_login_failures(&db);
        // Demo: never touch the real WebView2 profile (shared with the normal app).
        if !enabled && !crate::demo::is_demo() {
            NsBridge::open(&app, false)?.clear_saved_logins()?;
        }
        Ok(())
    })
    .await
}

/// Store the NS login for unattended runs (Windows Credential Manager). The password is never logged.
#[tauri::command]
pub async fn set_ns_login(email: String, password: String) -> CmdResult<()> {
    blocking(move || {
        let _lock = LedgerLock::take()?;
        secrets::set_ns_login(&email, &password)?;
        let db = open_db()?;
        db.set_setting("auto_login", "1")?;
        reset_auto_login_failures(&db);
        Ok(())
    })
    .await
}

/// A newly entered password (or switching automatic login on again) starts counting from zero.
fn reset_auto_login_failures(db: &ovrefund_core::db::Db) {
    let _ = db.conn.execute(
        "DELETE FROM settings WHERE key IN (?1, ?2)",
        [crate::ns_bridge::AUTO_LOGIN_FAILURES, crate::ns_bridge::AUTO_LOGIN_DISABLED],
    );
}

/// Reveal a masked value on explicit user request ("view" button). Values come from Credential Manager.
#[tauri::command]
pub async fn reveal(kind: String) -> CmdResult<String> {
    blocking(move || {
        let group4 = |s: &str| s.chars().collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ");
        match kind.as_str() {
            "iban" => Ok(group4(
                &secrets::bank_details()
                    .ok_or_else(|| anyhow::anyhow!("Er is geen rekening ingesteld. / No bank account is set up."))?
                    .iban,
            )),
            _ => anyhow::bail!("Ongeldig veld. / Invalid field."),
        }
    })
    .await
}

#[tauri::command]
pub async fn set_read_ns_mail(enabled: bool) -> CmdResult<()> {
    blocking(move || {
        open_db()?.set_setting("read_ns_mail", if enabled { "1" } else { "0" })?;
        Ok(())
    })
    .await
}

/// Read NS refund e-mails now (opt-in feature).
#[tauri::command]
pub async fn sync_ns_mail() -> CmdResult<Vec<String>> {
    let _g = BusyGuard::take()?;
    blocking(|| {
        // Writes the ledger: same run lock as checks (refused while a check runs; the check reads
        // the NS e-mails itself). Reading a large mailbox can take a few minutes.
        let _lock = LedgerLock::take_for(10)?;
        let mut db = open_db()?;
        runner::sync_ns_mail(&mut db)
    })
    .await
}

/// Open an official help page in the user's default browser. Allowlisted hosts only.
#[tauri::command]
pub async fn open_external(url: String) -> CmdResult<()> {
    if !external_url_ok(&url) {
        return Err("Deze link mag OV Refund niet openen. / OV Refund may not open this link.".into());
    }
    std::process::Command::new("explorer.exe").arg(&url).spawn().map_err(e)?;
    Ok(())
}

/// https only, allowlisted hosts; on github.com only the project's own pages (/ov-refund/...).
fn external_url_ok(url: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "myaccount.google.com",
        "account.live.com",
        "account.apple.com",
        "login.yahoo.com",
        "app.fastmail.com",
        "www.ns.nl",
        "www.rijdendetreinen.nl",
        "ov-refund.github.io",
        "github.com",
        // carriers (ovrefund_core::model::carrier_url)
        "www.nsinternational.com",
        "www.blauwnet.nl",
        "www.breng.nl",
        "www.arriva.nl",
        "klantenservice.arriva.nl",
        "www.keolis.nl",
        "www.qbuzz.nl",
        "www.connexxion.nl",
        "www.ebs-ov.nl",
        "www.rrreis.nl",
        "www.u-ov.info",
        "www.gvb.nl",
        "www.ret.nl",
        "www.htm.nl",
        "www.hermes.nl",
    ];
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | ',' | '\\')) {
        return false;
    }
    let Ok(parsed) = tauri::Url::parse(url) else { return false };
    let Some(host) = parsed.host_str() else { return false };
    if parsed.scheme() != "https" || !ALLOWED.contains(&host) || parsed.port().is_some()
        || !parsed.username().is_empty() || parsed.password().is_some()
    {
        return false;
    }
    host != "github.com" || (parsed.path().to_ascii_lowercase().starts_with("/ov-refund/") && !parsed.path().contains('%'))
}

/// The app's own redaction (e-mail addresses, tokens, password=..., UUIDs, IBANs, card and long
/// numbers), for text the GUI sends to the bug-report relay.
#[tauri::command]
pub fn redact_text(text: String) -> String {
    ovrefund_core::redact::redact(&text)
}

/// Small UI flags (whitelisted keys only).
#[tauri::command]
pub async fn set_flag(key: String, value: String) -> CmdResult<()> {
    blocking(move || {
        anyhow::ensure!(flag_ok(&key, &value), "Ongeldige instelling. / Invalid setting.");
        if key == "other_service" && value == "yes" {
            // Another claim service means a risk of double claims: stored at once, without the run
            // lock, and automatic submission switched off (consent withdrawn). A running check
            // reads both again right before each submission and submits nothing more.
            let db = open_db()?;
            db.set_setting(&key, &value)?;
            withdraw_auto_now(&db, StopReason::AutoOff)?;
        } else {
            with_ledger_lock(|db| db.set_setting(&key, &value))?;
        }
        Ok(())
    })
    .await
}

/// Whitelisted UI flags and their allowed values.
fn flag_ok(key: &str, value: &str) -> bool {
    match key {
        "wizard_done" | "disclaimer_accepted" | "update_checks" => value.len() <= 64,
        // resume point of the setup wizard: a short step id
        "wizard_step" => {
            (1..=40).contains(&value.len()) && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        }
        "other_service" => matches!(value, "none" | "yes" | "import_done"),
        _ => false,
    }
}

/// Commission withheld by another claim service for a payout (percent, e.g. 25 or 35; None = unknown).
#[tauri::command]
pub async fn set_commission(claim_id: i64, percent: Option<f64>) -> CmdResult<()> {
    blocking(move || with_ledger_lock(|db| db.set_commission(claim_id, percent.map(|p| p / 100.0)))).await
}

/// Temporarily select automatic mode without creating consent. Always restore the exact setting,
/// including its absence, even if the simulation fails. Caller holds the demo run lock.
fn with_demo_auto_mode<T>(db: &ovrefund_core::db::Db, run: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    let previous = db.get_setting("mode")?;
    db.set_setting("mode", "automatic")?;
    let result = run();
    // A protective action during the simulation (logout, claim-service answer, switch-off)
    // wins over the old mode. Restoring it would undo the user's change.
    if db.get_setting("mode")?.as_deref() != Some("automatic") {
        return result;
    }
    match previous {
        Some(mode) => db.set_setting("mode", &mode)?,
        None => {
            db.conn.execute("DELETE FROM settings WHERE key='mode'", [])?;
        }
    }
    result
}

/// DEMO only: simulate the scheduled background check in automatic mode (submits every
/// supported demo claim at once and sends the e-mail report, like an unattended run would).
#[tauri::command]
pub async fn demo_auto_run(app: AppHandle) -> CmdResult<Value> {
    if !crate::demo::is_demo() {
        return Err("Alleen beschikbaar in de demo. / Only available in the demo.".into());
    }
    let _g = BusyGuard::take()?;
    let a = app.clone();
    let r = blocking(move || {
        let db = open_db()?;
        with_demo_auto_mode(&db, || Ok(serde_json::to_value(runner::run(&a, "scheduled", true, false)?)?))
    })
    .await;
    let _ = app.emit("run-finished", &r);
    r
}

/// DEMO only: switch the demo user's subscription; journeys are re-read with the new product.
#[tauri::command]
pub async fn demo_set_subscription(product: String) -> CmdResult<()> {
    blocking(move || {
        anyhow::ensure!(crate::demo::is_demo(), "Alleen beschikbaar in de demo. / Only available in the demo.");
        anyhow::ensure!(["none", "dal_voordeel", "weekend_vrij", "dal_vrij", "altijd_vrij"].contains(&product.as_str()), "Onbekend product. / Unknown product.");
        let db = open_db()?;
        db.set_setting("demo_subscription", &product)?;
        // Re-read only trips that are not claimed yet; claimed ones keep their evidence and amounts.
        let claimed = "SELECT journey_id FROM claims WHERE status IN ('SUBMITTING','SUBMITTED','INDETERMINATE','APPROVED','REJECTED','PAID','EXTERNALLY_CLAIMED')";
        db.conn.execute_batch(&format!(
            "DELETE FROM candidates WHERE journey_id NOT IN ({claimed}); DELETE FROM schedules WHERE journey_id NOT IN ({claimed});
             DELETE FROM evidence_cache WHERE journey_key IN (SELECT fallback_key FROM journeys WHERE id NOT IN ({claimed}));
             UPDATE journeys SET fare_cents = NULL, subscription = NULL, subscription_fee_cents = NULL WHERE id NOT IN ({claimed});"
        ))?;
        Ok(())
    })
    .await
}

/// Settings that may appear in diagnostics (no addresses, no mailbox data).
const DIAG_SETTINGS: [&str; 10] = [
    "auto_login",
    "auto_opt_in_at",
    "auto_unverified_override",
    "interval_days",
    "last_mail_sync",
    "last_year_fetch",
    "mode",
    "no_card",
    "notify_email",
    "notify_windows",
];

/// Anonymised diagnostics for a bug report (no names, IBAN, card numbers, stations, e-mail,
/// cookies or passwords) — the user sees this text before anything is sent.
#[tauri::command]
pub async fn bug_report_context() -> CmdResult<Value> {
    blocking(bug_context_json).await
}

fn bug_context_json() -> anyhow::Result<Value> {
    {
        let db = open_db()?;
        let count = |sql: &str| db.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
        let runs: Vec<Value> = db
            .runs(5)?
            .into_iter()
            .map(|r| json!({"outcome": r["outcome"], "trigger": diagnostic_trigger(&r["trigger"]), "auth": diagnostic_auth(&r["auth_state"])}))
            .collect();
        let claims: Vec<Value> = {
            let mut st = db.conn.prepare("SELECT status, COUNT(*) FROM claims GROUP BY status")?;
            let v = st
                .query_map([], |r| Ok(json!({"status": r.get::<_, String>(0)?, "n": r.get::<_, i64>(1)?})))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            v
        };
        let diag = json!({
            "app_version": env!("CARGO_PKG_VERSION"),
            "windows": std::env::var("OS").unwrap_or_default(),
            "journeys": count("SELECT COUNT(*) FROM journeys"),
            "claims_by_status": claims,
            "recent_runs": runs,
            // NS-looking mails skipped at the last mail sync, per sender domain (counts only).
            "ns_mail_skipped": skipped_summary(&db),
            "settings": {
                "mode": setting(&db, "mode", "manual"),
                "auto_login": setting(&db, "auto_login", "1"),
                "read_ns_mail": setting(&db, "read_ns_mail", "0"),
                "schedule_installed": scheduler::status().installed,
            },
        });
        Ok(json!({ "diagnostics": ovrefund_core::redact::redact(&serde_json::to_string_pretty(&diag)?) }))
    }
}

/// Access key of the project's form-to-mail relay (Web3Forms). Public by design: it can only
/// deliver messages to the project mailbox, nothing else. Compiled in by build.rs from
/// %USERPROFILE%\.ovrefund-release\bug-form-key.txt (or env OVREFUND_BUG_FORM_KEY); never in the
/// repository. Empty = the GUI offers copying and GitHub Issues. The GUI alone posts to
/// the relay (CSP connect-src allows api.web3forms.com).
pub const BUG_FORM_KEY: &str = env!("OVREFUND_BUG_FORM_KEY");

/// NS customer service (general help, e.g. "vergeten uit te checken", objections).
pub const NS_HELP_URL: &str = "https://www.ns.nl/service-en-contact";

/// NS-looking mails skipped at the last mail sync: `{total, by_reason: {not_ns_sender, not_authenticated}}`.
fn skipped_summary(db: &ovrefund_core::db::Db) -> Value {
    let v = mail_skipped(db);
    let sum = |k: &str| v[k].as_object().map(|o| o.values().filter_map(Value::as_u64).sum::<u64>()).unwrap_or(0);
    let (a, b) = (sum("not_ns_sender"), sum("not_authenticated"));
    json!({ "total": a + b, "by_reason": { "not_ns_sender": a, "not_authenticated": b } })
}

/// Is a newer OV Refund available? (`{configured, current, available, version?, notes?}`)
#[tauri::command]
pub async fn check_update(app: AppHandle) -> CmdResult<Value> {
    if let Ok(db) = open_db() {
        if setting(&db, "update_checks", "1") != "1" {
            return Ok(
                json!({ "configured": crate::updater::configured(), "enabled": false, "current": env!("CARGO_PKG_VERSION"), "available": false }),
            );
        }
    }
    crate::updater::check(&app).await.map_err(|err| {
        log::warn!("update check failed: {err:#}");
        let d = ovrefund_core::redact::redact(&err.to_string()).replace(" / ", "/");
        format!("Zoeken naar updates is mislukt ({d}). / Checking for updates failed ({d}).")
    })
}

const UPDATE_BUSY_MSG: &str =
    "Er loopt een controle op de achtergrond. Probeer het over een paar minuten opnieuw. / A check is running in the background. Try again in a few minutes.";

/// Folder for exports and backups the user keeps (see [`crate::paths::export_location`]).
fn export_dir() -> std::path::PathBuf {
    crate::paths::export_location().dir
}

/// A new file name in the export folder (never overwrites).
fn new_export_path(prefix: &str, ext: &str) -> anyhow::Result<std::path::PathBuf> {
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    unique_export_path(&dir, prefix, ext, &stamp)
}

fn unique_export_path(dir: &std::path::Path, prefix: &str, ext: &str, stamp: &str) -> anyhow::Result<std::path::PathBuf> {
    for n in 1..100 {
        let name = if n == 1 { format!("{prefix}-{stamp}.{ext}") } else { format!("{prefix}-{stamp}-{n}.{ext}") };
        let p = dir.join(name);
        if !p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("Er is geen vrije bestandsnaam in de map OV Refund. Ruim oude exportbestanden op in de exportmap en probeer het opnieuw. / There is no free file name in the OV Refund folder. Remove old exports from the export folder and try again.")
}

fn show_in_explorer(p: &std::path::Path) {
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.display())).spawn();
}

/// `{path, folder, onedrive_avoided}` of an export.
fn export_result(p: &std::path::Path) -> Value {
    let loc = crate::paths::export_location();
    json!({ "path": p.to_string_lossy(), "folder": loc.dir.to_string_lossy(), "onedrive_avoided": loc.onedrive_avoided })
}

/// Backup of the ledger (no passwords, IBAN or card numbers: those live in Windows Credential
/// Manager; no e-mail, account, consent or PC-bound settings) to the export folder, then shows it
/// in Explorer.
#[tauri::command]
pub async fn export_backup() -> CmdResult<Value> {
    blocking(|| {
        let _lock = LedgerLock::take()?;
        let p = new_export_path("ovrefund-backup", "ovrbackup")?;
        open_db()?.backup_to(&p, crate::demo::is_demo())?;
        show_in_explorer(&p);
        Ok(export_result(&p))
    })
    .await
}

/// Restore a backup (file contents as base64). Only into an empty ledger, unless `force` (the
/// user confirmed replacing the current data). Refused while a check runs, for backups over
/// 20 MB, of a newer version or from the demo. Before a forced restore the current ledger is
/// backed up automatically (`auto_backup_path`). Claims that reached NS (or may have) and
/// "claimed elsewhere" entries are kept (`kept`); `replaced` counts the backup's claims that a
/// kept claim replaced. Only harmless settings are taken from a backup (db::BACKUP_SETTINGS).
#[tauri::command]
pub async fn import_backup(data_base64: String, force: Option<bool>) -> CmdResult<Value> {
    login_window_open()?;
    let _g = BusyGuard::take()?;
    blocking(move || {
        use base64::Engine;
        let max = ovrefund_core::db::MAX_BACKUP_BYTES;
        anyhow::ensure!(data_base64.len() <= max / 3 * 4 + 8, ovrefund_core::db::BACKUP_TOO_LARGE_MSG);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64.trim())
            .map_err(|_| anyhow::anyhow!(ovrefund_core::db::RESTORE_INVALID_MSG))?;
        anyhow::ensure!(bytes.len() <= max, ovrefund_core::db::BACKUP_TOO_LARGE_MSG);
        anyhow::ensure!(bytes.starts_with(b"SQLite format 3\0"), ovrefund_core::db::RESTORE_INVALID_MSG);
        // Same lock as checks: never while a (scheduled) check runs.
        let _lock = LedgerLock::take_for(5)?;
        let force = force.unwrap_or(false);
        let demo = crate::demo::is_demo();
        let dir = crate::paths::data_dir().join(format!("restore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("backup.sqlite");
        let r = (|| {
            std::fs::write(&tmp, &bytes)?;
            // Check the file before writing the safety backup (a refused file changes nothing).
            ovrefund_core::db::Db::check_backup_file(&tmp, demo)?;
            let mut db = open_db()?;
            let auto_backup = if force && !db.ledger_empty()? {
                let p = new_export_path("ovrefund-before-restore", "ovrbackup")?;
                db.backup_to(&p, demo)?;
                Some(p)
            } else {
                None
            };
            let mut st = db.restore_from(&tmp, force, demo)?;
            st.safety_backup = auto_backup.as_ref().map(|p| p.to_string_lossy().to_string());
            Ok(restore_result(&st))
        })();
        let _ = std::fs::remove_dir_all(&dir);
        r
    })
    .await
}

/// What the GUI shows after a restore.
fn restore_result(st: &ovrefund_core::db::RestoreStats) -> Value {
    json!({
        // Totals in the ledger after the restore.
        "journeys": st.journeys,
        "claims": st.claims,
        // Taken from the backup file.
        "restored": { "journeys": st.from_backup_journeys, "claims": st.from_backup_claims },
        "kept": { "claims": st.kept_claims, "journeys": st.kept_claims, "external": st.kept_external },
        // Backup claims replaced by a kept claim of this ledger for the same trip.
        "replaced_claims": st.replaced_from_backup,
        // Trips assessed again by the next check ("Controleer nu").
        "unassessed": st.unassessed,
        "auto_backup_path": st.safety_backup,
    })
}

/// All claims as a CSV file (Excel, ";"-separated, in the app's language) in the export folder.
#[tauri::command]
pub async fn export_claims_csv() -> CmdResult<Value> {
    blocking(|| {
        let _lock = LedgerLock::take()?;
        let db = open_db()?;
        let csv = db.claims_csv(runner::lang(&db) == ovrefund_core::email::Lang::Nl)?;
        let p = new_export_path("ovrefund-claims", "csv")?;
        // Exclusive creation also protects against another process racing the name check.
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&p)?;
        file.write_all(format!("\u{feff}{csv}").as_bytes())?;
        show_in_explorer(&p);
        Ok(export_result(&p))
    })
    .await
}

/// Open the export folder (exports and backups) in Explorer.
#[tauri::command]
pub async fn open_data_folder() -> CmdResult<()> {
    blocking(|| {
        let dir = export_dir();
        std::fs::create_dir_all(&dir)?;
        std::process::Command::new("explorer.exe").arg(&dir).spawn()?;
        Ok(())
    })
    .await
}

/// Hold SQLite's writer reservation without loading or migrating its schema. This also works
/// when a newer app wrote the ledger. Scheduled runs cannot acquire a lease during installation.
fn update_install_guard(path: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(path)?;
    reserve_update_install(conn)
}

fn reserve_update_install(conn: rusqlite::Connection) -> anyhow::Result<rusqlite::Connection> {
    use rusqlite::OptionalExtension;
    conn.busy_timeout(std::time::Duration::from_secs(1))?;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let has_lock_table: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='run_lock')", [], |r| r.get(0))?;
    let lock = if has_lock_table {
        conn.query_row("SELECT owner, expires_at FROM run_lock WHERE id=1", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .optional()?
    } else {
        None // a fresh/partial database has no running lease; the writer reservation still excludes scans
    };
    if let Some((owner, expiry)) = lock {
        let active = chrono::DateTime::parse_from_rfc3339(&expiry)?.with_timezone(&chrono::Utc) > chrono::Utc::now();
        let alive = runner::lock_owner_pid(&owner).map(runner::process_alive).unwrap_or(true);
        anyhow::ensure!(!active || !alive, UPDATE_BUSY_MSG);
    }
    Ok(conn) // dropping the connection releases the reservation, including on an install error
}

/// Download, verify and install the update; the app restarts into the new version.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> CmdResult<()> {
    login_window_open()?;
    // Never install during a check or submission. This also works with a newer schema.
    let _g = BusyGuard::take()?;
    let _lock = blocking(|| update_install_guard(&crate::paths::db_path())).await?;
    crate::updater::install(&app).await.map_err(|err| {
        log::warn!("update install failed: {err:#}");
        let d = ovrefund_core::redact::redact(&err.to_string()).replace(" / ", "/");
        format!("Bijwerken mislukt ({d}). / Update failed ({d}).")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_erase_keeps_forget_everything_over_a_logout() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        assert_eq!(pending_erase(&db), None);
        queue_erase(&db, ERASE_LOGOUT).unwrap();
        assert_eq!(pending_erase(&db).as_deref(), Some(ERASE_LOGOUT));
        queue_erase(&db, ERASE_FORGET).unwrap();
        assert_eq!(pending_erase(&db).as_deref(), Some(ERASE_FORGET));
        // A later logout never turns a waiting "forget everything" into less.
        queue_erase(&db, ERASE_LOGOUT).unwrap();
        assert_eq!(pending_erase(&db).as_deref(), Some(ERASE_FORGET));
        // Unknown values are ignored.
        db.set_setting(PENDING_ERASE_KEY, "x").unwrap();
        assert_eq!(pending_erase(&db), None);
    }

    #[test]
    fn stop_waits_only_for_a_check_that_is_submitting() {
        use ovrefund_core::pipeline::{auto_phase_active, AUTO_PHASE_KEY};
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let busy = || -> anyhow::Result<()> { anyhow::bail!("Er loopt al een controle. / A check is already running.") };
        let ms = std::time::Duration::from_millis;
        // A check that is still syncing holds the lock: refused right away with the normal message.
        assert!(db.acquire_run_lock("pid1-run-1", chrono::Duration::minutes(30)).unwrap());
        assert!(!auto_phase_active(&db).unwrap());
        let started = std::time::Instant::now();
        let stops = std::cell::Cell::new(0);
        let stop = || -> anyhow::Result<()> {
            stops.set(stops.get() + 1);
            Ok(())
        };
        let e = wait_while_auto_phase(busy, || auto_phase_active(&db), stop, std::time::Duration::from_secs(120), ms(10)).unwrap_err();
        assert!(e.to_string().starts_with("Er loopt al een controle"), "{e}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(stops.get(), 0, "a refused action never stops the check");
        // A stale marker of another (finished) run does not count either.
        db.set_setting(AUTO_PHASE_KEY, "pid1-run-0").unwrap();
        assert!(!auto_phase_active(&db).unwrap());
        // In its automatic-submission phase: waited for, then the stop message.
        db.set_setting(AUTO_PHASE_KEY, "pid1-run-1").unwrap();
        assert!(auto_phase_active(&db).unwrap());
        let e = wait_while_auto_phase(busy, || auto_phase_active(&db), stop, ms(50), ms(10)).unwrap_err();
        assert_eq!(e.to_string(), STOPPING_BUSY_MSG);
        assert!(stops.get() >= 1, "stopped (again before each retry), then waited");
        // The check ends after its current claim: the action goes ahead.
        let mut n = 0;
        let r = wait_while_auto_phase(
            || {
                n += 1;
                if n < 3 {
                    busy().map(|_| 0)
                } else {
                    Ok(n)
                }
            },
            || auto_phase_active(&db),
            stop,
            std::time::Duration::from_secs(120),
            ms(10),
        );
        assert_eq!(r.unwrap(), 3);
        // Once seen submitting, the check is waited for through its closing steps (the phase
        // marker is gone while it still holds the lock or the app is still busy).
        let n = std::cell::Cell::new(0);
        let r = wait_while_auto_phase(
            || {
                n.set(n.get() + 1);
                if n.get() < 5 {
                    busy().map(|_| 0)
                } else {
                    Ok(n.get())
                }
            },
            || {
                if n.get() <= 1 {
                    auto_phase_active(&db)
                } else {
                    Ok(false)
                }
            },
            stop,
            std::time::Duration::from_secs(120),
            ms(10),
        );
        assert_eq!(r.unwrap(), 5);
        assert!(
            AUTO_OFF_REST_NOT_SAVED_MSG.starts_with("Automatisch indienen staat uit.")
                && AUTO_OFF_REST_NOT_SAVED_MSG.contains(" / Automatic submission is off.")
        );
    }

    #[test]
    fn stop_is_repeated_for_a_new_lock_holder_and_no_false_stopping_message() {
        let busy = || -> anyhow::Result<()> { anyhow::bail!("Er loopt al een controle. / A check is already running.") };
        let ms = std::time::Duration::from_millis;
        // The submitting check ends; another check (e.g. a scheduled one that was waiting) takes
        // the lock while it still syncs: it is asked to stop too, and after the wait the plain
        // busy message comes back, not "stops after the current claim".
        let calls = std::cell::Cell::new(0);
        let stops = std::cell::Cell::new(0);
        let e = wait_while_auto_phase(
            busy,
            || {
                calls.set(calls.get() + 1);
                Ok(calls.get() == 1)
            },
            || {
                stops.set(stops.get() + 1);
                Ok(())
            },
            ms(60),
            ms(10),
        )
        .unwrap_err();
        assert!(e.to_string().starts_with("Er loopt al een controle"), "{e}");
        assert!(stops.get() > 1, "stop repeated while waiting");
    }

    #[test]
    fn settings_are_validated_before_anything_changes() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let settings = |mode: &str, notify_email: bool, host: &str| SettingsIn {
            mode: mode.into(),
            interval_days: 7,
            notify_windows: true,
            notify_email,
            smtp: ovrefund_core::email::SmtpConfig { host: host.into(), ..Default::default() },
            smtp_password: None,
            schedule_enabled: true,
            auto_consent: false,
            auto_unverified_consent: false,
        };
        assert!(validate_settings(&settings("sometimes", false, ""), &db).is_err());
        assert!(validate_settings(&settings("manual", false, ""), &db).is_ok());
        // Back to manual with an e-mail provider that cannot be used: refused before consent is touched.
        let e = validate_settings(&settings("manual", true, "smtp-mail.outlook.com"), &db).unwrap_err();
        assert_eq!(e.to_string(), ovrefund_core::email::UNSUPPORTED_PROVIDER_MSG);
        assert!(validate_settings(&settings("automatic", false, ""), &db).is_err(), "no explicit consent");
    }

    #[test]
    fn manual_submission_requires_fresh_duplicate_evidence_or_confirmation() {
        let mut db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let mut journey = crate::arriva::demo_journey();
        journey.carrier_code = Some(NS_CARRIER_CODE.into());
        journey.carrier_name = Some("NS".into());
        journey.coupling_id = Some("test-ns-card".into());
        let (jid, _) = db.upsert_journey(&journey).unwrap();
        let claim = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
        let mut req = SubmitRequest {
            claim_id: claim.id,
            confirmed: pipeline::build_facts(&journey, DelayBand::Min30To59, journey.check_in).unwrap(),
            declaration_accepted: true, confirm_resubmit: false, confirm_near: false,
            confirm_claim_list_unavailable: false, reached_destination: true,
        };
        assert_eq!(check_manual_claim_list(&mut db, &req, None).unwrap().unwrap()["needs_claim_list_confirmation"], true);
        req.confirm_claim_list_unavailable = true;
        assert!(check_manual_claim_list(&mut db, &req, None).unwrap().is_none());
        req.confirm_claim_list_unavailable = false;
        assert!(check_manual_claim_list(&mut db, &req, Some(&[])).unwrap().is_none());
        let mut remote = pipeline::RemoteClaim {
            reference: Some("fixture-ref".into()), travel_date: Some(journey.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive()),
            origin_uic: None, destination_uic: None, transaction_id: None,
            state: Some(ClaimState::Submitted), amount_cents: None, raw_status: "SUBMITTED".into(),
        };
        assert_eq!(check_manual_claim_list(&mut db, &req, Some(std::slice::from_ref(&remote))).unwrap().unwrap()["needs_near_confirmation"], true);
        req.confirm_near = true;
        remote.transaction_id = Some(journey.source_record_id.clone());
        let result = check_manual_claim_list(&mut db, &req, Some(&[remote])).unwrap().unwrap();
        assert_eq!(result["status"], "EXTERNALLY_CLAIMED");
        assert_eq!(db.claim(claim.id).unwrap().ns_reference.as_deref(), Some("fixture-ref"));
        let unavailable = check_manual_claim_list(&mut db, &req, None).unwrap().unwrap();
        assert_eq!(unavailable["status"], "EXTERNALLY_CLAIMED");
    }

    #[test]
    fn disabling_checks_takes_effect_even_when_task_removal_fails() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        save_schedule_intent(&db, true, 7).unwrap();
        let result = apply_schedule_intent::<()>(&db, false, 3, |db, _, _| {
            assert_eq!(db.get_setting("schedule_enabled").unwrap().as_deref(), Some("0"));
            anyhow::bail!("Testtaak bleef staan. / Fixture task removal failed.");
        });
        assert!(result.is_err());
        assert_eq!(db.get_setting("schedule_enabled").unwrap().as_deref(), Some("0"));
        let windows = scheduler::TaskStatus {
            installed: true, next_run: Some("fixture".into()), interval_days: Some(7), ..Default::default()
        };
        let shown = effective_schedule(&db, windows).unwrap();
        assert!(!shown.installed && shown.next_run.is_none() && shown.interval_days.is_none());
        assert!(schedule_needs_install(&db, true, 3), "the still-installed task may have a different interval");
    }

    #[test]
    fn a_failed_prerequisite_cannot_activate_automatic_submission() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        let settings = || SettingsIn {
            mode: "automatic".into(), interval_days: 7, notify_windows: true, notify_email: false,
            smtp: Default::default(), smtp_password: Some("fixture-password".into()),
            schedule_enabled: true, auto_consent: true, auto_unverified_consent: true,
        };
        let failed = || anyhow::Error::msg("fixture failure");
        assert!(save_settings_locked(&db, settings(), |_, _, _| Err(failed()), |_| panic!("schedule failed first")).is_err());
        assert_eq!(db.get_setting("schedule_enabled").unwrap(), None);
        assert!(save_settings_locked(&db, settings(), |_, _, _| Ok("installed".into()), |_| Err(failed())).is_err());
        assert_eq!(db.get_setting("schedule_enabled").unwrap().as_deref(), Some("1"));
        for key in ["mode", "auto_opt_in_at", "auto_unverified_override", "auto_enabled_via"] {
            assert_eq!(db.get_setting(key).unwrap(), None, "{key}");
        }
        save_settings_locked(&db, settings(), |_, _, _| Ok("installed".into()), |_| Ok(())).unwrap();
        assert!(runner::auto_enabled(&db));
        assert_eq!(db.get_setting("auto_enabled_via").unwrap().as_deref(), Some("override"));
    }

    #[test]
    fn diagnostics_exclude_scheduler_paths_and_run_times() {
        let status = scheduler::TaskStatus {
            installed: true,
            command: Some(r"C:\Users\private-name\ov-refund.exe".into()),
            interval_days: Some(7),
            last_run: Some("2026-11-01T12:34:56Z".into()),
            next_run: Some("2026-11-08T12:34:56Z".into()),
            last_result: Some("0".into()),
            deadline_run: Some("2026-11-20T10:00:00".into()),
        };
        assert_eq!(diagnostic_schedule(&status), json!({"installed":true,"interval_days":7,"last_result":"0"}));
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        db.start_run("manual", "gui").unwrap();
        for key in ["auto_opt_in_at", "last_mail_sync", "last_year_fetch"] {
            db.set_setting(key, "2026-11-01T12:34:56+01:00").unwrap();
        }
        let diag = diagnostics_json(&db).unwrap();
        assert!(diag["runs"][0].get("started_at").is_none());
        assert!(diag["runs"][0].get("completed_at").is_none());
        assert!(diag["schedule"].get("command").is_none());
        assert_eq!(diag["settings"]["auto_opt_in_at_present"], true);
        let text = diag.to_string();
        assert!(
            !text.as_bytes().windows(3).any(|w| w[0] == b'T' && w[1].is_ascii_digit() && w[2].is_ascii_digit()),
            "no RFC3339 times: {text}"
        );
        assert_eq!(diag["generated_at"].as_str().unwrap().len(), 10);
    }

    #[test]
    fn custom_run_triggers_are_not_in_diagnostics() {
        for value in [json!("Jan Jansen"), json!("a@example.org"), json!(null)] {
            assert_eq!(diagnostic_trigger(&value), "other");
        }
        assert_eq!(diagnostic_trigger(&json!("demo-private-name")), "demo");
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        db.start_run("manual", "Jan Jansen").unwrap();
        let diag = diagnostics_json(&db).unwrap().to_string();
        assert!(!diag.contains("Jan Jansen"));
    }

    #[test]
    fn update_guard_handles_a_missing_database_and_blocks_new_writers() {
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let temp = TempDir(std::env::temp_dir().join(format!(
            "ovrefund-update-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        )));
        let path = temp.0.join("missing.sqlite");
        let guard = update_install_guard(&path).unwrap();
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer.busy_timeout(std::time::Duration::from_millis(1)).unwrap();
        assert!(writer.execute_batch("BEGIN IMMEDIATE").is_err());
        let tables: i64 = guard.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 0);
        drop(guard);
        writer.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
    }

    #[test]
    fn update_guard_accepts_newer_schema_and_refuses_live_run() {
        let fresh = reserve_update_install(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let tables: i64 = fresh.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 0, "the updater must not initialize the ledger schema");
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        db.conn.pragma_update(None, "user_version", 9999).unwrap();
        let conn = reserve_update_install(db.conn).unwrap();
        assert_eq!(conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 9999);
        drop(conn);
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        db.acquire_run_lock(&edit_lock_owner(), chrono::Duration::minutes(10)).unwrap();
        assert!(reserve_update_install(db.conn).unwrap_err().to_string().contains(UPDATE_BUSY_MSG));
    }

    #[test]
    fn diagnostics_auth_does_not_expose_remote_text() {
        assert_eq!(diagnostic_auth(&json!("blocked: private name and station")), "blocked");
        assert_eq!(diagnostic_auth(&json!("private name")), "unknown");
    }

    #[test]
    fn logout_preserves_account_binding_for_existing_trips() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        db.set_setting("ns_account_email", "first@example.org").unwrap();
        db.set_setting("auto_login", "1").unwrap();
        db.conn.execute_batch(
            "INSERT INTO journeys(source_id,source_record_id,fallback_key,check_in_utc,origin_uic,origin_name,payload_sha256,first_seen_at,observed_at)
             VALUES('s','a','k','2026-10-28T19:01:00Z','1','X','x','t','t');"
        ).unwrap();
        super::record_disconnect(&db).unwrap();
        assert!(crate::runner::account_mismatch(&db, "second@example.org"));
        assert!(!crate::runner::account_mismatch(&db, "first@example.org"));
        assert_eq!(db.get_setting("auto_login").unwrap().as_deref(), Some("0"));
        assert_eq!(db.source_status("mijnns").unwrap().unwrap().1.as_deref(), Some("disconnected"));
    }

    #[test]
    fn demo_mode_is_restored_without_manufacturing_consent() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        for mode in [None, Some("manual"), Some("automatic")] {
            db.conn.execute("DELETE FROM settings WHERE key='mode'", []).unwrap();
            if let Some(mode) = mode {
                db.set_setting("mode", mode).unwrap();
            }
            for fail in [false, true] {
                let result = with_demo_auto_mode(&db, || {
                    assert_eq!(db.get_setting("mode")?.as_deref(), Some("automatic"));
                    assert!(db.get_setting("auto_opt_in_at")?.is_none());
                    anyhow::ensure!(!fail, "Gesimuleerde fout. / Simulated failure.");
                    Ok(())
                });
                assert_eq!(result.is_err(), fail);
                assert_eq!(db.get_setting("mode").unwrap().as_deref(), mode);
            }
        }
        db.set_setting("auto_opt_in_at", "existing consent").unwrap();
        with_demo_auto_mode(&db, || Ok(())).unwrap();
        assert_eq!(db.get_setting("auto_opt_in_at").unwrap().as_deref(), Some("existing consent"));
        with_demo_auto_mode(&db, || withdraw_auto_consent(&db)).unwrap();
        assert_eq!(db.get_setting("mode").unwrap().as_deref(), Some("manual"));
        assert!(db.get_setting("auto_opt_in_at").unwrap().is_none());
    }

    #[test]
    fn flags_are_whitelisted() {
        assert!(flag_ok("wizard_step", "bank-details"));
        assert!(flag_ok("wizard_step", "step_3"));
        assert!(!flag_ok("wizard_step", ""));
        assert!(!flag_ok("wizard_step", "<script>"));
        assert!(!flag_ok("wizard_step", &"x".repeat(41)));
        for v in ["none", "yes", "import_done"] {
            assert!(flag_ok("other_service", v));
        }
        assert!(!flag_ok("other_service", "maybe"));
        assert!(!flag_ok("mode", "automatic"), "consent never via set_flag");
        assert!(flag_ok("wizard_done", "1"));
    }

    #[test]
    fn submit_request_requires_reached_destination() {
        let f = r#"{"source_record_id":"x","coupling_id":"c","check_in":"2026-10-28T19:01:00Z","check_out":"2026-10-28T21:00:00Z","origin_name":"A","destination_name":"B","scheduled_departure_local":"20:06","delay_band":"min30_to59","reached_destination":true}"#;
        let r: SubmitRequest = serde_json::from_str(&format!(r#"{{"claim_id":1,"confirmed":{f},"declaration_accepted":true}}"#)).unwrap();
        assert!(!r.reached_destination);
        let r: SubmitRequest =
            serde_json::from_str(&format!(r#"{{"claim_id":1,"confirmed":{f},"declaration_accepted":true,"reached_destination":false}}"#))
                .unwrap();
        assert!(!r.reached_destination);
        assert!(NOT_REACHED_MSG.contains(" / ") && UPDATE_BUSY_MSG.contains(" / "));
    }

    #[test]
    fn external_links_are_allowlisted() {
        assert!(external_url_ok("https://www.ns.nl/service-en-contact"));
        assert!(external_url_ok("https://github.com/ov-refund/ov-refund/releases"));
        assert!(external_url_ok("https://github.com/OV-Refund/ov-refund/issues"));
        assert!(external_url_ok("https://github.com/ov-refund/OV-Refund/issues/new"));
        assert!(!external_url_ok("https://mail.google.com/mail/?view=cm"));
        assert!(!external_url_ok("https://outlook.live.com/mail/0/deeplink/compose"));
        assert!(!external_url_ok("https://github.com/attacker/repo"));
        assert!(!external_url_ok("https://github.com/ov-refund-evil/x"));
        assert!(!external_url_ok("https://github.com/ov-refund/../unrelated/repo"));
        assert!(!external_url_ok("https://github.com/ov-refund/%2e%2e/unrelated/repo"));
        assert!(!external_url_ok("https://github.com/ov-refund/%2f..%2funrelated/repo"));
        assert!(!external_url_ok("https://www.ns.nl@elsewhere.invalid/"));
        assert!(!external_url_ok("https://github.com"));
        assert!(!external_url_ok("http://www.ns.nl/"));
        assert!(!external_url_ok("https://www.ns.nl.evil.example/"));
        assert!(!external_url_ok("https://www.ns.nl/ x"));
    }

    #[test]
    fn redact_text_hides_personal_data() {
        let r = redact_text(
            "mail me at a.b@example.org, password=hunter2, IBAN NL91ABNA0417164300, id 1b4e28ba-2fa1-11d2-883f-0016d3cca427, 12345678901"
                .into(),
        );
        for leak in ["a.b@example", "hunter2", "NL91ABNA", "1b4e28ba", "12345678901"] {
            assert!(!r.contains(leak), "{leak} in {r}");
        }
    }

    #[test]
    fn wizard_done_is_explicit() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO journeys(source_id,source_record_id,fallback_key,check_in_utc,origin_uic,origin_name,payload_sha256,first_seen_at,observed_at)
                 VALUES('s','a','k1','2026-10-28T19:01:00Z','1','X','x','t','t');",
            )
            .unwrap();
        assert!(!wizard_done(&db), "trips alone (e.g. a restored backup) never skip the wizard");
        db.set_setting("onboarded", "1").unwrap();
        assert!(wizard_done(&db), "older installation without the flag");
        db.set_setting("wizard_done", "0").unwrap();
        assert!(!wizard_done(&db), "explicit flag wins");
        db.set_setting("wizard_done", "1").unwrap();
        assert!(wizard_done(&db));
    }

    #[test]
    fn smtp_password_is_bound_to_server_and_user() {
        let a = ovrefund_core::email::SmtpConfig { host: "smtp.gmail.com".into(), username: "Me@Example.org".into(), ..Default::default() };
        let same =
            ovrefund_core::email::SmtpConfig { host: " SMTP.gmail.com".into(), username: "me@example.org ".into(), ..Default::default() };
        let other_host = ovrefund_core::email::SmtpConfig { host: "smtp.evil.example".into(), ..a.clone() };
        let other_user = ovrefund_core::email::SmtpConfig { username: "x@example.org".into(), ..a.clone() };
        assert_eq!(a.password_fingerprint(), same.password_fingerprint());
        assert_ne!(a.password_fingerprint(), other_host.password_fingerprint());
        assert_ne!(a.password_fingerprint(), other_user.password_fingerprint());
        assert!(!a.password_fingerprint().contains("example"), "no address in the stored fingerprint");
        assert!(ovrefund_core::email::SMTP_REENTER_MSG.contains(" / "));
    }

    #[test]
    fn hostile_amounts_and_lock_owners() {
        assert!(e("Query returned no rows").contains(" / "));
        assert!(!e(r"C:\private-name\data.sqlite").contains("private-name"));
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.001, 1000.001] {
            assert!(amount_cents(v).is_err());
        }
        assert_eq!(amount_cents(12.345).unwrap(), 1235);
        assert_eq!(amount_cents(1000.0).unwrap(), 100_000);
        assert_eq!(cli_amount_cents("12,50").unwrap(), 1250);
        assert_eq!(cli_amount_cents(" 3.1 ").unwrap(), 310);
        for v in ["abc", "", "-1", "1000,01", "NaN", "inf", "1e9"] {
            assert!(cli_amount_cents(v).is_err(), "{v}");
        }
        let owners: std::collections::HashSet<_> = (0..1000).map(|_| edit_lock_owner()).collect();
        assert_eq!(owners.len(), 1000);
    }

    #[test]
    fn confirm_word_is_localised_and_case_insensitive() {
        for w in ["WISSEN", "wissen", " Erase ", "ERASE"] {
            assert!(confirm_word_ok(w), "{w}");
        }
        for w in ["", "WIS", "erase everything", "JA", "DELETE"] {
            assert!(!confirm_word_ok(w), "{w}");
        }
        assert!(CONFIRM_WORD_MSG.contains(" / "));
    }

    #[test]
    fn diagnostics_counts_have_no_trip_dates() {
        let db = ovrefund_core::db::Db::open_in_memory().unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO journeys(source_id,source_record_id,fallback_key,check_in_utc,origin_uic,origin_name,payload_sha256,first_seen_at,observed_at,carrier_code)
                 VALUES('s','a','k1','2026-10-28T19:01:00Z','1','Utrecht Centraal','x','t','t','4'),('s','b','k2','2026-10-27T08:00:00Z','2','Leiden Centraal','x','t','t','4');",
            )
            .unwrap();
        let v = grouped_counts(&db, "SELECT substr(j.check_in_utc, 1, 7), j.carrier_code, COUNT(*) FROM journeys j GROUP BY 1, 2").unwrap();
        assert_eq!(v["2026-10"]["4"], 2);
        let s = v.to_string();
        assert!(!s.contains("2026-10-28") && !s.contains("Utrecht") && !s.contains("19:01"), "{s}");
    }
}

#[cfg(test)]
mod export_name_tests {
    use super::*;
    #[test]
    fn exports_use_seconds_and_never_reuse_existing_names() {
        let mut paths = Vec::new();
        let dir = crate::paths::data_dir().join("export-name-test");
        std::fs::create_dir_all(&dir).unwrap();
        for n in 1..=3 {
            let p = unique_export_path(&dir, "ovrefund-name-test", "csv", "20261113-121314").unwrap();
            let suffix = if n == 1 { String::new() } else { format!("-{n}") };
            assert_eq!(p.file_name().unwrap().to_string_lossy(), format!("ovrefund-name-test-20261113-121314{suffix}.csv"));
            assert!(!paths.contains(&p));
            let name = p.file_stem().unwrap().to_string_lossy();
            assert!(name.split('-').nth(4).unwrap_or("").len() >= 6, "{name}");
            std::fs::write(&p, b"test").unwrap();
            paths.push(p);
        }
        for p in paths {
            std::fs::remove_file(p).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }
}

fn wipe_history_and_log(db: &mut ovrefund_core::db::Db, log_path: &std::path::Path) -> anyhow::Result<()> {
    let history = db.wipe_history();
    let attachments = crate::arriva_claim::remove_unneeded_attachments(db);
    let diagnostics = crate::paths::remove_own_diagnostics_checked();
    let log = match std::fs::OpenOptions::new().write(true).truncate(true).open(log_path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    };
    history.and(attachments).and(diagnostics).and(log)
}

#[cfg(test)]
mod history_log_tests {
    #[test]
    fn erasing_history_truncates_the_log() {
        let _serial = crate::arriva_claim::tests::ATTACHMENT_DIRS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = crate::paths::data_dir().join("history-log-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ovrefund.log");
        std::fs::write(&path, b"private log").unwrap();
        let mut db = ovrefund_core::db::Db::open_in_memory().unwrap();
        super::wipe_history_and_log(&mut db, &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}

fn confirm_all_word_ok(s: &str) -> bool {
    matches!(s.trim().to_uppercase().as_str(), "ALLES WISSEN" | "ERASE ALL")
}
#[cfg(test)]
mod erase_word_tests {
    #[test]
    fn full_erase_requires_the_stronger_phrase() {
        for s in ["ALLES WISSEN", " erase all ", "alles wissen"] {
            assert!(super::confirm_all_word_ok(s));
        }
        for s in ["WISSEN", "ERASE", "", "ALL", "DELETE ALL"] {
            assert!(!super::confirm_all_word_ok(s));
        }
    }
}
