//! Per-user Windows Task Scheduler integration (no admin, no resident process).
//! `StartWhenAvailable` makes Windows run a missed check after reboot/logon.
//! `InteractiveToken`: runs only while the user is logged on (WebView2 needs a desktop session).
use anyhow::{bail, Result};
use std::process::Command;

/// Name used by versions up to 0.1.1: one name for every Windows user on the PC, so a second
/// user's task replaced the first one's. Migrated to [`task_name`] when it is ours.
pub const LEGACY_TASK_NAME: &str = "OVRefund-ScheduledCheck";

/// Per-Windows-user task name: "OVRefund-ScheduledCheck-<USERNAME>-<hash>", where <hash> is a
/// short hash of DOMAIN\USER (lower case), so two accounts with the same user name (e.g. a local
/// and a domain "jan") never share a task. The uninstaller does not build this name itself: it
/// runs `ov-refund.exe remove-schedule`, which uses this rule.
pub fn task_name() -> String {
    // A developer instance with its own data folder never shares the installed app's task.
    let suffix = crate::paths::isolated_suffix().unwrap_or_default();
    format!("{}{suffix}", task_name_for(&std::env::var("USERNAME").unwrap_or_default(), &current_user()))
}

pub fn task_name_for(user: &str, account: &str) -> String {
    let base = v012_task_name_for(user);
    if base == LEGACY_TASK_NAME {
        return base;
    }
    format!("{base}-{}", ovrefund_core::short_hash(&account.trim().to_lowercase()))
}

/// Name used by 0.1.2: "OVRefund-ScheduledCheck-<USERNAME>" (letters, digits, '-', '_' and '.'
/// kept; anything else becomes '_'). Migrated to [`task_name`] when it is ours.
pub fn v012_task_name() -> String {
    v012_task_name_for(&std::env::var("USERNAME").unwrap_or_default())
}

pub fn v012_task_name_for(user: &str) -> String {
    let clean: String = user.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    if clean.is_empty() {
        LEGACY_TASK_NAME.to_string()
    } else {
        format!("{LEGACY_TASK_NAME}-{clean}")
    }
}

/// Text shown when Windows refuses to create the task (no rights, or blocked by policy).
pub const POLICY_REFUSED: &str = "Windows staat niet toe dat OV Refund een geplande taak maakt, misschien door beleid van je werkgever. Automatisch controleren werkt dan niet. Controleer zelf met Controleer nu. / Windows does not allow OV Refund to create a scheduled task, possibly because of your employer's policy. Automatic checks won't work. Use Check now instead.";

/// schtasks output that means "not allowed" (access denied, group policy), in English or Dutch Windows.
pub fn is_refusal(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    [
        "access is denied",
        "toegang geweigerd",
        "0x80070005",
        "policy",
        "beleid",
        "not allowed",
        "niet toegestaan",
        "privilege",
        "bevoegdheid",
    ]
    .iter()
    .any(|p| e.contains(p))
}

fn cmd(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// "elke dag" / "elke 7 dagen".
pub fn every_nl(days: u32) -> String {
    if days == 1 {
        "elke dag".into()
    } else {
        format!("elke {days} dagen")
    }
}

/// "every day" / "every 7 days".
pub fn every_en(days: u32) -> String {
    if days == 1 {
        "every day".into()
    } else {
        format!("every {days} days")
    }
}

pub fn task_xml(exe: &str, every_days: u32, start: chrono::NaiveDateTime) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>OV Refund: controleert je NS-reishistorie {nl} op vertraging. Uitzetten kan in de app of door OV Refund te verwijderen. / OV Refund: checks your NS travel history for delays {en}. Turn it off in the app or by uninstalling OV Refund.</Description>
  </RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>{start}</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByDay><DaysInterval>{every_days}</DaysInterval></ScheduleByDay>
    </CalendarTrigger>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <Delay>PT2M</Delay>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>true</RunOnlyIfNetworkAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <ExecutionTimeLimit>PT2H</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>--run-once</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        start = start.format("%Y-%m-%dT%H:%M:%S"),
        nl = every_nl(every_days),
        en = every_en(every_days),
        user = xml_escape(&current_user()),
        exe = xml_escape(exe)
    )
}

fn current_user() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{user}"),
        _ => user,
    }
}

