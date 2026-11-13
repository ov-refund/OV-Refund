// No console window when double-clicked (release). CLI subcommands attach to the parent console.
#![windows_subsystem = "windows"]
#![recursion_limit = "256"] // the large json!() of the overview command

mod commands;
mod demo;
#[cfg(test)]
mod message_lint;
mod ns_adapters;
mod ns_bridge;
mod paths;
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
        if let Ok(mut st) = db.conn.prepare("SELECT DISTINCT card_last4 FROM journeys WHERE card_last4 IS NOT NULL") {
            if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) {
                keys.extend(rows.flatten().map(|l4| format!("ov-card-{l4}")));
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
fn form_test_run(app: &tauri::AppHandle, jid: i64) -> anyhow::Result<()> {
    let db = runner::open_db()?;
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
    let notes = gw.fill_form(&facts);
    // Screenshot of the filled form for the user to check by hand (their own data; saved locally only).
    let shot = (|| -> anyhow::Result<std::path::PathBuf> {
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
    match notes {
        Ok(n) => n.iter().for_each(|x| println!("  ok: {}", ovrefund_core::redact::redact(x))),
        Err(e) => println!("  form fill error: {}", ovrefund_core::redact::redact(&format!("{e:#}"))),
    }
    match shot {
        Ok(p) => {
            println!("screenshot: {}", p.display());
            let _ = std::process::Command::new("explorer.exe").arg(&p).spawn();
        }
        Err(e) => println!("screenshot failed: {e:#}"),
    }
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
                "install-schedule" => {
                    scheduler::install(args.get(1).and_then(|d| d.parse().ok()).unwrap_or(7)).map(|m| println!("installed: {m}"))
                }
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
        Some("session-test") | Some("probe") | Some("form-test") | Some("ns-eval") | Some("update-check") | Some("update-install")
    );
    if devtool && !cfg!(feature = "devtools") {
        attach_console();
        eprintln!("this diagnostic is only available in developer builds (cargo build --features devtools)");
        std::process::exit(2);
    }
    let headless = matches!(first, Some("--run-once") | Some("scan")) || devtool;
    let ns_eval_expr: Option<String> = if first == Some("ns-eval") { args.get(1).cloned() } else { None };
    let form_test_journey: Option<i64> = if first == Some("form-test") { args.get(1).and_then(|s| s.parse().ok()) } else { None };
    let probe_route = if first == Some("probe") { args.get(1).cloned() } else { None };
    let session_test = first == Some("session-test");
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
            commands::set_unverified_override,
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
                        let code = match form_test_run(&handle, jid) {
                            Ok(()) => 0,
                            Err(e) => {
                                eprintln!("form-test failed: {}", ovrefund_core::redact::redact(&format!("{e:#}")));
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
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title(if demo::is_demo() { "OV Refund — DEMO" } else { "OV Refund" })
                    .inner_size(1180.0, 800.0)
                    .min_inner_size(820.0, 560.0)
                    .additional_browser_args(ns_bridge::BROWSER_ARGS)
                    .build()?;
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
                if let tauri::WindowEvent::Destroyed = event {
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
