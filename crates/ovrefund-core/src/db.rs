//! SQLite ledger: named migrations, journeys, evidence, candidates, claims state
//! machine with an append-only audit trail, runs, notifications, settings.
use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::path::Path;

pub const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_initial",
        r#"
CREATE TABLE sources (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  account_alias TEXT,
  connected INTEGER NOT NULL DEFAULT 0,
  last_sync TEXT,
  credential_ref TEXT,
  auth_state TEXT,
  updated_at TEXT NOT NULL
);
CREATE TABLE cards (
  id INTEGER PRIMARY KEY,
  source_id TEXT NOT NULL REFERENCES sources(id),
  card_type TEXT NOT NULL,
  last4 TEXT NOT NULL,
  secure_ref TEXT,
  status TEXT NOT NULL DEFAULT 'active',
  UNIQUE(source_id, last4)
);
CREATE TABLE journeys (
  id INTEGER PRIMARY KEY,
  source_id TEXT NOT NULL,
  source_record_id TEXT NOT NULL,
  fallback_key TEXT NOT NULL UNIQUE,
  coupling_id TEXT,
  card_last4 TEXT,
  check_in_utc TEXT NOT NULL,
  check_out_utc TEXT,
  origin_uic TEXT NOT NULL,
  origin_name TEXT NOT NULL,
  dest_uic TEXT,
  dest_name TEXT,
  carrier_code TEXT,
  carrier_name TEXT,
  product_name TEXT,
  travel_class INTEGER,
  fare_cents INTEGER,
  subscription TEXT,
  payload_sha256 TEXT NOT NULL,
  first_seen_at TEXT NOT NULL,
  observed_at TEXT NOT NULL,
  UNIQUE(source_id, source_record_id)
);
CREATE INDEX journeys_check_in ON journeys(check_in_utc);

CREATE TABLE schedules (
  journey_id INTEGER PRIMARY KEY REFERENCES journeys(id) ON DELETE CASCADE,
  provider TEXT NOT NULL,
  historical INTEGER NOT NULL,
  status TEXT NOT NULL,
  scheduled_departure_utc TEXT,
  scheduled_arrival_utc TEXT,
  planned_json TEXT,
  taken_json TEXT,
  notes_json TEXT NOT NULL DEFAULT '[]',
  evaluated_at TEXT NOT NULL
);
CREATE TABLE candidates (
  journey_id INTEGER PRIMARY KEY REFERENCES journeys(id) ON DELETE CASCADE,
  confidence TEXT NOT NULL CHECK (confidence IN ('supported_for_auto','needs_review','not_claimable_or_out_of_scope')),
  checkout_minus_scheduled_arrival_min INTEGER,
  estimated_arrival_delay_min INTEGER,
  delay_band TEXT,
  scheduled_departure_utc TEXT,
  reasons_json TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  assessed_at TEXT NOT NULL
);
CREATE TABLE claims (
  id INTEGER PRIMARY KEY,
  journey_id INTEGER NOT NULL UNIQUE REFERENCES journeys(id),
  journey_key TEXT NOT NULL UNIQUE,
  gateway TEXT NOT NULL,
  status TEXT NOT NULL,
  facts_json TEXT,
  authorized_mode TEXT,
  authorized_at TEXT,
  submitted_at TEXT,
  ns_reference TEXT,
  external_prior INTEGER NOT NULL DEFAULT 0,
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX claims_status ON claims(status);
CREATE TABLE claim_events (
  id INTEGER PRIMARY KEY,
  claim_id INTEGER NOT NULL REFERENCES claims(id),
  at TEXT NOT NULL,
  from_status TEXT,
  to_status TEXT NOT NULL,
  actor TEXT NOT NULL,
  detail TEXT NOT NULL DEFAULT ''
);
CREATE TRIGGER claim_events_no_update BEFORE UPDATE ON claim_events BEGIN SELECT RAISE(ABORT, 'claim_events is append-only'); END;
CREATE TRIGGER claim_events_no_delete BEFORE DELETE ON claim_events BEGIN SELECT RAISE(ABORT, 'claim_events is append-only'); END;

CREATE TABLE external_claims (
  id INTEGER PRIMARY KEY,
  travel_date TEXT NOT NULL,
  origin_text TEXT,
  dest_text TEXT,
  source TEXT NOT NULL,
  note TEXT,
  created_at TEXT NOT NULL
);
CREATE TABLE runs (
  id INTEGER PRIMARY KEY,
  started_at TEXT NOT NULL,
  completed_at TEXT,
  mode TEXT NOT NULL,
  trigger TEXT NOT NULL,
  outcome TEXT,
  summary_json TEXT,
  error_summary TEXT,
  auth_state TEXT
);
CREATE TABLE notifications (
  id INTEGER PRIMARY KEY,
  run_id INTEGER REFERENCES runs(id),
  channel TEXT NOT NULL,
  status TEXT NOT NULL,
  attempted_at TEXT NOT NULL,
  sent_at TEXT,
  error TEXT,
  UNIQUE(run_id, channel)
);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE run_lock (id INTEGER PRIMARY KEY CHECK (id = 1), owner TEXT NOT NULL, expires_at TEXT NOT NULL);

CREATE TABLE archive_stops (
  service_id INTEGER NOT NULL,
  stop_seq INTEGER NOT NULL,
  month TEXT NOT NULL,
  service_date TEXT NOT NULL,
  train_type TEXT NOT NULL,
  company TEXT NOT NULL,
  train_number TEXT NOT NULL,
  station_code TEXT NOT NULL,
  arr_planned TEXT, arr_delay INTEGER, arr_cancelled INTEGER NOT NULL,
  dep_planned TEXT, dep_delay INTEGER, dep_cancelled INTEGER NOT NULL,
  PRIMARY KEY(service_id, stop_seq)
);
CREATE INDEX archive_stops_station_date ON archive_stops(station_code, service_date);
CREATE TABLE archive_coverage (month TEXT NOT NULL, station_code TEXT NOT NULL, imported_at TEXT NOT NULL, PRIMARY KEY(month, station_code));
"#,
    ),
    (
        "0002_compensation",
        r#"
ALTER TABLE journeys ADD COLUMN fare_source TEXT;
ALTER TABLE candidates ADD COLUMN compensation_cents INTEGER;
ALTER TABLE candidates ADD COLUMN compensation_basis TEXT;
ALTER TABLE claims ADD COLUMN paid_cents INTEGER;
"#,
    ),
    (
        "0003_lean_evidence",
        r#"
CREATE TABLE evidence_cache (
  journey_key TEXT PRIMARY KEY,
  month TEXT NOT NULL,
  evidence_json TEXT NOT NULL,
  computed_at TEXT NOT NULL
);
-- keep already-computed per-journey evidence so nothing has to be re-downloaded
INSERT OR IGNORE INTO evidence_cache(journey_key, month, evidence_json, computed_at)
SELECT j.fallback_key, substr(j.check_in_utc, 1, 7),
       json_object('provider', s.provider, 'historical', json(CASE WHEN s.historical THEN 'true' ELSE 'false' END),
                   'planned', json(s.planned_json), 'taken', json(s.taken_json), 'status', s.status, 'notes', json(s.notes_json)),
       s.evaluated_at
FROM schedules s JOIN journeys j ON j.id = s.journey_id
WHERE s.status IN ('found', 'no_route');
DROP TABLE IF EXISTS archive_stops;
DROP TABLE IF EXISTS archive_coverage;
"#,
    ),
    (
        "0004_fare_details",
        r#"
ALTER TABLE journeys ADD COLUMN subscription_fee_cents INTEGER;
ALTER TABLE journeys ADD COLUMN invoiced INTEGER;
ALTER TABLE candidates ADD COLUMN compensation_exact INTEGER;
"#,
    ),
    (
        "0005_ns_mail",
        r#"
CREATE TABLE ns_mail (
  key TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  reference TEXT,
  amount_cents INTEGER,
  travel_date TEXT,
  mail_date TEXT NOT NULL,
  journey_id INTEGER REFERENCES journeys(id),
  claim_id INTEGER REFERENCES claims(id),
  processed_at TEXT NOT NULL
);
CREATE INDEX ns_mail_reference ON ns_mail(reference);
"#,
    ),
    (
        "0006_commission",
        r#"
-- Share withheld by another claim service (e.g. 0.25 or 0.35). NULL = unknown.
ALTER TABLE claims ADD COLUMN commission_pct REAL;
"#,
    ),
    (
        "0007_mail_match_strength",
        r#"
-- How an NS mail was tied to a trip: 'reference' (NS application number) or 'date' (travel date
-- + stations only). NULL = not tied (retried on later syncs).
ALTER TABLE ns_mail ADD COLUMN match_kind TEXT;
-- Station names found in the mail (JSON array), so an unmatched mail can be matched again later.
ALTER TABLE ns_mail ADD COLUMN stations_json TEXT;
-- Natural key (check-in time + origin) of a trip a date-only match pointed at WITHOUT applying
-- the mail (our own / still open claim): a caution flag, never proof.
ALTER TABLE ns_mail ADD COLUMN hint_key TEXT;
-- Older rows: how they were tied is unknown, so they count as the weaker date match.
UPDATE ns_mail SET match_kind = 'date' WHERE journey_id IS NOT NULL;
-- ns_claim_near used to hold journey row ids (reused after a history wipe): use natural keys.
UPDATE settings SET value = COALESCE((SELECT json_group_array(j.check_in_utc || '|' || j.origin_uic) FROM journeys j
                                      WHERE j.id IN (SELECT value FROM json_each(settings.value))), '[]')
 WHERE key = 'ns_claim_near' AND json_valid(value);
"#,
    ),
    (
        "0008_submitted_here",
        r#"
-- 1 = this claim was submitted by OV Refund on THIS installation (set when the submission starts).
-- Only such claims can verify the claim gateway for automatic submission; a restore clears it
-- for every claim taken from a backup. Existing installations (best effort): claims that were
-- submitted before this version are taken as submitted here.
ALTER TABLE claims ADD COLUMN submitted_here INTEGER NOT NULL DEFAULT 0;
UPDATE claims SET submitted_here = 1 WHERE submitted_at IS NOT NULL AND authorized_mode IN ('manual', 'automatic');
"#,
    ),
    (
        "0009_route_evidence_refresh",
        r#"
-- Recompute only unexpired, open trips using complete transfer search and local DST baselines.
-- The transactional Rust hook uses the same Amsterdam-local claim window as eligibility.
"#,
    ),
];

pub struct Db {
    pub conn: Connection,
}

/// Setting holding the journey ids flagged by [`crate::pipeline::reconcile_open`] (JSON array).
pub const NS_CLAIM_NEAR_KEY: &str = "ns_claim_near";
/// Trips that Mijn NS linked to another card after they were recorded.
pub const CARD_CONFLICT_KEY: &str = "card_conflict";

fn card_conflict_key(journey: &Journey) -> String {
    format!("tx|{}|{}", journey.source_id, journey.source_record_id)
}

fn ts(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

/// Run inside the migration transaction, before recording its completion. The stored fallback
/// key is exactly the key used by evidence_cache; no key reconstruction or month-wide wipe.
fn refresh_open_route_evidence(conn: &Connection, now: DateTime<Utc>) -> Result<()> {
    let mut st = conn.prepare(
        "SELECT j.id, j.fallback_key, j.check_in_utc FROM journeys j
         LEFT JOIN claims c ON c.journey_id=j.id
         WHERE c.id IS NULL OR c.status IN ('CANDIDATE','READY','NEEDS_REVIEW','FAILED')",
    )?;
    let journeys = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, key, check_in) in journeys {
        if crate::eligibility::expires_at(parse_ts(&check_in)?, crate::eligibility::ClaimWindow::default()) < now {
            continue;
        }
        // Old assessments must not authorize a send before the next successful check.
        conn.execute("DELETE FROM candidates WHERE journey_id=?1", [id])?;
        conn.execute("DELETE FROM schedules WHERE journey_id=?1", [id])?;
        conn.execute("DELETE FROM evidence_cache WHERE journey_key=?1", [key])?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct JourneyRow {
    pub id: i64,
    pub journey: Journey,
    pub confidence: Option<String>,
    pub proxy_min: Option<i64>,
    pub delay_min: Option<i64>,
    pub delay_band: Option<String>,
    pub scheduled_departure: Option<String>,
    pub reasons: Vec<String>,
    pub expires_at: Option<String>,
    pub evidence_status: Option<String>,
    pub planned: Option<Route>,
    pub taken: Option<Route>,
    pub claim_id: Option<i64>,
    pub claim_status: Option<String>,
    pub ns_reference: Option<String>,
    pub compensation_cents: Option<i64>,
    pub compensation_basis: Option<String>,
    pub compensation_exact: Option<bool>,
    pub paid_cents: Option<i64>,
    pub commission_pct: Option<f64>,
    /// Latest decision seen in an NS e-mail (submitted / approved / rejected / paid).
    pub ns_decision: Option<String>,
    /// NS's claim list (last check) shows a claim around this trip's date that could not be tied
    /// to this or another trip: possible duplicate, the user should check before submitting.
    pub ns_claim_near: bool,
    pub card_conflict: bool,
    /// An earlier submission may have reached NS ([`Db::was_uncertain`]): a resubmission needs
    /// `confirm_resubmit`.
    pub was_uncertain: bool,
    /// Trip still waiting for timetable data whose check-out came at least 35 min later than the
    /// fastest earlier trip on the same route suggests: a possible delay (minutes). Indication
    /// only, never submitted automatically.
    pub possible_delay_min: Option<i64>,
    /// Refund page (or homepage) of the carrier, for trips with another carrier than NS.
    pub carrier_url: Option<String>,
    /// The exact refund (from the trip's own fare) is below NS's EUR 2.30 minimum: not paid out.
    pub below_minimum: bool,
}

/// [`JourneyRow::evidence_status`] of a trip with another operator than NS (Arriva, ...): OV
/// Refund does not check it (the operator handles its own refunds), so it never waits for NS
/// timetable data.
pub const OTHER_OPERATOR_STATUS: &str = "other_operator";

/// Minimum check-out delay (minutes, against the fastest trip on the same route) for
/// [`JourneyRow::possible_delay_min`].
pub const POSSIBLE_DELAY_MIN: i64 = 35;
/// History note and refusal when a claim request was observed during "Eerst proberen".
pub const PREFLIGHT_WRITE_DETAIL: &str = "Tijdens Eerst proberen ging er mogelijk een claim naar NS. Kijk in Mijn teruggaven. / During Try first a claim may have been sent to NS. Check Mijn teruggaven.";

/// Refused when the data were written by a newer OV Refund ([`Db::open`], restore).
pub const NEWER_DATA_MSG: &str =
    "Deze versie is ouder dan je gegevens. Installeer de nieuwste versie. / This version is older than your data. Install the latest version.";

/// Refused restore of a backup made by a newer OV Refund.
pub const RESTORE_NEWER_MSG: &str = "Deze app is ouder dan de back-up. Werk eerst bij. / This app is older than the backup. Update first.";

/// ALLOWLIST: the only settings a backup carries and a restore takes over. Harmless display
/// preferences only. Everything else stays tied to this PC, this Windows user or an explicit
/// consent and is never put in a backup or taken from one: e-mail/SMTP and NS-mail reading,
/// the NS account, automatic login, consent and the "before a verified submission" override,
/// the claim-service answer, wizard/disclaimer state, the run mode, limits and look-back,
/// schedule and its interval (`interval_days` belongs with the local scheduled task), reminder
/// timestamps, claim-list status. [`NS_CLAIM_NEAR_KEY`] is handled separately (merged).
pub const BACKUP_SETTINGS: [&str; 3] = ["ui_lang", "notify_windows", "update_checks"];

/// Marker setting in a backup made by the demo; the real app refuses such a backup.
pub const BACKUP_DEMO_KEY: &str = "backup_demo";

/// Claims that reached NS (or may have), or were claimed elsewhere: never dropped by a history
/// wipe or a forced restore (duplicate protection). SQL condition on `claims`.
/// Refusal while another claim for the same carrier is being submitted.
pub const INFLIGHT_SAME_CARRIER_MSG: &str = "Er wordt al een andere claim bij deze vervoerder ingediend. Er is niets verstuurd. Wacht tot die klaar is. / Another claim is already being submitted to this carrier. Nothing was sent. Wait until it has finished.";
/// Refusal when the check no longer holds the run lock (lease expired or taken over) right before
/// a submission.
pub const LOCK_LOST_MSG: &str = "De controle was niet meer de enige die bezig was (de vergrendeling is verlopen of overgenomen). Er is niets verstuurd. / The check was no longer the only one running (its lock expired or was taken over). Nothing was sent.";

/// Is another claim (not `claim_id`) for the same carrier group as `claim_id`'s trip in
/// SUBMITTING? Groups: Arriva trips ([`crate::model::is_arriva_carrier`]) and all others (the NS
/// group, as [`crate::pipeline::unresolved_ns_claims`] counts them).
fn inflight_same_carrier(conn: &Connection, claim_id: i64) -> Result<bool> {
    let (code, name): (Option<String>, Option<String>) = conn.query_row(
        "SELECT j.carrier_code, j.carrier_name FROM claims c JOIN journeys j ON j.id=c.journey_id WHERE c.id=?1",
        [claim_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let arriva = crate::model::is_arriva_carrier(code.as_deref(), name.as_deref());
    let mut st = conn.prepare(
        "SELECT j.carrier_code, j.carrier_name FROM claims c JOIN journeys j ON j.id=c.journey_id WHERE c.status='SUBMITTING' AND c.id<>?1",
    )?;
    let rows = st.query_map([claim_id], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))?;
    for row in rows {
        let (c, n) = row?;
        if crate::model::is_arriva_carrier(c.as_deref(), n.as_deref()) == arriva {
            return Ok(true);
        }
    }
    Ok(false)
}

pub const KEEP_CLAIMS_SQL: &str = "status IN ('SUBMITTING','SUBMITTED','INDETERMINATE','APPROVED','REJECTED','PAID','EXTERNALLY_CLAIMED')
   OR id IN (SELECT claim_id FROM claim_events WHERE to_status='INDETERMINATE' OR from_status='INDETERMINATE'
               OR (from_status='SUBMITTING' AND to_status='FAILED' AND actor<>'gateway'))";

/// Largest backup file a restore accepts.
pub const MAX_BACKUP_BYTES: usize = 20 * 1024 * 1024;
pub const BACKUP_TOO_LARGE_MSG: &str =
    "Dit bestand is te groot voor een OV Refund-back-up (max. 20 MB). / This file is too large for an OV Refund backup (max 20 MB).";
pub const RESTORE_DEMO_MSG: &str =
    "Dit is een back-up uit de demo. Die kun je niet in OV Refund terugzetten. / This is a backup from the demo. It cannot be restored into OV Refund.";

/// What a restore brought in, and what it kept from the current ledger.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RestoreStats {
    /// Trips and claims in the ledger after the restore.
    pub journeys: i64,
    pub claims: i64,
    /// Claims of the current ledger that reached NS (or may have) or were claimed elsewhere,
    /// kept with their trip and history (duplicate protection).
    pub kept_claims: i64,
    /// Claims from the backup that were replaced by such a kept claim for the same trip.
    pub replaced_from_backup: i64,
    /// "Claimed elsewhere" entries of the current ledger that were kept.
    pub kept_external: i64,
    /// Trips and claims taken from the backup file.
    pub from_backup_journeys: i64,
    pub from_backup_claims: i64,
    /// Trips without an assessment after the restore (re-assessed by the next check).
    pub unassessed: i64,
    /// Automatic backup of the ledger written before a forced restore (set by the app).
    pub safety_backup: Option<String>,
}

/// How an NS e-mail was tied to a trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MailMatch {
    /// NS application number: certain.
    Reference,
    /// Travel date (+ stations) only: plausible, never proof.
    Date,
}

impl MailMatch {
    fn as_str(&self) -> &'static str {
        match self {
            MailMatch::Reference => "reference",
            MailMatch::Date => "date",
        }
    }
}

/// What one NS e-mail changed (or, when `applied` is false, only noted) for a trip.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MailUpdate {
    /// Travel date, dd-mm-yyyy (Europe/Amsterdam).
    pub date: String,
    /// "Origin → Destination".
    pub route: String,
    pub kind: crate::ns_mail::MailKind,
    pub amount_cents: Option<i64>,
    /// false: matched by date only to a claim this app filed or may still file — noted and
    /// flagged ([`Db::ns_claim_near_keys`]), nothing changed.
    pub applied: bool,
}