pub fn install(every_days: u32) -> Result<String> {
    if crate::demo::is_demo() {
        if let Ok(db) = crate::runner::open_db() {
            let _ = db.set_setting("demo_schedule", &every_days.to_string());
        }
        return Ok(format!(
            "DEMO: zou {} controleren; Windows Taakplanner wordt niet gewijzigd / DEMO: would check {}; Windows Task Scheduler is not changed",
            every_nl(every_days),
            every_en(every_days)
        ));
    }
    anyhow::ensure!((1..=31).contains(&every_days), "Kies een interval van 1 tot en met 31 dagen. / Choose an interval of 1 to 31 days.");
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    // First run: tomorrow 10:00 local; missed runs execute at next logon.
    let start = (chrono::Local::now() + chrono::Duration::days(1)).date_naive().and_hms_opt(10, 0, 0).unwrap();
    let xml = task_xml(&exe, every_days, start);
    create_task(&task_name(), &xml)?;
    // Older names (if they are ours) would run a second check.
    remove_old_if_ours();
    let first = start.format("%d-%m-%Y %H:%M");
    Ok(format!("{}; eerste controle {first} / {}; first check {first}", every_nl(every_days), every_en(every_days)))
}

/// 16 bytes from the system's cryptographic random generator, as hex.
fn random_hex() -> Result<String> {
    #[cfg(windows)]
    {
        use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
        let mut buf = [0u8; 16];
        // SAFETY: the buffer is a valid, writable slice; no algorithm handle with the system RNG flag.
        let status = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if status.is_ok() {
            return Ok(buf.iter().map(|b| format!("{b:02x}")).collect());
        }
    }
    bail!(SCHEDULE_FAILED_MSG)
}

const SCHEDULE_FAILED_MSG: &str = "De automatische controle kon niet worden ingepland. / The automatic check could not be scheduled.";

/// Create (or replace, `/F`) a task of this Windows user from its XML. The XML goes to a new
/// temporary file with an unpredictable name (created only if it does not exist yet, so no other
/// file or link can be put in its place) and is removed afterwards.
fn create_task(name: &str, xml: &str) -> Result<()> {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("ovrefund-task-{}.xml", random_hex()?));
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).and_then(|mut f| f.write_all(&bytes));
    if let Err(e) = written {
        log::warn!("task XML could not be written: {}", e.kind());
        let _ = std::fs::remove_file(&path);
        bail!(SCHEDULE_FAILED_MSG);
    }
    let out = cmd("schtasks").args(["/Create", "/TN", name, "/XML"]).arg(&path).arg("/F").output();
    invalidate();
    let _ = std::fs::remove_file(&path);
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            log::warn!("schtasks could not be started: {e}");
            bail!(POLICY_REFUSED)
        }
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if is_refusal(&err) {
            log::warn!("schtasks refused");
            bail!(POLICY_REFUSED);
        }
        bail!(
            "De automatische controle kon niet worden ingepland ({0}). / The automatic check could not be scheduled ({0}).",
            ovrefund_core::redact::redact(String::from_utf8_lossy(&out.stderr).trim())
        );
    }
    Ok(())
}

/// The one extra check before a claim deadline: a sibling of [`task_name`] with a single time
/// trigger. At most one exists (re-creating replaces it).
pub fn deadline_task_name() -> String {
    format!("{}-Deadline", task_name())
}

/// One-time extra check at `at` (local time), not started after `end`; Windows deletes the task
/// a day after it expired. Same check as the regular one, with its own trigger
/// (`--run-once --trigger deadline`), so the "checked less than 12 hours ago" skip never applies.
pub fn deadline_task_xml(exe: &str, at: chrono::NaiveDateTime, end: chrono::NaiveDateTime) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>OV Refund: één extra controle voordat de claimtermijn van een reis afloopt. / OV Refund: one extra check before the claim window of a trip ends.</Description>
  </RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <StartBoundary>{at}</StartBoundary>
      <EndBoundary>{end}</EndBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>true</RunOnlyIfNetworkAvailable>
    <DeleteExpiredTaskAfter>P1D</DeleteExpiredTaskAfter>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <ExecutionTimeLimit>PT2H</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>--run-once --trigger deadline</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        at = at.format("%Y-%m-%dT%H:%M:%S"),
        end = end.format("%Y-%m-%dT%H:%M:%S"),
        exe = xml_escape(exe)
    )
}

