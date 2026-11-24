// No console window when double-clicked (release). CLI subcommands attach to the parent console.
#![windows_subsystem = "windows"]
#![recursion_limit = "256"] // the large json!() of the overview command

mod arriva;
mod arriva_claim;
use ovrefund_core::carrier_policy;
mod commands;
mod demo;
#[cfg(test)]
mod message_lint;
mod ns_adapters;
mod ns_bridge;
mod ns_elements;
mod paths;
mod proof_cards;
mod runner;
mod scheduler;
mod secrets;
mod session_store;
mod updater;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

const HELP: &str = "OV Refund — local NS delay-compensation assistant

USAGE:
  ov-refund                         open the app
  ov-refund --run-once [--trigger T] run one check hidden (used by Task Scheduler), then exit
  ov-refund scan                    one check in manual mode (never submits), prints a summary
  ov-refund history                 print journeys and claim states from the local ledger
  ov-refund diagnose                print configuration, schedule and session state (no secrets)
  ov-refund install-schedule [days] register the per-user scheduled task (default 7)
  ov-refund remove-schedule         remove the scheduled task
";

fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Console::{AttachConsole, SetConsoleOutputCP, ATTACH_PARENT_PROCESS};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        // Output is UTF-8: without this a legacy console code page shows "—" as "â€”".
        let _ = SetConsoleOutputCP(65001);
    }
}

fn init_log() {
    let _ = std::fs::create_dir_all(paths::data_dir());
    // Minimal file logger; messages pass through the redactor.
    struct L(std::sync::Mutex<std::fs::File>);
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) && r.target().starts_with("ov") {
                use std::io::Write;
                let line = ovrefund_core::redact::redact(&format!(
                    "{} {} {}\n",
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                    r.level(),
                    r.args()
                ));
                let _ = self.0.lock().unwrap().write_all(line.as_bytes());
            }
        }
        fn flush(&self) {}
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(paths::log_path()) {
        if f.metadata().map(|m| m.len() > 5_000_000).unwrap_or(false) {
            let _ = f.set_len(0);
        }
        let _ = log::set_boxed_logger(Box::new(L(std::sync::Mutex::new(f)))).map(|_| log::set_max_level(log::LevelFilter::Info));
    }
}

/// Remove everything OV Refund stored for this Windows user (used by the uninstaller when
/// "delete application data" is ticked): credentials, scheduled task, data folder.
/// The WebView2 profile (%LOCALAPPDATA%\nl.ovrefund.app) is removed by the uninstaller itself.
///
/// Takes the same run lock as checks (see [`take_run_lock_for_wipe`]), so a running check can
/// never write data back after the wipe: it waits at most ~60 s, then stops the process that holds
/// the lock and proceeds.
fn forget_all() -> anyhow::Result<()> {
    // First, so no new scheduled check starts while we wait.
    let mut failures = CleanupErrors::default();
    failures.attempt("schedule", scheduler::remove);
    // A running (or waiting) check submits nothing more: consent is withdrawn and the check is
    // asked to stop, without the lock (the claim in progress completes).
    if let Ok(db) = runner::open_db() {
        failures.attempt("stop automatic submission", || {
            commands::withdraw_auto_consent(&db)?;
            runner::request_auto_stop(&db, ovrefund_core::pipeline::StopReason::Erased)
        });
    }
    let held = take_run_lock_for_wipe(std::time::Duration::from_secs(60));
    let mut keys = vec![secrets::BANK.to_string(), secrets::SMTP_PASSWORD.to_string(), secrets::NS_LOGIN.to_string()];
    if let Ok(db) = runner::open_db() {
        // A check that started before this point must not save anything afterwards.
        failures.attempt("session invalidation", || runner::bump_session_epoch(&db));
        if let Ok(mut st) = db.conn.prepare("SELECT DISTINCT card_last4, coupling_id FROM journeys WHERE card_last4 IS NOT NULL") {
            if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))) {
                for (l4, coupling) in rows.flatten() {
                    keys.push(secrets::legacy_card_key(&l4));
                    if let Some(c) = coupling {
                        keys.push(secrets::card_key(&c, &l4));
                    }
                }
            }
        }
    }
    for k in &keys {
        failures.attempt("credential", || secrets::delete(k));
    }
    // ...and anything else in our credential namespaces (orphaned cards, demo entries).
    let extra = failures.attempt("credential enumeration", secrets::delete_all).unwrap_or_default();
    failures.attempt("session", session_store::clear);
    // Backups and claim CSVs OV Refund wrote to Documents\OV Refund (only its own files).
    let exports = failures.attempt("exports", paths::remove_own_exports_checked).unwrap_or_default();
    failures.attempt("diagnostics", paths::remove_own_diagnostics_checked);
    let dir = paths::data_dir();
    // The lock row (if any) disappears with the data folder.
    let _ = held;
    if dir.exists() {
        failures.attempt("data folder", || std::fs::remove_dir_all(&dir).map_err(Into::into));
    }
    println!(
        "removed credential entries ({} known keys, {extra} found by enumeration), {exports} export file(s) and {}",
        keys.len(),
        dir.display()
    );
    failures.finish()
}