impl MailUpdate {
    fn kind_label(&self, nl: bool) -> &'static str {
        use crate::ns_mail::MailKind::*;
        match (self.kind, nl) {
            (Submitted, true) => "Ingediend",
            (Submitted, false) => "Submitted",
            (Approved, true) => "Toegekend",
            (Approved, false) => "Approved",
            (Rejected, true) => "Afgewezen",
            (Rejected, false) => "Rejected",
            (Paid, true) => "Uitbetaald",
            (Paid, false) => "Paid",
        }
    }

    fn amount(&self, nl: bool) -> String {
        match self.amount_cents.filter(|_| self.kind != crate::ns_mail::MailKind::Rejected) {
            Some(a) => format!(", {}", crate::email::euro(a, if nl { crate::email::Lang::Nl } else { crate::email::Lang::En })),
            None => String::new(),
        }
    }

    /// The claim-history note, without trip details.
    fn detail_in(&self, nl: bool) -> String {
        match (self.applied, nl) {
            (true, true) => format!("NS-mail: {}{}", self.kind_label(true), self.amount(true)),
            (true, false) => format!("NS e-mail: {}{}", self.kind_label(false), self.amount(false)),
            (false, true) => format!(
                "NS-mail over een claim rond deze datum: {}{} (niet toegepast: niet zeker van deze reis; controleer Mijn teruggaven)",
                self.kind_label(true),
                self.amount(true)
            ),
            (false, false) => format!(
                "NS e-mail about a claim around this date: {}{} (not applied: not certainly this trip; check Mijn teruggaven)",
                self.kind_label(false),
                self.amount(false)
            ),
        }
    }

    /// Claim-history note as "Nederlands / English" (the GUI shows the half for its language).
    pub fn detail(&self) -> String {
        format!("{} / {}", self.detail_in(true), self.detail_in(false))
    }

    /// One line for a toast or report, in one language.
    pub fn line(&self, nl: bool) -> String {
        format!("{} {}: {}", self.date, self.route, self.detail_in(nl))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimRow {
    pub id: i64,
    pub journey_id: i64,
    pub status: ClaimState,
    pub facts: Option<ClaimFacts>,
    pub authorized_mode: Option<String>,
    pub submitted_at: Option<String>,
    pub ns_reference: Option<String>,
    pub external_prior: bool,
    pub attempts: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimEvent {
    pub at: String,
    pub from_status: Option<String>,
    pub to_status: String,
    pub actor: String,
    pub detail: String,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open db {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Open (and migrate) a database file that came from outside, e.g. a backup to restore:
    /// defensive mode and an untrusted schema (no SQL functions with side effects run from
    /// anything stored in the file). Check it with [`Db::check_backup_file`] first.
    fn open_untrusted(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .with_context(|| format!("open db {}", path.display()))?;
        conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        conn.pragma_update(None, "trusted_schema", "OFF")?;
        Self::init(conn)
    }

    /// The triggers and views this version's migrations create: (type, name, SQL with its
    /// whitespace collapsed).
    fn own_triggers_and_views() -> Result<Vec<(String, String, String)>> {
        let fresh = Self::open_in_memory()?;
        schema_objects(&fresh.conn, &["trigger", "view"])
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        Self::refuse_newer(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut db = Self { conn };
        db.migrate()?;
        db.conn.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
        Ok(db)
    }

    /// A database written by a newer OV Refund (a migration this build does not know, or a higher
    /// `user_version`) is never opened: an older build could damage it.
    fn refuse_newer(conn: &Connection) -> Result<()> {
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        anyhow::ensure!(version <= MIGRATIONS.len() as i64, NEWER_DATA_MSG);
        let has_table: bool =
            conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_migrations'", [], |r| {
                r.get::<_, i64>(0)
            })? > 0;
        if has_table {
            let mut st = conn.prepare("SELECT name FROM schema_migrations")?;
            let names = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            anyhow::ensure!(names.iter().all(|n| MIGRATIONS.iter().any(|(m, _)| m == n)), NEWER_DATA_MSG);
        }
        Ok(())
    }

    pub fn migrate(&mut self) -> Result<Vec<String>> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY, applied_at TEXT NOT NULL);")?;
        let mut applied = vec![];
        for (name, sql) in MIGRATIONS {
            let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Re-check inside the write lock: another process may have just applied it.
            let done: bool = tx.query_row("SELECT COUNT(*) FROM schema_migrations WHERE name=?1", [name], |r| r.get::<_, i64>(0))? > 0;
            if done {
                continue;
            }
            // Opening a newer app must not migrate data beneath an older active run.
            let has_lock: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='run_lock')", [], |r| r.get(0))?;
            if has_lock {
                let active: bool =
                    tx.query_row("SELECT EXISTS(SELECT 1 FROM run_lock WHERE expires_at > ?1)", [ts(Utc::now())], |r| r.get(0))?;
                anyhow::ensure!(!active, "Er loopt een controle. Open OV Refund opnieuw zodra die klaar is. / A check is running. Reopen OV Refund when it has finished.");
            }
            tx.execute_batch(sql).with_context(|| format!("migration {name}"))?;
            if *name == "0009_route_evidence_refresh" {
                refresh_open_route_evidence(&tx, Utc::now()).with_context(|| format!("migration {name}"))?;
            }
            tx.execute("INSERT INTO schema_migrations(name, applied_at) VALUES (?1, ?2)", params![name, ts(Utc::now())])?;
            tx.commit()?;
            applied.push(name.to_string());
        }
        if applied.iter().any(|m| m == "0003_lean_evidence") {
            // Give the space of the dropped raw timetable tables back to the OS (best effort).
            if let Err(e) = self.conn.execute_batch("VACUUM;") {
                log::warn!("VACUUM after migration failed (harmless): {e}");
            }
        }
        Ok(applied)
    }

    // ---------- settings ----------
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
    pub fn all_settings(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare("SELECT key, value FROM settings ORDER BY key")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---------- sources ----------
    pub fn upsert_source(&self, id: &str, kind: &str, connected: bool, auth_state: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sources(id,kind,connected,auth_state,updated_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET connected=excluded.connected, auth_state=excluded.auth_state, updated_at=excluded.updated_at",
            params![id, kind, connected as i64, auth_state, ts(Utc::now())],
        )?;
        Ok(())
    }
    pub fn mark_source_synced(&self, id: &str) -> Result<()> {
        self.conn.execute("UPDATE sources SET last_sync=?2 WHERE id=?1", params![id, ts(Utc::now())])?;
        Ok(())
    }
    pub fn source_status(&self, id: &str) -> Result<Option<(bool, Option<String>, Option<String>)>> {
        Ok(self
            .conn
            .query_row("SELECT connected, auth_state, last_sync FROM sources WHERE id=?1", [id], |r| {
                Ok((r.get::<_, i64>(0)? != 0, r.get(1)?, r.get(2)?))
            })
            .optional()?)
    }

    // ---------- journeys ----------
    // (see `station_matches` below for how typed station names are compared)
    /// Insert or refresh a journey. Returns (journey id, newly inserted?).
    ///
    /// The trip with the same source record id wins over one that only has the same fallback key
    /// (when both exist as different rows), and a key another row already has is never copied
    /// (no UNIQUE collision). A trip whose claim has left the pre-submission states (it was sent,
    /// may have been, or was claimed elsewhere: [`ClaimState::blocks_new_submission`]) keeps its
    /// recorded trip facts (check-out, destination, card, ids): only the observation time and the
    /// fare details are refreshed.
    pub fn upsert_journey(&self, j: &Journey) -> Result<(i64, bool)> {
        self.upsert_imported_journey(j, true)
    }

    pub(crate) fn upsert_imported_journey(&self, j: &Journey, allow_card_confirmation: bool) -> Result<(i64, bool)> {
        let now = ts(Utc::now());
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM journeys WHERE (source_id=?1 AND source_record_id=?2) OR fallback_key=?3
                 ORDER BY (source_id=?1 AND source_record_id=?2) DESC, id LIMIT 1",
                params![j.source_id, j.source_record_id, j.fallback_key()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let previous = self.journey(id)?;
            let key = card_conflict_key(&previous);
            let coupling_conflict = previous.coupling_id.as_ref().zip(j.coupling_id.as_ref()).is_some_and(|(a, b)| a != b);
            let suffix_conflict = previous.card_last4.as_ref().zip(j.card_last4.as_ref()).is_some_and(|(a, b)| a != b);
            let tx = self.conn.unchecked_transaction()?;
            let conflict = coupling_conflict || suffix_conflict;
            // A fresh observation of the same transaction on the recorded card resolves a hold.
            let same_record = previous.source_id == j.source_id && previous.source_record_id == j.source_record_id;
            let card_confirmed = allow_card_confirmation && same_record
                && previous.coupling_id.as_deref().is_some_and(|coupling| !coupling.is_empty())
                && previous.coupling_id == j.coupling_id
                && (previous.card_last4.is_none() || previous.card_last4 == j.card_last4);
            if conflict {
                self.set_card_conflict(&key, true)?;
            } else if card_confirmed {
                self.clear_card_conflict(&previous)?;
            }
            // A fallback match must not replace the record id that a held trip needs to confirm.
            if conflict || !same_record && self.has_card_conflict(&previous)? {
                tx.execute("UPDATE journeys SET observed_at=?2 WHERE id=?1", params![id, now])?;
                tx.commit()?;
                return Ok((id, false));
            }
            if self.claim_for_journey(id)?.is_some_and(|c| c.status.blocks_new_submission()) {
                tx.execute(
                    "UPDATE journeys SET observed_at=?2, fare_cents=COALESCE(NULLIF(?3,0),fare_cents,?3), subscription=COALESCE(?4,subscription),
                     subscription_fee_cents=COALESCE(?5,subscription_fee_cents), invoiced=COALESCE(?6,invoiced),
                     travel_class=COALESCE(?7,travel_class) WHERE id=?1",
                    params![id, now, j.fare_cents, j.subscription, j.subscription_fee_cents, j.invoiced, j.travel_class],
                )?;
                tx.commit()?;
                return Ok((id, false));
            }
            let carrier_changed = (j.carrier_code.is_some() || j.is_arriva())
                && (previous.is_arriva() != j.is_arriva() || previous.is_ns_carrier() != j.is_ns_carrier());
            let updated_key =
                Journey {
                    destination: j.destination.clone().or_else(|| previous.destination.clone()),
                    card_last4: j.card_last4.clone().or_else(|| previous.card_last4.clone()),
                    ..previous.clone()
                }.fallback_key();
            // Check-out corrections change the train actually taken. Commit the journey and
            // cache invalidation together; absent fields in an incremental response keep theirs.
            tx.execute(
                "UPDATE journeys SET check_out_utc=COALESCE(?2,check_out_utc), dest_uic=COALESCE(?3,dest_uic), dest_name=COALESCE(?4,dest_name),
                 payload_sha256=?5, observed_at=?6, coupling_id=COALESCE(?7,coupling_id),
                 fare_cents=COALESCE(NULLIF(?8,0),fare_cents,?8), subscription=COALESCE(?9,subscription),
                 subscription_fee_cents=COALESCE(?10,subscription_fee_cents), invoiced=COALESCE(?11,invoiced),
                 source_record_id=CASE WHEN source_id=?12 AND NOT EXISTS(SELECT 1 FROM journeys o WHERE o.source_id=?12 AND o.source_record_id=?13 AND o.id<>?1)
                   THEN ?13 ELSE source_record_id END,
                 fallback_key=CASE WHEN NOT EXISTS(SELECT 1 FROM journeys o WHERE o.fallback_key=?14 AND o.id<>?1) THEN ?14 ELSE fallback_key END,
                 carrier_code=CASE WHEN ?15 THEN ?16 ELSE COALESCE(?16,carrier_code) END,
                 carrier_name=CASE WHEN ?15 THEN ?17 ELSE COALESCE(?17,carrier_name) END,
                 travel_class=COALESCE(?18,travel_class), card_last4=COALESCE(?19,card_last4)
                 WHERE id=?1",
                params![
                    id,
                    j.check_out.map(ts),
                    j.destination.as_ref().map(|d| d.uic.clone()),
                    j.destination.as_ref().map(|d| d.name.clone()),
                    j.payload_sha256,
                    now,
                    j.coupling_id,
                    j.fare_cents,
                    j.subscription,
                    j.subscription_fee_cents,
                    j.invoiced,
                    j.source_id,
                    j.source_record_id,
                    updated_key,
                    carrier_changed,
                    j.carrier_code,
                    j.carrier_name,
                    j.travel_class,
                    j.card_last4
                ],
            )?;
            if carrier_changed || updated_key != previous.fallback_key() || j.check_out.is_some_and(|co| Some(co) != previous.check_out)
                || j.destination.as_ref().is_some_and(|d| Some(d) != previous.destination.as_ref())
            {
                tx.execute(
                    "DELETE FROM evidence_cache WHERE journey_key=?1 OR journey_key=?2",
                    params![previous.fallback_key(), j.fallback_key()],
                )?;
                if !self.claim_for_journey(id)?.is_some_and(|c| c.status.blocks_new_submission()) {
                    tx.execute("DELETE FROM candidates WHERE journey_id=?1", [id])?;
                    tx.execute("DELETE FROM schedules WHERE journey_id=?1", [id])?;
                }
            }
            tx.commit()?;
            return Ok((id, false));
        }
        self.conn.execute(
            "INSERT INTO journeys(source_id,source_record_id,fallback_key,coupling_id,card_last4,check_in_utc,check_out_utc,origin_uic,origin_name,
               dest_uic,dest_name,carrier_code,carrier_name,product_name,travel_class,fare_cents,subscription,payload_sha256,first_seen_at,observed_at,
               subscription_fee_cents,invoiced)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?19,?20,?21)",
            params![
                j.source_id,
                j.source_record_id,
                j.fallback_key(),
                j.coupling_id,
                j.card_last4,
                ts(j.check_in),
                j.check_out.map(ts),
                j.origin.uic,
                j.origin.name,
                j.destination.as_ref().map(|d| d.uic.clone()),
                j.destination.as_ref().map(|d| d.name.clone()),
                j.carrier_code,
                j.carrier_name,
                j.product_name,
                j.travel_class,
                j.fare_cents,
                j.subscription,
                j.payload_sha256,
                now,
                j.subscription_fee_cents,
                j.invoiced
            ],
        )?;
        Ok((self.conn.last_insert_rowid(), true))
    }

    fn row_to_journey(r: &rusqlite::Row) -> rusqlite::Result<Journey> {
        let ci: String = r.get("check_in_utc")?;
        let co: Option<String> = r.get("check_out_utc")?;
        let dest_uic: Option<String> = r.get("dest_uic")?;
        Ok(Journey {
            source_id: r.get("source_id")?,
            source_record_id: r.get("source_record_id")?,
            coupling_id: r.get("coupling_id")?,
            card_last4: r.get("card_last4")?,
            check_in: parse_ts(&ci).map_err(|_| rusqlite::Error::InvalidQuery)?,
            check_out: co.and_then(|s| parse_ts(&s).ok()),
            origin: StationRef { uic: r.get("origin_uic")?, name: r.get("origin_name")? },
            destination: dest_uic
                .map(|u| StationRef { uic: u, name: r.get::<_, Option<String>>("dest_name").ok().flatten().unwrap_or_default() }),
            carrier_code: r.get("carrier_code")?,
            carrier_name: r.get("carrier_name")?,
            product_name: r.get("product_name")?,
            travel_class: r.get::<_, Option<i64>>("travel_class")?.map(|c| c as u8),
            fare_cents: r.get("fare_cents")?,
            subscription: r.get("subscription")?,
            subscription_fee_cents: r.get("subscription_fee_cents")?,
            invoiced: r.get::<_, Option<i64>>("invoiced")?.map(|v| v != 0),
            payload_sha256: r.get("payload_sha256")?,
        })
    }

    pub fn journey(&self, id: i64) -> Result<Journey> {
        Ok(self.conn.query_row("SELECT * FROM journeys WHERE id=?1", [id], Self::row_to_journey)?)
    }

    pub fn journeys_since(&self, since: DateTime<Utc>) -> Result<Vec<(i64, Journey)>> {
        let mut st = self.conn.prepare("SELECT * FROM journeys WHERE check_in_utc >= ?1 ORDER BY check_in_utc DESC")?;
        let rows =
            st.query_map([ts(since)], |r| Ok((r.get::<_, i64>("id")?, Self::row_to_journey(r)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Everything the GUI needs for the journey table.
    pub fn journey_rows(&self, limit: i64) -> Result<Vec<JourneyRow>> {
        let mut st = self.conn.prepare(
            "SELECT j.*, c.confidence, c.checkout_minus_scheduled_arrival_min AS proxy, c.estimated_arrival_delay_min AS delay, c.delay_band,
                    c.scheduled_departure_utc AS sdep, c.reasons_json, c.expires_at, c.compensation_cents, c.compensation_basis, c.compensation_exact, cl.paid_cents, cl.commission_pct,
                    (SELECT kind FROM ns_mail m WHERE m.journey_id=j.id AND (cl.status='EXTERNALLY_CLAIMED' OR UPPER(m.kind)=cl.status)
                     ORDER BY (m.kind <> 'submitted') DESC, m.mail_date DESC LIMIT 1) AS mail_kind, s.status AS ev_status, s.planned_json, s.taken_json,
                    cl.id AS claim_id, cl.status AS claim_status, cl.ns_reference,
                    (SELECT COUNT(*) FROM claim_events e WHERE e.claim_id = cl.id AND (e.to_status = 'INDETERMINATE' OR e.from_status = 'INDETERMINATE'
                       OR (e.from_status = 'SUBMITTING' AND e.to_status = 'FAILED' AND e.actor <> 'gateway'))) AS uncertain
             FROM journeys j LEFT JOIN candidates c ON c.journey_id=j.id LEFT JOIN schedules s ON s.journey_id=j.id
             LEFT JOIN claims cl ON cl.journey_id=j.id ORDER BY j.check_in_utc DESC LIMIT ?1",
        )?;
        let rows = st
            .query_map([limit], |r| {
                Ok(JourneyRow {
                    id: r.get("id")?,
                    journey: Self::row_to_journey(r)?,
                    confidence: r.get("confidence")?,
                    proxy_min: r.get("proxy")?,
                    delay_min: r.get("delay")?,
                    delay_band: r.get("delay_band")?,
                    scheduled_departure: r.get("sdep")?,
                    reasons: r.get::<_, Option<String>>("reasons_json")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                    expires_at: r.get("expires_at")?,
                    evidence_status: r.get("ev_status")?,
                    planned: r.get::<_, Option<String>>("planned_json")?.and_then(|s| serde_json::from_str(&s).ok()),
                    taken: r.get::<_, Option<String>>("taken_json")?.and_then(|s| serde_json::from_str(&s).ok()),
                    claim_id: r.get("claim_id")?,
                    claim_status: r.get("claim_status")?,
                    ns_reference: r.get("ns_reference")?,
                    compensation_cents: r.get("compensation_cents")?,
                    compensation_basis: r.get("compensation_basis")?,
                    compensation_exact: r.get::<_, Option<i64>>("compensation_exact")?.map(|v| v != 0),
                    paid_cents: r.get("paid_cents")?,
                    commission_pct: r.get("commission_pct")?,
                    ns_decision: r.get("mail_kind")?,
                    ns_claim_near: false,
                    card_conflict: false,
                    was_uncertain: r.get::<_, i64>("uncertain")? > 0,
                    possible_delay_min: None,
                    carrier_url: None,
                    below_minimum: false,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let near = self.ns_claim_near_keys()?;
        let card_conflicts: std::collections::HashSet<_> = self.card_conflict_keys()?.into_iter().collect();
        // Fastest trip per route (origin, destination): the baseline for a possible delay.
        let duration = |j: &Journey| j.check_out.map(|co| (co - j.check_in).num_minutes()).filter(|m| *m > 0 && *m < 6 * 60);
        let route = |j: &Journey| j.destination.as_ref().map(|d| (j.origin.uic.clone(), d.uic.clone()));
        let mut fastest: std::collections::HashMap<(String, String), Vec<(i64, i64)>> = Default::default();
        for r in rows.iter().filter(|r| r.journey.is_ns_carrier()) {
            if let (Some(k), Some(d)) = (route(&r.journey), duration(&r.journey)) {
                fastest.entry(k).or_default().push((r.id, d));
            }
        }
        Ok(rows
            .into_iter()
            .map(|mut r| {
                r.ns_claim_near = near.contains(&r.journey.natural_key());
                r.card_conflict = card_conflicts.contains(&card_conflict_key(&r.journey))
                    || card_conflicts.contains(&r.journey.natural_key());
                r.ns_claim_near |= r.card_conflict;
                r.carrier_url = r.journey.carrier_url().map(str::to_string);
                // Existing Arriva rows can predate the fare calculation or have since aged into it.
                if r.journey.is_arriva() {
                    let fare = crate::compensation::effective_arriva_fare(&r.journey);
                    if fare.is_some() {
                        r.reasons.retain(|reason| reason != crate::eligibility::FARE_UNKNOWN_REASON);
                    }
                    let comp = fare.and_then(|fare| fare.compensation(&r.journey, r.delay_min?));
                    r.compensation_cents = comp.as_ref().map(|c| c.cents);
                    r.compensation_exact = comp.as_ref().map(|c| c.exact);
                    r.compensation_basis = comp.map(|c| c.basis);
                }
                r.below_minimum = r.compensation_exact == Some(true) && r.compensation_cents == Some(0);
                // Trips with another operator never get timetable data: not "waiting". Arriva trips
                // are checked with Arriva's rules (they do wait for the timetable).
                if !r.journey.is_ns_carrier() && !r.journey.is_arriva() && r.evidence_status.as_deref() == Some("pending") {
                    r.evidence_status = Some(OTHER_OPERATOR_STATUS.into());
                }
                let waiting = r.evidence_status.as_deref() == Some("pending")
                    && r.journey.is_ns_carrier()
                    && !r.claim_status.as_deref().and_then(ClaimState::parse).map(|c| c.blocks_new_submission()).unwrap_or(false);
                if waiting {
                    if let (Some(k), Some(d)) = (route(&r.journey), duration(&r.journey)) {
                        let base = fastest.get(&k).and_then(|v| v.iter().filter(|(id, _)| *id != r.id).map(|(_, m)| *m).min());
                        r.possible_delay_min = base.map(|b| d - b).filter(|x| *x >= POSSIBLE_DELAY_MIN);
                    }
                }
                r
            })
            .collect())
    }

    /// Open claims (claimable / to review / not sent) whose claim window ends within `within`.
    pub fn expiring_soon(&self, now: DateTime<Utc>, within: chrono::Duration) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM claims cl JOIN candidates c ON c.journey_id = cl.journey_id
             WHERE cl.status IN ('READY','NEEDS_REVIEW','FAILED') AND c.expires_at > ?1 AND c.expires_at <= ?2",
            params![ts(now), ts(now + within)],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// The subscription(s) NS listed on trips in the last 120 days, grouped by (description,
    /// monthly fee, class): `{current, previous (max 2, newest first, with "until"), none}`.
    pub fn subscription_history(&self, now: DateTime<Utc>) -> Result<serde_json::Value> {
        let mut st = self.conn.prepare(
            "SELECT subscription, subscription_fee_cents, travel_class, MIN(check_in_utc), MAX(check_in_utc) FROM journeys
             WHERE subscription IS NOT NULL AND TRIM(subscription) <> '' AND check_in_utc >= ?1
             GROUP BY 1, 2, 3 ORDER BY 5 DESC",
        )?;
        let rows = st
            .query_map([ts(now - chrono::Duration::days(120))], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let item = |(desc, fee, class, since, last): &(String, Option<i64>, Option<i64>, String, String)| {
            serde_json::json!({
                "parts": crate::compensation::describe_subscription(desc),
                "fee_cents": fee,
                "class": class.filter(|c| *c == 1 || *c == 2),
                "since": since,
                "last_seen": last,
            })
        };
        let previous: Vec<serde_json::Value> = rows
            .iter()
            .skip(1)
            .take(2)
            .map(|r| {
                let mut v = item(r);
                v["until"] = serde_json::json!(r.4);
                v
            })
            .collect();
        Ok(serde_json::json!({ "current": rows.first().map(item), "previous": previous, "none": rows.is_empty() }))
    }

    /// Claims as CSV (";"-separated, for Excel in Dutch and English), one row per trip with a claim.
    /// No card numbers, IBAN or e-mail addresses.
    pub fn claims_csv(&self, nl: bool) -> Result<String> {
        let head = if nl {
            "datum;vertrek;van;naar;vertraging_min;status;ns_referentie;verwacht_eur;ontvangen_eur;claimen_voor"
        } else {
            "date;departure;from;to;delay_min;status;ns_reference;expected_eur;received_eur;claim_before"
        };
        let cell = |s: &str| {
            let s = csv_safe(s);
            if s.contains([';', '"', '\n', '\r']) {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s
            }
        };
        // Decimal comma in both languages, like the app (and ";" separators for Excel).
        let eur = |c: Option<i64>| c.map(|c| format!("{},{:02}", c / 100, c % 100));
        let mut out = String::from(head);
        out.push_str("\r\n");
        for r in self.journey_rows(i64::MAX)? {
            let Some(st) = r.claim_status.as_deref().and_then(ClaimState::parse) else { continue };
            let ci = r.journey.check_in.with_timezone(&chrono_tz::Europe::Amsterdam);
            let exp = r
                .expires_at
                .as_deref()
                .and_then(|e| parse_ts(e).ok())
                .filter(|_| matches!(st, ClaimState::Ready | ClaimState::NeedsReview | ClaimState::Failed))
                .map(|e| e.with_timezone(&chrono_tz::Europe::Amsterdam).format("%Y-%m-%d").to_string());
            let fields = [
                ci.format("%Y-%m-%d").to_string(),
                ci.format("%H:%M").to_string(),
                r.journey.origin.name.clone(),
                r.journey.destination.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
                csv_delay(&r).map(|d| d.to_string()).unwrap_or_default(),
                csv_status(st, nl).to_string(),
                r.ns_reference.clone().unwrap_or_default(),
                eur(r.compensation_cents).unwrap_or_default(),
                eur(r.paid_cents).unwrap_or_default(),
                exp.unwrap_or_default(),
            ];
            out.push_str(&fields.iter().map(|f| cell(f)).collect::<Vec<_>>().join(";"));
            out.push_str("\r\n");
        }
        Ok(out)
    }

    /// Write a copy of the ledger to `path` (a new file) without anything tied to this PC, this
    /// user or a consent: no run lock, no NS session state, no notification log, and only the
    /// harmless settings in [`BACKUP_SETTINGS`] (plus the "NS may already have a claim" flags),
    /// so a backup holds no e-mail address, SMTP or account settings. Passwords, IBAN and card
    /// numbers are never in the database (Windows Credential Manager). `demo` marks a backup made
    /// by the demo ([`BACKUP_DEMO_KEY`]); the real app refuses it.
    pub fn backup_to(&self, path: &Path, demo: bool) -> Result<()> {
        anyhow::ensure!(!path.exists(), "backup file already exists");
        // Sanitize the snapshot in memory before the export path is visible to another process.
        static BACKUP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = BACKUP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = format!("file:ovrefund-backup-{}-{id}?mode=memory&cache=shared", std::process::id());
        let b = Connection::open(&staging)?;
        self.conn.execute("VACUUM INTO ?1", [&staging])?;
        b.execute_batch("PRAGMA temp_store=MEMORY; PRAGMA secure_delete=ON;")?;
        // Run error texts are free text from the last checks: not part of a backup.
        b.execute_batch(
            "DELETE FROM run_lock; DELETE FROM sources; DELETE FROM notifications;
             UPDATE runs SET error_summary=NULL,
                 summary_json=CASE WHEN json_valid(summary_json) THEN json_set(summary_json, '$.errors', json('[]')) END;",
        )?;
        let keep = BACKUP_SETTINGS.iter().chain([NS_CLAIM_NEAR_KEY, CARD_CONFLICT_KEY].iter()).map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
        b.execute(&format!("DELETE FROM settings WHERE key NOT IN ({keep})"), [])?;
        if demo {
            b.execute("INSERT OR REPLACE INTO settings(key, value) VALUES(?1, '1')", [BACKUP_DEMO_KEY])?;
        }
        b.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])?;
        Ok(())
    }

    /// Is there anything in the ledger (trips, claims or "claimed elsewhere" entries)?
    pub fn ledger_empty(&self) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM journeys) + (SELECT COUNT(*) FROM claims) + (SELECT COUNT(*) FROM external_claims)",
            [],
            |r| r.get(0),
        )?;
        Ok(n == 0)
    }

    /// Checks on a backup file before anything else touches it (opened read-only, defensive,
    /// untrusted schema): size, SQLite integrity (`quick_check`), every expected table really is
    /// a table, no views or unknown triggers, not from a newer version, and not a demo backup
    /// (unless `demo`).
    pub fn check_backup_file(path: &Path, demo: bool) -> Result<()> {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        anyhow::ensure!(size <= MAX_BACKUP_BYTES as u64, BACKUP_TOO_LARGE_MSG);
        // A database file that cannot be read completely and is cut off or carries OV Refund's table
        // names is a damaged or incomplete backup (e.g. cut off while copying), not someone else's file.
        let unreadable = || if looks_like_ov_backup(path) { anyhow!(RESTORE_DAMAGED_MSG) } else { anyhow!(RESTORE_INVALID_MSG) };
        let probe = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| unreadable())?;
        let invalid = |_| anyhow!(RESTORE_INVALID_MSG);
        probe.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true).map_err(invalid)?;
        probe.pragma_update(None, "trusted_schema", "OFF").map_err(invalid)?;
        // The full check (also every index against its table), one "ok" row when sound.
        let ok: String = probe.query_row("PRAGMA integrity_check", [], |r| r.get(0)).map_err(|_| unreadable())?;
        if ok != "ok" {
            return Err(unreadable());
        }
        // Triggers and views only as this version's migrations create them (same name and SQL):
        // a backup can never bring its own code into the ledger.
        let own = Self::own_triggers_and_views()?;
        for obj in schema_objects(&probe, &["trigger", "view"]).map_err(|_| unreadable())? {
            anyhow::ensure!(own.contains(&obj), RESTORE_INVALID_MSG);
        }
        let objects: Vec<(String, String)> = {
            let mut st = probe.prepare("SELECT type, name FROM sqlite_master").map_err(|_| unreadable())?;
            let v = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                .map_err(|_| unreadable())?;
            v
        };
        const REQUIRED: [&str; 13] = [
            "schema_migrations",
            "sources",
            "cards",
            "journeys",
            "schedules",
            "candidates",
            "claims",
            "claim_events",
            "external_claims",
            "runs",
            "notifications",
            "settings",
            "run_lock",
        ];
        const OPTIONAL: [&str; 2] = ["evidence_cache", "ns_mail"];
        for t in REQUIRED {
            anyhow::ensure!(objects.iter().any(|(ty, n)| ty == "table" && n == t), RESTORE_INVALID_MSG);
        }
        for (ty, n) in &objects {
            let allowed = match ty.as_str() {
                "table" | "index" => true,
                // compared with this version's own triggers above
                "trigger" => true,
                _ => false, // views and anything else
            };
            anyhow::ensure!(allowed, RESTORE_INVALID_MSG);
            if REQUIRED.contains(&n.as_str()) || OPTIONAL.contains(&n.as_str()) {
                anyhow::ensure!(ty == "table", RESTORE_INVALID_MSG);
            }
        }
        Self::refuse_newer(&probe).map_err(|_| anyhow!(RESTORE_NEWER_MSG))?;
        if !demo {
            let is_demo = probe
                .query_row("SELECT COUNT(*) FROM settings WHERE key=?1", [BACKUP_DEMO_KEY], |r| r.get::<_, i64>(0))
                .map_err(invalid)?
                > 0;
            anyhow::ensure!(!is_demo, RESTORE_DEMO_MSG);
        }
        Ok(())
    }

    /// Replace the ledger with a backup made by [`Db::backup_to`]. The backup file is checked
    /// ([`Db::check_backup_file`]) and migrated in place first (pass a temporary copy). Refused
    /// when the backup comes from a newer version, is not an OV Refund backup, is a demo backup in
    /// the real app, or this ledger is not empty and `force` is false.
    ///
    /// Duplicate protection: every claim of the current ledger that reached NS (or may have) or
    /// was claimed elsewhere ([`KEEP_CLAIMS_SQL`], the same rule as [`Db::wipe_history`]) is kept
    /// with its trip, assessment, history and NS e-mails; it replaces the backup's claim for the
    /// same trip. "Claimed elsewhere" entries and the "NS may already have a claim" flags are
    /// merged. Only [`BACKUP_SETTINGS`] are taken from the backup. Claims from the backup never
    /// count as submitted on this installation (`submitted_here` is cleared). Deleted rows are
    /// purged from free pages and the WAL afterwards. The caller holds the run lock.
    pub fn restore_from(&mut self, path: &Path, force: bool, demo: bool) -> Result<RestoreStats> {
        Self::check_backup_file(path, demo)?;
        // Bring the backup to this version's schema (same migrations as the live ledger), with
        // its schema untrusted.
        drop(Db::open_untrusted(path)?);
        anyhow::ensure!(force || self.ledger_empty()?, RESTORE_NOT_EMPTY_MSG);
        self.conn.pragma_update(None, "trusted_schema", "OFF")?;
        self.conn.execute("ATTACH DATABASE ?1 AS bk", [path.to_string_lossy().to_string()])?;
        let r = self.restore_attached();
        let _ = self.conn.execute("DETACH DATABASE bk", []);
        let _ = self.conn.execute_batch(
            "DROP TABLE IF EXISTS temp.k_claims; DROP TABLE IF EXISTS temp.k_journeys; DROP TABLE IF EXISTS temp.k_events;
             DROP TABLE IF EXISTS temp.k_cand; DROP TABLE IF EXISTS temp.k_sched; DROP TABLE IF EXISTS temp.k_mail; DROP TABLE IF EXISTS temp.k_ext;",
        );
        let r = r?;
        // Deleted rows must not linger in free pages or the WAL file.
        self.conn.execute_batch("PRAGMA secure_delete=ON; VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(r)
    }

    fn restore_attached(&mut self) -> Result<RestoreStats> {
        const TABLES: [&str; 10] = [
            "journeys",
            "runs",
            "claims",
            "claim_events",
            "notifications",
            "candidates",
            "schedules",
            "evidence_cache",
            "external_claims",
            "ns_mail",
        ];
        let mut cols: std::collections::HashMap<&str, Vec<String>> = std::collections::HashMap::new();
        for t in TABLES {
            let mut st = self.conn.prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))?;
            let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            cols.insert(t, v);
        }
        let quoted = |cols: &[String]| cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(",");
        let except = |t: &str, skip: &[&str]| -> String {
            quoted(&cols[t].iter().filter(|c| !skip.contains(&c.as_str())).cloned().collect::<Vec<_>>())
        };
        let tx = self.conn.transaction()?;
        let held_fallbacks = {
            let mut stmt = tx.prepare(
                "SELECT j.fallback_key, h.value FROM main.journeys j
                 JOIN main.settings s ON s.key=?1
                 JOIN json_each(CASE WHEN json_valid(s.value) THEN s.value ELSE '[]' END) h
                     ON h.value = 'tx|' || j.source_id || '|' || j.source_record_id",
            )?;
            let rows = stmt.query_map([CARD_CONFLICT_KEY], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        // 1. Set aside what must survive (duplicate protection).
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE k_claims AS SELECT * FROM main.claims WHERE {KEEP_CLAIMS_SQL};
             CREATE TEMP TABLE k_journeys AS SELECT * FROM main.journeys WHERE id IN (SELECT journey_id FROM temp.k_claims);
             CREATE TEMP TABLE k_events AS SELECT * FROM main.claim_events WHERE claim_id IN (SELECT id FROM temp.k_claims) ORDER BY id;
             CREATE TEMP TABLE k_cand AS SELECT * FROM main.candidates WHERE journey_id IN (SELECT journey_id FROM temp.k_claims);
             CREATE TEMP TABLE k_sched AS SELECT * FROM main.schedules WHERE journey_id IN (SELECT journey_id FROM temp.k_claims);
             CREATE TEMP TABLE k_mail AS SELECT * FROM main.ns_mail WHERE claim_id IN (SELECT id FROM temp.k_claims);
             CREATE TEMP TABLE k_ext AS SELECT * FROM main.external_claims;"
        ))?;
        // 2. Replace the ledger with the backup.
        let mut sql = String::from("DROP TRIGGER IF EXISTS claim_events_no_delete;\n");
        for t in TABLES.iter().rev() {
            sql.push_str(&format!("DELETE FROM main.{t};\n"));
        }
        for t in TABLES {
            let c = quoted(&cols[t]);
            sql.push_str(&format!("INSERT INTO main.{t}({c}) SELECT {c} FROM bk.{t};\n"));
        }
        // Submitted on another installation (or restored before): never proof for this one.
        sql.push_str("UPDATE main.claims SET submitted_here = 0;\n");
        let allow = BACKUP_SETTINGS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
        sql.push_str(&format!(
            "INSERT OR REPLACE INTO main.settings(key, value) SELECT key, value FROM bk.settings WHERE key IN ({allow});\n"
        ));
        tx.execute_batch(&sql)?;
        // The runs were replaced: a stored stop request no longer means anything.
        tx.execute(
            "DELETE FROM main.settings WHERE key IN (?1, ?2)",
            [crate::pipeline::AUTO_STOP_KEY, crate::pipeline::AUTO_STOP_PENDING_KEY],
        )?;
        // 3. Put the kept claims back (they win over the backup's claim for the same trip).
        let mut stats = RestoreStats {
            from_backup_journeys: tx.query_row("SELECT COUNT(*) FROM bk.journeys", [], |r| r.get(0))?,
            from_backup_claims: tx.query_row("SELECT COUNT(*) FROM bk.claims", [], |r| r.get(0))?,
            ..Default::default()
        };
        let kept: Vec<(i64, i64)> = {
            let mut st = tx.prepare("SELECT id, journey_id FROM temp.k_claims ORDER BY id")?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            v
        };
        for (old_cid, old_jid) in kept {
            let (fkey, src, rec): (String, String, String) =
                tx.query_row("SELECT fallback_key, source_id, source_record_id FROM temp.k_journeys WHERE id=?1", [old_jid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            let found: Option<i64> = tx
                .query_row(
                    "SELECT id FROM main.journeys WHERE fallback_key=?1 OR (source_id=?2 AND source_record_id=?3) ORDER BY id LIMIT 1",
                    params![fkey, src, rec],
                    |r| r.get(0),
                )
                .optional()?;
            let jid = match found {
                Some(j) => j,
                None => {
                    let c = except("journeys", &["id"]);
                    tx.execute(&format!("INSERT INTO main.journeys({c}) SELECT {c} FROM temp.k_journeys WHERE id=?1"), [old_jid])?;
                    tx.last_insert_rowid()
                }
            };
            for (t, k) in [("candidates", "k_cand"), ("schedules", "k_sched")] {
                let c = except(t, &["journey_id"]);
                tx.execute(
                    &format!("INSERT OR REPLACE INTO main.{t}(\"journey_id\",{c}) SELECT ?2,{c} FROM temp.{k} WHERE journey_id=?1"),
                    params![old_jid, jid],
                )?;
            }
            // The backup's claim for this trip (if any) is replaced; its NS e-mails become
            // unmatched again (tied again on the next mail sync).
            if let Some(bcid) = tx.query_row("SELECT id FROM main.claims WHERE journey_id=?1", [jid], |r| r.get::<_, i64>(0)).optional()? {
                tx.execute("UPDATE main.ns_mail SET claim_id=NULL, journey_id=NULL, match_kind=NULL WHERE claim_id=?1", [bcid])?;
                tx.execute("DELETE FROM main.claim_events WHERE claim_id=?1", [bcid])?;
                tx.execute("DELETE FROM main.claims WHERE id=?1", [bcid])?;
                stats.replaced_from_backup += 1;
            }
            let c = except("claims", &["id", "journey_id"]);
            tx.execute(
                &format!("INSERT INTO main.claims(\"journey_id\",{c}) SELECT ?2,{c} FROM temp.k_claims WHERE id=?1"),
                params![old_cid, jid],
            )?;
            let cid = tx.last_insert_rowid();
            let c = except("claim_events", &["id", "claim_id"]);
            tx.execute(
                &format!("INSERT INTO main.claim_events(\"claim_id\",{c}) SELECT ?2,{c} FROM temp.k_events WHERE claim_id=?1 ORDER BY id"),
                params![old_cid, cid],
            )?;
            let c = except("ns_mail", &["claim_id", "journey_id"]);
            tx.execute(
                &format!(
                    "INSERT OR REPLACE INTO main.ns_mail(\"claim_id\",\"journey_id\",{c}) SELECT ?2,?3,{c} FROM temp.k_mail WHERE claim_id=?1"
                ),
                params![old_cid, cid, jid],
            )?;
            stats.kept_claims += 1;
        }
        // Backup assessments never authorise a send. Recompute open trips on the next check.
        tx.execute_batch(&format!(
            "DELETE FROM main.candidates WHERE journey_id NOT IN (SELECT journey_id FROM main.claims WHERE {KEEP_CLAIMS_SQL});
             DELETE FROM main.schedules WHERE journey_id NOT IN (SELECT journey_id FROM main.claims WHERE {KEEP_CLAIMS_SQL});
             DELETE FROM main.evidence_cache WHERE journey_key NOT IN
               (SELECT fallback_key FROM main.journeys WHERE id IN (SELECT journey_id FROM main.claims WHERE {KEEP_CLAIMS_SQL}));"
        ))?;
        // 4. "Claimed elsewhere" entries of this ledger are kept as well (no duplicates).
        let c = except("external_claims", &["id"]);
        stats.kept_external = tx.execute(
            &format!(
                "INSERT INTO main.external_claims({c}) SELECT {c} FROM temp.k_ext k WHERE NOT EXISTS (SELECT 1 FROM main.external_claims e
                   WHERE e.travel_date = k.travel_date AND e.origin_text IS k.origin_text AND e.dest_text IS k.dest_text AND e.source = k.source)"
            ),
            [],
        )? as i64;
        // 5. Preserve the current and backed-up warnings through restore.
        for key in [NS_CLAIM_NEAR_KEY, CARD_CONFLICT_KEY] {
            let mut merged = std::collections::BTreeSet::<String>::new();
            for schema in ["main", "bk"] {
                let stored: Option<String> = tx.query_row(
                    &format!("SELECT value FROM {schema}.settings WHERE key=?1"), [key], |r| r.get(0),
                ).optional()?;
                merged.extend(stored.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()).unwrap_or_default());
            }
            if key == CARD_CONFLICT_KEY {
                for (fallback, old_key) in &held_fallbacks {
                    let replacement: Option<String> = tx.query_row(
                        "SELECT 'tx|' || source_id || '|' || source_record_id FROM main.journeys
                         WHERE fallback_key=?1 AND NOT EXISTS (
                             SELECT 1 FROM main.journeys WHERE 'tx|' || source_id || '|' || source_record_id=?2
                         )",
                        [fallback, old_key], |r| r.get(0),
                    ).optional()?;
                    if let Some(new_key) = replacement {
                        merged.insert(new_key);
                    }
                }
            }
            if merged.is_empty() {
                tx.execute("DELETE FROM main.settings WHERE key=?1", [key])?;
            } else {
                tx.execute(
                    "INSERT OR REPLACE INTO main.settings(key, value) VALUES(?1, ?2)",
                    params![key, serde_json::to_string(&merged)?],
                )?;
            }
        }
        tx.execute_batch(
            "CREATE TRIGGER claim_events_no_delete BEFORE DELETE ON claim_events BEGIN SELECT RAISE(ABORT, 'claim_events is append-only'); END;",
        )?;
        tx.commit()?;
        stats.journeys = self.conn.query_row("SELECT COUNT(*) FROM journeys", [], |r| r.get(0))?;
        stats.claims = self.conn.query_row("SELECT COUNT(*) FROM claims", [], |r| r.get(0))?;
        stats.unassessed =
            self.conn.query_row("SELECT COUNT(*) FROM journeys WHERE id NOT IN (SELECT journey_id FROM candidates)", [], |r| r.get(0))?;
        Ok(stats)
    }

    fn card_conflict_keys(&self) -> Result<Vec<String>> {
        Ok(self.get_setting(CARD_CONFLICT_KEY)?.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()).unwrap_or_default())
    }

    pub fn has_card_conflict(&self, journey: &Journey) -> Result<bool> {
        let keys = self.card_conflict_keys()?;
        Ok(keys.contains(&card_conflict_key(journey)) || keys.contains(&journey.natural_key()))
    }

    fn set_card_conflict(&self, key: &str, hold: bool) -> Result<()> {
        let mut keys = self.card_conflict_keys()?;
        match (keys.iter().position(|k| k == key), hold) {
            (Some(index), false) => { keys.remove(index); }
            (None, true) => keys.push(key.to_string()),
            _ => return Ok(()),
        }
        self.set_setting(CARD_CONFLICT_KEY, &serde_json::to_string(&keys)?)
    }

    fn clear_card_conflict(&self, journey: &Journey) -> Result<()> {
        self.set_card_conflict(&card_conflict_key(journey), false)?;
        let legacy = journey.natural_key();
        if !self.card_conflict_keys()?.contains(&legacy) {
            return Ok(());
        }
        // Old holds identified only a time and station. Preserve the warning on other matching rows.
        let mut stmt = self.conn.prepare(
            "SELECT 'tx|' || source_id || '|' || source_record_id FROM journeys
             WHERE check_in_utc=?1 AND origin_uic=?2 AND (source_id<>?3 OR source_record_id<>?4)",
        )?;
        for key in stmt.query_map(params![ts(journey.check_in), journey.origin.uic, journey.source_id, journey.source_record_id],
            |r| r.get::<_, String>(0))?
        {
            self.set_card_conflict(&key?, true)?;
        }
        self.set_card_conflict(&legacy, false)
    }

    /// Trips (by [`Journey::natural_key`]) with a possible claim at NS that could not be tied to
    /// them for certain: the last available NS claim list showed an unexplained claim around the
    /// date, or an NS e-mail matched the trip by date only. Such a trip is never submitted
    /// automatically, and manually only after an explicit acknowledgement.
    pub fn ns_claim_near_keys(&self) -> Result<std::collections::HashSet<String>> {
        let mut keys: std::collections::HashSet<String> = self
            .get_setting(NS_CLAIM_NEAR_KEY)?
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        // Date-only NS e-mail hints, while the trip's claim is still open (not one we filed).
        let mut st = self.conn.prepare(
            "SELECT m.hint_key FROM ns_mail m JOIN journeys j ON (j.check_in_utc || '|' || j.origin_uic) = m.hint_key
             LEFT JOIN claims c ON c.journey_id = j.id
             WHERE m.journey_id IS NULL AND m.hint_key IS NOT NULL
               AND (c.id IS NULL OR c.status NOT IN ('SUBMITTING','SUBMITTED','INDETERMINATE','APPROVED','REJECTED','PAID','EXTERNALLY_CLAIMED'))",
        )?;
        keys.extend(st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
        // An NS mail can name a travel date without enough information to distinguish two trips.
        // Keep every plausible open NS trip on that day waiting for the user's duplicate check.
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT j.check_in_utc, j.origin_uic, m.travel_date
             FROM ns_mail m JOIN journeys j ON j.check_in_utc >= date(m.travel_date, '-1 day')
               AND j.check_in_utc < date(m.travel_date, '+1 day')
             LEFT JOIN claims c ON c.journey_id = j.id
             WHERE m.journey_id IS NULL AND m.hint_key IS NULL AND j.carrier_code=?1
               AND (c.id IS NULL OR c.status NOT IN ('SUBMITTING','SUBMITTED','INDETERMINATE','APPROVED','REJECTED','PAID','EXTERNALLY_CLAIMED'))",
        )?;
        let possible = stmt.query_map([NS_CARRIER_CODE], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in possible {
            let (check_in, origin, date) = row?;
            let time = parse_ts(&check_in)?;
            if time.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive().to_string() == date {
                keys.insert(format!("{check_in}|{origin}"));
            }
        }
        Ok(keys)
    }

    pub fn is_ns_claim_near(&self, j: &Journey) -> Result<bool> {
        Ok(self.has_card_conflict(j)? || self.ns_claim_near_keys()?.contains(&j.natural_key()))
    }

    /// Replace the claim-list part of [`Db::ns_claim_near_keys`]. Only called when NS's claim list
    /// was actually read: when it is unavailable, the previous flags are kept as they are (and no
    /// new ones are created), so a failed lookup never silently clears a warning.
    pub fn set_ns_claim_near_keys(&self, keys: &[String]) -> Result<()> {
        self.set_setting(NS_CLAIM_NEAR_KEY, &serde_json::to_string(keys)?)
    }

    // ---------- evidence + candidates ----------
    pub fn save_evidence(&self, journey_id: i64, ev: &ScheduleEvidence) -> Result<()> {
        self.conn.execute(
            "INSERT INTO schedules(journey_id,provider,historical,status,scheduled_departure_utc,scheduled_arrival_utc,planned_json,taken_json,notes_json,evaluated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(journey_id) DO UPDATE SET provider=excluded.provider, historical=excluded.historical, status=excluded.status,
               scheduled_departure_utc=excluded.scheduled_departure_utc, scheduled_arrival_utc=excluded.scheduled_arrival_utc,
               planned_json=excluded.planned_json, taken_json=excluded.taken_json, notes_json=excluded.notes_json, evaluated_at=excluded.evaluated_at",
            params![
                journey_id,
                ev.provider,
                ev.historical as i64,
                serde_json::to_value(ev.status)?.as_str().unwrap_or("").to_string(),
                ev.planned.as_ref().map(|p| ts(p.dep_planned())),
                ev.planned.as_ref().map(|p| ts(p.arr_planned())),
                ev.planned.as_ref().map(serde_json::to_string).transpose()?,
                ev.taken.as_ref().map(serde_json::to_string).transpose()?,
                serde_json::to_string(&ev.notes)?,
                ts(Utc::now())
            ],
        )?;
        Ok(())
    }

    pub fn evidence_status(&self, journey_id: i64) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT status FROM schedules WHERE journey_id=?1", [journey_id], |r| r.get(0)).optional()?)
    }

    pub fn save_assessment(&self, journey_id: i64, a: &Assessment) -> Result<()> {
        self.conn.execute(
            "INSERT INTO candidates(journey_id,confidence,checkout_minus_scheduled_arrival_min,estimated_arrival_delay_min,delay_band,scheduled_departure_utc,reasons_json,expires_at,assessed_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(journey_id) DO UPDATE SET confidence=excluded.confidence,
               checkout_minus_scheduled_arrival_min=excluded.checkout_minus_scheduled_arrival_min,
               estimated_arrival_delay_min=excluded.estimated_arrival_delay_min, delay_band=excluded.delay_band,
               scheduled_departure_utc=excluded.scheduled_departure_utc, reasons_json=excluded.reasons_json,
               expires_at=excluded.expires_at, assessed_at=excluded.assessed_at",
            params![
                journey_id,
                a.confidence.as_str(),
                a.checkout_minus_scheduled_arrival_min,
                a.estimated_arrival_delay_min,
                a.delay_band.map(|b| b.as_str()),
                a.scheduled_departure.map(ts),
                serde_json::to_string(&a.reasons)?,
                ts(a.expires_at),
                ts(Utc::now())
            ],
        )?;
        let j = self.journey(journey_id)?;
        // What NS actually paid (GROSS) for a free trip on the same subscription and band, closest
        // in time. Payouts via other services count only when their commission is known.
        let observed: Option<i64> = match (&j.subscription, a.delay_band) {
            (Some(sub), Some(band)) => self
                .conn
                .query_row(
                    "SELECT CAST(ROUND(CASE WHEN cl.status = 'EXTERNALLY_CLAIMED' THEN cl.paid_cents / (1.0 - cl.commission_pct)
                                            ELSE cl.paid_cents END) AS INTEGER)
                     FROM claims cl JOIN journeys jj ON jj.id = cl.journey_id
                     JOIN candidates cc ON cc.journey_id = cl.journey_id
                     WHERE cl.paid_cents > 0 AND jj.subscription = ?1 AND (jj.fare_cents IS NULL OR jj.fare_cents = 0)
                       AND cc.delay_band = ?2 AND cl.journey_id <> ?3 AND COALESCE(jj.travel_class, 2) = ?5
                       AND (cl.status <> 'EXTERNALLY_CLAIMED' OR (cl.commission_pct IS NOT NULL AND cl.commission_pct < 1.0))
                     ORDER BY ABS(julianday(jj.check_in_utc) - julianday(?4)) LIMIT 1",
                    params![sub, band.as_str(), journey_id, ts(j.check_in), j.travel_class.unwrap_or(2)],
                    |r| r.get(0),
                )
                .optional()?,
            _ => None,
        };
        let comp = if j.is_arriva() {
            a.estimated_arrival_delay_min.and_then(|d| crate::compensation::effective_arriva_fare(&j)?.compensation(&j, d))
        } else {
            crate::compensation::expected(&j, a.delay_band, observed)
        };
        self.conn.execute(
            "UPDATE candidates SET compensation_cents=?2, compensation_basis=?3, compensation_exact=?4 WHERE journey_id=?1",
            params![
                journey_id,
                comp.as_ref().map(|c| c.cents),
                comp.as_ref().map(|c| c.basis.clone()),
                comp.as_ref().map(|c| c.exact as i64)
            ],
        )?;
        Ok(())
    }

    pub fn set_fare(&self, journey_id: i64, fare_cents: i64, source: &str) -> Result<()> {
        self.conn.execute("UPDATE journeys SET fare_cents=?2, fare_source=?3 WHERE id=?1", params![journey_id, fare_cents, source])?;
        Ok(())
    }

    /// Commission withheld by another claim service (0.0-0.9), or None when unknown.
    pub fn set_commission(&self, claim_id: i64, pct: Option<f64>) -> Result<()> {
        anyhow::ensure!(
            pct.map(|p| (0.0..=0.9).contains(&p)).unwrap_or(true),
            "De commissie moet tussen 0 en 90% liggen. / The commission must be between 0 and 90%."
        );
        self.conn.execute("UPDATE claims SET commission_pct=?2 WHERE id=?1", params![claim_id, pct])?;
        Ok(())
    }

    pub fn paid_cents(&self, claim_id: i64) -> Result<Option<i64>> {
        Ok(self.conn.query_row("SELECT paid_cents FROM claims WHERE id=?1", [claim_id], |r| r.get(0))?)
    }

    pub fn set_paid_amount(&self, claim_id: i64, cents: Option<i64>) -> Result<()> {
        self.conn.execute("UPDATE claims SET paid_cents=?2 WHERE id=?1", params![claim_id, cents])?;
        Ok(())
    }

    /// Totals for the summary line (cents). Claims with unknown amounts are counted separately.
    pub fn totals(&self) -> Result<serde_json::Value> {
        let j = |t: (i64, i64, i64)| serde_json::json!({"count": t.0, "cents": t.1, "unknown": t.2});
        let q2 = |cond: &str| -> Result<(i64, i64, i64)> {
            Ok(self.conn.query_row(
                &format!(
                    "SELECT COUNT(*), COALESCE(SUM(COALESCE(cl.paid_cents, c.compensation_cents)),0),
                            COALESCE(SUM(CASE WHEN COALESCE(cl.paid_cents, c.compensation_cents) IS NULL THEN 1 ELSE 0 END),0)
                     FROM claims cl LEFT JOIN candidates c ON c.journey_id=cl.journey_id WHERE {cond}"
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        };
        // Money actually received: only recorded amounts (never estimates).
        let paid_only = |cond: &str| -> Result<(i64, i64, i64)> {
            Ok(self.conn.query_row(
                &format!("SELECT COUNT(*), COALESCE(SUM(cl.paid_cents),0), 0 FROM claims cl WHERE cl.paid_cents > 0 AND ({cond})"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        };
        let received = paid_only("cl.status = 'PAID'")?;
        Ok(serde_json::json!({
            // An approval and its amount are not proof of payment.
            "paid": j(received),
            "approved_unpaid": j(q2("cl.status = 'APPROVED'")?),
            "external": j(paid_only("cl.status = 'EXTERNALLY_CLAIMED' AND COALESCE((SELECT kind FROM ns_mail m WHERE m.claim_id=cl.id ORDER BY (m.kind <> 'submitted') DESC, m.mail_date DESC LIMIT 1),'') <> 'approved'")?),
            "pending": j(q2("cl.status IN ('SUBMITTED','SUBMITTING','INDETERMINATE') OR cl.status = 'APPROVED'")?),
            "claimable": j(q2(&format!(
                "cl.status IN ('READY','NEEDS_REVIEW','FAILED') AND (c.expires_at IS NULL OR c.expires_at > '{}')",
                ts(Utc::now())
            ))?),
        }))
    }

    pub fn assessment(&self, journey_id: i64) -> Result<Option<(Confidence, Option<DelayBand>, Option<DateTime<Utc>>, DateTime<Utc>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT confidence, delay_band, scheduled_departure_utc, expires_at FROM candidates WHERE journey_id=?1",
                [journey_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?)),
            )
            .optional()?
            .map(|(c, b, s, e)| {
                (
                    Confidence::parse(&c).unwrap_or(Confidence::NeedsReview),
                    b.and_then(|b| DelayBand::parse(&b)),
                    s.and_then(|s| parse_ts(&s).ok()),
                    parse_ts(&e).unwrap_or_else(|_| Utc::now()),
                )
            }))
    }

    // ---------- external claims (migration/import) ----------
    pub fn add_external_claim(
        &self,
        travel_date: chrono::NaiveDate,
        origin: Option<&str>,
        dest: Option<&str>,
        source: &str,
        note: Option<&str>,
    ) -> Result<i64> {
        // A date alone would block every trip that day (including an unrelated return trip).
        anyhow::ensure!(
            origin.map(|o| !o.trim().is_empty()).unwrap_or(false) && dest.map(|d| !d.trim().is_empty()).unwrap_or(false),
            "Vul het vertrek- en aankomststation in. / Enter both the departure and arrival station."
        );
        anyhow::ensure!(
            travel_date <= Utc::now().with_timezone(&chrono_tz::Europe::Amsterdam).date_naive(),
            "Deze datum ligt in de toekomst. / This date is in the future."
        );
        let (o, d) = (origin.unwrap_or_default(), dest.unwrap_or_default());
        let today = Utc::now().with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
        anyhow::ensure!(
            travel_date >= today.checked_sub_months(chrono::Months::new(18)).unwrap_or(today),
            "Kies een datum in de afgelopen 18 maanden. / Choose a date in the past 18 months."
        );
        let station_ok = |s: &str| {
            s.chars().count() <= 60
                && s.chars().any(char::is_alphabetic)
                && s.chars().all(|c| c.is_alphabetic() || matches!(c, ' ' | '.' | '\'' | '-' | '(' | ')'))
        };
        anyhow::ensure!(
            station_ok(o) && station_ok(d),
            "Controleer de stationsnamen (maximaal 60 tekens). / Check the station names (at most 60 characters)."
        );
        // Each name stands for exactly one station (an unknown or partial name would match the
        // wrong trips, or none): from the station list, or as NS names it in the stored trips.
        let Some(o) = self.resolve_station(o)? else {
            anyhow::bail!(
                "Vertrekstation niet gevonden. Kies een station uit de lijst. / Departure station not found. Pick a station from the list."
            )
        };
        let Some(d) = self.resolve_station(d)? else {
            anyhow::bail!(
                "Aankomststation niet gevonden. Kies een station uit de lijst. / Arrival station not found. Pick a station from the list."
            )
        };
        anyhow::ensure!(o.uic != d.uic, "Vertrek- en aankomststation zijn hetzelfde. / The departure and arrival station are the same.");
        let entry =
            ExternalEntry { id: 0, origin: Some(o.name.clone()), dest: Some(d.name.clone()), uics: Some((o.uic.clone(), d.uic.clone())) };
        // The same entry twice adds nothing (and would have to be removed twice).
        let dup_entry = self.external_entries(travel_date)?.iter().any(|e| match &e.uics {
            Some(u) => *u == (o.uic.clone(), d.uic.clone()),
            None => e.matches(&o.name, &o.uic, &d.name, &d.uic),
        });
        anyhow::ensure!(
            !dup_entry,
            "Deze reis staat al in je lijst met reizen die elders geclaimd zijn. / This trip is already in your list of trips claimed elsewhere."
        );
        // A matching trip that already has a claim (Ingediend, Elders geclaimd, ...) needs no entry.
        let local = |j: &Journey| j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
        let from = ts((travel_date.pred_opt().unwrap_or(travel_date)).and_hms_opt(0, 0, 0).unwrap().and_utc());
        for (jid, j) in self.journeys_since(parse_ts(&from)?)? {
            if local(&j) != travel_date || !entry.matches_journey(&j) {
                continue;
            }
            if let Some(c) = self.claim_for_journey(jid)?.filter(|c| c.status.blocks_new_submission()) {
                anyhow::bail!(
                    "Voor deze reis is al een claim ({}). Je hoeft hem niet als elders geclaimd toe te voegen. / This trip already has a claim ({}). There is no need to add it as claimed elsewhere.",
                    status_nl(c.status),
                    status_en(c.status)
                );
            }
        }
        // The full station names are stored (and shown); the note keeps the stations themselves
        // ("uic:<from>|<to>", then the caller's note), so trips are matched by station.
        let note = match note {
            Some(n) => format!("{EXTERNAL_UIC_PREFIX}{}|{} {n}", o.uic, d.uic),
            None => format!("{EXTERNAL_UIC_PREFIX}{}|{}", o.uic, d.uic),
        };
        self.conn.execute(
            "INSERT INTO external_claims(travel_date,origin_text,dest_text,source,note,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![travel_date.to_string(), o.name, d.name, source, note, ts(Utc::now())],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The one station a typed name stands for (see [`crate::stations::StationIndex::resolve`]),
    /// else the one station of the stored trips with exactly that name. `None`: unknown or more
    /// than one.
    pub fn resolve_station(&self, typed: &str) -> Result<Option<crate::stations::Station>> {
        let hits = crate::stations::STATIONS.resolve(typed);
        if !hits.is_empty() {
            return Ok((hits.len() == 1).then(|| hits[0].clone()));
        }
        let t = station_words(typed);
        let mut st = self.conn.prepare(
            "SELECT DISTINCT origin_name, origin_uic FROM journeys UNION SELECT DISTINCT dest_name, dest_uic FROM journeys WHERE dest_uic IS NOT NULL",
        )?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut found: Vec<crate::stations::Station> = vec![];
        for (name, uic) in rows {
            let (Some(name), Some(uic)) = (name, uic) else { continue };
            if !t.is_empty() && station_words(&name) == t && !found.iter().any(|s| s.uic == uic) {
                found.push(crate::stations::Station { code: String::new(), uic, name });
            }
        }
        Ok((found.len() == 1).then(|| found.remove(0)))
    }

    /// The "claimed elsewhere" entries of one travel day.
    fn external_entries(&self, day: chrono::NaiveDate) -> Result<Vec<ExternalEntry>> {
        let mut st = self.conn.prepare("SELECT id, origin_text, dest_text, note FROM external_claims WHERE travel_date=?1")?;
        let rows = st
            .query_map([day.to_string()], |r| {
                let note: Option<String> = r.get(3)?;
                Ok(ExternalEntry { id: r.get(0)?, origin: r.get(1)?, dest: r.get(2)?, uics: note.as_deref().and_then(parse_external_uics) })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Apply the "claimed elsewhere" entries to the stored trips of `travel_date` right away, the
    /// same way a check does: a matching trip whose claim is still open becomes "claimed
    /// elsewhere" (so it can no longer be submitted). Returns the claims that changed.
    pub fn apply_external_claims(&mut self, gateway_id: &str, travel_date: chrono::NaiveDate) -> Result<Vec<i64>> {
        let local = |j: &Journey| j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
        let from = travel_date.pred_opt().unwrap_or(travel_date).and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
        let mut changed = vec![];
        for (jid, j) in self.journeys_since(from)? {
            if local(&j) != travel_date || !self.has_external_claim(&j)? {
                continue;
            }
            let c = self.ensure_claim(jid, gateway_id, ClaimState::Discovered)?;
            if !c.status.blocks_new_submission() {
                self.mark_external(c.id, "import")?;
                changed.push(c.id);
            }
        }
        Ok(changed)
    }

    /// Stored trips that "claimed elsewhere" entry `entry_id` matches and that are still open
    /// (no claim yet, or one that can still be submitted): what marking it would change.
    pub fn open_trips_matching_external(&self, entry_id: i64) -> Result<Vec<Journey>> {
        let date: Option<String> =
            self.conn.query_row("SELECT travel_date FROM external_claims WHERE id=?1", [entry_id], |r| r.get(0)).optional()?;
        let Some(day) = date.and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()) else { return Ok(vec![]) };
        let from = day.pred_opt().unwrap_or(day).and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
        let mut out = vec![];
        for (jid, j) in self.journeys_since(from)? {
            if j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive() != day
                || !self.matching_external_ids(&j)?.contains(&entry_id)
            {
                continue;
            }
            if !self.claim_for_journey(jid)?.is_some_and(|c| c.status.blocks_new_submission()) {
                out.push(j);
            }
        }
        Ok(out)
    }

    /// True if the user recorded an external claim on the same local date whose
    /// station text (if given) matches the journey. Conservative: date-only
    /// records block all journeys on that date.
    pub fn has_external_claim(&self, j: &Journey) -> Result<bool> {
        Ok(!self.matching_external_ids(j)?.is_empty())
    }

    /// "Claimed elsewhere" entries that match this journey (same local date, both stations).
    pub fn matching_external_ids(&self, j: &Journey) -> Result<Vec<i64>> {
        let day = j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
        Ok(self.external_entries(day)?.into_iter().filter(|e| e.matches_journey(j)).map(|e| e.id).collect())
    }

    pub fn external_claims(&self) -> Result<Vec<(i64, String, Option<String>, Option<String>, String)>> {
        let mut st =
            self.conn.prepare("SELECT id, travel_date, origin_text, dest_text, source FROM external_claims ORDER BY travel_date DESC")?;
        let rows =
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_external_claim(&mut self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM external_claims WHERE id=?1", [id])?;
        self.release_unmatched_imports()?;
        Ok(())
    }

    /// Claims that were blocked only because an "already claimed elsewhere" entry matched them
    /// go back to review once no entry matches any more (nothing was sent by this app).
    pub fn release_unmatched_imports(&mut self) -> Result<usize> {
        let ids: Vec<(i64, i64)> = {
            let mut st = self.conn.prepare(
                "SELECT cl.id, cl.journey_id FROM claims cl WHERE cl.status='EXTERNALLY_CLAIMED' AND cl.ns_reference IS NULL
                   AND cl.paid_cents IS NULL
                   AND (SELECT actor FROM claim_events e WHERE e.claim_id=cl.id ORDER BY e.id DESC LIMIT 1) = 'import'",
            )?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            v
        };
        let mut n = 0;
        for (cid, jid) in ids {
            if !self.has_external_claim(&self.journey(jid)?)? {
                self.undo_external(cid, "import")?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Undo "claimed elsewhere" (a mis-click or a wrong import). Only for claims this app never
    /// sent and without a recorded payout; the journey is re-assessed on the next check.
    pub fn undo_external(&mut self, claim_id: i64, actor: &str) -> Result<ClaimRow> {
        let c = self.claim(claim_id)?;
        anyhow::ensure!(
            c.status == ClaimState::ExternallyClaimed,
            "Deze claim staat niet op Elders geclaimd. / This claim is not marked as Claimed elsewhere."
        );
        let j = self.journey(c.journey_id)?;
        // User/import marks and legacy date-matched mail marks are reversible; NS-list proof is kept.
        let marked_by: Option<String> = self
            .conn
            .query_row(
                "SELECT actor FROM claim_events WHERE claim_id=?1 AND to_status='EXTERNALLY_CLAIMED' ORDER BY id DESC LIMIT 1",
                [claim_id],
                |r| r.get(0),
            )
            .optional()?;
        anyhow::ensure!(
            matches!(marked_by.as_deref(), Some("user") | Some("import") | Some("ns-mail")),
            "NS heeft deze claim zelf gemeld, dus die blijft staan. / NS itself reported this claim, so it stays."
        );
        let legacy_mail = marked_by.as_deref() == Some("ns-mail")
            && c.submitted_at.is_none()
            && self.conn.query_row("SELECT submitted_here=0 FROM claims WHERE id=?1", [claim_id], |r| r.get::<_, bool>(0))?
            && self.conn.query_row("SELECT COUNT(*)=0 FROM claim_events WHERE claim_id=?1 AND actor='reconcile'", [claim_id], |r| {
                r.get::<_, bool>(0)
            })?;
        anyhow::ensure!(legacy_mail || (c.ns_reference.is_none() && self.paid_cents(claim_id)?.is_none()),
            "Deze claim heeft een NS-referentie of uitbetaling en blijft daarom staan. / This claim has an NS reference or payout, so it stays.");
        if legacy_mail {
            // Preserve a caution flag before clearing a formerly applied date-only result.
            self.conn.execute(
                "UPDATE ns_mail SET hint_key=?2, journey_id=NULL, claim_id=NULL WHERE claim_id=?1",
                params![claim_id, j.natural_key()],
            )?;
        }
        if actor == "user" {
            // remove the import entries that match THIS trip, otherwise the next check re-blocks it
            for id in self.matching_external_ids(&j)? {
                self.conn.execute("DELETE FROM external_claims WHERE id=?1", [id])?;
            }
        }
        self.transition_with(
            claim_id,
            Some(ClaimState::ExternallyClaimed),
            ClaimState::NeedsReview,
            actor,
            "Toch niet elders geclaimd / Not claimed elsewhere after all",
            Some((
                if legacy_mail {
                    "UPDATE claims SET external_prior=0, ns_reference=NULL, paid_cents=NULL, commission_pct=NULL WHERE id=?1"
                } else {
                    "UPDATE claims SET external_prior=0 WHERE id=?1"
                },
                &[&claim_id],
            )),
        )
    }

    // ---------- claims state machine ----------
    pub fn claim_for_journey(&self, journey_id: i64) -> Result<Option<ClaimRow>> {
        self.conn.query_row("SELECT * FROM claims WHERE journey_id=?1", [journey_id], Self::row_to_claim).optional().map_err(Into::into)
    }

    pub fn claim(&self, id: i64) -> Result<ClaimRow> {
        Ok(self.conn.query_row("SELECT * FROM claims WHERE id=?1", [id], Self::row_to_claim)?)
    }

    fn row_to_claim(r: &rusqlite::Row) -> rusqlite::Result<ClaimRow> {
        let st: String = r.get("status")?;
        Ok(ClaimRow {
            id: r.get("id")?,
            journey_id: r.get("journey_id")?,
            status: ClaimState::parse(&st).unwrap_or(ClaimState::Indeterminate),
            facts: r.get::<_, Option<String>>("facts_json")?.and_then(|s| serde_json::from_str(&s).ok()),
            authorized_mode: r.get("authorized_mode")?,
            submitted_at: r.get("submitted_at")?,
            ns_reference: r.get("ns_reference")?,
            external_prior: r.get::<_, i64>("external_prior")? != 0,
            attempts: r.get("attempts")?,
            updated_at: r.get("updated_at")?,
        })
    }

    pub fn claims_in(&self, states: &[ClaimState]) -> Result<Vec<ClaimRow>> {
        let list = states.iter().map(|s| format!("'{}'", s.as_str())).collect::<Vec<_>>().join(",");
        let mut st = self.conn.prepare(&format!("SELECT * FROM claims WHERE status IN ({list}) ORDER BY updated_at DESC"))?;
        let rows = st.query_map([], Self::row_to_claim)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn all_claims(&self) -> Result<Vec<ClaimRow>> {
        self.claims_in(&ClaimState::ALL)
    }

    /// Create the ledger entry for a journey, or return the existing one (idempotent).
    pub fn ensure_claim(&mut self, journey_id: i64, gateway: &str, initial: ClaimState) -> Result<ClaimRow> {
        if let Some(c) = self.claim_for_journey(journey_id)? {
            return Ok(c);
        }
        let j = self.journey(journey_id)?;
        let now = ts(Utc::now());
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO claims(journey_id,journey_key,gateway,status,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5)",
            params![journey_id, j.fallback_key(), gateway, initial.as_str(), now],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO claim_events(claim_id,at,from_status,to_status,actor,detail) VALUES(?1,?2,NULL,?3,'system','Aangemaakt / Created')",
            params![id, now, initial.as_str()],
        )?;
        tx.commit()?;
        self.claim(id)
    }

    /// Durable, validated state transition, journaled in the same transaction.
    pub fn transition(
        &mut self,
        claim_id: i64,
        expected_from: Option<ClaimState>,
        to: ClaimState,
        actor: &str,
        detail: &str,
    ) -> Result<ClaimRow> {
        self.transition_with(claim_id, expected_from, to, actor, detail, None)
    }

    /// [`Db::transition`] and the setting `key` = `value` written in the SAME transaction (e.g. the
    /// in-flight marker of a submission together with SUBMITTING).
    #[allow(clippy::too_many_arguments)]
    pub fn transition_and_set(
        &mut self,
        claim_id: i64,
        expected_from: Option<ClaimState>,
        to: ClaimState,
        actor: &str,
        detail: &str,
        key: &str,
        value: &str,
    ) -> Result<ClaimRow> {
        self.transition_and_set_checked(claim_id, expected_from, to, actor, detail, key, value, |_| Ok(()))
    }

    /// Checks current data under the same SQLite write reservation as the transition and setting.
    #[allow(clippy::too_many_arguments)]
    pub fn transition_and_set_checked(
        &mut self,
        claim_id: i64,
        expected_from: Option<ClaimState>,
        to: ClaimState,
        actor: &str,
        detail: &str,
        key: &str,
        value: &str,
        check: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<ClaimRow> {
        self.transition_with_checked(
            claim_id,
            expected_from,
            to,
            actor,
            detail,
            Some(("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", &[&key, &value])),
            check,
        )
    }

    /// The linked cards (coupling ids) in the ledger whose number ends in `last4`.
    pub fn couplings_for_last4(&self, last4: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT DISTINCT coupling_id FROM journeys WHERE card_last4=?1 AND coupling_id IS NOT NULL")?;
        let rows = st.query_map([last4], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Journeys of one linked card (`coupling_id`) in the ledger that checked in within [from, to].
    pub fn journey_count_for_coupling(&self, coupling_id: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM journeys WHERE coupling_id=?1 AND check_in_utc>=?2 AND check_in_utc<=?3",
            params![coupling_id, ts(from), ts(to)],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// [`Db::transition`] plus an extra `UPDATE claims ... WHERE id=?1` applied in the SAME
    /// transaction (only when the transition itself is valid).
    fn transition_with(
        &mut self,
        claim_id: i64,
        expected_from: Option<ClaimState>,
        to: ClaimState,
        actor: &str,
        detail: &str,
        extra_update: Option<(&str, &[&dyn rusqlite::ToSql])>,
    ) -> Result<ClaimRow> {
        self.transition_with_checked(claim_id, expected_from, to, actor, detail, extra_update, |_| Ok(()))
    }

    #[allow(clippy::too_many_arguments)]
    fn transition_with_checked(
        &self,
        claim_id: i64,
        expected_from: Option<ClaimState>,
        to: ClaimState,
        actor: &str,
        detail: &str,
        extra_update: Option<(&str, &[&dyn rusqlite::ToSql])>,
        check: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<ClaimRow> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        check(self)?;
        let cur: String = tx.query_row("SELECT status FROM claims WHERE id=?1", [claim_id], |r| r.get(0))?;
        let cur = ClaimState::parse(&cur).ok_or_else(|| anyhow!("corrupt status {cur}"))?;
        if let Some(exp) = expected_from {
            if cur != exp {
                bail!(
                    "De status van deze claim is intussen veranderd ({1}, verwacht {0}). Vernieuw en probeer het opnieuw. / The status of this claim changed in the meantime ({1}, expected {0}). Refresh and try again.",
                    exp.as_str(),
                    cur.as_str()
                );
            }
        }
        if !cur.can_transition_to(to) {
            bail!(
                "Deze statuswijziging kan niet (van {0} naar {1}). / This status change is not possible (from {0} to {1}).",
                cur.as_str(),
                to.as_str()
            );
        }
        if to == ClaimState::Submitting && inflight_same_carrier(&tx, claim_id)? {
            bail!(INFLIGHT_SAME_CARRIER_MSG);
        }
        let now = ts(Utc::now());
        tx.execute("UPDATE claims SET status=?2, updated_at=?3 WHERE id=?1", params![claim_id, to.as_str(), now])?;
        if cur == ClaimState::Rejected {
            // The "nothing paid" (0) recorded with the rejection no longer holds.
            tx.execute("UPDATE claims SET paid_cents=NULL WHERE id=?1 AND paid_cents=0", [claim_id])?;
        }
        if let Some((sql, args)) = extra_update {
            tx.execute(sql, args)?;
        }
        if cur == ClaimState::Submitting {
            tx.execute("DELETE FROM settings WHERE key=?1", [format!("{}{claim_id}", crate::pipeline::INFLIGHT_KEY_PREFIX)])?;
        }
        tx.execute(
            "INSERT INTO claim_events(claim_id,at,from_status,to_status,actor,detail) VALUES(?1,?2,?3,?4,?5,?6)",
            params![claim_id, now, cur.as_str(), to.as_str(), actor, crate::redact::redact(detail)],
        )?;
        tx.commit()?;
        self.claim(claim_id)
    }

    /// Record facts + authorization and move READY -> SUBMITTING atomically,
    /// BEFORE any network side effect. Fails if any claim for the same journey
    /// is already in a blocking state, or another claim for the same carrier (NS or Arriva) is
    /// being submitted.
    pub fn begin_submission(&mut self, claim_id: i64, facts: &ClaimFacts, mode: AuthorizedMode) -> Result<ClaimRow> {
        self.begin_submission_inner(claim_id, facts, mode, None)
    }

    /// [`Db::begin_submission`] for a check that holds the run lock as `owner`. In the same
    /// IMMEDIATE transaction the lock must still be this owner's with an unexpired lease;
    /// otherwise nothing changes and the submission is refused (another check may have taken over).
    pub fn begin_submission_under_lock(
        &mut self,
        claim_id: i64,
        facts: &ClaimFacts,
        mode: AuthorizedMode,
        owner: &str,
    ) -> Result<ClaimRow> {
        self.begin_submission_inner(claim_id, facts, mode, Some(owner))
    }

    fn begin_submission_inner(&mut self, claim_id: i64, facts: &ClaimFacts, mode: AuthorizedMode, owner: Option<&str>) -> Result<ClaimRow> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(owner) = owner {
            let held: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM run_lock WHERE id=1 AND owner=?1 AND expires_at > ?2)",
                params![owner, ts(Utc::now())],
                |r| r.get(0),
            )?;
            if !held {
                bail!(LOCK_LOST_MSG);
            }
        }
        let (cur, journey_id): (String, i64) =
            tx.query_row("SELECT status, journey_id FROM claims WHERE id=?1", [claim_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let cur = ClaimState::parse(&cur).ok_or_else(|| anyhow!("corrupt status"))?;
        if cur != ClaimState::Ready {
            bail!("claim {claim_id} is {} — only READY claims can be submitted", cur.as_str());
        }
        let blocking: i64 = tx.query_row(
            "SELECT COUNT(*) FROM claims WHERE journey_id=?1 AND id<>?2 AND status IN ('SUBMITTING','SUBMITTED','INDETERMINATE','APPROVED','REJECTED','PAID','EXTERNALLY_CLAIMED')",
            params![journey_id, claim_id],
            |r| r.get(0),
        )?;
        if blocking > 0 {
            bail!("another claim for this journey is already in flight or done");
        }
        // At most one claim per carrier in flight: an Arriva claim being submitted never holds up
        // NS (it can never become an NS duplicate), nor the other way round.
        if inflight_same_carrier(&tx, claim_id)? {
            bail!(INFLIGHT_SAME_CARRIER_MSG);
        }
        let now = ts(Utc::now());
        tx.execute(
            "UPDATE claims SET status='SUBMITTING', facts_json=?2, authorized_mode=?3, authorized_at=?4, attempts=attempts+1, updated_at=?4, submitted_here=1 WHERE id=?1",
            params![claim_id, serde_json::to_string(facts)?, mode.as_str(), now],
        )?;
        if let Some(owner) = owner {
            tx.execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![format!("{}{claim_id}", crate::pipeline::INFLIGHT_KEY_PREFIX), format!("{now}|{owner}")],
            )?;
        }
        tx.execute(
            "INSERT INTO claim_events(claim_id,at,from_status,to_status,actor,detail) VALUES(?1,?2,'READY','SUBMITTING',?3,?4)",
            params![claim_id, now, mode.as_str(), authorized_detail(mode, facts.delay_band)],
        )?;
        tx.commit()?;
        self.claim(claim_id)
    }

    /// Undo a move to READY that was made right before a submission attempt (an automatic retry
    /// of a FAILED claim, or the user's approval of a FAILED, NEEDS_REVIEW or CANDIDATE claim)
    /// when that attempt was refused or failed before anything was sent: the claim returns to
    /// `prev` with `detail` (the reason) journaled. Only when the claim is still READY and its
    /// last history entry is exactly `prev` -> READY; otherwise nothing changes (`false`). This is
    /// the only way back from READY to FAILED (not a transition anyone else may make).
    pub fn restore_after_refusal(&mut self, claim_id: i64, prev: ClaimState, actor: &str, detail: &str) -> Result<bool> {
        if !matches!(prev, ClaimState::Failed | ClaimState::NeedsReview | ClaimState::Candidate) {
            return Ok(false);
        }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cur: String = tx.query_row("SELECT status FROM claims WHERE id=?1", [claim_id], |r| r.get(0))?;
        let last: Option<(Option<String>, String)> = tx
            .query_row("SELECT from_status, to_status FROM claim_events WHERE claim_id=?1 ORDER BY id DESC LIMIT 1", [claim_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let approved_from_prev = last.as_ref().is_some_and(|(f, t)| f.as_deref() == Some(prev.as_str()) && t == ClaimState::Ready.as_str());
        if cur != ClaimState::Ready.as_str() || !approved_from_prev {
            return Ok(false);
        }
        let now = ts(Utc::now());
        tx.execute("UPDATE claims SET status=?2, updated_at=?3 WHERE id=?1", params![claim_id, prev.as_str(), now])?;
        tx.execute(
            "INSERT INTO claim_events(claim_id,at,from_status,to_status,actor,detail) VALUES(?1,?2,'READY',?3,?4,?5)",
            params![claim_id, now, prev.as_str(), actor, crate::redact::redact(detail)],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// A claim request to NS was observed while the claim form was only being tried ("Eerst
    /// proberen", nothing should be sent). The claim may have reached NS: record it as
    /// INDETERMINATE (via SUBMITTING, journaled in one transaction) so it is never sent again
    /// without reconciliation. Claims already sent, in flight or uncertain are left as they are.
    pub fn record_write_during_preflight(&mut self, claim_id: i64, facts: &ClaimFacts) -> Result<ClaimRow> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cur: String = tx.query_row("SELECT status FROM claims WHERE id=?1", [claim_id], |r| r.get(0))?;
        let cur = ClaimState::parse(&cur).ok_or_else(|| anyhow!("corrupt status {cur}"))?;
        if !matches!(
            cur,
            ClaimState::Discovered
                | ClaimState::Candidate
                | ClaimState::NeedsReview
                | ClaimState::Ready
                | ClaimState::Failed
                | ClaimState::Dismissed
        ) {
            drop(tx);
            return self.claim(claim_id);
        }
        let now = ts(Utc::now());
        tx.execute(
            "UPDATE claims SET status='INDETERMINATE', facts_json=?2, authorized_mode=?3, authorized_at=?4, attempts=attempts+1, updated_at=?4, submitted_here=1 WHERE id=?1",
            params![claim_id, serde_json::to_string(facts)?, AuthorizedMode::Manual.as_str(), now],
        )?;
        tx.execute(
            "INSERT INTO claim_events(claim_id,at,from_status,to_status,actor,detail) VALUES(?1,?2,?3,'SUBMITTING','preflight',?4)",
            params![claim_id, now, cur.as_str(), "Formulier geprobeerd / Form tried"],
        )?;
        tx.execute(
            "INSERT INTO claim_events(claim_id,at,from_status,to_status,actor,detail) VALUES(?1,?2,'SUBMITTING','INDETERMINATE','gateway',?3)",
            params![claim_id, now, PREFLIGHT_WRITE_DETAIL],
        )?;
        tx.commit()?;
        self.claim(claim_id)
    }

    pub(crate) fn record_not_sent(&mut self, claim_id: i64, detail: &str, restore_attempt: bool) -> Result<ClaimRow> {
        self.transition_with(
            claim_id, Some(ClaimState::Submitting), ClaimState::Failed, "gateway", detail,
            restore_attempt.then_some(("UPDATE claims SET attempts=MAX(0, attempts-1) WHERE id=?1", &[&claim_id as &dyn rusqlite::ToSql])),
        )
    }

    /// Record a confirmed receipt exactly once.
    pub fn complete_submission(&mut self, claim_id: i64, receipt: &SubmissionReceipt) -> Result<ClaimRow> {
        self.store_receipt(claim_id, ClaimState::Submitting, receipt, "gateway")
    }

    pub(crate) fn recover_receipt(&mut self, claim_id: i64, receipt: &SubmissionReceipt) -> Result<ClaimRow> {
        self.store_receipt(claim_id, ClaimState::Indeterminate, receipt, "reconcile")
    }

    fn store_receipt(&mut self, claim_id: i64, from: ClaimState, receipt: &SubmissionReceipt, actor: &str) -> Result<ClaimRow> {
        let to = if receipt.confirmed { ClaimState::Submitted } else { ClaimState::Indeterminate };
        let now = ts(Utc::now());
        self.transition_with(
            claim_id,
            Some(from),
            to,
            actor,
            &receipt.detail,
            Some((
                "UPDATE claims SET submitted_at=COALESCE(submitted_at, ?2), ns_reference=COALESCE(ns_reference, ?3) WHERE id=?1",
                &[&claim_id, &now, &receipt.reference],
            )),
        )
    }

    /// Called at startup: any SUBMITTING left behind by a crash becomes INDETERMINATE.
    pub fn recover_inflight(&mut self) -> Result<Vec<i64>> {
        self.recover_inflight_except(&[])
    }

    /// Like [`Db::recover_inflight`], leaving the claims in `skip` (a submission another,
    /// still running check is busy with) as they are.
    pub fn recover_inflight_except(&mut self, skip: &[i64]) -> Result<Vec<i64>> {
        let ids: Vec<i64> = self.claims_in(&[ClaimState::Submitting])?.into_iter().map(|c| c.id).filter(|id| !skip.contains(id)).collect();
        for id in &ids {
            self.transition(
                *id,
                Some(ClaimState::Submitting),
                ClaimState::Indeterminate,
                "recovery",
                "Het programma stopte tijdens het indienen. Controleer de claim bij de vervoerder. / The app stopped during submission. Check the claim with the carrier.",
            )?;
        }
        Ok(ids)
    }

    pub(crate) fn reconcile_decision(
        &mut self, claim_id: i64, from: ClaimState, to: ClaimState, detail: &str, amount: Option<i64>,
    ) -> Result<ClaimRow> {
        self.transition_with(
            claim_id, Some(from), to, "reconcile", detail,
            Some(("UPDATE claims SET paid_cents=COALESCE(paid_cents, ?2) WHERE id=?1", &[&claim_id, &amount])),
        )
    }

    pub fn set_ns_reference(&self, claim_id: i64, reference: &str) -> Result<()> {
        self.conn.execute("UPDATE claims SET ns_reference=COALESCE(ns_reference, ?2) WHERE id=?1", params![claim_id, reference])?;
        Ok(())
    }

    pub fn mark_external(&mut self, claim_id: i64, actor: &str) -> Result<ClaimRow> {
        self.mark_external_detail(claim_id, actor, "Gemarkeerd als Elders geclaimd / Marked as claimed elsewhere")
    }

    /// Like [`Db::mark_external`] with a custom event line. `external_prior` is set in the same
    /// transaction as the state change (never on an illegal transition).
    pub fn mark_external_detail(&mut self, claim_id: i64, actor: &str, detail: &str) -> Result<ClaimRow> {
        self.mark_external_with_reference(claim_id, actor, detail, None)
    }

    pub(crate) fn mark_external_with_reference(
        &mut self, claim_id: i64, actor: &str, detail: &str, reference: Option<&str>,
    ) -> Result<ClaimRow> {
        self.transition_with(
            claim_id,
            None,
            ClaimState::ExternallyClaimed,
            actor,
            detail,
            Some(("UPDATE claims SET external_prior=1, ns_reference=COALESCE(ns_reference, ?2) WHERE id=?1", &[&claim_id, &reference])),
        )
    }

    /// True when a submission of this claim may have reached NS without being confirmed: it was
    /// ever INDETERMINATE (an event into or out of it), or it left SUBMITTING as FAILED by anyone other than the gateway's own
    /// "conclusively not sent" result. Such a claim is never submitted again automatically, and
    /// manually only after an explicit confirmation (duplicate risk at NS).
    pub fn was_uncertain(&self, claim_id: i64) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM claim_events WHERE claim_id=?1 AND (to_status='INDETERMINATE' OR from_status='INDETERMINATE'
               OR (from_status='SUBMITTING' AND to_status='FAILED' AND actor<>'gateway'))",
            [claim_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// A status change the user makes by hand in the GUI. Only outcomes the user learned from NS
    /// (or "don't claim") can be set; a claim that is SUBMITTING is never touched, and FAILED
    /// ("not sent") is only accepted for an uncertain (INDETERMINATE) submission. A REJECTED
    /// claim can be corrected to APPROVED / PAID (e.g. NS reversed its decision, or an e-mail was
    /// tied to the wrong trip); the transition rules are those of [`ClaimState::can_transition_to`].
    pub fn set_status_by_user(&mut self, claim_id: i64, to: ClaimState) -> Result<ClaimRow> {
        anyhow::ensure!(
            matches!(
                to,
                ClaimState::Dismissed
                    | ClaimState::Approved
                    | ClaimState::Rejected
                    | ClaimState::Paid
                    | ClaimState::Submitted
                    | ClaimState::Failed
                    | ClaimState::NeedsReview
            ),
            "Deze status kun je niet zelf instellen. / You cannot set this status yourself."
        );
        let cur = self.claim(claim_id)?.status;
        anyhow::ensure!(
            cur != ClaimState::Submitting,
            "Deze claim wordt nu verzonden. Wacht tot dat klaar is. / This claim is being submitted right now. Wait until it has finished."
        );
        if to == ClaimState::Failed {
            anyhow::ensure!(
                cur == ClaimState::Indeterminate,
                "Alleen een claim met onzekere uitkomst kan als niet verzonden worden gemarkeerd. / Only a claim with an uncertain outcome can be marked as not sent."
            );
        }
        if to == ClaimState::NeedsReview {
            anyhow::ensure!(
                cur == ClaimState::Dismissed,
                "Alleen een reis die je niet wilde claimen kan terug naar Beoordelen. / Only a trip you chose not to claim can go back to Review."
            );
            // "Review anyway" restores the state the USER dismissed it from (a READY claim stays
            // READY); after a dismissal by the policy it goes to review.
            let restore = self.state_before_user_dismissal(claim_id)?.unwrap_or(ClaimState::NeedsReview);
            return self.transition(claim_id, Some(cur), restore, "user", "Door jou teruggezet / Restored by you");
        }
        self.transition(claim_id, Some(cur), to, "user", "Door jou aangepast / Changed by you")
    }

    /// The state a claim had when the user chose "don't claim" (only READY / NEEDS_REVIEW /
    /// CANDIDATE), if the latest dismissal was the user's.
    fn state_before_user_dismissal(&self, claim_id: i64) -> Result<Option<ClaimState>> {
        let last: Option<(Option<String>, String)> = self
            .conn
            .query_row(
                "SELECT from_status, actor FROM claim_events WHERE claim_id=?1 AND to_status='DISMISSED' AND from_status<>'DISMISSED' ORDER BY id DESC LIMIT 1",
                [claim_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(last
            .filter(|(_, actor)| actor == "user")
            .and_then(|(from, _)| from.as_deref().and_then(ClaimState::parse))
            .filter(|s| matches!(s, ClaimState::Ready | ClaimState::NeedsReview | ClaimState::Candidate)))
    }

    pub fn events(&self, claim_id: i64) -> Result<Vec<ClaimEvent>> {
        let mut st =
            self.conn.prepare("SELECT at, from_status, to_status, actor, detail FROM claim_events WHERE claim_id=?1 ORDER BY id")?;
        let rows = st
            .query_map([claim_id], |r| {
                Ok(ClaimEvent {
                    at: r.get(0)?,
                    from_status: r.get(1)?,
                    to_status: r.get(2)?,
                    actor: r.get(3)?,
                    detail: history_detail(&r.get::<_, String>(4)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---------- run lock ----------
    /// Prevents concurrent runs (GUI scan + scheduled task). Expiring lease.
    pub fn acquire_run_lock(&self, owner: &str, ttl: chrono::Duration) -> Result<bool> {
        let now = Utc::now();
        let n = self.conn.execute(
            "INSERT INTO run_lock(id,owner,expires_at) VALUES(1,?1,?2)
             ON CONFLICT(id) DO UPDATE SET owner=excluded.owner, expires_at=excluded.expires_at WHERE run_lock.expires_at < ?3 OR run_lock.owner = ?1",
            params![owner, ts(now + ttl), ts(now)],
        )?;
        Ok(n == 1)
    }
    /// Owner of the current lease while it has not expired.
    pub fn live_run_lock_owner(&self) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT owner FROM run_lock WHERE id=1 AND expires_at >= ?1", [ts(Utc::now())], |r| r.get(0)).optional()?)
    }
    /// Owner string of the current lease, if any.
    pub fn run_lock_owner(&self) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT owner FROM run_lock WHERE id=1", [], |r| r.get(0)).optional()?)
    }

    /// Break a lease whose owning process is gone and close its unfinished runs.
    pub fn break_stale_lock(&self, owner: &str) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if tx.execute("DELETE FROM run_lock WHERE owner=?1", [owner])? == 1 {
            tx.execute(
                "UPDATE runs SET completed_at=started_at, outcome='interrupted', error_summary='Het programma stopte tijdens de controle / The app stopped during the check' WHERE completed_at IS NULL",
                [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Extend our own lease (long runs must not lose the lock to another process).
    /// Fails when the lease is no longer ours (expired and taken over, or erased): the caller
    /// must stop before doing anything with side effects.
    pub fn renew_run_lock(&self, owner: &str, ttl: chrono::Duration) -> Result<()> {
        let n = self.conn.execute("UPDATE run_lock SET expires_at=?2 WHERE id=1 AND owner=?1", params![owner, ts(Utc::now() + ttl)])?;
        anyhow::ensure!(
            n == 1,
            "De controle is gestopt: de vergrendeling is verlopen of overgenomen. / The check stopped: its lock expired or was taken over."
        );
        Ok(())
    }

    pub fn release_run_lock(&self, owner: &str) -> Result<()> {
        self.conn.execute("DELETE FROM run_lock WHERE owner=?1", [owner])?;
        Ok(())
    }

    // ---------- runs ----------
    pub fn start_run(&self, mode: &str, trigger: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO runs(started_at,mode,trigger) VALUES(?1,?2,?3)", params![ts(Utc::now()), mode, trigger])?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn finish_run(&self, run_id: i64, outcome: &str, summary: &RunSummary) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET completed_at=?2, outcome=?3, summary_json=?4, error_summary=?5, auth_state=?6 WHERE id=?1",
            params![
                run_id,
                ts(Utc::now()),
                outcome,
                serde_json::to_string(summary)?,
                crate::redact::redact(&summary.errors.join("; ")),
                summary.auth_state
            ],
        )?;
        Ok(())
    }
    pub fn runs(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        let mut st = self.conn.prepare("SELECT id, started_at, completed_at, mode, trigger, outcome, summary_json, error_summary, auth_state FROM runs ORDER BY id DESC LIMIT ?1")?;
        let rows = st
            .query_map([limit], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?, "started_at": r.get::<_, String>(1)?, "completed_at": r.get::<_, Option<String>>(2)?,
                    "mode": r.get::<_, String>(3)?, "trigger": r.get::<_, String>(4)?, "outcome": r.get::<_, Option<String>>(5)?,
                    "summary": r.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error_summary": r.get::<_, Option<String>>(7)?, "auth_state": r.get::<_, Option<String>>(8)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// End of the last successful background check (Task Scheduler: trigger `cli` or `deadline`)
    /// in this mode. Checks from the app submit only urgent claims, so they never count here.
    pub fn last_scheduled_ok_run_at(&self, mode: &str) -> Result<Option<DateTime<Utc>>> {
        let s: Option<String> = self.conn.query_row(
            "SELECT MAX(completed_at) FROM runs WHERE outcome='ok' AND trigger IN ('cli','deadline') AND mode=?1",
            [mode],
            |r| r.get(0),
        )?;
        Ok(s.and_then(|s| parse_ts(&s).ok()))
    }

    pub fn last_completed_run_at(&self) -> Result<Option<DateTime<Utc>>> {
        let s: Option<String> = self.conn.query_row("SELECT MAX(completed_at) FROM runs WHERE outcome IS NOT NULL", [], |r| r.get(0))?;
        Ok(s.and_then(|s| parse_ts(&s).ok()))
    }

    // ---------- notifications ----------
    /// Returns false if this (run, channel) was already sent — prevents duplicate summaries.
    pub fn begin_notification(&self, run_id: i64, channel: &str) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT INTO notifications(run_id,channel,status,attempted_at) VALUES(?1,?2,'attempting',?3)
             ON CONFLICT(run_id,channel) DO UPDATE SET status='attempting', attempted_at=excluded.attempted_at WHERE notifications.status <> 'sent'",
            params![run_id, channel, ts(Utc::now())],
        )?;
        Ok(n == 1)
    }
    pub fn finish_notification(&self, run_id: i64, channel: &str, result: &Result<()>) -> Result<()> {
        match result {
            Ok(()) => self.conn.execute(
                "UPDATE notifications SET status='sent', sent_at=?3, error=NULL WHERE run_id=?1 AND channel=?2",
                params![run_id, channel, ts(Utc::now())],
            )?,
            Err(e) => self.conn.execute(
                "UPDATE notifications SET status='failed', error=?3 WHERE run_id=?1 AND channel=?2",
                params![run_id, channel, crate::redact::redact(&e.to_string())],
            )?,
        };
        Ok(())
    }

    // ---------- NS e-mail decisions ----------
    fn add_event(&self, claim_id: i64, actor: &str, detail: &str) -> Result<()> {
        let st: String = self.conn.query_row("SELECT status FROM claims WHERE id=?1", [claim_id], |r| r.get(0))?;
        self.conn.execute(
            "INSERT INTO claim_events(claim_id,at,from_status,to_status,actor,detail) VALUES(?1,?2,?3,?3,?4,?5)",
            params![claim_id, ts(Utc::now()), st, actor, crate::redact::redact(detail)],
        )?;
        Ok(())
    }

    /// Find the journey an NS mail refers to: by NS reference (certain), else by local travel
    /// date (+ stations) — a date-only match is plausible, never proof.
    fn match_mail_journey(&self, m: &crate::ns_mail::NsMail) -> Result<Option<(i64, MailMatch)>> {
        if let Some(r) = &m.reference {
            if let Some(j) = self.conn.query_row(
                "SELECT c.journey_id FROM claims c JOIN journeys j ON j.id=c.journey_id WHERE c.ns_reference=?1 AND j.carrier_code=?2",
                params![r, NS_CARRIER_CODE],
                |x| x.get(0),
            ).optional()? {
                return Ok(Some((j, MailMatch::Reference)));
            }
            // An earlier mail with the same application number: as strong as that earlier match.
            if let Some((j, kind)) = self
                .conn
                .query_row(
                    "SELECT m.journey_id, m.match_kind FROM ns_mail m JOIN journeys j ON j.id=m.journey_id
                     WHERE m.reference=?1 AND j.carrier_code=?2
                     ORDER BY (match_kind = 'reference') DESC LIMIT 1",
                    params![r, NS_CARRIER_CODE],
                    |x| Ok((x.get::<_, i64>(0)?, x.get::<_, Option<String>>(1)?)),
                )
                .optional()?
            {
                return Ok(Some((j, if kind.as_deref() == Some("reference") { MailMatch::Reference } else { MailMatch::Date })));
            }
        }
        let Some(date) = m.travel_date else { return Ok(None) };
        let from = (date - chrono::Duration::days(1)).to_string();
        let to = (date + chrono::Duration::days(2)).to_string();
        let mut st = self.conn.prepare("SELECT id FROM journeys WHERE check_in_utc >= ?1 AND check_in_utc < ?2")?;
        let ids: Vec<i64> = st.query_map(params![from, to], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut cands: Vec<(i64, usize)> = vec![]; // (journey id, station hits)
        for id in ids {
            let j = self.journey(id)?;
            if j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive() != date || !j.is_ns_carrier() {
                continue;
            }
            // A mail WITH an application number never goes to a trip that already has a
            // different NS application number (on its claim, or from another NS mail).
            if let Some(r) = &m.reference {
                let other: i64 = self.conn.query_row(
                    "SELECT (SELECT COUNT(*) FROM claims WHERE journey_id=?1 AND ns_reference IS NOT NULL AND ns_reference<>?2)
                          + (SELECT COUNT(*) FROM ns_mail WHERE journey_id=?1 AND reference IS NOT NULL AND reference<>?2)",
                    params![id, r],
                    |x| x.get(0),
                )?;
                if other > 0 {
                    continue;
                }
            }
            let names = format!("{} {}", j.origin.name, j.destination.as_ref().map(|d| d.name.as_str()).unwrap_or("")).to_lowercase();
            cands.push((id, m.stations.iter().filter(|s| names.contains(s.as_str())).count()));
        }
        // Only a unique answer is accepted: one NS trip that day, or one clear best station match.
        if cands.len() == 1 {
            return Ok(Some((cands[0].0, MailMatch::Date)));
        }
        cands.sort_by_key(|c| std::cmp::Reverse(c.1));
        Ok(match cands.as_slice() {
            [first, second, ..] if first.1 > 0 && first.1 > second.1 => Some((first.0, MailMatch::Date)),
            _ => None,
        })
    }

    /// Apply one NS decision mail exactly once. Returns what changed (or was noted), if anything.
    ///
    /// Safety rules: a claim this app filed (or may have filed) only changes on a match by NS
    /// application number; a date-only match is recorded as a note and a caution flag
    /// ([`Db::ns_claim_near_keys`]: no automatic submission, manual only after acknowledgement),
    /// never as a state change. A mail that cannot be tied to a trip is kept and tried again by
    /// [`Db::retry_unmatched_mails`].
    pub fn apply_ns_mail(&mut self, m: &crate::ns_mail::NsMail) -> Result<Option<MailUpdate>> {
        let row: Option<(Option<i64>, Option<String>)> = self
            .conn
            .query_row("SELECT journey_id, hint_key FROM ns_mail WHERE key=?1", [&m.key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match row {
            None => {
                self.conn.execute(
                    "INSERT INTO ns_mail(key,kind,reference,amount_cents,travel_date,mail_date,journey_id,claim_id,processed_at,stations_json)
                     VALUES(?1,?2,?3,?4,?5,?6,NULL,NULL,?7,?8)",
                    params![
                        m.key,
                        m.kind.as_str(),
                        m.reference,
                        m.amount_cents,
                        m.travel_date.map(|d| d.to_string()),
                        ts(m.mail_date),
                        ts(Utc::now()),
                        serde_json::to_string(&m.stations)?
                    ],
                )?;
                self.apply_mail_row(m, None)
            }
            // already tied to a trip: processed once
            Some((Some(_), _)) => Ok(None),
            // seen before but not tied: try again
            Some((None, hint)) => self.apply_mail_row(m, hint),
        }
    }

    /// Match NS mails that could not be tied to a trip before (e.g. the trip was fetched later, or
    /// a claim got its NS application number since). Oldest mail first.
    pub fn retry_unmatched_mails(&mut self) -> Result<Vec<MailUpdate>> {
        type Row = (String, String, Option<String>, Option<i64>, Option<String>, String, Option<String>);
        let rows: Vec<Row> = {
            let mut st = self.conn.prepare(
                "SELECT key, kind, reference, amount_cents, travel_date, mail_date, stations_json FROM ns_mail
                 WHERE journey_id IS NULL ORDER BY mail_date, key",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
                .collect::<rusqlite::Result<Vec<Row>>>()?;
            v
        };
        let mut out = vec![];
        for (key, kind, reference, amount_cents, td, md, stations) in rows {
            let (Some(kind), Ok(mail_date)) = (crate::ns_mail::MailKind::parse(&kind), parse_ts(&md)) else { continue };
            let m = crate::ns_mail::NsMail {
                key,
                kind,
                reference,
                amount_cents,
                travel_date: td.and_then(|d| d.parse().ok()),
                mail_date,
                stations: stations.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            };
            if let Some(u) = self.apply_ns_mail(&m)? {
                out.push(u);
            }
        }
        Ok(out)
    }

    /// Is there a decision mail for this claim written AFTER `mail_date`? (An older mail that is
    /// matched late must never undo a newer decision.)
    fn newer_decision_applied(&self, claim_id: i64, mail_date: DateTime<Utc>) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ns_mail WHERE claim_id=?1 AND kind IN ('approved','rejected','paid') AND mail_date > ?2",
            params![claim_id, ts(mail_date)],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    fn apply_mail_row(&mut self, m: &crate::ns_mail::NsMail, prev_hint: Option<String>) -> Result<Option<MailUpdate>> {
        use crate::ns_mail::MailKind;
        let Some((jid, how)) = self.match_mail_journey(m)? else { return Ok(None) };
        let j = self.journey(jid)?;
        let existing = self.claim_for_journey(jid)?;
        let status = existing.as_ref().map(|c| c.status);
        let ours = matches!(
            status,
            Some(
                ClaimState::Submitting
                    | ClaimState::Submitted
                    | ClaimState::Indeterminate
                    | ClaimState::Approved
                    | ClaimState::Paid
                    | ClaimState::Rejected
            )
        );
        // Still open: this app may submit it (or the user may, from the GUI).
        let mut update = MailUpdate {
            date: j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).format("%d-%m-%Y").to_string(),
            route: format!("{} → {}", j.origin.name, j.destination.as_ref().map(|d| d.name.as_str()).unwrap_or("?")),
            kind: m.kind,
            amount_cents: m.amount_cents,
            applied: true,
        };
        if how == MailMatch::Date {
            // Plausible, not proven: note it once, flag the trip, change nothing.
            let key = j.natural_key();
            if prev_hint.as_deref() == Some(key.as_str()) {
                return Ok(None);
            }
            update.applied = false;
            self.conn.execute("UPDATE ns_mail SET hint_key=?2, processed_at=?3 WHERE key=?1", params![m.key, key, ts(Utc::now())])?;
            if let Some(c) = &existing {
                self.add_event(c.id, "ns-mail", &update.detail())?;
            }
            return Ok(Some(update));
        }
        let c = match existing {
            Some(c) => c,
            None => self.ensure_claim(jid, "external", ClaimState::Discovered)?,
        };
        if c.ns_reference.is_none() {
            // Reference match: the claim already has it. Date match: only reached for trips this
            // app never filed (claimed elsewhere / no open claim).
            if let Some(r) = &m.reference {
                self.set_ns_reference(c.id, r)?;
            }
        }
        let target = match m.kind {
            MailKind::Submitted => ClaimState::Submitted,
            MailKind::Approved => ClaimState::Approved,
            MailKind::Rejected => ClaimState::Rejected,
            MailKind::Paid => ClaimState::Paid,
        };
        let detail = update.detail();
        let newer_decision = self.newer_decision_applied(c.id, m.mail_date)?;
        // Amounts are only recorded when the mail's decision actually applies to the claim
        // (e.g. a late "rejected" mail must never wipe a recorded payout of a PAID claim).
        let decision_applies = if ours {
            // only by NS application number (see above)
            if c.status != target && c.status.can_transition_to(target) && !newer_decision {
                self.transition(c.id, Some(c.status), target, "ns-mail", &detail)?;
                true
            } else {
                self.add_event(c.id, "ns-mail", &detail)?;
                c.status == target && !newer_decision
            }
        } else {
            // Filed outside this app (e.g. another service): record it as claimed elsewhere.
            if c.status != ClaimState::ExternallyClaimed {
                self.mark_external_detail(
                    c.id,
                    "ns-mail",
                    "NS-mail over deze reis: Elders geclaimd / NS e-mail about this trip: claimed elsewhere",
                )?;
            }
            self.add_event(c.id, "ns-mail", &detail)?;
            let paid = self.paid_cents(c.id)?;
            !newer_decision && (m.kind != MailKind::Rejected || paid.is_none())
        };
        if decision_applies {
            if let Some(a) = m.amount_cents.filter(|_| matches!(m.kind, MailKind::Approved | MailKind::Paid)) {
                self.set_paid_amount(c.id, Some(a))?;
            }
            if m.kind == MailKind::Rejected {
                self.set_paid_amount(c.id, Some(0))?;
            }
        }
        self.conn.execute(
            "UPDATE ns_mail SET journey_id=?2, claim_id=?3, match_kind=?4, hint_key=NULL, processed_at=?5 WHERE key=?1",
            params![m.key, jid, c.id, how.as_str(), ts(Utc::now())],
        )?;
        Ok(Some(update))
    }

    pub fn ns_mail_summary(&self) -> Result<serde_json::Value> {
        let (total, unmatched): (i64, i64) =
            self.conn.query_row("SELECT COUNT(*), COALESCE(SUM(journey_id IS NULL),0) FROM ns_mail", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let last: Option<String> = self.conn.query_row("SELECT MAX(processed_at) FROM ns_mail", [], |r| r.get(0))?;
        Ok(serde_json::json!({"total": total, "unmatched": unmatched, "last": last}))
    }

    /// Invalidate derived timetable evidence once when the matching policy changes.
    /// Journeys, claim proof and submitted facts remain intact; no schema migration is needed.
    pub fn refresh_matching_policy(&mut self) -> Result<()> {
        const VERSION: &str = "conservative-arrival-v2";
        if self.get_setting("matching_policy_version")?.as_deref() != Some(VERSION) {
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM evidence_cache", [])?;
            tx.execute(
                "INSERT INTO settings(key,value) VALUES('matching_policy_version',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [VERSION],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    // ---------- evidence cache (per journey; raw timetable data is never stored) ----------
    pub fn cached_evidence(&self, journey_key: &str) -> Result<Option<ScheduleEvidence>> {
        let s: Option<String> =
            self.conn.query_row("SELECT evidence_json FROM evidence_cache WHERE journey_key=?1", [journey_key], |r| r.get(0)).optional()?;
        Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn put_cached_evidence(&self, journey_key: &str, month: &str, ev: &ScheduleEvidence) -> Result<()> {
        self.conn.execute(
            "INSERT INTO evidence_cache(journey_key,month,evidence_json,computed_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(journey_key) DO UPDATE SET evidence_json=excluded.evidence_json, computed_at=excluded.computed_at",
            params![journey_key, month, serde_json::to_string(ev)?, ts(Utc::now())],
        )?;
        Ok(())
    }

    /// Delete local history (user-confirmed in the UI). Claims that reached NS or were claimed
    /// elsewhere (and their journeys) are KEPT, so a wipe can never cause a duplicate claim. NS
    /// e-mails are kept as well (they hold no trip record, and they tell that NS already has a
    /// claim around a date); a link to a deleted trip or claim is removed, so they are matched
    /// again to the trips of the next import.
    pub fn wipe_history(&mut self) -> Result<()> {
        // claim_events is append-only by trigger; drop triggers inside the wipe transaction.
        let tx = self.conn.transaction()?;
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE keep_claims AS SELECT id, journey_id FROM claims WHERE {KEEP_CLAIMS_SQL};
             UPDATE ns_mail SET claim_id=NULL, journey_id=NULL WHERE claim_id IS NOT NULL AND claim_id NOT IN (SELECT id FROM keep_claims);
             UPDATE ns_mail SET journey_id=NULL WHERE journey_id IS NOT NULL AND journey_id NOT IN (SELECT journey_id FROM keep_claims);
             DROP TRIGGER claim_events_no_delete;
             DELETE FROM claim_events WHERE claim_id NOT IN (SELECT id FROM keep_claims);
             CREATE TRIGGER claim_events_no_delete BEFORE DELETE ON claim_events BEGIN SELECT RAISE(ABORT, 'claim_events is append-only'); END;
             DELETE FROM claims WHERE id NOT IN (SELECT id FROM keep_claims);
             DELETE FROM notifications; DELETE FROM runs;
             DELETE FROM candidates WHERE journey_id NOT IN (SELECT journey_id FROM keep_claims);
             DELETE FROM schedules; DELETE FROM evidence_cache;
             DELETE FROM journeys WHERE id NOT IN (SELECT journey_id FROM keep_claims);
             DROP TABLE keep_claims;"
        ))?;
        // A stop request belongs to the history too (run ids start again after the wipe).
        tx.execute("DELETE FROM settings WHERE key IN (?1, ?2)", [crate::pipeline::AUTO_STOP_KEY, crate::pipeline::AUTO_STOP_PENDING_KEY])?;
        tx.execute(
            "UPDATE settings SET value=(SELECT json_group_array(k.value) FROM json_each(settings.value) k
               WHERE EXISTS(SELECT 1 FROM journeys j WHERE k.value IN
                 (j.check_in_utc || '|' || j.origin_uic, 'tx|' || j.source_id || '|' || j.source_record_id)))
             WHERE key=?1",
            [CARD_CONFLICT_KEY],
        )?;
        tx.commit()?;
        self.purge_deleted_pages()
    }

    /// "Forget everything": remove every row (also claims kept for duplicate protection) and settings.
    /// The caller should hold the run lock. A lease that has not expired (the caller's own, or a
    /// live run's) is never removed here; only expired leases are cleaned up.
    pub fn erase_all(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS claim_events_no_delete;
             DELETE FROM claim_events; DELETE FROM ns_mail; DELETE FROM notifications; DELETE FROM claims; DELETE FROM runs;
             DELETE FROM candidates; DELETE FROM schedules; DELETE FROM evidence_cache; DELETE FROM external_claims;
             DELETE FROM journeys; DELETE FROM cards; DELETE FROM settings; DELETE FROM sources;
             CREATE TRIGGER claim_events_no_delete BEFORE DELETE ON claim_events BEGIN SELECT RAISE(ABORT, 'claim_events is append-only'); END;",
        )?;
        tx.execute("DELETE FROM run_lock WHERE expires_at < ?1", [ts(Utc::now())])?;
        tx.commit()?;
        self.purge_deleted_pages()
    }

    fn purge_deleted_pages(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA secure_delete=ON; VACUUM;")?;
        let busy: i64 = self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        anyhow::ensure!(busy == 0, "Het databasebestand is nog in gebruik. Probeer het wissen opnieuw. / The database file is still in use. Try erasing again.");
        Ok(())
    }
}

/// Schema objects of the given types: (type, name, SQL with whitespace collapsed), sorted.
fn schema_objects(conn: &Connection, types: &[&str]) -> Result<Vec<(String, String, String)>> {
    let mut st = conn.prepare("SELECT type, name, COALESCE(sql, '') FROM sqlite_master ORDER BY type, name")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    let mut out = vec![];
    for row in rows {
        let (ty, name, sql) = row?;
        if types.contains(&ty.as_str()) {
            out.push((ty, name, sql.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    }
    Ok(out)
}

pub const RESTORE_INVALID_MSG: &str = "Dit is geen back-up van OV Refund. / This is not an OV Refund backup.";
/// Refusal for a file that looks like an OV Refund backup but cannot be read completely.
pub const RESTORE_DAMAGED_MSG: &str = "Back-up is beschadigd of onvolledig. / Backup is damaged or incomplete.";

/// Is this unreadable file a damaged or cut-off backup rather than some other file? Yes for a
/// database file that is shorter than its own header says (cut off while copying or
/// downloading), and for one that still carries the names of OV Refund's own tables.
pub fn looks_like_ov_backup(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    if !bytes.starts_with(b"SQLite format 3\0") || bytes.len() < 100 {
        return false;
    }
    let be32 = |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as u64;
    let page_size = match u16::from_be_bytes([bytes[16], bytes[17]]) as u64 {
        1 => 65536,
        n => n,
    };
    // The page count in the header is valid when its "version valid for" equals the change counter.
    let (pages, change_counter, valid_for) = (be32(28), be32(24), be32(92));
    if pages > 0 && valid_for == change_counter && (bytes.len() as u64) < pages * page_size {
        return true;
    }
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    has(b"claim_events") && has(b"schema_migrations") && has(b"external_claims")
}
pub const RESTORE_NOT_EMPTY_MSG: &str = "Er staan al reizen of claims in OV Refund. Bevestig dat de back-up je huidige gegevens mag vervangen. / OV Refund already has trips or claims. Confirm that the backup may replace your current data.";

/// Status column of the CSV export (glossary).
/// The delay the app shows for a trip: the estimate, the delay against the regular timetable
/// on an adjusted day, or the possible delay from the check-out time.
fn csv_delay(r: &JourneyRow) -> Option<i64> {
    let adjusted = r.reasons.iter().find_map(|s| {
        s.strip_prefix("timetable was adjusted that day; against the regular timetable you arrived about ")?
            .split(' ')
            .next()?
            .parse::<i64>()
            .ok()
    });
    adjusted.or(r.delay_min).or(r.possible_delay_min)
}

fn csv_status(s: ClaimState, nl: bool) -> &'static str {
    let (a, b) = match s {
        ClaimState::Ready | ClaimState::Candidate | ClaimState::Discovered => ("Te claimen", "Claimable"),
        ClaimState::NeedsReview => ("Beoordelen", "Review"),
        ClaimState::Dismissed => ("Niet claimen", "Not claiming"),
        other => (status_nl(other), status_en(other)),
    };
    if nl {
        a
    } else {
        b
    }
}

/// Status names as the GUI shows them (glossary).
fn status_nl(s: ClaimState) -> &'static str {
    match s {
        ClaimState::Submitting => "Wordt ingediend",
        ClaimState::Submitted => "Ingediend",
        ClaimState::Indeterminate => "Onzeker",
        ClaimState::Approved => "Toegekend",
        ClaimState::Rejected => "Afgewezen",
        ClaimState::Paid => "Uitbetaald",
        ClaimState::ExternallyClaimed => "Elders geclaimd",
        ClaimState::Failed => "Niet verzonden",
        _ => "Te claimen",
    }
}
fn status_en(s: ClaimState) -> &'static str {
    match s {
        ClaimState::Submitting => "Submitting",
        ClaimState::Submitted => "Submitted",
        ClaimState::Indeterminate => "Uncertain",
        ClaimState::Approved => "Approved",
        ClaimState::Rejected => "Rejected",
        ClaimState::Paid => "Paid",
        ClaimState::ExternallyClaimed => "Claimed elsewhere",
        ClaimState::Failed => "Not sent",
        _ => "Claimable",
    }
}

/// A CSV cell that a spreadsheet never runs as a formula: text starting with = + - @ (also
/// after leading spaces), or with a tab or carriage return, gets a leading apostrophe.
pub fn csv_safe(s: &str) -> String {
    let t = s.trim_start();
    if s.starts_with(['\t', '\r']) || t.starts_with(['=', '+', '-', '@']) {
        format!("'{s}")
    } else {
        s.to_string()
    }
}

/// History note for READY -> SUBMITTING ("Nederlands / English").
pub fn authorized_detail(mode: AuthorizedMode, band: DelayBand) -> String {
    match mode {
        AuthorizedMode::Manual => {
            format!("Goedgekeurd door jou; vertraging {} / Approved by you; delay {}", band.label_nl(), band.label_en())
        }
        AuthorizedMode::Automatic => {
            format!(
                "Automatisch ingediend met je toestemming; vertraging {} / Submitted automatically with your consent; delay {}",
                band.label_nl(),
                band.label_en()
            )
        }
    }
}

/// "{nl}{reason} / {en}{reason}" for a history note or run error with a reason; a bilingual
/// reason ("NL / EN") is split over the two halves. Any further " / " inside the reason is
/// written as "/" so the result always splits in exactly two halves.
pub fn bilingual_detail(nl: &str, en: &str, reason: &str) -> String {
    let (a, b) = reason.split_once(" / ").unwrap_or((reason, reason));
    format!("{nl}{} / {en}{}", a.replace(" / ", "/"), b.replace(" / ", "/"))
}

/// Claim-history note as "Nederlands / English" (the GUI shows the half for its language).
/// New notes are stored bilingual; older rows stored English notes, which are translated here.
/// Notes that are already bilingual, NS's own status text and policy reasons (translated by the
/// GUI per "; "-separated reason) are returned unchanged.
pub fn history_detail(d: &str) -> String {
    if d.contains(" / ") {
        return d.to_string();
    }
    if d == "waiting for timetable data" {
        return "Wacht op treintijden / Awaiting timetable".into();
    }
    const FIXED: [(&str, &str); 11] = [
        ("created", "Aangemaakt"),
        ("set manually by user", "Door jou aangepast"),
        ("restored by user", "Door jou teruggezet"),
        ("marked as claimed outside this app", "Gemarkeerd als Elders geclaimd"),
        ("NS e-mail about this trip: claimed outside this app", "NS-mail over deze reis: Elders geclaimd"),
        ("not claimed elsewhere after all", "Toch niet elders geclaimd"),
        ("reviewed and approved by user", "Door jou bekeken en goedgekeurd"),
        ("reviewed and approved by user (resubmission confirmed)", "Door jou bekeken en goedgekeurd (opnieuw indienen bevestigd)"),
        ("process ended during submission; needs reconciliation", "OV Refund stopte tijdens het indienen; uitkomst Onzeker"),
        ("automatic retry (previous attempt was not sent)", "Automatisch opnieuw geprobeerd (de vorige poging is niet verzonden)"),
        ("DEMO: simulated NS acknowledgement (nothing was sent)", "DEMO: gesimuleerde bevestiging van NS (er is niets verzonden)"),
    ];
    if let Some((en, nl)) = FIXED.iter().find(|(en, _)| *en == d) {
        let en = format!("{}{}", en[..1].to_uppercase(), &en[1..]);
        return format!("{nl} / {en}");
    }
    const PREFIX: [(&str, &str, &str); 7] = [
        ("authorized; delay band ", "Toestemming gegeven; vertraging ", "Authorized; delay "),
        ("not sent: ", "Niet verzonden: ", "Not sent: "),
        ("outcome unknown: ", "Uitkomst onzeker: ", "Outcome uncertain: "),
        ("no longer claimable: ", "Niet meer te claimen: ", "No longer claimable: "),
        ("found at NS ", "Gevonden bij NS ", "Found at NS "),
        ("NS accepted: ", "Ontvangen door NS: ", "Received by NS: "),
        ("listed in NS claim list ", "Staat in Mijn teruggaven ", "Listed in Mijn teruggaven "),
    ];
    for (p, nl, en) in PREFIX {
        if let Some(rest) = d.strip_prefix(p) {
            let rest_nl = rest
                .replace("; possibly this app's earlier uncertain submission", "; mogelijk de eerdere onzekere indiening van OV Refund");
            let rest_en = rest
                .replace("; possibly this app's earlier uncertain submission", "; possibly OV Refund's earlier uncertain submission")
                .replace("30 t/m 59 min", "30–59 min")
                .replace("60 minuten of meer", "60 min or more");
            return format!("{nl}{rest_nl} / {en}{rest_en}");
        }
    }
    d.to_string()
}

/// Does a station name the user typed ("Utrecht C.", "Amsterdam CS", "Den Bosch", "zwolle")
/// refer to this official station name? Word-prefix matching with common abbreviations.
pub fn station_matches(typed: &str, official: &str) -> bool {
    let (t, o) = (station_words(typed), station_words(official));
    if t.is_empty() || o.is_empty() {
        return false;
    }
    // every typed word must be the start of a word in the official name, and the first words agree
    t.iter().all(|w| o.iter().any(|x| x.starts_with(w.as_str()))) && o[0].starts_with(t[0].as_str())
}

/// The words of a station name, lower case, with "C"/"CS" as "centraal" and "Den Bosch" as
/// "Hertogenbosch" (for comparing station names).
pub fn station_words(s: &str) -> Vec<String> {
    let s = s.to_lowercase().replace("den bosch", "hertogenbosch").replace("'s-", "").replace("'s ", "");
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| match w {
            "cs" | "c" | "centr" => "centraal".to_string(),
            "s" => String::new(),
            _ => w.to_string(),
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Start of the note of a "claimed elsewhere" entry that stores its stations ("uic:<from>|<to>").
const EXTERNAL_UIC_PREFIX: &str = "uic:";

/// The stations stored with an entry (see [`EXTERNAL_UIC_PREFIX`]); `None` for entries from
/// before they were stored.
fn parse_external_uics(note: &str) -> Option<(String, String)> {
    let rest = note.strip_prefix(EXTERNAL_UIC_PREFIX)?;
    let (o, d) = rest.split(' ').next()?.split_once('|')?;
    (!o.is_empty() && !d.is_empty()).then(|| (o.to_string(), d.to_string()))
}

/// One "claimed elsewhere" entry.
struct ExternalEntry {
    id: i64,
    origin: Option<String>,
    dest: Option<String>,
    /// Its stations (newer entries): matched exactly. Older entries match their typed names.
    uics: Option<(String, String)>,
}

impl ExternalEntry {
    /// Does this entry match a trip from `o` to `d` (name and UIC code of each station)?
    fn matches(&self, o_name: &str, o_uic: &str, d_name: &str, d_uic: &str) -> bool {
        match &self.uics {
            Some((eo, ed)) => {
                let same = |uic: &str, stored: &str, name: &Option<String>, trip: &str| {
                    uic == stored || name.as_deref().is_some_and(|n| station_words(n) == station_words(trip))
                };
                same(o_uic, eo, &self.origin, o_name) && same(d_uic, ed, &self.dest, d_name)
            }
            // As before (typed names, every typed word the start of a word of the station).
            None => {
                self.origin.as_deref().map(|e| station_matches(e, o_name)).unwrap_or(true)
                    && self.dest.as_deref().map(|e| station_matches(e, d_name)).unwrap_or(true)
            }
        }
    }

    fn matches_journey(&self, j: &Journey) -> bool {
        let (dn, du) = j.destination.as_ref().map(|d| (d.name.as_str(), d.uic.as_str())).unwrap_or(("", ""));
        // An entry with stations needs the trip's destination; older entries match as before.
        (self.uics.is_none() || j.destination.is_some()) && self.matches(&j.origin.name, &j.origin.uic, dn, du)
    }
}

#[cfg(test)]
mod station_match_tests {
    use super::station_matches;
    #[test]
    fn csv_preserves_inflight_status_in_both_languages() {
        use super::{csv_status, ClaimState};
        assert_eq!(csv_status(ClaimState::Submitting, true), "Wordt ingediend");
        assert_eq!(csv_status(ClaimState::Submitting, false), "Submitting");
        assert_eq!(csv_status(ClaimState::Submitted, true), "Ingediend");
        assert_eq!(csv_status(ClaimState::Submitted, false), "Submitted");
    }

    #[test]
    fn abbreviations_and_variants() {
        assert!(station_matches("Utrecht C.", "Utrecht Centraal"));
        assert!(station_matches("Amsterdam CS", "Amsterdam Centraal"));
        assert!(station_matches("Den Bosch", "'s-Hertogenbosch"));
        assert!(station_matches("zwolle", "Zwolle"));
        assert!(station_matches("Rotterdam", "Rotterdam Centraal"));
        assert!(!station_matches("Utrecht", "Amsterdam Centraal"));
        assert!(!station_matches("Utrecht C.", "Utrecht Overvecht"));
        assert!(!station_matches("", "Utrecht Centraal"));
    }
}

#[cfg(test)]
pub(crate) mod submission_tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    pub(crate) fn journey(id: &str, carrier: (&str, &str)) -> Journey {
        Journey {
            source_id: "fixture".into(),
            source_record_id: id.into(),
            coupling_id: Some("00000000-0000-0000-0000-000000000000".into()),
            card_last4: Some("0000".into()),
            check_in: utc("2026-10-28T20:01:00+01:00") + chrono::Duration::minutes(id.len() as i64 * 7),
            check_out: Some(utc("2026-10-28T22:41:00+01:00")),
            origin: StationRef { uic: format!("84000{}", id.len()), name: "A".into() },
            destination: Some(StationRef { uic: "8400319".into(), name: "B".into() }),
            carrier_code: Some(carrier.0.into()),
            carrier_name: Some(carrier.1.into()),
            product_name: None,
            travel_class: Some(2),
            fare_cents: None,
            subscription: None,
            subscription_fee_cents: None,
            invoiced: None,
            payload_sha256: "x".into(),
        }
    }

    pub(crate) const NS: (&str, &str) = ("4", "Nederlandse Spoorwegen");
    pub(crate) const ARRIVA: (&str, &str) = ("12", "Arriva");

    pub(crate) fn facts(j: &Journey) -> ClaimFacts {
        ClaimFacts {
            source_record_id: j.source_record_id.clone(),
            coupling_id: "c".into(),
            check_in: j.check_in,
            check_out: j.check_out.unwrap(),
            origin_name: "A".into(),
            destination_name: "B".into(),
            scheduled_departure_local: "20:01".into(),
            delay_band: DelayBand::Min30To59,
            reached_destination: true,
        }
    }

    pub(crate) fn ready(db: &mut Db, id: &str, carrier: (&str, &str)) -> (i64, ClaimFacts) {
        let j = journey(id, carrier);
        let (jid, _) = db.upsert_journey(&j).unwrap();
        (db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap().id, facts(&j))
    }

    #[test]
    fn history_wipe_reports_a_reader_pinning_the_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sqlite");
        let mut db = Db::open(&path).unwrap();
        let _ = db.upsert_journey(&journey("fixture", NS)).unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN; SELECT COUNT(*) FROM journeys;").unwrap();
        db.conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let error = db.wipe_history().unwrap_err().to_string();
        assert!(error.contains("Try erasing again"), "{error}");
        reader.execute_batch("ROLLBACK;").unwrap();
        db.wipe_history().unwrap();
        assert!(db.journey_rows(10).unwrap().is_empty());
    }

    #[test]
    fn an_arriva_claim_in_flight_never_blocks_an_ns_claim_and_the_other_way_round() {
        let mut db = Db::open_in_memory().unwrap();
        let (arriva1, _) = ready(&mut db, "AR1", ARRIVA);
        let (arriva2, _) = ready(&mut db, "AR22", ARRIVA);
        let (ns1, f1) = ready(&mut db, "NS333", NS);
        let (ns2, f2) = ready(&mut db, "NS4444", NS);
        // Arriva claim in flight (Arriva's own path: a transition with its in-flight marker).
        db.transition_and_set(arriva1, Some(ClaimState::Ready), ClaimState::Submitting, "user", "t", "k", "v").unwrap();
        // NS goes on ...
        db.begin_submission(ns1, &f1, AuthorizedMode::Automatic).unwrap();
        // ... but never two claims of one carrier at the same time.
        let err = db.begin_submission(ns2, &f2, AuthorizedMode::Automatic).unwrap_err();
        assert_eq!(err.to_string(), INFLIGHT_SAME_CARRIER_MSG);
        assert_eq!(db.claim(ns2).unwrap().status, ClaimState::Ready);
        let err = db.transition(arriva2, Some(ClaimState::Ready), ClaimState::Submitting, "user", "t").unwrap_err();
        assert_eq!(err.to_string(), INFLIGHT_SAME_CARRIER_MSG);
        assert_eq!(db.claim(arriva2).unwrap().status, ClaimState::Ready);
        // Once the NS claim has an outcome, the next NS claim may go.
        db.transition(ns1, Some(ClaimState::Submitting), ClaimState::Failed, "gateway", "t").unwrap();
        db.begin_submission(ns2, &f2, AuthorizedMode::Automatic).unwrap();
        // An NS claim in flight does not block Arriva either.
        db.transition(arriva1, Some(ClaimState::Submitting), ClaimState::Failed, "gateway", "t").unwrap();
        db.transition(arriva2, Some(ClaimState::Ready), ClaimState::Submitting, "user", "t").unwrap();
    }

    #[test]
    fn begin_submission_under_lock_refuses_a_lost_or_expired_lock_and_changes_nothing() {
        let mut db = Db::open_in_memory().unwrap();
        let (id, f) = ready(&mut db, "NS1", NS);
        let events = |db: &Db| db.events(id).unwrap().len();
        let before = events(&db);
        // No lock at all.
        let err = db.begin_submission_under_lock(id, &f, AuthorizedMode::Automatic, "pid1-run-1").unwrap_err();
        assert_eq!(err.to_string(), LOCK_LOST_MSG);
        // The lease ran out.
        assert!(db.acquire_run_lock("pid1-run-1", chrono::Duration::minutes(-1)).unwrap());
        let err = db.begin_submission_under_lock(id, &f, AuthorizedMode::Automatic, "pid1-run-1").unwrap_err();
        assert_eq!(err.to_string(), LOCK_LOST_MSG);
        // Another check took the lock over.
        assert!(db.acquire_run_lock("pid2-run-2", chrono::Duration::minutes(30)).unwrap());
        let err = db.begin_submission_under_lock(id, &f, AuthorizedMode::Automatic, "pid1-run-1").unwrap_err();
        assert_eq!(err.to_string(), LOCK_LOST_MSG);
        let c = db.claim(id).unwrap();
        assert_eq!(c.status, ClaimState::Ready);
        assert_eq!(c.attempts, 0);
        assert_eq!(events(&db), before, "nothing journaled");
        // The owner with a live lease may submit.
        db.begin_submission_under_lock(id, &f, AuthorizedMode::Automatic, "pid2-run-2").unwrap();
        assert_eq!(db.claim(id).unwrap().status, ClaimState::Submitting);
    }

    #[test]
    fn a_history_wipe_keeps_ns_mails_and_unlinks_deleted_trips() {
        let mut db = Db::open_in_memory().unwrap();
        let (c, _) = ready(&mut db, "NS1", NS);
        let jid = db.claim(c).unwrap().journey_id;
        let mail = |key: &str, journey: Option<i64>, claim: Option<i64>| {
            db.conn
                .execute(
                    "INSERT INTO ns_mail(key,kind,reference,amount_cents,travel_date,mail_date,journey_id,claim_id,processed_at)
                     VALUES(?1,'received','123',NULL,'2026-10-28','2026-10-29T10:00:00Z',?2,?3,'2026-10-29T10:00:00Z')",
                    params![key, journey, claim],
                )
                .unwrap();
        };
        mail("unmatched", None, None);
        mail("trip", Some(jid), None);
        mail("claim", Some(jid), Some(c));
        db.wipe_history().unwrap();
        assert_eq!(db.claims_in(&[ClaimState::Ready]).unwrap().len(), 0, "the READY claim went with the history");
        let rows: Vec<(String, Option<i64>, Option<i64>)> = {
            let mut st = db.conn.prepare("SELECT key, journey_id, claim_id FROM ns_mail ORDER BY key").unwrap();
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
            v
        };
        assert_eq!(rows, vec![("claim".into(), None, None), ("trip".into(), None, None), ("unmatched".into(), None, None)]);
    }

    #[test]
    fn upsert_prefers_the_record_id_and_never_copies_another_rows_key() {
        let db = Db::open_in_memory().unwrap();
        let a = journey("A", NS);
        let b = journey("BB", NS);
        let (ida, _) = db.upsert_journey(&a).unwrap();
        let (idb, _) = db.upsert_journey(&b).unwrap();
        // Record id of A, fallback key of B: A is updated, B's key is not copied.
        let mixed = Journey { check_in: b.check_in, origin: b.origin.clone(), ..a.clone() };
        assert_eq!(mixed.fallback_key(), b.fallback_key());
        let (id, new) = db.upsert_journey(&mixed).unwrap();
        assert_eq!((id, new), (ida, false));
        assert_eq!(db.journey(ida).unwrap().fallback_key(), a.fallback_key());
        assert_eq!(db.journey(idb).unwrap().source_record_id, "BB");
        // Another record id with A's fallback key: A again, its record id stays (no collision).
        let renamed = Journey { source_record_id: "BB".into(), ..a.clone() };
        let (id, _) = db.upsert_journey(&renamed).unwrap();
        assert_eq!(id, idb, "the record id wins");
    }

    #[test]
    fn an_open_trip_cannot_switch_cards_and_uses_the_corrected_carrier() {
        let db = Db::open_in_memory().unwrap();
        let mut original = journey("open-test-trip", NS);
        original.coupling_id = Some("test-card-a".into());
        let (id, _) = db.upsert_journey(&original).unwrap();
        let mut conflict = original.clone();
        conflict.coupling_id = Some("test-card-b".into());
        // The import continues: the recorded card stays, and the trip is held out of automatic submission.
        assert_eq!(db.upsert_journey(&conflict).unwrap(), (id, false));
        assert_eq!(db.journey(id).unwrap().coupling_id, original.coupling_id);
        assert!(db.is_ns_claim_near(&original).unwrap());
        db.upsert_journey(&conflict).unwrap();
        assert_eq!(db.card_conflict_keys().unwrap().len(), 1);
        original.carrier_code = Some("ARR".into());
        original.carrier_name = Some("Arriva".into());
        assert_eq!(db.upsert_journey(&original).unwrap(), (id, false));
        assert!(db.journey(id).unwrap().is_arriva());
        assert!(!db.has_card_conflict(&original).unwrap(), "a later observation confirms the original card");
    }

    #[test]
    fn card_conflicts_survive_backup_and_clear_only_on_matching_card_evidence() {
        let db = Db::open_in_memory().unwrap();
        let original = journey("held-trip", NS);
        db.upsert_journey(&original).unwrap();
        let mut observed = original.clone();
        observed.coupling_id = Some("different-test-card".into());
        db.upsert_journey(&observed).unwrap();
        db.set_ns_claim_near_keys(&[]).unwrap();
        assert!(db.is_ns_claim_near(&original).unwrap());

        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("fixture.sqlite");
        db.backup_to(&backup, false).unwrap();
        let mut restored = Db::open_in_memory().unwrap();
        restored.restore_from(&backup, false, false).unwrap();
        assert!(restored.has_card_conflict(&original).unwrap());

        let replacement = Journey { source_record_id: "replacement-test-record".into(), ..original.clone() };
        let newer = Db::open_in_memory().unwrap();
        newer.upsert_journey(&replacement).unwrap();
        let newer_backup = dir.path().join("replacement.sqlite");
        newer.backup_to(&newer_backup, false).unwrap();
        restored.restore_from(&newer_backup, true, false).unwrap();
        assert!(restored.has_card_conflict(&replacement).unwrap(), "restore must preserve a hold through a fallback-key match");
        assert!(restored.wipe_history().is_ok());
        assert!(!restored.has_card_conflict(&replacement).unwrap(), "wiping the open trip drops its card hold");

        observed.source_record_id = "unconfirmed-record".into();
        observed.coupling_id.clone_from(&original.coupling_id);
        db.upsert_journey(&observed).unwrap();
        assert!(db.has_card_conflict(&original).unwrap());
        db.upsert_journey(&observed).unwrap();
        assert!(db.has_card_conflict(&original).unwrap(), "repeated fallback observations cannot replace the held record");
        db.upsert_journey(&original).unwrap();
        assert!(!db.has_card_conflict(&original).unwrap());
    }

    #[test]
    fn a_card_hold_does_not_follow_another_trip_at_the_same_time_and_station() {
        let db = Db::open_in_memory().unwrap();
        let trip = journey("first-test-trip", NS);
        let other = Journey {
            source_record_id: "other-test-trip".into(), card_last4: Some("0002".into()),
            coupling_id: Some("other-test-card".into()), ..trip.clone()
        };
        let (_, new) = db.upsert_journey(&trip).unwrap();
        assert!(new);
        let (_, new) = db.upsert_journey(&other).unwrap();
        assert!(new);
        assert_eq!(trip.natural_key(), other.natural_key());
        let conflict = Journey { coupling_id: Some("changed-test-card".into()), ..trip.clone() };
        db.upsert_journey(&conflict).unwrap();
        assert!(db.has_card_conflict(&trip).unwrap());
        assert!(!db.has_card_conflict(&other).unwrap());
        db.upsert_journey(&other).unwrap();
        assert!(db.has_card_conflict(&trip).unwrap());
        db.upsert_journey(&trip).unwrap();
        assert!(!db.has_card_conflict(&trip).unwrap());

        db.set_setting(CARD_CONFLICT_KEY, &serde_json::to_string(&[trip.natural_key()]).unwrap()).unwrap();
        assert!(db.has_card_conflict(&other).unwrap(), "an old hold covers both trips until confirmed");
        db.upsert_journey(&other).unwrap();
        assert!(!db.has_card_conflict(&other).unwrap());
        assert!(db.has_card_conflict(&trip).unwrap(), "confirming one trip preserves the old warning on the other");
        db.upsert_journey(&trip).unwrap();
        assert!(!db.has_card_conflict(&trip).unwrap());
    }

    #[test]
    fn arriva_import_refreshes_the_vrij_class_and_preserves_a_later_recorded_fare() {
        let mut db = Db::open_in_memory().unwrap();
        let mut trip = journey("vrij-trip", ("ARR", "Arriva"));
        trip.check_in = "2026-10-17T17:30:00Z".parse().unwrap();
        trip.fare_cents = None;
        trip.subscription = Some("FLEXWVR".into());
        trip.subscription_fee_cents = None;
        trip.invoiced = Some(true);
        trip.travel_class = None;
        let (id, _) = db.upsert_journey(&trip).unwrap();
        let current_fare = |db: &Db| crate::compensation::effective_arriva_fare(&db.journey(id).unwrap()).unwrap().cents();
        assert_eq!(current_fare(&db), 658);
        trip.travel_class = Some(1);
        db.upsert_journey(&trip).unwrap();
        assert_eq!(current_fare(&db), 833);
        trip.fare_cents = Some(1400);
        db.upsert_journey(&trip).unwrap();
        assert_eq!(current_fare(&db), 1400);
        trip.fare_cents = Some(0);
        db.upsert_journey(&trip).unwrap();
        assert_eq!(current_fare(&db), 1400);
        let claim = db.ensure_claim(id, "fixture", ClaimState::Ready).unwrap();
        db.transition(claim.id, Some(ClaimState::Ready), ClaimState::Submitting, "user", "fixture").unwrap();
        db.upsert_journey(&trip).unwrap();
        assert_eq!(current_fare(&db), 1400);
        trip.coupling_id = Some("other-fixture-card".into());
        trip.travel_class = Some(2);
        trip.fare_cents = Some(2300);
        db.upsert_journey(&trip).unwrap();
        assert_eq!(current_fare(&db), 1400);
        assert_eq!(db.journey(id).unwrap().travel_class, Some(1));
        assert!(db.has_card_conflict(&trip).unwrap());
    }

    #[test]
    fn ns_inflight_marker_follows_the_claim_on_start_and_recovery() {
        let mut db = Db::open_in_memory().unwrap();
        let (id, facts) = ready(&mut db, "test-ns", NS);
        let key = format!("{}{id}", crate::pipeline::INFLIGHT_KEY_PREFIX);
        let owner = "pid123-test-run";
        assert!(db.begin_submission_under_lock(id, &facts, AuthorizedMode::Manual, owner).is_err());
        assert!(db.get_setting(&key).unwrap().is_none());
        assert!(db.acquire_run_lock(owner, chrono::Duration::minutes(2)).unwrap());
        db.begin_submission_under_lock(id, &facts, AuthorizedMode::Manual, owner).unwrap();
        assert!(db.get_setting(&key).unwrap().unwrap().ends_with(owner));
        assert!(db.recover_inflight_except(&[id]).unwrap().is_empty());
        assert_eq!(db.recover_inflight().unwrap(), vec![id]);
        assert_eq!(db.claim(id).unwrap().status, ClaimState::Indeterminate);
        assert!(db.get_setting(&key).unwrap().is_none());
        assert_eq!(db.events(id).unwrap().last().unwrap().actor, "recovery");
    }

    #[test]
    fn a_sent_trip_keeps_its_recorded_facts() {
        let mut db = Db::open_in_memory().unwrap();
        let (c, f) = ready(&mut db, "NS1", NS);
        let jid = db.claim(c).unwrap().journey_id;
        let before = db.journey(jid).unwrap();
        db.begin_submission(c, &f, AuthorizedMode::Manual).unwrap();
        db.transition(c, Some(ClaimState::Submitting), ClaimState::Submitted, "gateway", "t / t").unwrap();
        let mut changed = before.clone();
        changed.check_out = Some(before.check_out.unwrap() + chrono::Duration::minutes(30));
        changed.destination = Some(StationRef { uic: "8400058".into(), name: "C".into() });
        changed.coupling_id = Some("other".into());
        changed.fare_cents = Some(1234);
        db.upsert_journey(&changed).unwrap();
        let after = db.journey(jid).unwrap();
        assert_eq!(after.check_out, before.check_out);
        assert_eq!(after.destination, before.destination);
        assert_eq!(after.coupling_id, before.coupling_id);
        assert_eq!(after.fare_cents, before.fare_cents, "a conflicting card cannot change the fare");
        changed.coupling_id.clone_from(&before.coupling_id);
        db.upsert_journey(&changed).unwrap();
        let after = db.journey(jid).unwrap();
        assert_eq!(after.check_out, before.check_out);
        assert_eq!(after.destination, before.destination);
        assert_eq!(after.fare_cents, Some(1234), "fare details from the recorded card are refreshed");
        // A trip whose claim is still READY takes the correction.
        let (r, _) = ready(&mut db, "NS22", NS);
        let rj = db.claim(r).unwrap().journey_id;
        let mut corr = db.journey(rj).unwrap();
        corr.check_out = Some(corr.check_out.unwrap() + chrono::Duration::minutes(5));
        db.upsert_journey(&corr).unwrap();
        assert_eq!(db.journey(rj).unwrap().check_out, corr.check_out);
    }

    #[test]
    fn leaving_indeterminate_marks_a_claim_as_uncertain() {
        let mut db = Db::open_in_memory().unwrap();
        let (c, _) = ready(&mut db, "NS1", NS);
        assert!(!db.was_uncertain(c).unwrap());
        // Only an event out of INDETERMINATE is left (e.g. an older history).
        db.conn
            .execute(
                "INSERT INTO claim_events(claim_id,at,from_status,to_status,actor,detail) VALUES(?1,'2026-10-29T10:00:00Z','INDETERMINATE','FAILED','user','t')",
                [c],
            )
            .unwrap();
        assert!(db.was_uncertain(c).unwrap());
        let row = db.journey_rows(10).unwrap().into_iter().find(|r| r.claim_id == Some(c)).unwrap();
        assert!(row.was_uncertain);
        let kept: i64 =
            db.conn.query_row(&format!("SELECT COUNT(*) FROM claims WHERE id=?1 AND ({KEEP_CLAIMS_SQL})"), [c], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1, "kept by a history wipe");
    }

    #[test]
    fn stale_lock_recovery_leaves_the_new_owners_run_open() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.acquire_run_lock("old", chrono::Duration::seconds(-1)).unwrap());
        assert!(db.acquire_run_lock("new", chrono::Duration::minutes(30)).unwrap());
        let run = db.start_run("manual", "app").unwrap();
        db.break_stale_lock("old").unwrap();
        assert!(db.runs(1).unwrap()[0]["completed_at"].is_null());
        assert_eq!(db.run_lock_owner().unwrap().as_deref(), Some("new"));
        db.break_stale_lock("new").unwrap();
        assert_eq!(db.runs(1).unwrap()[0]["id"], run);
        assert_eq!(db.runs(1).unwrap()[0]["outcome"], "interrupted");
    }

    fn decision_mail(key: &str, date: &str, amount: i64) -> crate::ns_mail::NsMail {
        crate::ns_mail::NsMail {
            key: key.into(),
            kind: crate::ns_mail::MailKind::Paid,
            reference: Some("600000999".into()),
            amount_cents: Some(amount),
            travel_date: None,
            mail_date: utc(date),
            stations: vec![],
        }
    }

    #[test]
    fn an_older_payment_mail_cannot_replace_a_newer_amount() {
        let mut db = Db::open_in_memory().unwrap();
        let (id, facts) = ready(&mut db, "NS1", NS);
        db.begin_submission(id, &facts, AuthorizedMode::Manual).unwrap();
        db.transition(id, Some(ClaimState::Submitting), ClaimState::Submitted, "gateway", "t").unwrap();
        db.set_ns_reference(id, "600000999").unwrap();
        db.apply_ns_mail(&decision_mail("new", "2026-11-04T12:00:00Z", 321)).unwrap();
        db.apply_ns_mail(&decision_mail("old", "2026-11-03T12:00:00Z", 123)).unwrap();
        assert_eq!(db.claim(id).unwrap().status, ClaimState::Paid);
        assert_eq!(db.paid_cents(id).unwrap(), Some(321));
    }

    #[test]
    fn an_ns_mail_reference_does_not_match_an_arriva_claim() {
        let mut db = Db::open_in_memory().unwrap();
        let (id, _) = ready(&mut db, "AR1", ARRIVA);
        db.transition(id, Some(ClaimState::Ready), ClaimState::Submitting, "user", "t").unwrap();
        db.transition(id, Some(ClaimState::Submitting), ClaimState::Submitted, "gateway", "t").unwrap();
        db.set_ns_reference(id, "600000999").unwrap();
        assert!(db.apply_ns_mail(&decision_mail("ns", "2026-11-03T12:00:00Z", 123)).unwrap().is_none());
        assert_eq!(db.claim(id).unwrap().status, ClaimState::Submitted);
    }

    #[test]
    fn backup_removes_errors_from_the_run_summary_too() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let run = db.start_run("manual", "test").unwrap();
        let summary = RunSummary { run_id: run, journeys_seen: 13, errors: vec!["fake private path".into()], ..Default::default() };
        db.finish_run(run, "error", &summary).unwrap();
        let path = dir.path().join("backup.sqlite");
        db.backup_to(&path, false).unwrap();
        let backup = Db::open(&path).unwrap();
        let runs = backup.runs(1).unwrap();
        assert!(runs[0]["error_summary"].is_null());
        assert_eq!(runs[0]["summary"]["errors"], serde_json::json!([]));
        assert_eq!(runs[0]["summary"]["journeys_seen"], 13);
    }

    fn backup_with(dir: &Path, sql: &str) -> std::path::PathBuf {
        let db = Db::open(&dir.join("live.sqlite")).unwrap();
        let path = dir.join(format!("backup-{}.sqlite", sql.len()));
        db.backup_to(&path, false).unwrap();
        if !sql.is_empty() {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(sql).unwrap();
        }
        path
    }

    #[test]
    fn a_backup_with_its_own_triggers_or_views_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("target.sqlite")).unwrap();
        // A backup as this version makes it is restored.
        let ok = backup_with(dir.path(), "");
        db.restore_from(&ok, true, false).unwrap();
        for sql in [
            "CREATE TRIGGER extra AFTER INSERT ON journeys BEGIN DELETE FROM settings; END;",
            // An own trigger name with another body.
            "DROP TRIGGER claim_events_no_update; CREATE TRIGGER claim_events_no_update BEFORE UPDATE ON claim_events BEGIN DELETE FROM settings; END;",
            "CREATE VIEW v AS SELECT * FROM journeys;",
        ] {
            let path = backup_with(dir.path(), sql);
            let err = db.restore_from(&path, true, false).unwrap_err();
            assert_eq!(err.to_string(), RESTORE_INVALID_MSG, "{sql}");
        }
        // The own triggers are recognised whatever their whitespace.
        let own = Db::own_triggers_and_views().unwrap();
        assert_eq!(own.iter().filter(|(t, _, _)| t == "trigger").count(), 2);
    }
}