/// Plan the extra check at `at` (UTC) for a claim whose window ends at `deadline`; replaces an
/// earlier one. In the demo only a setting is written.
pub fn install_deadline(at: chrono::DateTime<chrono::Utc>, deadline: chrono::DateTime<chrono::Utc>) -> Result<()> {
    if crate::demo::is_demo() {
        if let Ok(db) = crate::runner::open_db() {
            let _ = db.set_setting("demo_deadline_run", &at.to_rfc3339());
        }
        return Ok(());
    }
    let local = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&chrono::Local).naive_local();
    let end = (at + chrono::Duration::days(3)).min(deadline);
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    create_task(&deadline_task_name(), &deadline_task_xml(&exe, local(at), local(end)))
}

/// Start time (local, as in the task) of the planned extra check, if there is one.
pub fn deadline_run() -> Option<String> {
    if crate::demo::is_demo() {
        return crate::runner::open_db().ok().and_then(|db| db.get_setting("demo_deadline_run").ok().flatten());
    }
    query_xml(&deadline_task_name()).and_then(|x| grab_tag(&x, "StartBoundary"))
}

/// Remove the extra check (if any). Also forgets which deadline was planned, so a new plan is
/// installed right away.
pub fn remove_deadline() -> Result<()> {
    if let Ok(db) = crate::runner::open_db() {
        let _ = db.conn.execute("DELETE FROM settings WHERE key=?1", [crate::runner::DEADLINE_PLANNED_KEY]);
    }
    if crate::demo::is_demo() {
        if let Ok(db) = crate::runner::open_db() {
            let _ = db.conn.execute("DELETE FROM settings WHERE key='demo_deadline_run'", []);
        }
        return Ok(());
    }
    let name = deadline_task_name();
    if query_xml(&name).is_none() {
        return Ok(());
    }
    let out = cmd("schtasks").args(["/Delete", "/TN", &name, "/F"]).output()?;
    invalidate();
    anyhow::ensure!(out.status.success(), "De extra controle kon niet worden verwijderd. / The extra check could not be removed.");
    Ok(())
}

pub fn remove() -> Result<()> {
    let extra = remove_deadline();
    if crate::demo::is_demo() {
        if let Ok(db) = crate::runner::open_db() {
            let _ = db.set_setting("demo_schedule", "0");
        }
        return Ok(());
    }
    remove_old_if_ours();
    if !status().installed {
        return extra;
    }
    let out = cmd("schtasks").args(["/Delete", "/TN", &task_name(), "/F"]).output()?;
    invalidate();
    if !out.status.success() {
        bail!(
            "De geplande taak kon niet worden verwijderd ({0}). / The scheduled task could not be removed ({0}).",
            ovrefund_core::redact::redact(String::from_utf8_lossy(&out.stderr).trim())
        );
    }
    extra
}

/// True when a task XML belongs to this Windows user's OV Refund (its logon trigger names this
/// user and it runs an OV Refund `--run-once`).
pub fn legacy_is_ours(xml: &str, user: &str) -> bool {
    let x = xml.to_lowercase();
    let u = xml_escape(user).to_lowercase();
    x.contains("--run-once") && x.contains("ov-refund") && !u.is_empty() && x.contains(&format!("<userid>{u}</userid>"))
}