#[derive(Default)]
struct CleanupErrors(Vec<&'static str>);
impl CleanupErrors {
    fn attempt<T>(&mut self, label: &'static str, action: impl FnOnce() -> anyhow::Result<T>) -> Option<T> {
        match action() {
            Ok(value) => Some(value),
            Err(_) => {
                self.0.push(label);
                None
            }
        }
    }
    fn finish(self) -> anyhow::Result<()> {
        anyhow::ensure!(self.0.is_empty(), "Wissen mislukt / Erasure failed: {}", self.0.join(", "));
        Ok(())
    }
}

/// Take the run lock for a wipe ("forget-all", "forget-login"). Waits up to `max` for a running
/// check (or GUI action); a lock left by a process that no longer exists, or an expired lease, is
/// taken over. After `max` the process that still holds the lock is stopped (its pid is part of
/// the lock owner) and the lock is taken anyway: the uninstaller must never hang, and nothing may
/// be written back after the wipe. Returns the owner string held by this process, or None when
/// there is no (readable) ledger at all.
fn take_run_lock_for_wipe(max: std::time::Duration) -> Option<String> {
    if !paths::db_path().exists() {
        return None;
    }
    let owner = format!("pid{}-cli-forget-{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    let start = std::time::Instant::now();
    loop {
        let Ok(db) = runner::open_db() else { return None };
        let _ = runner::recover_stale_lock(&db);
        if db.acquire_run_lock(&owner, chrono::Duration::minutes(5)).unwrap_or(false) {
            return Some(owner);
        }
        if start.elapsed() >= max {
            if let Ok(Some(holder)) = db.run_lock_owner() {
                if let Some(pid) = runner::lock_owner_pid(&holder).filter(|p| *p != std::process::id()) {
                    eprintln!("a check (process {pid}) still holds the lock after {} s; stopping it", max.as_secs());
                    runner::kill_process(pid);
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                let _ = db.break_stale_lock(&holder);
            }
            return db.acquire_run_lock(&owner, chrono::Duration::minutes(5)).unwrap_or(false).then_some(owner);
        }
        drop(db);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Used by the uninstaller (not on updates): the stored NS password and session never outlive the
/// app. Same run lock as checks, and the session epoch changes, so a running check cannot save the
/// session again afterwards.
fn cli_forget_login() -> anyhow::Result<()> {
    let held = take_run_lock_for_wipe(std::time::Duration::from_secs(60));
    let _ = secrets::delete(secrets::NS_LOGIN);
    let _ = session_store::clear();
    if let Ok(db) = runner::open_db() {
        let _ = runner::bump_session_epoch(&db);
        let _ = db.upsert_source("mijnns", "mijnns", false, "disconnected");
        if let Some(owner) = &held {
            let _ = db.release_run_lock(owner);
        }
    }
    println!("NS login removed");
    Ok(())
}

fn cli_history() -> anyhow::Result<()> {
    let db = runner::open_db()?;
    for r in db.journey_rows(200)? {
        let j = &r.journey;
        println!(
            "{}  {:<22} → {:<22} proxy {:>4} delay {:>4}  {:<30} {}",
            j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).format("%Y-%m-%d %H:%M"),
            j.origin.name.chars().take(22).collect::<String>(),
            j.destination.as_ref().map(|d| d.name.chars().take(22).collect::<String>()).unwrap_or_default(),
            r.proxy_min.map(|v| v.to_string()).unwrap_or("-".into()),
            r.delay_min.map(|v| v.to_string()).unwrap_or("-".into()),
            r.confidence.unwrap_or_default(),
            r.claim_status.unwrap_or_default()
        );
    }
    Ok(())
}

fn cli_diagnose() -> anyhow::Result<()> {
    let db = runner::open_db()?;
    println!("data dir      : {}", paths::data_dir().display());
    println!("mode          : {} (opt-in at {:?})", runner::setting(&db, "mode", "manual"), db.get_setting("auto_opt_in_at")?);
    println!("gateway       : {} verified={}", ns_adapters::GATEWAY_ID, runner::gateway_verified(&db));
    println!("source        : {:?}", db.source_status("mijnns")?);
    println!("bank details  : {}", if secrets::bank_details().is_some() { "configured (Credential Manager)" } else { "not configured" });
    println!("schedule      : {:?}", scheduler::status());
    for r in db.runs(5)? {
        println!("run           : {}", r);
    }
    Ok(())
}

/// Mask personal values but keep structure, numbers that look like money, dates and enums.
fn shape(v: &serde_json::Value, key: &str, depth: usize) -> serde_json::Value {
    use serde_json::Value as V;
    let sensitive = [
        "name", "naam", "email", "iban", "address", "adres", "street", "postal", "zip", "birth", "card", "kaart", "engraved", "number",
        "token", "phone", "initial", "city", "house", "addition", "nsr", "sex", "customer", "person", "id",
    ];
    match v {
        V::Array(a) => {
            let mut out = vec![V::String(format!("<array len={}>", a.len()))];
            out.extend(a.iter().take(2).map(|x| shape(x, key, depth + 1)));
            V::Array(out)
        }
        V::Object(o) if depth < 8 => V::Object(o.iter().map(|(k, x)| (k.clone(), shape(x, k, depth + 1))).collect()),
        V::String(s) if sensitive.iter().any(|w| key.to_lowercase().contains(w)) => V::String(format!("<str:{}>", s.len())),
        V::Number(_) if sensitive.iter().any(|w| key.to_lowercase().contains(w)) => V::String("<num>".into()),
        V::String(s) => V::String(ovrefund_core::redact::redact(&s.chars().take(60).collect::<String>())),
        other => other.clone(),
    }
}

/// Live test of the claim form WITHOUT submitting: opens NS's journey-linked form for the given
/// journey, fills every field (placeholder delay band if the trip had none) and leaves the page.
fn form_test_run(app: &tauri::AppHandle, jid: i64, screenshot: bool) -> anyhow::Result<()> {
    let db = runner::open_db()?;
    let jid = if jid > 0 {
        jid
    } else {
        db.conn
            .query_row(
                "SELECT j.id FROM journeys j JOIN candidates c ON c.journey_id = j.id
                 WHERE c.scheduled_departure_utc IS NOT NULL AND j.check_out_utc IS NOT NULL
                   AND c.expires_at > datetime('now', '+1 day') AND j.check_in_utc < datetime('now', '-1 day')
                 ORDER BY j.check_in_utc DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .map_err(|_| anyhow::anyhow!("no recent trip to test with (run a check first)"))?
    };
    let j = db.journey(jid)?;
    let (_, band, sdep, _) = db.assessment(jid)?.ok_or_else(|| anyhow::anyhow!("journey not assessed"))?;
    let sdep = sdep.ok_or_else(|| anyhow::anyhow!("no scheduled departure for this journey"))?;
    let facts = ovrefund_core::pipeline::build_facts(&j, band.unwrap_or(ovrefund_core::model::DelayBand::Min30To59), sdep)
        .ok_or_else(|| anyhow::anyhow!("journey lacks check-out/destination/coupling"))?;
    let mut gw =
        ns_adapters::NsClaimGateway { bridge: ns_bridge::NsBridge::open(app, false)?, verified: false, bank: secrets::bank_details() };
    println!(
        "form test (NOT submitting): {} -> {} scheduled dep {}",
        facts.origin_name, facts.destination_name, facts.scheduled_departure_local
    );
    let mut notes = gw.fill_form(&facts);
    // Like a real submission up to (not including) "Verzenden": tick the declaration on the page and run
    // the same pre-submit form check. Nothing leaves the computer; the page is closed below.
    if let Ok(n) = notes.as_mut() {
        match ns_adapters::click_choice(&gw.bridge, &ns_elements::DECLARATION) {
            Ok(()) => n.push("Verklaring aangevinkt (test, niet verzonden) / Declaration ticked (test, not sent)".into()),
            Err(e) => n.push(format!("verklaring niet aan te vinken / declaration could not be ticked: {e:#}")),
        }
        // The same Verzenden check as a real submission (unique, enabled, part of the declaration's form); never clicked.
        let button = ns_elements::resolve(&gw.bridge, &ns_elements::SUBMIT, None);
        let declaration = ns_elements::resolve(&gw.bridge, &ns_elements::DECLARATION, None);
        let own = match (&button, &declaration) {
            (Ok(btn), Ok(d)) => gw
                .bridge
                .eval(&format!("(window.__ovr ? window.__ovr.submitOk({}, {}) : false)", btn.expr, d.expr))
                .ok()
                .and_then(|v| v.as_bool())
                == Some(true)
                // The click would land on the button itself (same hit test as the real click; not clicked).
                && gw
                    .bridge
                    .eval(&format!(
                        "(() => {{ const e = {}; e.scrollIntoView({{block:'center'}}); const b = e.getBoundingClientRect(); const x = b.x + b.width/2, y = b.y + b.height/2; let hit = document.elementFromPoint(x, y); while (hit?.shadowRoot) {{ const inner = hit.shadowRoot.elementFromPoint(x, y); if (!inner || inner === hit) break; hit = inner; }} for (let n = hit; n; n = n.assignedSlot || n.parentNode || n.host) if (n === e) return true; return false; }})()",
                        btn.expr
                    ))
                    .ok()
                    .and_then(|v| v.as_bool())
                    == Some(true),
            _ => false,
        };
        n.push(if own {
            "Knop Verzenden gevonden en bruikbaar (niet ingedrukt) / Verzenden button found and usable (not pressed)".into()
        } else {
            let why = match (&button, &declaration) {
                (Err(e), _) => format!(
                    "button: {e:#}; {}",
                    gw.bridge
                        .eval("(() => { const out = []; const walk = r => { for (const e of r.querySelectorAll('*')) { if (e.matches('button[type=submit]')) { const b = e.getBoundingClientRect(); let f = null; for (let n = e; n; n = n.parentNode || n.host) if (n.tagName === 'FORM') { f = n; break; } out.push({w: Math.round(b.width), h: Math.round(b.height), vis: e.checkVisibility ? e.checkVisibility() : null, text: (e.innerText || '').trim().slice(0, 20), form: f ? (f.id || f.getAttribute('name') || 'form') : null, hidden: !!e.closest('[hidden],[aria-hidden=true]'), shadow: e.getRootNode() !== document}); } if (e.shadowRoot) walk(e.shadowRoot); } }; walk(document); return JSON.stringify(out); })()")
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                ),
                (_, Err(e)) => format!("declaration: {e:#}"),
                (Ok(btn), Ok(d)) => gw
                    .bridge
                    .eval(&format!(
                        "(() => {{ const b = {}, d = {}; const up = n => n.parentNode || n.host; const formOf = e => {{ if (e && e.form) return e.form; for (let n = e; n; n = up(n)) if (n.tagName === 'FORM') return n; return null; }}; const fb = formOf(b), fd = formOf(d); return JSON.stringify({{tag: b.tagName, type: b.type, disabled: !!b.disabled, aria: b.getAttribute('aria-disabled'), buttonForm: !!fb, declForm: !!fd, same: fb === fd, submits: document.querySelectorAll('button[type=submit], input[type=submit]').length, submitOk: window.__ovr.submitOk(b, d), ancDisabled: window.__ovr.disabled(b), hit: (() => {{ const r = b.getBoundingClientRect(); const x = r.x + r.width/2, y = r.y + r.height/2; let h = document.elementFromPoint(x, y); while (h?.shadowRoot) {{ const i = h.shadowRoot.elementFromPoint(x, y); if (!i || i === h) break; h = i; }} return h ? h.tagName + '.' + String(h.className).slice(0, 30) : null; }})()}}); }})()",
                        btn.expr, d.expr
                    ))
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
            };
            format!("knop Verzenden ontbreekt of hoort niet bij het formulier / Verzenden button missing or not part of the form ({why})")
        });
        // The same last reread as a real submission: the reviewed values (route, card times, delay band,
        // declaration, name and IBAN) are what the form now shows.
        let reread = match (&gw.bank, &declaration) {
            (Some(bank), Ok(d)) => {
                ns_adapters::current_claim_route(&gw.bridge, &facts).unwrap_or(false)
                    && ns_adapters::final_form_values(&gw.bridge, &facts, bank, d).unwrap_or(false)
            }
            _ => false,
        };
        n.push(if reread {
            "Ingevulde waarden opnieuw gelezen en gelijk / Filled-in values reread and matching".into()
        } else {
            "ingevulde waarden wijken af of zijn niet te lezen / filled-in values differ or cannot be read".into()
        });
        let problems = ns_adapters::form_problems(&ns_adapters::form_check(&gw.bridge));
        n.push(if problems.is_empty() {
            "Formuliercontrole vóór Verzenden: geen problemen / Pre-submit form check: no problems".into()
        } else {
            format!("Formuliercontrole vóór Verzenden / Pre-submit form check: {problems:?}")
        });
    }
    // Screenshot of the filled form for the user to check by hand (their own data; saved locally only).
    let shot = (|| -> anyhow::Result<std::path::PathBuf> {
        anyhow::ensure!(screenshot, "skipped (--no-screenshot)");
        let r = gw.bridge.cdp(
            "Page.captureScreenshot",
            serde_json::json!({"format": "png", "captureBeyondViewport": true, "fromSurface": true}),
            std::time::Duration::from_secs(30),
        )?;
        let data = r["data"].as_str().ok_or_else(|| anyhow::anyhow!("no screenshot data"))?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
        let dir = dirs::download_dir().unwrap_or_else(paths::data_dir);
        let p = dir.join(format!("ovrefund-formtest-{}.png", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        std::fs::write(&p, bytes)?;
        Ok(p)
    })();
    // Leave the form WITHOUT pressing Verzenden.
    let _ = gw.bridge.navigate("about:blank");
    let failed = match &notes {
        Ok(n) => {
            n.iter().for_each(|x| println!("  ok: {}", ovrefund_core::redact::redact(x)));
            // The declaration and the pre-submit check must both pass for the route to be usable.
            n.iter().any(|x| {
                x.starts_with("verklaring niet aan te vinken")
                    || x.starts_with("Formuliercontrole vóór Verzenden /")
                    || x.starts_with("knop Verzenden ontbreekt")
                    || x.starts_with("ingevulde waarden wijken af")
            })
        }
        Err(e) => {
            println!("  form fill error: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
            true
        }
    };
    match shot {
        Ok(p) => {
            println!("screenshot: {}", p.display());
            let _ = std::process::Command::new("explorer.exe").arg(&p).spawn();
        }
        Err(e) if screenshot => println!("screenshot failed: {e:#}"),
        Err(_) => {}
    }
    anyhow::ensure!(!failed, "the claim form route does not work (details above); nothing was sent");
    println!("form test passed: the claim form can be filled up to Verzenden (nothing was sent)");
    Ok(())
}

/// Devtools `history-shot <journey id|auto> <out.png>`: the travel proof for a claim, a cropped PNG of
/// the trip's row in the Mijn NS travel history (any carrier, Arriva included), using the saved login.
/// `auto` takes the most recent trip with a check-out. Prints only the journey id and the file size.
/// With `--strict` it behaves like an Arriva claim: the day filter and the row check must succeed.
fn history_shot_run(app: &tauri::AppHandle, which: &str, out: &str, strict: bool) -> anyhow::Result<()> {
    let db = runner::open_db()?;
    let jid: i64 = match which.parse() {
        Ok(id) => id,
        Err(_) => db
            .conn
            .query_row("SELECT id FROM journeys WHERE check_out_utc IS NOT NULL ORDER BY check_in_utc DESC LIMIT 1", [], |r| r.get(0))
            .map_err(|_| anyhow::anyhow!("no trip to capture (run a check first)"))?,
    };
    let j = db.journey(jid)?;
    let b = ns_bridge::NsBridge::open(app, false)?;
    let r = ns_adapters::capture_history_trip(&b, &j, strict);
    let _ = b.navigate("about:blank");
    let shot = r?;
    // Caption strip under the unedited screenshot: full card number (Credential Manager), date, route.
    let card_number = j.card_last4.as_deref().and_then(|l| secrets::card_number(&db, j.coupling_id.as_deref(), l));
    let local = j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam);
    let caption = proof_cards::Caption {
        card_number,
        date: local.format("%d-%m-%Y").to_string(),
        route: format!("{} \u{2013} {}", j.origin.name, j.destination.as_ref().map(|d| d.name.as_str()).unwrap_or("?")),
        demo: false,
    };
    let png = arriva::travel_proof_png(app, &shot.png, &caption)?;
    std::fs::write(out, &png)?;
    println!(
        "history shot saved: journey {jid}, {} KB (screenshot {} KB + caption strip; card number {}; list: {}; capture: {})",
        png.len().div_ceil(1024),
        shot.png.len().div_ceil(1024),
        if caption.card_number.is_some() { "included" } else { "not stored" },
        shot.filter,
        shot.capture
    );
    Ok(())
}

/// Devtools `delay-shot <journey id> <out.png>`: the Rijden de Treinen open-data delay card for any
/// matched journey (NS trains too), from the app's own train match (planned route, plus the taken route
/// when it differs). The month files are downloaded now; their download time is stored like the
/// scheduler does and printed on the card. Prints only the journey id and the file size.
fn delay_shot_run(app: &tauri::AppHandle, which: &str, out: &str) -> anyhow::Result<()> {
    let db = runner::open_db()?;
    let jid: i64 = which.parse().map_err(|_| anyhow::anyhow!("usage: delay-shot <journey id> <out.png>"))?;
    let j = db.journey(jid)?;
    let ev =
        db.cached_evidence(&j.fallback_key())?.ok_or_else(|| anyhow::anyhow!("no train match for this journey yet (run a check first)"))?;
    let mut legs: Vec<ovrefund_core::model::Leg> = ev.planned.map(|r| r.legs).unwrap_or_default();
    for l in ev.taken.map(|r| r.legs).unwrap_or_default() {
        if !legs.iter().any(|p| p.train_number == l.train_number && p.dep_planned == l.dep_planned) {
            legs.push(l);
        }
    }
    let (png, months) = arriva::delay_shot_run(app, &legs)?;
    for (month, at) in &months {
        db.set_setting(&ovrefund_core::archive::retrieved_key(month), &at.to_rfc3339())?;
    }
    std::fs::write(out, &png)?;
    println!("delay shot saved: journey {jid}, {} KB", png.len().div_ceil(1024));
    Ok(())
}

/// Sends a short maintainer report through the same relay as the in-app bug form, from the app's own
/// browser (the relay only accepts browser requests). The message is sent as given: callers must keep
/// personal data out of it (tools/canary/dry-run.ps1 only sends step names and error texts).
fn relay_report_run(app: &tauri::AppHandle, subject: &str, file: &str) -> anyhow::Result<()> {
    let key = commands::BUG_FORM_KEY.trim();
    anyhow::ensure!(!key.is_empty(), "this build has no relay key");
    let message = std::fs::read_to_string(file)?;
    let body = serde_json::json!({
        "access_key": key,
        "subject": subject.chars().take(150).collect::<String>(),
        "from_name": "OV Refund daily check",
        "message": ovrefund_core::redact::redact(&message.chars().take(4000).collect::<String>()),
    });
    let b = ns_bridge::NsBridge::open(app, false)?;
    let js = format!(
        "fetch('https://api.web3forms.com/submit', {{method: 'POST', headers: {{'Content-Type': 'application/json', 'Accept': 'application/json'}}, body: JSON.stringify({body})}}).then(async r => r.status + ' ' + (await r.text()).slice(0, 200)).catch(e => 'error ' + e)"
    );
    let r = b.eval(&js)?;
    let out = r.as_str().unwrap_or("").to_string();
    let _ = b.window.close();
    anyhow::ensure!(out.starts_with("200"), "relay answered: {}", out.replace(key, "<key>"));
    println!("report sent");
    Ok(())
}

/// Diagnostic: open a Mijn NS page in the hidden window and list the gateway data it loads (masked).
fn probe_run(app: &tauri::AppHandle, route: &str) -> anyhow::Result<()> {
    anyhow::ensure!(route.starts_with('#') && !route.contains(".."), "route must look like #/betaaloverzicht");
    ns_bridge::CAPTURE_ALL_GATEWAY.store(true, std::sync::atomic::Ordering::Relaxed);
    let b = ns_bridge::NsBridge::open(app, false)?;
    let st = b.ensure_session(std::time::Duration::from_secs(60))?;
    println!("session: {st:?}");
    let mark = b.mark();
    b.navigate(&format!("https://www.ns.nl/mijnns{route}"))?;
    b.reload()?;
    std::thread::sleep(std::time::Duration::from_secs(15));
    let text = b.eval("document.querySelector('main') ? document.querySelector('main').innerText.slice(0, 1500) : ''")?;
    println!("--- page text (redacted) ---\n{}", ovrefund_core::redact::redact(text.as_str().unwrap_or("")));
    for c in b.captured_since(mark) {
        let path = c.url.split('?').next().unwrap_or("").replace("https://gateway.apiportal.ns.nl", "");
        let path = regex_lite_mask(&path);
        let q: Vec<&str> =
            c.url.split('?').nth(1).map(|q| q.split('&').map(|kv| kv.split('=').next().unwrap_or("")).collect()).unwrap_or_default();
        println!("\n=== {} {} {} ?{:?}", c.method, c.status, path, q);
        if let Some(body) = &c.body {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                println!("{}", serde_json::to_string_pretty(&shape(&v, "", 0))?.chars().take(4000).collect::<String>());
            }
        }
    }
    Ok(())
}

fn regex_lite_mask(s: &str) -> String {
    ovrefund_core::redact::redact(s)
}

/// Diagnostic: restore the saved NS session in a fresh hidden webview and report whether NS accepts it.
fn session_test_run(app: &tauri::AppHandle) -> anyhow::Result<()> {
    let b = ns_bridge::NsBridge::open(app, false)?;
    let r = b.cdp("Network.getAllCookies", serde_json::json!({}), std::time::Duration::from_secs(10))?;
    let names: Vec<String> = r["cookies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|c| c["domain"].as_str().unwrap_or("").ends_with("ns.nl"))
        .map(|c| {
            format!(
                "{}@{}{}",
                c["name"].as_str().unwrap_or(""),
                c["domain"].as_str().unwrap_or(""),
                if c["session"].as_bool().unwrap_or(false) { "*" } else { "" }
            )
        })
        .collect();
    println!("cookies in jar after restore ({}): {}", names.len(), names.join(" "));
    let st = b.ensure_session(std::time::Duration::from_secs(60))?;
    let ps = b.page_state()?;
    println!("session: {st:?}; final host {}", ps.host);
    let page = b.eval("({url: location.origin + location.pathname, text: document.body ? document.body.innerText.slice(0, 500) : '', ls: Object.keys(localStorage), ss: Object.keys(sessionStorage)})")?;
    println!("{}", ovrefund_core::redact::redact(&serde_json::to_string_pretty(&page)?));
    Ok(())
}

/// Developer builds only: `update-check` / `update-install` run the in-app updater commands
/// (`check_update` / `install_update`, same endpoint, signature check and run-lock guard)
/// without a window, for the installed-update CI test. Prints one JSON line; returns the exit code.
#[cfg(feature = "devtools")]
fn update_cli_run(app: &tauri::AppHandle, mode: &str) -> i32 {
    use serde_json::json;
    let (out, code) = if mode == "update-check" {
        match tauri::async_runtime::block_on(commands::check_update(app.clone())) {
            Ok(mut v) => {
                v["package_version"] = json!(app.package_info().version.to_string());
                (v, 0)
            }
            Err(msg) => (json!({ "error": "check", "message": msg }), 1),
        }
    } else {
        // On Windows a successful install starts the installer and exits this process (code 0)
        // inside the updater; it only returns here on failure (or on other platforms).
        match tauri::async_runtime::block_on(commands::install_update(app.clone())) {
            Ok(()) => (json!({ "installed": true }), 0),
            Err(msg) if msg.to_lowercase().contains("signature") => (json!({ "error": "signature", "message": msg }), 3),
            Err(msg) => (json!({ "error": "install", "message": msg }), 1),
        }
    };
    // A closed stdout must not panic: the process would then never exit.
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{out}");
    code
}

#[cfg(not(feature = "devtools"))]
fn update_cli_run(_app: &tauri::AppHandle, _mode: &str) -> i32 {
    2 // unreachable: main() refuses developer commands in release builds
}

/// HH:MM in Europe/Amsterdam.
pub fn fmt_local(t: chrono::DateTime<chrono::Utc>) -> String {
    t.with_timezone(&chrono_tz::Europe::Amsterdam).format("%H:%M").to_string()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Demo sandbox: separate data, credentials and a simulated NS (see demo.rs).
    if let Some(i) = args.iter().position(|a| a == "--demo") {
        args.remove(i);
        if !cfg!(feature = "devtools") {
            attach_console();
            eprintln!("--demo is only available in developer builds (cargo build --features devtools)");
            std::process::exit(2);
        }
        demo::enable();
    }
    init_log();
    let first = args.first().map(|s| s.as_str());
    match first {
        Some("--help") | Some("-h") | Some("help") => {
            attach_console();
            print!("{HELP}");
            return;
        }
        Some("history")
        | Some("diagnose")
        | Some("install-schedule")
        | Some("remove-schedule")
        | Some("record-external")
        | Some("forget-all")
        | Some("forget-login") => {
            attach_console();
            let r = match first.unwrap() {
                "history" => cli_history(),
                "diagnose" => cli_diagnose(),
                "record-external" => (|| -> anyhow::Result<()> {
                    let jid: i64 = args
                        .get(1)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| anyhow::anyhow!("usage: record-external <journey_id> [euros]"))?;
                    let cents = args.get(2).map(|s| commands::cli_amount_cents(s)).transpose()?;
                    let mut db = runner::open_db()?;
                    runner::recover_stale_lock(&db)?;
                    let owner = format!("pid{}-cli-external", std::process::id());
                    anyhow::ensure!(
                        db.acquire_run_lock(&owner, chrono::Duration::minutes(5))?,
                        "Er loopt al een controle. / A check is already running."
                    );
                    let result = (|| -> anyhow::Result<()> {
                        let c = db.ensure_claim(jid, "external", ovrefund_core::model::ClaimState::Discovered)?;
                        if c.status != ovrefund_core::model::ClaimState::ExternallyClaimed {
                            db.mark_external(c.id, "user")?;
                        }
                        db.set_paid_amount(c.id, cents)?;
                        println!("journey {jid}: claimed elsewhere, received {:?} cents", cents);
                        Ok(())
                    })();
                    let _ = db.release_run_lock(&owner);
                    result
                })(),
                "forget-all" => forget_all(),
                "install-schedule" => (|| -> anyhow::Result<()> {
                    let days = args.get(1).and_then(|d| d.parse().ok()).unwrap_or(7);
                    let _lease = commands::LedgerLock::take_with(5, runner::RUN_BUSY_MSG)?;
                    let db = runner::open_db()?;
                    let message = scheduler::install(days)?;
                    commands::save_schedule_intent(&db, true, days)?;
                    println!("installed: {message}");
                    Ok(())
                })(),
                "remove-schedule" => scheduler::remove().map(|_| {
                    if let Ok(db) = runner::open_db() {
                        let _ = db.set_setting("schedule_enabled", "0");
                    }
                    println!("removed")
                }),
                "forget-login" => cli_forget_login(),
                _ => unreachable!(),
            };
            if let Err(e) = r {
                eprintln!("error: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }
    // Developer diagnostics (session-test, probe, form-test, ns-eval, update-check, update-install) exist only in builds with the
    // `devtools` feature; release/installer builds refuse them.
    let devtool = matches!(
        first,
        Some("session-test")
            | Some("probe")
            | Some("form-test")
            | Some("ns-eval")
            | Some("update-check")
            | Some("update-install")
            | Some("relay-report")
            | Some("arriva-form-test")
            | Some("arriva-eval")
            | Some("arriva-view-test")
            | Some("history-shot")
            | Some("delay-shot")
    );
    if devtool && !cfg!(feature = "devtools") {
        attach_console();
        eprintln!("this diagnostic is only available in developer builds (cargo build --features devtools)");
        std::process::exit(2);
    }
    let headless = matches!(first, Some("--run-once") | Some("scan")) || devtool;
    let ns_eval_expr: Option<String> = if first == Some("ns-eval") { args.get(1).cloned() } else { None };
    // `form-test <journey id>`, or `form-test auto` (also without an id): the most recent trip NS still accepts a claim for.
    let form_test_journey: Option<i64> =
        if first == Some("form-test") { Some(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0)) } else { None };
    let form_test_shot = !args.iter().any(|a| a == "--no-screenshot");
    let probe_route = if first == Some("probe") { args.get(1).cloned() } else { None };
    // `relay-report <subject> <message file>`: maintainer dry-run report to the project mailbox.
    let relay_report: Option<(String, String)> =
        if first == Some("relay-report") { args.get(1).cloned().zip(args.get(2).cloned()) } else { None };
    let session_test = first == Some("session-test");
    // `arriva-form-test`: fill Arriva's form with synthetic demo data up to (not including) Indienen.
    let arriva_form_test = first == Some("arriva-form-test");
    // `arriva-eval <url|filled> <js file> [png] [--show]`: page-script diagnostic in a guarded window.
    let arriva_eval: Option<Vec<String>> =
        if first == Some("arriva-eval") { Some(args.iter().skip(1).filter(|a| *a != "--show").cloned().collect()) } else { None };
    let arriva_eval_show = args.iter().any(|a| a == "--show");
    // `arriva-view-test <dir>`: screenshots of the CAPTCHA-only provider window and the attachments.
    let arriva_view_test: Option<String> = if first == Some("arriva-view-test") { args.get(1).cloned() } else { None };
    // `history-shot <journey id|auto> <out.png>`: cropped travel-history row of one trip.
    let history_shot: Option<(String, String)> =
        if first == Some("history-shot") { args.get(1).cloned().zip(args.get(2).cloned()) } else { None };
    // `delay-shot <journey id> <out.png>`: RDT open-data delay card of one matched journey.
    let delay_shot: Option<(String, String)> =
        if first == Some("delay-shot") { args.get(1).cloned().zip(args.get(2).cloned()) } else { None };
    let update_cli: Option<String> = first.filter(|f| matches!(*f, "update-check" | "update-install")).map(str::to_string);
    let scheduled = first == Some("--run-once");
    // `scan` is a diagnostic: always its own trigger, which never submits (see runner::SCAN_TRIGGER).
    let trigger = if first == Some("scan") {
        runner::SCAN_TRIGGER.to_string()
    } else {
        args.iter().position(|a| a == "--trigger").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| "cli".into())
    };
    if headless {
        attach_console();
    }
    // Scheduled task fires at logon and on the interval: skip if a background check completed
    // in the last 12 h (see runner::skip_scheduled_run; never the extra check before a deadline).
    if scheduled {
        if let Ok(db) = runner::open_db() {
            if let Some(last) = runner::skip_scheduled_run(&db, &trigger, chrono::Utc::now(), runner::auto_allowed_now(&db)) {
                println!("skipped: last check {} (< 12 h ago)", last.to_rfc3339());
                return;
            }
        }
    }

    let mut builder = tauri::Builder::default();
    // One window per Windows user: a second GUI launch focuses the open window and exits. Only
    // GUI launches register (headless --run-once / scan / CLI paths never do), so a scheduled
    // check always runs while the app is open, and the app opens while a check runs (the run
    // lock keeps them apart). The demo is a separate sandbox and may run next to the real app.
    if !headless && !demo::is_demo() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }));
    }
    let app = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            arriva::arriva_demo_state,
            arriva::arriva_demo_enable,
            arriva::arriva_demo_prepare,
            arriva::arriva_demo_open,
            arriva::arriva_demo_probe,
            arriva::arriva_demo_reset,
            arriva::arriva_demo_attachment,
            arriva::arriva_demo_attachment_data,
            arriva_claim::arriva_state,
            arriva_claim::arriva_contact,
            arriva_claim::set_arriva_contact,
            arriva_claim::clear_arriva_contact,
            arriva_claim::arriva_claim,
            arriva_claim::arriva_claim_preview,
            arriva_claim::arriva_claim_cancel,
            arriva_claim::arriva_quit_anyway,
            arriva_claim::arriva_claim_show,
            arriva_claim::arriva_claim_attachments,
            arriva_claim::arriva_claim_attachment_data,
            arriva_claim::arriva_claim_attachment,
            commands::overview,
            commands::journeys,
            commands::claims,
            commands::scan_now,
            commands::stop_auto_run,
            commands::open_login,
            commands::check_session,
            commands::claim_facts,
            commands::preflight_claim,
            commands::submit_claim,
            commands::set_claim_status,
            commands::mark_external,
            commands::add_external_claim,
            commands::external_claims,
            commands::delete_external_claim,
            commands::get_settings,
            commands::save_settings,
            commands::set_bank_details,
            commands::clear_bank_details,
            commands::set_language,
            commands::test_notification,
            commands::test_email,
            commands::disconnect,
            commands::export_diagnostics,
            commands::open_diagnostics_folder,
            commands::wipe_history,
            commands::set_paid_amount,
            commands::set_schedule,
            commands::startup_check,
            commands::set_auto_login,
            commands::set_ns_login,
            commands::reveal,
            commands::set_read_ns_mail,
            commands::sync_ns_mail,
            commands::open_external,
            commands::set_flag,
            commands::set_commission,
            commands::bug_report_context,
            commands::demo_set_subscription,
            commands::demo_auto_run,
            commands::undo_external,
            commands::forget_everything,
            commands::check_update,
            commands::install_update,
            commands::export_backup,
            commands::import_backup,
            commands::export_claims_csv,
            commands::open_data_folder,
            commands::redact_text,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            if headless {
                std::thread::spawn(move || {
                    if let Some(mode) = update_cli.as_deref() {
                        let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| update_cli_run(&handle, mode))).unwrap_or(1);
                        handle.exit(code);
                        return;
                    }
                    if let Some(expr) = ns_eval_expr.clone() {
                        let r = (|| -> anyhow::Result<()> {
                            let b = ns_bridge::NsBridge::open(&handle, false)?;
                            println!("session: {:?}", b.ensure_session(std::time::Duration::from_secs(60))?);
                            b.navigate("https://www.ns.nl/mijnns#/reishistorie")?;
                            b.reload()?;
                            std::thread::sleep(std::time::Duration::from_secs(10));
                            let v = b.eval(&expr)?;
                            println!("{}", ovrefund_core::redact::redact(&serde_json::to_string_pretty(&v)?));
                            Ok(())
                        })();
                        if let Err(e) = &r {
                            eprintln!("ns-eval failed: {e:#}");
                        }
                        handle.exit(if r.is_ok() { 0 } else { 1 });
                        return;
                    }
                    if let Some(jid) = form_test_journey {
                        let code = match form_test_run(&handle, jid, form_test_shot) {
                            Ok(()) => 0,
                            Err(e) => {
                                eprintln!("form-test failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                                1
                            }
                        };
                        handle.exit(code);
                        return;
                    }
                    if let Some(a) = arriva_eval.clone() {
                        let r = match (a.first(), a.get(1)) {
                            (Some(url), Some(js)) => arriva::eval_run(&handle, url, js, a.get(2).map(|s| s.as_str()), arriva_eval_show),
                            _ => Err(anyhow::anyhow!("usage: arriva-eval <url|filled> <js file> [png] [--show]")),
                        };
                        if let Err(e) = &r {
                            eprintln!("arriva-eval failed: {e:#}");
                        }
                        handle.exit(if r.is_ok() { 0 } else { 1 });
                        return;
                    }
                    if let Some((which, out)) = history_shot.clone() {
                        let code = match history_shot_run(&handle, &which, &out, std::env::args().any(|a| a == "--strict")) {
                            Ok(()) => 0,
                            Err(e) => {
                                eprintln!("history-shot failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                                1
                            }
                        };
                        handle.exit(code);
                        return;
                    }
                    if let Some((which, out)) = delay_shot.clone() {
                        let code = match delay_shot_run(&handle, &which, &out) {
                            Ok(()) => 0,
                            Err(e) => {
                                eprintln!("delay-shot failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                                1
                            }
                        };
                        handle.exit(code);
                        return;
                    }
                    if let Some(dir) = arriva_view_test.clone() {
                        let r = arriva::view_test_run(&handle, std::path::Path::new(&dir));
                        if let Err(e) = &r {
                            eprintln!("arriva-view-test failed: {e:#}");
                        }
                        handle.exit(if r.is_ok() { 0 } else { 1 });
                        return;
                    }
                    if arriva_form_test {
                        let code = match arriva::form_test_run(&handle) {
                            Ok(summary) => {
                                println!("{summary}");
                                0
                            }
                            Err(e) => {
                                eprintln!("arriva-form-test failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                                1
                            }
                        };
                        handle.exit(code);
                        return;
                    }
                    if let Some((subject, file)) = relay_report.clone() {
                        let code = match relay_report_run(&handle, &subject, &file) {
                            Ok(()) => 0,
                            Err(e) => {
                                eprintln!("relay-report failed: {e:#}");
                                1
                            }
                        };
                        handle.exit(code);
                        return;
                    }
                    if let Some(route) = probe_route.clone() {
                        let code = match probe_run(&handle, &route) {
                            Ok(()) => 0,
                            Err(e) => {
                                eprintln!("probe failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                                1
                            }
                        };
                        handle.exit(code);
                        return;
                    }
                    if session_test {
                        let code = match session_test_run(&handle) {
                            Ok(()) => 0,
                            Err(e) => {
                                eprintln!("session-test failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
                                1
                            }
                        };
                        handle.exit(code);
                        return;
                    }
                    // A panic must never leave a scheduled process hanging (the event loop keeps
                    // running until exit is called): treat it as a failed run and exit.
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runner::run(&handle, &trigger, scheduled, false)))
                            .unwrap_or_else(|p| {
                                let msg = p
                                    .downcast_ref::<&str>()
                                    .map(|s| s.to_string())
                                    .or_else(|| p.downcast_ref::<String>().cloned())
                                    .unwrap_or_else(|| "unknown panic".into());
                                Err(anyhow::anyhow!("internal error (panic): {msg}"))
                            });
                    let code = match outcome {
                        Ok(s) => {
                            println!(
                                "run #{}: {} journeys ({} new), {} claimable, {} review, {} submitted, {} pending timetable, session {}",
                                s.run_id,
                                s.journeys_seen,
                                s.journeys_new,
                                s.candidates,
                                s.needs_review,
                                s.submitted,
                                s.pending_evidence,
                                s.auth_state
                            );
                            for e in &s.errors {
                                println!("  problem: {e}");
                            }
                            log::info!("run finished: {} error(s)", s.errors.len());
                            if s.errors.is_empty() {
                                0
                            } else {
                                2
                            }
                        }
                        Err(_) => {
                            eprintln!("Controle mislukt / Check failed: pipeline error");
                            log::error!("run failed: pipeline error");
                            1
                        }
                    };
                    // let toast/email flush
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    handle.exit(code);
                });
            } else {
                if let Err(e) = commands::recover_claims_on_start() {
                    log::warn!("claim recovery failed: {}", ovrefund_core::redact::redact(&e.to_string()));
                }
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title(if demo::is_demo() { "OV Refund — DEMO" } else { "OV Refund" })
                    .inner_size(1180.0, 800.0)
                    .min_inner_size(820.0, 560.0)
                    .additional_browser_args(ns_bridge::BROWSER_ARGS)
                    .build()?;
                // Developer screenshots of the GUI (devtools builds only): see arriva::ui_script_run.
                if cfg!(feature = "devtools") {
                    if let (Ok(script), Ok(out)) = (std::env::var("OVREFUND_UI_SCRIPT"), std::env::var("OVREFUND_UI_SHOTS")) {
                        let ui = handle.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_secs(4));
                            if let Err(e) = arriva::ui_script_run(&ui, &script, std::path::Path::new(&out)) {
                                println!("ui script failed: {e:#}");
                            }
                            if std::env::var("OVREFUND_UI_EXIT").as_deref() == Ok("1") {
                                ui.exit(0);
                            }
                        });
                    }
                }
                // Repair the scheduled task if it went missing (e.g. removed by an upgrade's
                // uninstall step) while the user still wants automatic checks.
                let erase_app = handle.clone();
                std::thread::spawn(move || {
                    // Pre-0.1.2 shared task name -> per-Windows-user name (only this user's task).
                    scheduler::migrate_legacy();
                    let Ok(db) = runner::open_db() else { return };
                    // A logout or "forget everything" that waited for a check when the app closed.
                    if commands::pending_erase(&db).is_some() {
                        commands::resume_pending_erase(erase_app);
                    }
                    if runner::setting(&db, "schedule_enabled", "0") == "1" && !scheduler::status().installed {
                        let days = runner::setting(&db, "schedule_days", &runner::setting(&db, "interval_days", "7")).parse().unwrap_or(7);
                        match scheduler::install(days) {
                            Ok(_) => log::info!("scheduled task restored ({days} days)"),
                            Err(e) => log::warn!("could not restore scheduled task: {e:#}"),
                        }
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the NS window only hides it, so the session (and bridge) stays usable.
            if window.label() == ns_bridge::NS_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    let app = window.app_handle().clone();
                    std::thread::spawn(move || {
                        use tauri::Emitter;
                        let _ = app.emit("login-window-closed", ());
                    });
                }
            } else if window.label() == "main" {
                // An Arriva claim waiting for its outcome (e.g. a CAPTCHA puzzle): closing would end it
                // halfway. The app asks first (see arriva_claim::arriva_quit_anyway).
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if arriva_claim::running() && !arriva_claim::quit_confirmed() {
                        api.prevent_close();
                        use tauri::Emitter;
                        let _ = window.emit("close-blocked", ());
                    }
                }
                if let tauri::WindowEvent::Destroyed = event {
                    arriva_claim::cancel_open_previews();
                    window.app_handle().exit(0);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building OV Refund");

    app.run(move |_app, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            // Keep running in headless mode until the worker calls exit(code).
            if code.is_none() && headless {
                api.prevent_exit();
            }
        }
    });
}

#[cfg(test)]
mod cleanup_tests {
    #[test]
    fn cleanup_continues_after_errors_and_lists_each_failure() {
        let mut errors = super::CleanupErrors::default();
        let mut completed = Vec::new();
        errors.attempt("credentials", || {
            completed.push("credentials");
            Err::<(), _>(anyhow::anyhow!("Prive-inhoud / private payload"))
        });
        errors.attempt("session", || {
            completed.push("session");
            Ok(())
        });
        errors.attempt("data", || {
            completed.push("data");
            Err::<(), _>(anyhow::anyhow!("Prive-pad / private path"))
        });
        assert_eq!(completed, ["credentials", "session", "data"]);
        let message = errors.finish().unwrap_err().to_string();
        assert!(message.contains("credentials, data") && !message.contains("private"));
        assert!(super::CleanupErrors::default().finish().is_ok());
    }
}