fn query_xml(name: &str) -> Option<String> {
    let out = cmd("schtasks").args(["/Query", "/TN", name, "/XML"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// Older task names: the pre-0.1.2 shared one and the 0.1.2 per-user one (without hash).
fn old_task_names() -> Vec<String> {
    let mut v = vec![v012_task_name()];
    if !v.contains(&LEGACY_TASK_NAME.to_string()) {
        v.push(LEGACY_TASK_NAME.to_string());
    }
    v.retain(|n| *n != task_name());
    v
}

/// Interval (days) of an older task when it exists and this Windows user created it.
fn old_interval_if_ours(name: &str) -> Option<u32> {
    let xml = query_xml(name)?;
    legacy_is_ours(&xml, &current_user()).then(|| grab_tag(&xml, "DaysInterval").and_then(|d| d.parse().ok()).unwrap_or(7))
}

/// Delete the older tasks (see [`old_task_names`]) that this Windows user created. Returns the
/// interval (days) of one that was removed. Another user's task is never touched.
pub fn remove_old_if_ours() -> Option<u32> {
    // The demo and a developer instance with its own data folder never touch the installed app's tasks.
    if crate::demo::is_demo() || crate::paths::isolated_suffix().is_some() {
        return None;
    }
    let mut removed = None;
    for name in old_task_names() {
        if let Some(days) = old_interval_if_ours(&name) {
            let ok = cmd("schtasks").args(["/Delete", "/TN", &name, "/F"]).output().map(|o| o.status.success()).unwrap_or(false);
            invalidate();
            if ok {
                removed = removed.or(Some(days));
            }
        }
    }
    removed
}

/// At app start: move this user's older task to the current per-user name (same interval). The
/// new task is installed first and the old one removed only after that succeeded, so automatic
/// checks never stop because of the move.
pub fn migrate_legacy() {
    if crate::demo::is_demo() || crate::paths::isolated_suffix().is_some() {
        return;
    }
    if status().installed {
        let _ = remove_old_if_ours();
        return;
    }
    let Some(days) = old_task_names().iter().find_map(|n| old_interval_if_ours(n)) else { return };
    // install() removes the old task(s) after the new one was created.
    match install(days) {
        Ok(_) => log::info!("scheduled task moved to the per-user name ({days} days)"),
        Err(e) => log::warn!("could not move the scheduled task (the old one is kept): {e:#}"),
    }
}

fn grab_tag(xml: &str, tag: &str) -> Option<String> {
    xml.split(&format!("<{tag}>")).nth(1).and_then(|s| s.split(&format!("</{tag}>")).next()).map(|s| s.trim().to_string())
}

#[derive(serde::Serialize, Debug, Default, Clone)]
pub struct TaskStatus {
    pub installed: bool,
    pub command: Option<String>,
    pub interval_days: Option<u32>,
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub last_result: Option<String>,
    /// Start (local time) of the one extra check before a claim deadline, if planned.
    pub deadline_run: Option<String>,
}

static CACHE: std::sync::Mutex<Option<(std::time::Instant, TaskStatus)>> = std::sync::Mutex::new(None);

/// Cached for 30 s; install/remove invalidate it.
pub fn status() -> TaskStatus {
    if crate::demo::is_demo() {
        let d: u32 = crate::runner::open_db()
            .ok()
            .and_then(|db| db.get_setting("demo_schedule").ok().flatten())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        return TaskStatus { installed: d > 0, interval_days: (d > 0).then_some(d), deadline_run: deadline_run(), ..Default::default() };
    }
    let mut c = CACHE.lock().unwrap();
    if let Some((t, s)) = c.as_ref() {
        if t.elapsed() < std::time::Duration::from_secs(30) {
            return s.clone();
        }
    }
    let s = status_uncached();
    *c = Some((std::time::Instant::now(), s.clone()));
    s
}

fn invalidate() {
    *CACHE.lock().unwrap() = None;
}

fn status_uncached() -> TaskStatus {
    let name = task_name();
    let Some(xml) = query_xml(&name) else {
        return TaskStatus::default();
    };
    let grab = |tag: &str| grab_tag(&xml, tag);
    let mut st = TaskStatus {
        installed: true,
        command: grab("Command"),
        interval_days: grab("DaysInterval").and_then(|d| d.parse().ok()),
        deadline_run: deadline_run(),
        ..Default::default()
    };
    // Locale-independent: ISO 8601 timestamps from the Task Scheduler API via PowerShell.
    let ps = format!(
        "$i = Get-ScheduledTaskInfo -TaskName '{}'; if ($i.NextRunTime) {{ $i.NextRunTime.ToString('o') }} else {{ '' }}; if ($i.LastRunTime -and $i.LastRunTime.Year -gt 2000) {{ $i.LastRunTime.ToString('o') }} else {{ '' }}; $i.LastTaskResult",
        name.replace('\'', "''")
    );
    if let Ok(o) = cmd("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &ps]).output() {
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        let lines: Vec<&str> = out.lines().map(str::trim).collect();
        let non_empty = |s: Option<&&str>| s.map(|v| v.to_string()).filter(|v| !v.is_empty());
        st.next_run = non_empty(lines.first());
        st.last_run = non_empty(lines.get(1));
        st.last_result = non_empty(lines.get(2));
    }
    st
}

#[cfg(test)]
mod tests {
    #[test]
    fn task_xml_file_names_are_random() {
        let (a, b) = (super::random_hex().unwrap(), super::random_hex().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn xml_has_catch_up_and_user_scope() {
        let x = super::task_xml(
            r"C:\Users\x\AppData\Local\OV Refund\ov-refund.exe",
            7,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap().and_hms_opt(10, 0, 0).unwrap(),
        );
        assert!(x.contains("<StartWhenAvailable>true</StartWhenAvailable>"));
        assert!(x.contains("<DaysInterval>7</DaysInterval>"));
        assert!(x.contains("InteractiveToken"));
        assert!(x.contains("LeastPrivilege"));
        assert!(x.contains("--run-once"));
        assert!(x.contains("<LogonTrigger>") && x.contains("<Delay>PT2M</Delay>"));
        assert!(x.contains("elke 7 dagen") && x.contains("every 7 days") && !x.contains("(s)"));
        let one = super::task_xml("x", 1, chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap().and_hms_opt(10, 0, 0).unwrap());
        assert!(one.contains("elke dag") && one.contains("every day"));
    }

    #[test]
    fn deadline_task_is_one_shot_and_expires() {
        let d = |h| chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap().and_hms_opt(h, 0, 0).unwrap();
        let x = super::deadline_task_xml(r"C:\OV Refund\ov-refund.exe", d(10), d(12));
        assert!(x.contains("<TimeTrigger>") && !x.contains("<CalendarTrigger>") && !x.contains("<LogonTrigger>"));
        assert!(x.contains("<StartBoundary>2026-11-20T10:00:00</StartBoundary>"));
        assert!(x.contains("<EndBoundary>2026-11-20T12:00:00</EndBoundary>"));
        assert!(x.contains("<DeleteExpiredTaskAfter>P1D</DeleteExpiredTaskAfter>"));
        assert!(
            x.contains("<Arguments>--run-once --trigger deadline</Arguments>")
                && x.contains("<StartWhenAvailable>true</StartWhenAvailable>")
        );
        assert!(!super::legacy_is_ours(&x, r"PC\jan"), "never mistaken for an older regular task");
        assert!(super::deadline_task_name().starts_with(&super::task_name()) && super::deadline_task_name().ends_with("-Deadline"));
    }

    #[test]
    fn per_user_task_name_is_sanitized() {
        assert_eq!(super::v012_task_name_for("jan"), "OVRefund-ScheduledCheck-jan");
        assert_eq!(super::v012_task_name_for("Jan de Vries"), "OVRefund-ScheduledCheck-Jan_de_Vries");
        assert_eq!(super::v012_task_name_for("a\\b/c:d\"e'"), "OVRefund-ScheduledCheck-a_b_c_d_e_");
        assert_eq!(super::v012_task_name_for("jöns"), "OVRefund-ScheduledCheck-j_ns");
        assert_eq!(super::v012_task_name_for(""), super::LEGACY_TASK_NAME);
    }

    #[test]
    fn task_name_has_a_hash_of_the_account() {
        let local = super::task_name_for("jan", r"PC1\jan");
        let domain = super::task_name_for("jan", r"CORP\jan");
        assert!(local.starts_with("OVRefund-ScheduledCheck-jan-") && local.len() == "OVRefund-ScheduledCheck-jan-".len() + 8, "{local}");
        assert_ne!(local, domain, "same user name, different account");
        assert_eq!(local, super::task_name_for("jan", r"pc1\JAN"), "case-insensitive");
        assert!(local.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(super::task_name_for("", "x"), super::LEGACY_TASK_NAME);
    }

    #[test]
    fn refusals_are_recognised() {
        assert!(super::is_refusal("ERROR: Access is denied."));
        assert!(super::is_refusal("FOUT: Toegang geweigerd."));
        assert!(super::is_refusal("ERROR: 0x80070005"));
        assert!(!super::is_refusal("ERROR: The task XML is malformed."));
        assert!(super::POLICY_REFUSED.contains(" / "));
    }

    #[test]
    fn legacy_task_only_ours_when_user_matches() {
        let xml = r"<Task><Triggers><LogonTrigger><UserId>PC\jan</UserId></LogonTrigger></Triggers><Exec><Command>C:\x\ov-refund.exe</Command><Arguments>--run-once</Arguments></Exec></Task>";
        assert!(super::legacy_is_ours(xml, r"PC\jan"));
        assert!(!super::legacy_is_ours(xml, r"PC\anna"));
        assert!(!super::legacy_is_ours(r"<Task><UserId>PC\jan</UserId><Command>other.exe</Command></Task>", r"PC\jan"));
    }
}
