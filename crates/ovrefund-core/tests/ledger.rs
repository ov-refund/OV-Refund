//! Ledger, state machine and pipeline tests (fixture data only — no network).
use chrono::{DateTime, Duration, Utc};
use ovrefund_core::db::Db;
use ovrefund_core::fixtures::*;
use ovrefund_core::model::*;
use ovrefund_core::pipeline::*;
use ovrefund_core::schedule::ScheduleProvider;
use std::collections::HashSet;

// Exercise the archive provider's real prepare path with an injected offline fetch.
#[allow(dead_code)]
#[path = "../src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../src/schedule.rs"]
mod schedule;
pub use ovrefund_core::{db, eligibility, matcher, model, progress, stations};

/// The tests' "now" (fixture trips are from October 2026).
fn t0() -> DateTime<Utc> {
    utc("2026-11-12T10:00:00Z")
}

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// Journey matching the fixture archive: IC 28571 Amersfoort → 's-Hertogenbosch, +33 min (synthetic archive scenario).
fn delayed_journey(id: &str) -> Journey {
    Journey {
        source_id: "fixture".into(),
        source_record_id: id.into(),
        coupling_id: Some("00000000-0000-0000-0000-000000000000".into()),
        card_last4: Some("0000".into()),
        check_in: utc("2026-10-28T20:01:00+01:00"),
        check_out: Some(utc("2026-10-28T22:41:00+01:00")),
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
    }
}

fn on_time_journey() -> Journey {
    let mut j = delayed_journey("ONTIME");
    // IC 28569, +5 at 's-Hertogenbosch (synthetic archive scenario)
    j.check_in = utc("2026-10-28T19:31:00+01:00");
    j.check_out = Some(utc("2026-10-28T21:43:00+01:00"));
    j
}

/// Schedule provider fed from the fixture archive (offline; same matching code path as live).
struct FixtureSchedule {
    inner: ovrefund_core::schedule::ArchiveScheduleProvider,
    services: Vec<Vec<ArchiveStop>>,
}

impl FixtureSchedule {
    fn new(_db: &mut Db) -> Self {
        let st: HashSet<String> = ["AMF", "HT"].iter().map(|s| s.to_string()).collect();
        let services = ovrefund_core::schedule::parse_services(include_str!("fixtures/archive_sample.csv").as_bytes(), false, &st).unwrap();
        let mut inner = ovrefund_core::schedule::ArchiveScheduleProvider::new();
        inner.offline = true;
        FixtureSchedule { inner, services }
    }
}

impl ScheduleProvider for FixtureSchedule {
    fn prepare(&mut self, db: &mut Db, j: &[Journey]) -> anyhow::Result<Vec<String>> {
        let refs: Vec<&Journey> = j.iter().collect();
        self.inner.ingest(db, "2026-10", &self.services, &refs)?;
        Ok(vec![])
    }
    fn evidence(&self, db: &Db, j: &Journey) -> anyhow::Result<ScheduleEvidence> {
        self.inner.evidence(db, j)
    }
}

fn opts(mode: RunMode) -> RunOptions {
    RunOptions { mode, now: utc("2026-11-12T10:00:00Z"), auto_pause: std::time::Duration::ZERO, ..Default::default() }
}

fn run_once(db: &mut Db, src: &mut FixtureJourneySource, gw: &mut FixtureClaimGateway, mode: RunMode) -> RunSummary {
    let mut sched = FixtureSchedule::new(db);
    let mut n = RecordingNotifier::default();
    run(RunContext { db, source: src, schedule: &mut sched, gateway: Some(gw), notifiers: vec![&mut n] }, &opts(mode)).unwrap()
}

#[test]
fn malformed_cached_routes_and_extreme_archive_delays_fail_closed() {
    assert!(serde_json::from_str::<Route>(r#"{"legs":[]}"#).is_err());
    let mut db = Db::open_in_memory().unwrap();
    let schedule = FixtureSchedule::new(&mut db);
    let mut stop = schedule.services[0][0].clone();
    stop.arr_planned = Some(utc("2026-10-28T19:01:00Z"));
    stop.arr_delay_min = Some(i64::MAX);
    stop.dep_delay_min = Some(i64::MIN);
    assert!(stop.arr_actual().is_none());
    assert!(stop.dep_actual().is_none());
    let j = delayed_journey("malformed");
    db.conn
        .execute(
            "INSERT INTO evidence_cache(journey_key, month, evidence_json, computed_at) VALUES(?1,'2026-10',?2,'t')",
            rusqlite::params![
                j.fallback_key(),
                r#"{"provider":"x","historical":true,"planned":{"legs":[]},"taken":null,"status":"found","notes":[]}"#
            ],
        )
        .unwrap();
    assert!(db.cached_evidence(&j.fallback_key()).unwrap().is_none());
}

#[test]
fn migrations_are_idempotent_and_named() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.sqlite");
    let mut db = Db::open(&p).unwrap();
    assert!(db.migrate().unwrap().is_empty(), "second migrate applies nothing");
    let n: i64 = db.conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0)).unwrap();
    assert_eq!(n as usize, ovrefund_core::db::MIGRATIONS.len());
    drop(db);
    Db::open(&p).unwrap(); // reopen ok
}

#[test]
fn journey_dedup_by_record_id_and_fallback_key() {
    let mut db = Db::open_in_memory().unwrap();
    let j = delayed_journey("A1");
    assert!(db.upsert_journey(&j).unwrap().1);
    assert!(!db.upsert_journey(&j).unwrap().1, "same record id");
    let mut j2 = j.clone();
    j2.source_record_id = "A1-different-id".into();
    assert!(!db.upsert_journey(&j2).unwrap().1, "same card/time/stations = same journey");
    let _ = &mut db;
}

#[test]
fn manual_mode_never_submits_and_classifies() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1"), on_time_journey()], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    assert_eq!(s.journeys_seen, 2);
    assert_eq!(s.candidates, 1, "{s:?}");
    assert_eq!(s.submitted, 0);
    assert!(gw.submitted.is_empty());
    let rows = db.journey_rows(10).unwrap();
    let d = rows.iter().find(|r| r.journey.source_record_id == "D1").unwrap();
    assert_eq!(d.claim_status.as_deref(), Some("READY"));
    assert_eq!(d.delay_min, Some(33));
    let o = rows.iter().find(|r| r.journey.source_record_id == "ONTIME").unwrap();
    assert_eq!(o.confidence.as_deref(), Some("not_claimable_or_out_of_scope"));
    assert!(o.claim_status.is_none());
}

#[test]
fn automatic_mode_submits_once_and_reruns_are_idempotent() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let s1 = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s1.submitted, 1);
    let f = &gw.submitted[0];
    assert_eq!(f.scheduled_departure_local, "20:06");
    assert_eq!(f.delay_band, DelayBand::Min30To59);
    // second + third scheduled runs: no duplicates
    let s2 = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let s3 = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s2.submitted + s3.submitted, 0);
    assert_eq!(gw.submitted.len(), 1);
    let c = db.all_claims().unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].status, ClaimState::Submitted);
    assert_eq!(c[0].ns_reference.as_deref(), Some("FIXTURE-1"));
}

#[test]
fn unverified_gateway_blocks_auto_submission() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: false, ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.submitted, 0);
    assert!(gw.submitted.is_empty());
}

#[test]
fn timeout_after_click_is_indeterminate_and_pauses_auto() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j2 = delayed_journey("D2");
    j2.check_in += Duration::seconds(5); // second journey, same train
    j2.card_last4 = Some("1111".into());
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1"), j2], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["unknown"], ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.indeterminate, 1);
    assert_eq!(s.submitted, 0, "stops after an uncertain outcome");
    // next run: still unresolved → no new submissions, no retry
    let s2 = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s2.submitted, 0);
    assert_eq!(gw.submitted.len(), 1, "no automatic retry of an indeterminate claim");
    let uncertain = db.all_claims().unwrap().into_iter().find(|c| c.status == ClaimState::Indeterminate).unwrap();
    let uncertain_tx = db.journey(uncertain.journey_id).unwrap().source_record_id;
    // NS claim list shows it → reconciled to SUBMITTED, then the other one proceeds
    gw.remote = vec![RemoteClaim {
        reference: Some("NS-123".into()),
        travel_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()),
        origin_uic: Some("8400055".into()),
        destination_uic: Some("8400319".into()),
        transaction_id: Some(uncertain_tx.clone()),
        state: Some(ClaimState::Submitted),
        raw_status: "In behandeling".into(),
        amount_cents: None,
    }];
    let s3 = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s3.submitted, 1, "{:?}", s3);
    let st: Vec<ClaimState> = db.all_claims().unwrap().iter().map(|c| c.status).collect();
    assert!(st.iter().all(|s| *s == ClaimState::Submitted), "{st:?}");
}

#[test]
fn final_submission_boundary_checks_external_imports_and_confirmed_facts() {
    let mut db = Db::open_in_memory().unwrap();
    let j = delayed_journey("D1");
    let mut src = FixtureJourneySource { journeys: vec![j.clone()], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    let (_, band, dep, _) = db.assessment(c.journey_id).unwrap().unwrap();
    let mut wrong = build_facts(&j, band.unwrap(), dep.unwrap()).unwrap();
    wrong.source_record_id = "another-trip".into();
    assert!(submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, Some(wrong), t0()).is_err());
    db.add_external_claim(j.check_in.date_naive(), Some(&j.origin.name), Some(&j.destination.as_ref().unwrap().name), "other", None)
        .unwrap();
    for mode in [AuthorizedMode::Manual, AuthorizedMode::Automatic] {
        let err = submit_claim_at(&mut db, &mut gw, c.id, mode, None, t0()).unwrap_err().to_string();
        assert!(err.contains("already claimed elsewhere"));
    }
    assert!(gw.submitted.is_empty());
    assert_eq!(db.claim(c.id).unwrap().attempts, 0);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Ready);
}

#[test]
fn crash_between_submit_and_receipt_recovers_as_indeterminate() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    let j = db.journey(c.journey_id).unwrap();
    let facts = build_facts(&j, DelayBand::Min30To59, utc("2026-10-28T20:06:00+01:00")).unwrap();
    db.begin_submission(c.id, &facts, AuthorizedMode::Manual).unwrap();
    // simulated crash: process ends here. On restart:
    let rec = db.recover_inflight().unwrap();
    assert_eq!(rec, vec![c.id]);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Indeterminate);
    // cannot be submitted again
    assert!(db.begin_submission(c.id, &facts, AuthorizedMode::Manual).is_err());
}

#[test]
fn a_submission_another_running_check_is_busy_with_is_left_alone() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    let j = db.journey(c.journey_id).unwrap();
    let facts = build_facts(&j, DelayBand::Min30To59, utc("2026-10-28T20:06:00+01:00")).unwrap();
    // Another check is submitting this claim; its lease ran out (e.g. the PC slept).
    db.set_setting(&format!("{INFLIGHT_KEY_PREFIX}{}", c.id), &format!("{}|pid999-run-1", Utc::now().to_rfc3339())).unwrap();
    db.begin_submission(c.id, &facts, AuthorizedMode::Automatic).unwrap();
    let alive_run = |alive: bool, db: &mut Db, gw: &mut FixtureClaimGateway| {
        let mut o = opts(RunMode::Automatic);
        o.owner_alive = Some(Box::new(move |owner: &str| alive && owner == "pid999-run-1"));
        run_with(db, &mut FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None }, gw, o)
    };
    // Its process still runs: the claim stays in progress and nothing else is submitted.
    let s = alive_run(true, &mut db, &mut gw);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitting);
    assert_eq!(s.submitted, 0);
    assert_eq!(s.auto_paused_code.as_deref(), Some("uncertain"));
    // An old marker (more than 2 hours) or a process that is gone: uncertain, as after a crash.
    db.set_setting(&format!("{INFLIGHT_KEY_PREFIX}{}", c.id), &format!("{}|pid999-run-1", (Utc::now() - Duration::hours(3)).to_rfc3339()))
        .unwrap();
    assert_eq!(inflight_elsewhere(&db, "pid1-me", &|_| true, Utc::now()).unwrap(), Vec::<i64>::new());
    db.set_setting(&format!("{INFLIGHT_KEY_PREFIX}{}", c.id), &format!("{}|pid999-run-1", Utc::now().to_rfc3339())).unwrap();
    alive_run(false, &mut db, &mut gw);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Indeterminate);
    assert_eq!(db.get_setting(&format!("{INFLIGHT_KEY_PREFIX}{}", c.id)).unwrap(), None, "marker removed");
    assert!(gw.submitted.is_empty());
}

#[test]
fn automatic_submission_marks_and_clears_its_claim_in_progress() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    assert_eq!(run_once(&mut db, &mut src, &mut gw, RunMode::Automatic).submitted, 1);
    let left: i64 =
        db.conn.query_row("SELECT COUNT(*) FROM settings WHERE key LIKE ?1", [format!("{INFLIGHT_KEY_PREFIX}%")], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
}

#[test]
fn a_trip_added_as_claimed_elsewhere_is_blocked_right_away() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Ready);
    let j = db.journey(c.journey_id).unwrap();
    let day = j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
    db.add_external_claim(day, Some("Amersfoort"), Some("'s-Hertogenbosch"), "other", None).unwrap();
    // Applied when it is added, not only at the next check.
    assert_eq!(db.apply_external_claims("fixture", day).unwrap(), vec![c.id]);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::ExternallyClaimed);
    assert!(submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).is_err());
    assert!(gw.submitted.is_empty());
    // Applying again changes nothing; another day is not touched.
    assert!(db.apply_external_claims("fixture", day).unwrap().is_empty());
    assert!(db.apply_external_claims("fixture", day.pred_opt().unwrap()).unwrap().is_empty());
    // Removing the entry gives the trip back for review, as before.
    let id = db.external_claims().unwrap()[0].0;
    db.delete_external_claim(id).unwrap();
    assert_ne!(db.claim(c.id).unwrap().status, ClaimState::ExternallyClaimed);
}

#[test]
fn a_trip_with_another_operator_is_not_waiting_for_timetables() {
    let mut db = Db::open_in_memory().unwrap();
    let mut other = delayed_journey("ARR1");
    other.check_in = utc("2026-10-27T09:01:00+01:00");
    other.check_out = Some(utc("2026-10-27T09:41:00+01:00"));
    // Keolis: no rules in OV Refund (Arriva trips are checked with Arriva's rules and do wait).
    other.carrier_code = Some("KEO".into());
    other.carrier_name = Some("Keolis".into());
    let mut src = FixtureJourneySource { journeys: vec![other, delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway::default();
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let id = |db: &Db, rec: &str| db.journey_rows(10).unwrap().into_iter().find(|r| r.journey.source_record_id == rec).unwrap();
    // Even when the evidence store says "pending", the trip is reported as another operator's.
    let arr = id(&db, "ARR1").id;
    db.conn
        .execute(
            "INSERT OR REPLACE INTO schedules(journey_id, provider, historical, status, evaluated_at) VALUES(?1, 'x', 1, 'pending', 't')",
            [arr],
        )
        .unwrap();
    assert_eq!(id(&db, "ARR1").evidence_status.as_deref(), Some(ovrefund_core::db::OTHER_OPERATOR_STATUS));
    // An NS trip waiting for timetables stays "pending".
    let ns = id(&db, "D1").id;
    db.conn.execute("UPDATE schedules SET status='pending' WHERE journey_id=?1", [ns]).unwrap();
    db.conn
        .execute(
            "INSERT OR IGNORE INTO schedules(journey_id, provider, historical, status, evaluated_at) VALUES(?1, 'x', 1, 'pending', 't')",
            [ns],
        )
        .unwrap();
    assert_eq!(id(&db, "D1").evidence_status.as_deref(), Some("pending"));
}

#[test]
fn claim_write_during_preflight_makes_the_claim_uncertain() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Ready);
    let j = db.journey(c.journey_id).unwrap();
    let facts = build_facts(&j, DelayBand::Min30To59, utc("2026-10-28T20:06:00+01:00")).unwrap();
    let r = db.record_write_during_preflight(c.id, &facts).unwrap();
    assert_eq!(r.status, ClaimState::Indeterminate);
    assert!(db.was_uncertain(c.id).unwrap());
    assert!(db.begin_submission(c.id, &facts, AuthorizedMode::Manual).is_err(), "never sent again unchecked");
    assert!(submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Automatic, None, t0()).is_err());
    assert!(gw.submitted.is_empty());
    // a second observation leaves the uncertain claim as it is
    assert_eq!(db.record_write_during_preflight(c.id, &facts).unwrap().status, ClaimState::Indeterminate);
}

#[test]
fn check_now_submits_only_claims_due_within_the_window() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut run_at = |db: &mut Db, gw: &mut FixtureClaimGateway, now: &str| {
        let mut sched = FixtureSchedule::new(db);
        let mut o = opts(RunMode::Automatic);
        o.now = utc(now);
        o.auto_only_due_within = Some(Duration::days(14));
        run(RunContext { db, source: &mut src, schedule: &mut sched, gateway: Some(gw), notifiers: vec![] }, &o).unwrap()
    };
    // Claim window ends 2027-01-28: not urgent on 12 November, the scheduled check submits it.
    let s = run_at(&mut db, &mut gw, "2026-11-12T10:00:00Z");
    assert_eq!(s.submitted, 0);
    assert!(gw.submitted.is_empty());
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Ready);
    // Within 14 days of the deadline: submitted by the same automatic path.
    let s = run_at(&mut db, &mut gw, "2027-01-20T10:00:00Z");
    assert_eq!(s.submitted, 1, "{s:?}");
    assert_eq!(gw.submitted.len(), 1);
}

#[test]
fn extra_check_before_a_deadline() {
    let now = utc("2026-11-13T10:00:00Z");
    let next = now + Duration::days(7);
    // Deadline well after the next regular check (+3 days margin): no extra check.
    assert_eq!(deadline_run_at(now, next, &[now + Duration::days(11)]), None);
    assert_eq!(deadline_run_at(now, next, &[]), None);
    // Deadline before the next regular check + 3 days: 10:00 Dutch time, 4 days before its
    // last day, earliest deadline first.
    let d = now + Duration::days(9); // 22 November
    assert_eq!(deadline_run_at(now, next, &[now + Duration::days(9) + Duration::hours(5), d]), Some(utc("2026-11-18T10:00:00+01:00")));
    // Daylight saving time: still 10:00 local, also on the day the clocks change.
    let spring = utc("2027-03-20T12:00:00Z");
    let in_week = spring + Duration::days(10);
    assert_eq!(deadline_run_at(spring, in_week, &[utc("2027-03-30T23:59:59+02:00")]), Some(utc("2027-03-26T10:00:00+01:00")));
    assert_eq!(deadline_run_at(spring, in_week, &[utc("2027-04-01T23:59:59+02:00")]), Some(utc("2027-03-28T10:00:00+02:00")));
    // Already less than 4 days left: as soon as possible, but not within the next hour.
    assert_eq!(deadline_run_at(now, next, &[now + Duration::days(2)]), Some(now + Duration::hours(1)));
    // Deadline within the hour or past: nothing to schedule.
    assert_eq!(deadline_run_at(now, next, &[now + Duration::minutes(30)]), None);
    assert_eq!(deadline_run_at(now, next, &[now - Duration::days(1)]), None);
    // A deadline within the hour never takes the extra check away from the other claims.
    assert_eq!(deadline_run_at(now, next, &[now + Duration::minutes(30), d]), Some(utc("2026-11-18T10:00:00+01:00")));
    assert_eq!(deadline_run_at(now, next, &[now + Duration::minutes(59), now + Duration::days(2)]), Some(now + Duration::hours(1)));
}

#[test]
fn notsent_failure_can_be_retried_by_user() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent"], ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.failed, 1);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Failed);
    db.transition(c.id, Some(ClaimState::Failed), ClaimState::Ready, "user", "retry").unwrap();
    let r = submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).unwrap();
    assert_eq!(r.status, ClaimState::Submitted);
}

#[test]
fn illegal_transitions_rejected_and_events_append_only() {
    let mut db = Db::open_in_memory().unwrap();
    let (jid, _) = db.upsert_journey(&delayed_journey("D1")).unwrap();
    let c = db.ensure_claim(jid, "fixture", ClaimState::Candidate).unwrap();
    assert!(db.transition(c.id, None, ClaimState::Submitted, "x", "").is_err(), "cannot skip SUBMITTING");
    assert!(db.transition(c.id, None, ClaimState::Paid, "x", "").is_err());
    assert!(db.conn.execute("DELETE FROM claim_events", []).is_err());
    assert!(db.conn.execute("UPDATE claim_events SET detail='x'", []).is_err());
    // idempotent ensure
    assert_eq!(db.ensure_claim(jid, "fixture", ClaimState::Candidate).unwrap().id, c.id);
}

#[test]
fn submitted_claim_survives_failed_or_empty_lookup() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    reconcile(&mut db, &[]).unwrap();
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Submitted);
}

#[test]
fn approval_and_rejection_reconciled() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let remote = vec![RemoteClaim {
        reference: Some("FIXTURE-1".into()),
        travel_date: None,
        origin_uic: None,
        destination_uic: None,
        transaction_id: None,
        state: Some(ClaimState::Approved),
        raw_status: "Toegekend".into(),
        amount_cents: None,
    }];
    let (n, _) = reconcile(&mut db, &remote).unwrap();
    assert_eq!(n, 1);
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Approved);
}

#[test]
fn external_claim_import_blocks_auto() {
    let mut db = Db::open_in_memory().unwrap();
    db.add_external_claim(
        chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
        Some("Amersfoort"),
        Some("Hertogenbosch"),
        "trein-vertraging.nl",
        None,
    )
    .unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.submitted, 0);
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::ExternallyClaimed);
}

#[test]
fn login_required_persists_run_and_notifies() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![], fail_with: Some(|| FetchError::LoginRequired) };
    let mut gw = FixtureClaimGateway::default();
    let mut sched = FixtureSchedule::new(&mut db);
    let mut n = RecordingNotifier::default();
    let s = run(
        RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: Some(&mut gw), notifiers: vec![&mut n] },
        &opts(RunMode::Automatic),
    )
    .unwrap();
    assert_eq!(s.auth_state, "login_required");
    assert_eq!(n.sent.len(), 1);
    let runs = db.runs(5).unwrap();
    assert_eq!(runs[0]["outcome"], "auth");
}

#[test]
fn failed_notification_is_recorded_and_not_fatal() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![], fail_with: None };
    let mut gw = FixtureClaimGateway::default();
    let mut sched = FixtureSchedule::new(&mut db);
    let mut n = RecordingNotifier { fail: true, ..Default::default() };
    let s = run(
        RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: Some(&mut gw), notifiers: vec![&mut n] },
        &opts(RunMode::Manual),
    )
    .unwrap();
    let st: String = db.conn.query_row("SELECT status FROM notifications WHERE run_id=?1", [s.run_id], |r| r.get(0)).unwrap();
    assert_eq!(st, "failed");
}

#[test]
fn concurrent_run_lock() {
    let db = Db::open_in_memory().unwrap();
    assert!(db.acquire_run_lock("a", Duration::minutes(5)).unwrap());
    assert!(!db.acquire_run_lock("b", Duration::minutes(5)).unwrap());
    db.release_run_lock("a").unwrap();
    assert!(db.acquire_run_lock("b", Duration::minutes(5)).unwrap());
    assert!(!db.acquire_run_lock("c", Duration::minutes(-1)).unwrap(), "a live lease cannot be stolen");
    db.release_run_lock("b").unwrap();
    assert!(db.acquire_run_lock("expired", Duration::minutes(-1)).unwrap());
    assert!(db.acquire_run_lock("c", Duration::minutes(5)).unwrap(), "an expired lease can be taken over");
    assert!(db.renew_run_lock("expired", Duration::minutes(5)).is_err());
    assert_eq!(db.run_lock_owner().unwrap().as_deref(), Some("c"));
}

#[test]
fn pending_archive_month_reported_not_guessed() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j = delayed_journey("NOV");
    j.check_in = utc("2026-11-05T08:00:00Z");
    j.check_out = Some(utc("2026-11-05T09:30:00Z"));
    let mut src = FixtureJourneySource { journeys: vec![j], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.pending_evidence, 1);
    assert_eq!(s.submitted, 0);
}

#[test]
fn lean_migration_keeps_evidence_and_drops_raw_tables() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway::default();
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let j = delayed_journey("D1");
    let ev = db.cached_evidence(&j.fallback_key()).unwrap().expect("evidence cached");
    assert_eq!(ev.status, EvidenceStatus::Found);
    let raw: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('archive_stops','archive_coverage')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(raw, 0, "raw timetable tables are gone");
}

#[test]
fn ns_mail_decisions_update_claims_once() {
    use ovrefund_core::ns_mail::{parse, MailKind};
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway::default();
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual); // READY claim, not filed by the app
    let names = vec!["Amersfoort Centraal".to_string(), "'s-Hertogenbosch".to_string()];
    let m = parse(
        "mail-1",
        "je aanvraag met 600000001 is goedgekeurd.",
        "Reis Amersfoort Centraal - 's-Hertogenbosch op 28-10-2026. We maken € 2,12 over.",
        utc("2026-10-30T10:00:00Z"),
        &names,
    )
    .unwrap();
    assert_eq!(m.kind, MailKind::Approved);
    let u = db.apply_ns_mail(&m).unwrap().expect("matched by date + stations");
    assert!(!u.applied, "a date-only match is only noted for a claim this app may still file");
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Ready, "a date-only match never marks a READY claim as claimed elsewhere");
    assert_eq!(c.ns_reference, None, "no reference from a date-only match");
    assert_eq!(db.paid_cents(c.id).unwrap(), None);
    // ...but the trip is flagged (no automatic submission, manual only after acknowledgement)
    let j = db.journey(c.journey_id).unwrap();
    assert!(db.is_ns_claim_near(&j).unwrap());
    assert!(db.journey_rows(10).unwrap()[0].ns_claim_near);
    let err = approve_for_manual_submission(&mut db, c.id, false, false).unwrap_err().to_string();
    assert!(err.contains(" / ") && err.contains("Mijn teruggaven"), "{err}");
    // processed once: the same mail again changes nothing and adds no second note
    let n_events = db.events(c.id).unwrap().len();
    assert!(db.apply_ns_mail(&m).unwrap().is_none(), "processed once");
    assert!(db.retry_unmatched_mails().unwrap().is_empty());
    assert_eq!(db.events(c.id).unwrap().len(), n_events);
    // the note is bilingual for the GUI
    assert!(db.events(c.id).unwrap().last().unwrap().detail.contains(" / NS e-mail about a claim around this date"));
    // explicit acknowledgement allows the manual submission
    approve_for_manual_submission(&mut db, c.id, false, true).unwrap();
}

#[test]
fn ambiguous_ns_mail_protects_both_cards_until_the_user_checks() {
    use ovrefund_core::ns_mail::{MailKind, NsMail};
    let mut db = Db::open_in_memory().unwrap();
    let mut journeys = two_journeys_same_day();
    journeys[1].coupling_id = Some("test-second-card".into());
    let day = journeys[0].check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
    let mut source = FixtureJourneySource { journeys: journeys.clone(), fail_with: None };
    let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut source, &mut gateway, RunMode::Manual);
    let mail = NsMail {
        key: "fixture-mail".into(),
        kind: MailKind::Submitted,
        reference: Some("fixture-ref".into()),
        amount_cents: None,
        travel_date: Some(day),
        mail_date: t0(),
        stations: Vec::new(),
    };
    assert!(db.apply_ns_mail(&mail).unwrap().is_none(), "neither trip has enough evidence for an exact match");
    assert!(journeys.iter().all(|journey| db.is_ns_claim_near(journey).unwrap()));
    let summary = run_once(&mut db, &mut source, &mut gateway, RunMode::Automatic);
    assert_eq!(summary.submitted, 0);
    assert_eq!(summary.near_skipped, 2);
    assert!(gateway.submitted.is_empty());
}

#[test]
fn date_only_mail_hint_blocks_automatic_submission() {
    use ovrefund_core::ns_mail::{MailKind, NsMail};
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let mut o = opts(RunMode::Automatic);
    // e.g. an NS e-mail (read inside the run lock) about a claim filed elsewhere that day
    o.before_submit = Some(Box::new(|db: &mut Db| {
        let m = NsMail {
            key: "elsewhere".into(),
            kind: MailKind::Submitted,
            reference: Some("600000555".into()),
            amount_cents: None,
            travel_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 28),
            mail_date: utc("2026-10-26T10:00:00Z"),
            stations: vec![],
        };
        db.apply_ns_mail(&m).unwrap();
        vec![]
    }));
    let s = run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!(s.submitted, 0, "{s:?}");
    assert!(gw.submitted.is_empty(), "not submitted unattended");
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Ready, "and not marked claimed elsewhere either");
}

#[test]
fn date_only_mail_never_changes_a_claim_this_app_filed() {
    use ovrefund_core::ns_mail::{MailKind, NsMail};
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Submitted);
    let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 28);
    let mail = |key: &str, kind, reference: Option<&str>| NsMail {
        key: key.into(),
        kind,
        reference: reference.map(str::to_string),
        amount_cents: None,
        travel_date: date,
        mail_date: utc("2026-11-01T10:00:00Z"),
        stations: vec![],
    };
    // (a) a mail with a DIFFERENT application number never falls back to this trip by date
    assert!(db.apply_ns_mail(&mail("x1", MailKind::Rejected, Some("600000999"))).unwrap().is_none());
    // (b) a date-only match without a number is only noted
    let u = db.apply_ns_mail(&mail("x2", MailKind::Rejected, None)).unwrap().unwrap();
    assert!(!u.applied);
    let c2 = db.claim(c.id).unwrap();
    assert_eq!(c2.status, ClaimState::Submitted, "never REJECTED by a date-only match");
    assert_eq!(db.paid_cents(c.id).unwrap(), None);
    assert!(!db.journey_rows(10).unwrap()[0].ns_claim_near, "our own claim is not flagged");
}

#[test]
fn rejection_can_be_reversed_by_exact_reference_and_by_the_user() {
    use ovrefund_core::ns_mail::{MailKind, NsMail};
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    let mail = |key: &str, kind, amount, at: &str| NsMail {
        key: key.into(),
        kind,
        reference: c.ns_reference.clone(),
        amount_cents: amount,
        travel_date: None,
        mail_date: utc(at),
        stations: vec![],
    };
    db.apply_ns_mail(&mail("r", MailKind::Rejected, None, "2026-11-01T10:00:00Z")).unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Rejected);
    assert_eq!(db.paid_cents(c.id).unwrap(), Some(0));
    // an OLDER approval matched late never undoes the newer rejection
    db.apply_ns_mail(&mail("old", MailKind::Approved, None, "2026-10-30T10:00:00Z")).unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Rejected);
    // a NEWER payout by the exact application number reverses it
    db.apply_ns_mail(&mail("p", MailKind::Paid, Some(212), "2026-11-05T10:00:00Z")).unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Paid);
    assert_eq!(db.paid_cents(c.id).unwrap(), Some(212));
    // the user can correct a rejection too; the 0 recorded with it is cleared
    db.conn.execute("UPDATE claims SET status='REJECTED', paid_cents=0 WHERE id=?1", [c.id]).unwrap();
    db.set_status_by_user(c.id, ClaimState::Approved).unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Approved);
    assert_eq!(db.paid_cents(c.id).unwrap(), None, "approved, amount not known yet");
}

#[test]
fn unmatched_mail_is_retried_when_the_trip_appears() {
    use ovrefund_core::ns_mail::{MailKind, NsMail};
    let mut db = Db::open_in_memory().unwrap();
    // a mail about a trip filed elsewhere, before the trip is in the local history
    let m = NsMail {
        key: "early".into(),
        kind: MailKind::Paid,
        reference: Some("600000321".into()),
        amount_cents: Some(342),
        travel_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 28),
        mail_date: utc("2026-10-28T10:00:00Z"),
        stations: vec!["amersfoort centraal".into()],
    };
    assert!(db.apply_ns_mail(&m).unwrap().is_none(), "no trip yet");
    assert_eq!(db.ns_mail_summary().unwrap()["unmatched"], 1);
    // the trip arrives (not claimable: on time), then the mails are retried
    db.upsert_journey(&on_time_journey()).unwrap();
    let u = db.retry_unmatched_mails().unwrap();
    assert_eq!(u.len(), 1, "matched on retry");
    assert!(!u[0].applied);
    assert!(u[0].line(true).contains("Uitbetaald, € 3,42") && u[0].line(false).contains("Paid, €3.42"), "{:?}", u[0]);
    assert!(db.all_claims().unwrap().is_empty(), "date-only mail never creates a permanent claim");
    assert!(db.is_ns_claim_near(&on_time_journey()).unwrap());
    assert_eq!(db.ns_mail_summary().unwrap()["unmatched"], 1);
    assert!(db.retry_unmatched_mails().unwrap().is_empty(), "applied once");
}

#[test]
fn wipe_keeps_duplicate_protection() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(gw.submitted.len(), 1);
    db.wipe_history().unwrap();
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Submitted, "submitted claim survives a wipe");
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(gw.submitted.len(), 1, "no second submission after wipe");
}

#[test]
fn unexpected_outcome_stops_auto_loop() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j2 = delayed_journey("D2");
    j2.check_in += Duration::seconds(5);
    j2.card_last4 = Some("1111".into());
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1"), j2], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent", "ok"], ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!((s.failed, s.submitted), (1, 0), "loop stops after the first non-success");
}

#[test]
fn auto_never_submits_a_trip_already_listed_at_ns() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    // The user already filed this trip on ns.nl: it appears in NS's claim list.
    let mut gw = FixtureClaimGateway {
        verified: true,
        remote: vec![RemoteClaim {
            reference: Some("NS-9".into()),
            travel_date: None,
            origin_uic: None,
            destination_uic: None,
            transaction_id: Some("D1".into()),
            state: Some(ClaimState::Submitted),
            raw_status: "In behandeling".into(),
            amount_cents: None,
        }],
        ..Default::default()
    };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.submitted, 0);
    assert!(gw.submitted.is_empty(), "must not file twice");
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::ExternallyClaimed);
}

#[test]
fn auto_skips_when_ns_claim_list_unavailable() {
    struct NoList(FixtureClaimGateway);
    impl ClaimGateway for NoList {
        fn id(&self) -> &str {
            "nolist"
        }
        fn verified_for_auto(&self) -> bool {
            true
        }
        fn preflight(&mut self, f: &ClaimFacts) -> anyhow::Result<Vec<String>> {
            self.0.preflight(f)
        }
        fn submit(&mut self, f: &ClaimFacts) -> SubmitOutcome {
            self.0.submit(f)
        }
        fn list_remote_claims(&mut self) -> anyhow::Result<Option<Vec<RemoteClaim>>> {
            Ok(None)
        }
    }
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = NoList(FixtureClaimGateway { verified: true, ..Default::default() });
    let mut sched = FixtureSchedule::new(&mut db);
    let s = run(
        RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: Some(&mut gw), notifiers: vec![] },
        &opts(RunMode::Automatic),
    )
    .unwrap();
    assert_eq!(s.submitted, 0);
    assert!(gw.0.submitted.is_empty());
}

fn remote(tx: Option<&str>, reference: Option<&str>, date: Option<(i32, u32, u32)>, state: ClaimState, amount: Option<i64>) -> RemoteClaim {
    RemoteClaim {
        reference: reference.map(str::to_string),
        travel_date: date.map(|(y, m, d)| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()),
        origin_uic: None,
        destination_uic: None,
        transaction_id: tx.map(str::to_string),
        state: Some(state),
        raw_status: "x".into(),
        amount_cents: amount,
    }
}

#[test]
fn exact_recovery_records_the_ns_receipt_and_rejects_a_conflicting_trip_id() {
    let mut db = Db::open_in_memory().unwrap();
    let trip = delayed_journey("test-correct-trip");
    let (id, _) = db.upsert_journey(&trip).unwrap();
    let claim = db.ensure_claim(id, "fixture", ClaimState::Ready).unwrap();
    let facts = build_facts(&trip, DelayBand::Min30To59, trip.check_in).unwrap();
    db.begin_submission(claim.id, &facts, AuthorizedMode::Manual).unwrap();
    db.recover_inflight().unwrap();
    db.set_ns_reference(claim.id, "fixture-ref").unwrap();
    let conflict = remote(Some("other-test-trip"), Some("fixture-ref"), None, ClaimState::Submitted, None);
    reconcile(&mut db, &[conflict]).unwrap();
    assert_eq!(db.claim(claim.id).unwrap().status, ClaimState::Indeterminate);
    let listed = remote(Some(&trip.source_record_id), Some("fixture-ref"), None, ClaimState::Paid, Some(450));
    assert_eq!(reconcile(&mut db, &[listed]).unwrap().0, 1);
    let recovered = db.claim(claim.id).unwrap();
    assert_eq!(recovered.status, ClaimState::Paid);
    assert_eq!(db.paid_cents(claim.id).unwrap(), Some(450));
    assert_eq!(recovered.ns_reference.as_deref(), Some("fixture-ref"));
    assert!(recovered.submitted_at.is_some());
    assert_eq!(db.events(claim.id).unwrap().last().unwrap().actor, "reconcile");
}

#[test]
fn claim_list_amount_never_overwrites_a_known_amount() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    db.set_paid_amount(c.id, Some(417)).unwrap(); // from NS's e-mail
    reconcile(&mut db, &[remote(Some("D1"), None, None, ClaimState::Approved, Some(3))]).unwrap();
    assert_eq!(db.paid_cents(c.id).unwrap(), Some(417));
}

#[test]
fn date_only_match_does_not_confirm_an_uncertain_submission() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["unknown"], ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Indeterminate);
    reconcile(&mut db, &[remote(None, Some("OTHER"), Some((2026, 10, 28)), ClaimState::Submitted, None)]).unwrap();
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Indeterminate, "a claim that day may be someone else's filing");
}

#[test]
fn auto_holds_back_when_ns_lists_an_unexplained_claim_that_day() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j2 = delayed_journey("D2");
    j2.check_in += Duration::seconds(5);
    j2.card_last4 = Some("1111".into());
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1"), j2], fail_with: None };
    let mut gw = FixtureClaimGateway {
        verified: true,
        remote: vec![remote(None, Some("ELSEWHERE"), Some((2026, 10, 28)), ClaimState::Submitted, None)],
        ..Default::default()
    };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.submitted, 0);
    assert!(gw.submitted.is_empty());
}

#[test]
fn claimed_elsewhere_can_be_undone_and_needs_both_stations() {
    let mut db = Db::open_in_memory().unwrap();
    let d = chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
    assert!(db.add_external_claim(d, None, None, "x", None).is_err(), "a date alone would block every trip that day");
    let id = db.add_external_claim(d, Some("Amersfoort"), Some("Hertogenbosch"), "x", None).unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::ExternallyClaimed);
    // deleting the wrong import entry releases the trip again
    db.delete_external_claim(id).unwrap();
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::NeedsReview);
    // a user mis-click can be undone as well
    let c = db.all_claims().unwrap()[0].clone();
    db.mark_external(c.id, "user").unwrap();
    db.undo_external(c.id, "user").unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::NeedsReview);
    // ...but never once a payout was recorded
    db.mark_external(c.id, "user").unwrap();
    db.set_paid_amount(c.id, Some(212)).unwrap();
    assert!(db.undo_external(c.id, "user").is_err());
}

#[test]
fn approval_amount_is_pending_until_paid() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    db.set_status_by_user(c.id, ClaimState::Approved).unwrap();
    db.set_paid_amount(c.id, Some(212)).unwrap();
    assert_eq!(db.totals().unwrap()["paid"]["cents"], 0);
    assert_eq!(db.totals().unwrap()["pending"]["cents"], 212);
    db.set_status_by_user(c.id, ClaimState::Paid).unwrap();
    assert_eq!(db.totals().unwrap()["paid"]["cents"], 212);
    assert_eq!(db.totals().unwrap()["pending"]["cents"], 0);
}

#[test]
fn received_total_counts_real_payouts_only() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    db.mark_external(c.id, "user").unwrap();
    let t = db.totals().unwrap();
    assert_eq!(t["external"]["cents"], 0, "an estimate is not money received");
    db.set_paid_amount(c.id, Some(212)).unwrap();
    assert_eq!(db.totals().unwrap()["external"]["cents"], 212);
}

#[test]
fn claim_listed_by_ns_cannot_be_undone_into_a_duplicate() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway {
        verified: true,
        remote: vec![remote(Some("D1"), Some("NS-77"), None, ClaimState::Submitted, None)],
        ..Default::default()
    };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::ExternallyClaimed);
    assert_eq!(c.ns_reference.as_deref(), Some("NS-77"), "NS's reference is kept");
    assert!(db.undo_external(c.id, "user").is_err(), "NS itself listed it: no undo");
}

#[test]
fn erase_all_leaves_nothing() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    db.set_setting("ns_account_email", "x@example.org").unwrap();
    db.upsert_source("test-card", "mijnns", true, "ok").unwrap();
    db.conn.execute("INSERT INTO cards(source_id,card_type,last4) VALUES('test-card','personal','1234')", []).unwrap();
    db.erase_all().unwrap();
    for t in ["journeys", "claims", "claim_events", "settings", "runs", "candidates", "cards"] {
        let n: i64 = db.conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "{t} not empty");
    }
}

// ---------- duplicate-safety regressions ----------

fn two_journeys_same_day() -> Vec<Journey> {
    let mut j2 = delayed_journey("D2");
    j2.check_in += Duration::seconds(5);
    j2.card_last4 = Some("1111".into());
    vec![delayed_journey("D1"), j2]
}

#[test]
fn uncertain_claim_marked_not_sent_is_never_auto_retried() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["unknown"], ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Indeterminate);
    // user says "Niet verzonden" (it may still have reached NS)
    db.set_status_by_user(c.id, ClaimState::Failed).unwrap();
    assert!(db.was_uncertain(c.id).unwrap());
    for _ in 0..3 {
        run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    }
    assert_eq!(gw.submitted.len(), 1, "no automatic resubmission of a once-uncertain claim");
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Failed);
    // the automatic path refuses it directly as well
    db.transition(c.id, Some(ClaimState::Failed), ClaimState::Ready, "user", "x").unwrap();
    assert!(submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Automatic, None, t0()).is_err());
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(gw.submitted.len(), 1, "not even when it is READY again");
}

#[test]
fn manual_resubmission_of_uncertain_claim_needs_explicit_confirmation() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["unknown"], ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    db.set_status_by_user(c.id, ClaimState::Failed).unwrap();
    let err = approve_for_manual_submission(&mut db, c.id, false, false).unwrap_err().to_string();
    assert!(err.contains(" / "), "bilingual message: {err}");
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Failed, "nothing changed");
    approve_for_manual_submission(&mut db, c.id, true, false).unwrap();
    let r = submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).unwrap();
    assert_eq!(r.status, ClaimState::Submitted);
}

#[test]
fn gateway_not_sent_is_not_uncertain() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent"], ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Failed);
    assert!(!db.was_uncertain(c.id).unwrap());
    // conclusively not sent: the automatic retry still works
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(gw.submitted.len(), 1, "retried after a not-sent failure");
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitted);
}

#[test]
fn user_status_changes_are_limited() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    assert!(db.set_status_by_user(c.id, ClaimState::Failed).is_err(), "READY cannot become FAILED by hand");
    assert!(db.set_status_by_user(c.id, ClaimState::Ready).is_err());
    let j = db.journey(c.journey_id).unwrap();
    let facts = build_facts(&j, DelayBand::Min30To59, utc("2026-10-28T20:06:00+01:00")).unwrap();
    db.begin_submission(c.id, &facts, AuthorizedMode::Manual).unwrap();
    for to in [ClaimState::Failed, ClaimState::Submitted, ClaimState::Dismissed] {
        assert!(db.set_status_by_user(c.id, to).is_err(), "SUBMITTING is never changed by hand ({to:?})");
    }
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitting);
}

#[test]
fn manual_mode_blocks_trips_ns_already_lists() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Ready);
    // the user filed it on ns.nl meanwhile
    gw.remote = vec![remote(Some("D1"), Some("NS-5"), None, ClaimState::Submitted, None)];
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.claim(c.id).unwrap();
    assert_eq!(c.status, ClaimState::ExternallyClaimed);
    assert_eq!(c.ns_reference.as_deref(), Some("NS-5"));
    assert_eq!(db.events(c.id).unwrap().last().unwrap().actor, "reconcile");
    assert!(approve_for_manual_submission(&mut db, c.id, true, false).is_err(), "cannot be submitted any more");
    assert!(gw.submitted.is_empty());
}

#[test]
fn date_near_ns_claim_is_flagged_not_changed() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    let mut gw = FixtureClaimGateway {
        verified: true,
        remote: vec![remote(None, Some("ELSEWHERE"), Some((2026, 10, 28)), ClaimState::Submitted, None)],
        ..Default::default()
    };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let rows = db.journey_rows(10).unwrap();
    let with_claim: Vec<_> = rows.iter().filter(|r| r.claim_id.is_some()).collect();
    assert!(!with_claim.is_empty());
    assert!(with_claim.iter().all(|r| r.ns_claim_near && r.claim_status.as_deref() == Some("READY")), "{with_claim:?}");
    // NS list no longer shows it -> flag cleared on the next run
    gw.remote.clear();
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    assert!(db.journey_rows(10).unwrap().iter().all(|r| !r.ns_claim_near));
}

fn run_with(db: &mut Db, src: &mut FixtureJourneySource, gw: &mut FixtureClaimGateway, o: RunOptions) -> RunSummary {
    let mut sched = FixtureSchedule::new(db);
    run(RunContext { db, source: src, schedule: &mut sched, gateway: Some(gw), notifiers: vec![] }, &o).unwrap()
}

#[test]
fn auto_rechecks_consent_before_each_submission() {
    // Another actor (the app, another process) switches automatic mode off during the pause.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("consent.sqlite");
    let mut db = Db::open(&path).unwrap();
    db.set_setting("mode", "automatic").unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut o = opts(RunMode::Automatic);
    o.auto_pause = std::time::Duration::from_secs(2);
    o.auto_still_allowed = Some(Box::new(|db: &Db| db.get_setting("mode").unwrap().as_deref() == Some("automatic")));
    let actor = second_actor(&path, |db| {
        db.set_setting("mode", "manual").unwrap();
    });
    let s = run_with(&mut db, &mut src, &mut gw, o);
    actor.join().unwrap();
    assert_eq!(s.submitted, 1, "{s:?}");
    assert_eq!(gw.submitted.len(), 1, "second submission skipped once consent is gone");
}

/// Waits until the run has submitted its first claim (it is then in the pause), then acts on the
/// same database through its own connection, like the app or another process would.
fn second_actor(path: &std::path::Path, act: impl FnOnce(&Db) + Send + 'static) -> std::thread::JoinHandle<()> {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let db = Db::open(&path).unwrap();
        let start = std::time::Instant::now();
        while db.claims_in(&[ClaimState::Submitted]).unwrap().is_empty() {
            assert!(start.elapsed() < std::time::Duration::from_secs(60), "the run never submitted");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        act(&db);
    })
}

#[test]
fn stop_request_during_the_pause_stops_after_the_current_claim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stop.sqlite");
    let mut db = Db::open(&path).unwrap();
    // A stop requested before this run started does not affect it.
    db.set_setting(AUTO_STOP_KEY, &(chrono::Utc::now() - Duration::minutes(1)).to_rfc3339()).unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut o = opts(RunMode::Automatic);
    o.auto_pause = std::time::Duration::from_secs(30);
    let (tx, rx) = std::sync::mpsc::channel();
    let actor = second_actor(&path, move |db| {
        // The run holds its lock through the pause: nobody else can take it ...
        let taken = db.acquire_run_lock("pid1-gui-edit", Duration::minutes(2)).unwrap();
        // ... but a stop needs no lock.
        request_auto_stop(db, StopReason::User).unwrap();
        tx.send(taken).unwrap();
    });
    let started = std::time::Instant::now();
    let s = run_with(&mut db, &mut src, &mut gw, o);
    actor.join().unwrap();
    assert!(!rx.recv().unwrap(), "the lock is refused during the pause");
    assert_eq!(s.submitted, 1, "{s:?}");
    assert_eq!(gw.submitted.len(), 1, "no new submission after the stop");
    assert!(started.elapsed() < std::time::Duration::from_secs(15), "the pause ends within about a second of the stop");
    assert_eq!(s.auto_paused_code.as_deref(), Some("stopped"), "{s:?}");
    assert_eq!(s.auto_paused_reason.as_deref(), Some(PAUSED_STOPPED_REASON));
    assert_eq!(db.get_setting(AUTO_STOP_KEY).unwrap(), None, "the stop went with the run");
    assert_eq!(db.claims_in(&[ClaimState::Ready]).unwrap().len(), 1, "the other claim waits");
    assert!(db.run_lock_owner().unwrap().is_none(), "lock released");
    // The next run is not affected by the earlier stop.
    let mut o = opts(RunMode::Automatic);
    o.auto_pause = std::time::Duration::ZERO;
    let s = run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!(s.submitted, 1, "{s:?}");
}

/// Fixture gateway with scripted behaviour for the automatic-submission loop.
#[derive(Default)]
struct ScriptedGateway {
    inner: FixtureClaimGateway,
    /// Added to NS's claim list after the first submission, except the one for the trip just
    /// submitted (filed elsewhere in the meantime).
    appears_after_first: Option<Vec<RemoteClaim>>,
    /// NS's claim list fails from this read on (0-based).
    list_fails_from: Option<usize>,
    lists: usize,
    /// `verified_for_auto` answers false on this call (0-based): a refusal before NS is reached.
    unverified_call: Option<usize>,
    verified_calls: std::cell::Cell<usize>,
    /// Fake clock advanced by this many minutes per submission.
    clock: Option<(std::rc::Rc<std::cell::Cell<i64>>, i64)>,
    /// A settings change while the gateway is preparing the remote form.
    withdraw_during_prepare: Option<std::path::PathBuf>,
    unavailable_session: Option<&'static str>,
    bank_changed: bool,
}

impl ClaimGateway for ScriptedGateway {
    fn id(&self) -> &str {
        "fixture"
    }
    fn verified_for_auto(&self) -> bool {
        let n = self.verified_calls.get();
        self.verified_calls.set(n + 1);
        self.unverified_call != Some(n)
    }
    fn preflight(&mut self, f: &ClaimFacts) -> anyhow::Result<Vec<String>> {
        self.inner.preflight(f)
    }
    fn submit(&mut self, f: &ClaimFacts) -> SubmitOutcome {
        let r = self.inner.submit(f);
        if let Some(extra) = self.appears_after_first.take() {
            self.inner.remote.extend(extra.into_iter().filter(|r| r.transaction_id.as_deref() != Some(f.source_record_id.as_str())));
        }
        if let Some((c, step)) = &self.clock {
            c.set(c.get() + step);
        }
        r
    }
    fn submit_checked(&mut self, facts: &ClaimFacts, check: &dyn Fn() -> anyhow::Result<()>) -> SubmitOutcome {
        if let Some(path) = self.withdraw_during_prepare.take() {
            let db = Db::open(&path).unwrap();
            assert_eq!(db.claims_in(&[ClaimState::Submitting]).unwrap().len(), 1);
            db.set_setting("mode", "manual").unwrap();
        }
        let session = self.unavailable_session.take();
        match (check(), session) {
            (Ok(()), Some(state)) => SubmitOutcome::SessionUnavailable { state: state.into(), reason: auth_message(state).unwrap().into() },
            (Ok(()), None) if std::mem::take(&mut self.bank_changed) => SubmitOutcome::Deferred("fixture bank changed".into()),
            (Ok(()), None) => self.submit(facts),
            (Err(e), _) => SubmitOutcome::NotSent(e.to_string()),
        }
    }
    fn list_remote_claims(&mut self) -> anyhow::Result<Option<Vec<RemoteClaim>>> {
        let n = self.lists;
        self.lists += 1;
        if self.list_fails_from.is_some_and(|f| n >= f) {
            anyhow::bail!("claim list timed out");
        }
        self.inner.list_remote_claims()
    }
}

#[test]
fn session_expiry_stops_an_automatic_run_without_spending_an_attempt() {
    let mut db = Db::open_in_memory().unwrap();
    let journeys = two_journeys_same_day();
    let mut initial = ScriptedGateway {
        inner: FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent"], ..Default::default() },
        ..Default::default()
    };
    run_scripted(&mut db, journeys.clone(), &mut initial, opts(RunMode::Automatic));
    let retry = db.claims_in(&[ClaimState::Failed]).unwrap().first().unwrap().id;
    let fresh = db.claims_in(&[ClaimState::Ready]).unwrap().first().unwrap().id;
    db.transition(fresh, Some(ClaimState::Ready), ClaimState::Dismissed, "user", "fixture").unwrap();
    let attempts = db.claim(retry).unwrap().attempts;
    for (state, message) in [("login_required", LOGIN_NEEDED_MSG), ("account_unreadable", ACCOUNT_UNKNOWN_MSG)] {
        let mut gateway = ScriptedGateway { unavailable_session: Some(state), ..Default::default() };
        let summary = run_scripted(&mut db, journeys.clone(), &mut gateway, opts(RunMode::Automatic));
        assert_eq!(summary.auth_state, state);
        assert_eq!(summary.errors, vec![message]);
        assert!(gateway.inner.submitted.is_empty());
        assert_eq!(db.claim(retry).unwrap().attempts, attempts);
        assert_eq!(db.claim(retry).unwrap().status, ClaimState::Failed);
        assert_eq!(db.get_setting(&format!("{INFLIGHT_KEY_PREFIX}{retry}")).unwrap(), None);
        assert!(db.run_lock_owner().unwrap().is_none());
    }
}

#[test]
fn a_bank_change_before_the_press_does_not_spend_a_submission_attempt() {
    let mut db = Db::open_in_memory().unwrap();
    let journeys = vec![delayed_journey("D1")];
    let mut gateway = ScriptedGateway { bank_changed: true, ..Default::default() };
    run_scripted(&mut db, journeys, &mut gateway, opts(RunMode::Manual));
    let id = db.claims_in(&[ClaimState::Ready]).unwrap()[0].id;
    let owner = "fixture-manual";
    assert!(db.acquire_run_lock(owner, Duration::minutes(10)).unwrap());
    let row = submit_claim_under_lock(&mut db, &mut gateway, id, AuthorizedMode::Manual, None, owner, t0()).unwrap();
    assert_eq!((row.status, row.attempts), (ClaimState::Failed, 0));
    assert!(db.get_setting(&format!("{INFLIGHT_KEY_PREFIX}{id}")).unwrap().is_none());
    assert!(gateway.inner.submitted.is_empty());
}

#[test]
fn an_automatic_retry_deferred_by_a_bank_change_leaves_later_claims_for_the_next_run() {
    let mut db = Db::open_in_memory().unwrap();
    let journeys = two_journeys_same_day();
    let mut source = FixtureJourneySource { journeys: journeys.clone(), fail_with: None };
    let mut initial = FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent"], ..Default::default() };
    run_once(&mut db, &mut source, &mut initial, RunMode::Automatic);
    let failed = db.claims_in(&[ClaimState::Failed]).unwrap().remove(0);
    let ready = db.claims_in(&[ClaimState::Ready]).unwrap().remove(0);
    let mut options = opts(RunMode::Automatic);
    options.before_submit = Some(Box::new(move |db| {
        db.conn
            .execute(
                "UPDATE candidates SET expires_at=?1 WHERE journey_id=?2",
                ((t0() + Duration::days(1)).to_rfc3339(), failed.journey_id),
            )
            .unwrap();
        vec![]
    }));
    let mut gateway = ScriptedGateway { bank_changed: true, ..Default::default() };
    let summary = run_scripted(&mut db, journeys.clone(), &mut gateway, options);
    assert!(!gateway.bank_changed, "the urgent retry reached form preparation");
    assert!(gateway.inner.submitted.is_empty());
    assert_eq!(summary.errors, vec!["fixture bank changed"]);
    assert_eq!(db.claim(failed.id).unwrap().attempts, failed.attempts);
    assert_eq!(db.claim(failed.id).unwrap().status, ClaimState::Failed);
    assert_eq!(db.get_setting(&format!("{INFLIGHT_KEY_PREFIX}{}", failed.id)).unwrap(), None);
    assert_eq!(db.claim(ready.id).unwrap().status, ClaimState::Ready);
    assert_eq!(run_scripted(&mut db, journeys, &mut gateway, opts(RunMode::Automatic)).submitted, 2);
}

#[test]
fn a_conflicting_card_blocks_both_ns_submission_modes() {
    let mut db = Db::open_in_memory().unwrap();
    let mut trip = delayed_journey("D1");
    let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut source = FixtureJourneySource { journeys: vec![trip.clone()], fail_with: None };
    run_once(&mut db, &mut source, &mut gateway, RunMode::Manual);
    let id = db.claims_in(&[ClaimState::Ready]).unwrap()[0].id;
    trip.coupling_id = Some("different-fixture-card".into());
    db.upsert_journey(&trip).unwrap();
    for mode in [AuthorizedMode::Manual, AuthorizedMode::Automatic] {
        assert_eq!(submit_claim_at(&mut db, &mut gateway, id, mode, None, t0()).unwrap_err().to_string(), CARD_CONFLICT_MSG);
    }
    assert_eq!(db.claim(id).unwrap().attempts, 0);
    assert!(gateway.submitted.is_empty());
}

#[test]
fn conflicting_card_observations_in_the_same_import_cannot_clear_the_hold() {
    let trip = delayed_journey("D1");
    let mut other = trip.clone();
    other.coupling_id = Some("different-fixture-card".into());
    for observations in [vec![trip.clone(), other.clone()], vec![other, trip.clone()]] {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_journey(&trip).unwrap();
        let mut gateway = ScriptedGateway::default();
        run_scripted(&mut db, observations, &mut gateway, opts(RunMode::Manual));
        assert!(db.has_card_conflict(&trip).unwrap());
        run_scripted(&mut db, vec![trip.clone()], &mut gateway, opts(RunMode::Manual));
        assert!(!db.has_card_conflict(&trip).unwrap());
    }
}

fn run_scripted(db: &mut Db, journeys: Vec<Journey>, gw: &mut ScriptedGateway, o: RunOptions) -> RunSummary {
    let mut src = FixtureJourneySource { journeys, fail_with: None };
    let mut sched = FixtureSchedule::new(db);
    let mut n = RecordingNotifier::default();
    run(RunContext { db, source: &mut src, schedule: &mut sched, gateway: Some(gw), notifiers: vec![&mut n] }, &o).unwrap()
}

#[test]
fn consent_withdrawn_during_form_preparation_prevents_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite");
    let mut db = Db::open(&path).unwrap();
    db.set_setting("mode", "automatic").unwrap();
    let mut gateway = ScriptedGateway { withdraw_during_prepare: Some(path), ..Default::default() };
    let mut options = opts(RunMode::Automatic);
    options.auto_still_allowed = Some(Box::new(|db| db.get_setting("mode").unwrap().as_deref() == Some("automatic")));
    let summary = run_scripted(&mut db, vec![delayed_journey("D1")], &mut gateway, options);
    assert!(gateway.withdraw_during_prepare.is_none());
    assert!(gateway.inner.submitted.is_empty());
    assert_eq!(summary.submitted, 0);
    assert_eq!(db.claims_in(&[ClaimState::Failed]).unwrap().len(), 1);
}

#[test]
fn claim_list_is_read_again_before_every_further_submission() {
    let mut db = Db::open_in_memory().unwrap();
    // The other trip is filed on ns.nl while the run submits the first one.
    let mut gw = ScriptedGateway {
        inner: FixtureClaimGateway { verified: true, ..Default::default() },
        appears_after_first: Some(vec![
            remote(Some("D1"), Some("NS-OTHER-1"), None, ClaimState::Submitted, None),
            remote(Some("D2"), Some("NS-OTHER-2"), None, ClaimState::Submitted, None),
        ]),
        ..Default::default()
    };
    let s = run_scripted(&mut db, two_journeys_same_day(), &mut gw, opts(RunMode::Automatic));
    assert_eq!(s.submitted, 1, "{s:?}");
    assert_eq!(gw.inner.submitted.len(), 1, "the trip filed elsewhere is never submitted");
    assert_eq!(gw.lists, 2, "read at the start and again before the second claim");
    let d2 = db.all_claims().unwrap().into_iter().find(|c| c.status != ClaimState::Submitted).unwrap();
    assert_eq!(d2.status, ClaimState::ExternallyClaimed);
}

#[test]
fn claim_list_failing_mid_run_stops_automatic_submission() {
    let mut db = Db::open_in_memory().unwrap();
    let mut gw = ScriptedGateway {
        inner: FixtureClaimGateway { verified: true, ..Default::default() },
        list_fails_from: Some(1),
        ..Default::default()
    };
    let s = run_scripted(&mut db, two_journeys_same_day(), &mut gw, opts(RunMode::Automatic));
    assert_eq!(s.submitted, 1, "{s:?}");
    assert_eq!(gw.inner.submitted.len(), 1);
    assert_eq!(db.get_setting(CLAIM_LIST_STATUS_KEY).unwrap().as_deref(), Some("unavailable"));
    assert_eq!(db.claims_in(&[ClaimState::Ready]).unwrap().len(), 1);
}

#[test]
fn no_pause_after_a_claim_refused_before_reaching_ns() {
    let mut db = Db::open_in_memory().unwrap();
    // Call 0: the run's own gate. Call 1: the first claim's final check refuses it.
    let mut gw = ScriptedGateway {
        inner: FixtureClaimGateway { verified: true, ..Default::default() },
        unverified_call: Some(1),
        ..Default::default()
    };
    let mut o = opts(RunMode::Automatic);
    o.auto_pause = std::time::Duration::from_secs(30);
    let started = std::time::Instant::now();
    let s = run_scripted(&mut db, two_journeys_same_day(), &mut gw, o);
    assert_eq!(s.submitted, 1, "{s:?}");
    assert_eq!((s.failed, s.refused), (0, 1), "refused before NS, counted apart: {s:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(15), "no pause: NS was not contacted for the refused claim");
    assert_eq!(gw.lists, 1, "nothing reached NS before the second claim: no extra read");
}

fn three_journeys_same_day() -> Vec<Journey> {
    let mut js = two_journeys_same_day();
    let mut j3 = delayed_journey("D3");
    j3.check_in += Duration::seconds(10);
    j3.card_last4 = Some("2222".into());
    js.push(j3);
    js
}

#[test]
fn time_budget_leaves_the_rest_for_the_next_check() {
    let mut db = Db::open_in_memory().unwrap();
    let minutes = std::rc::Rc::new(std::cell::Cell::new(0i64));
    let mut gw = ScriptedGateway {
        inner: FixtureClaimGateway { verified: true, ..Default::default() },
        clock: Some((minutes.clone(), 50)),
        ..Default::default()
    };
    let mut o = opts(RunMode::Automatic);
    let base = o.now;
    let m = minutes.clone();
    o.clock = Some(Box::new(move || base + Duration::minutes(m.get())));
    let mut n = RecordingNotifier::default();
    let mut src = FixtureJourneySource { journeys: three_journeys_same_day(), fail_with: None };
    let mut sched = FixtureSchedule::new(&mut db);
    let s = run(RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: Some(&mut gw), notifiers: vec![&mut n] }, &o)
        .unwrap();
    // 0 and 50 minutes in: submitted. 100 minutes in (budget 90): left for the next check.
    assert_eq!(s.submitted, 2, "{s:?}");
    assert_eq!(gw.inner.submitted.len(), 2);
    assert_eq!(db.claims_in(&[ClaimState::Ready]).unwrap().len(), 1);
    assert_eq!(s.auto_paused_code.as_deref(), Some("budget"));
    assert_eq!(s.auto_paused_reason.as_deref(), Some(PAUSED_BUDGET_REASON));
    assert_eq!(
        time_budget_line(1),
        "Tijdslimiet bereikt: 1 claim wacht op de volgende controle. / Time limit reached: 1 claim waits for the next check."
    );
}

#[test]
fn urgent_window_includes_exactly_fourteen_days() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    let exp = db.assessment(c.journey_id).unwrap().unwrap().3;
    let mut at = |db: &mut Db, gw: &mut FixtureClaimGateway, now: chrono::DateTime<chrono::Utc>| {
        let mut o = opts(RunMode::Automatic);
        o.now = now;
        o.clock = Some(Box::new(move || now));
        o.auto_only_due_within = Some(Duration::days(14));
        run_with(db, &mut src, gw, o)
    };
    // One second more than 14 days before the deadline: waits for the scheduled check.
    assert_eq!(at(&mut db, &mut gw, exp - Duration::days(14) - Duration::seconds(1)).submitted, 0);
    assert!(gw.submitted.is_empty());
    // Exactly 14 days before the deadline: submitted.
    assert_eq!(at(&mut db, &mut gw, exp - Duration::days(14)).submitted, 1);
    assert_eq!(gw.submitted.len(), 1);
}

#[test]
fn before_submit_hook_runs_before_automatic_submission() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut o = opts(RunMode::Automatic);
    // e.g. an NS e-mail shows the trip was already filed elsewhere
    o.before_submit = Some(Box::new(|db: &mut Db| {
        for c in db.claims_in(&[ClaimState::Ready]).unwrap() {
            db.mark_external(c.id, "ns-mail").unwrap();
        }
        vec![]
    }));
    let s = run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!(s.submitted, 0);
    assert!(gw.submitted.is_empty());
}

#[test]
fn lost_run_lock_is_an_error_and_erase_keeps_live_lease() {
    let mut db = Db::open_in_memory().unwrap();
    assert!(db.acquire_run_lock("a", Duration::minutes(5)).unwrap());
    db.renew_run_lock("a", Duration::minutes(5)).unwrap();
    assert!(db.renew_run_lock("b", Duration::minutes(5)).is_err(), "not ours");
    db.erase_all().unwrap();
    assert_eq!(db.run_lock_owner().unwrap().as_deref(), Some("a"), "a live lease survives the erase");
    db.release_run_lock("a").unwrap();
    assert!(db.renew_run_lock("a", Duration::minutes(5)).is_err(), "released");
    // an expired lease is cleaned up
    assert!(db.acquire_run_lock("old", Duration::minutes(-1)).unwrap());
    db.erase_all().unwrap();
    assert!(db.run_lock_owner().unwrap().is_none());
}

#[test]
fn late_rejection_mail_never_wipes_a_recorded_payout() {
    use ovrefund_core::ns_mail::{MailKind, NsMail};
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    let reference = c.ns_reference.clone();
    let mail = |key: &str, kind: MailKind, amount: Option<i64>| NsMail {
        key: key.into(),
        kind,
        reference: reference.clone(),
        amount_cents: amount,
        travel_date: None,
        mail_date: utc("2026-11-01T10:00:00Z"),
        stations: vec![],
    };
    db.apply_ns_mail(&mail("m1", MailKind::Paid, Some(212))).unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Paid);
    db.apply_ns_mail(&mail("m2", MailKind::Rejected, None)).unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Paid);
    assert_eq!(db.paid_cents(c.id).unwrap(), Some(212), "payout kept");
}

#[test]
fn rejected_mail_records_zero_when_it_applies() {
    use ovrefund_core::ns_mail::{MailKind, NsMail};
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    let m = NsMail {
        key: "r1".into(),
        kind: MailKind::Rejected,
        reference: c.ns_reference.clone(),
        amount_cents: None,
        travel_date: None,
        mail_date: utc("2026-11-01T10:00:00Z"),
        stations: vec![],
    };
    db.apply_ns_mail(&m).unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Rejected);
    assert_eq!(db.paid_cents(c.id).unwrap(), Some(0));
}

#[test]
fn external_prior_only_set_with_a_valid_transition() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Submitted);
    assert!(db.mark_external(c.id, "user").is_err(), "SUBMITTED -> EXTERNALLY_CLAIMED is illegal");
    assert!(!db.claim(c.id).unwrap().external_prior, "flag untouched by the failed transition");
}

#[test]
fn wipe_keeps_once_uncertain_claims() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["unknown"], ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    db.set_status_by_user(c.id, ClaimState::Failed).unwrap();
    db.wipe_history().unwrap();
    assert!(db.was_uncertain(c.id).unwrap(), "history of the uncertain attempt survives a wipe");
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(gw.submitted.len(), 1);
}

#[test]
fn date_only_claim_list_match_is_caution_not_proof() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    // the only trip that day and the only NS claim that day, but no transaction id / reference
    let mut gw = FixtureClaimGateway {
        verified: true,
        remote: vec![remote(None, Some("NS-1"), Some((2026, 10, 28)), ClaimState::Approved, Some(212))],
        ..Default::default()
    };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.submitted, 0);
    assert!(gw.submitted.is_empty(), "not submitted unattended");
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Ready, "not marked claimed elsewhere on a date-only match");
    assert_eq!(c.ns_reference, None);
    assert!(db.journey_rows(10).unwrap()[0].ns_claim_near);
    // a claim this app filed is not changed by a date-only match either
    approve_for_manual_submission(&mut db, c.id, false, true).unwrap();
    let r = submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).unwrap();
    assert_eq!(r.status, ClaimState::Submitted);
    db.conn.execute("UPDATE claims SET ns_reference=NULL WHERE id=?1", [c.id]).unwrap();
    reconcile(&mut db, &[remote(None, Some("NS-1"), Some((2026, 10, 28)), ClaimState::Rejected, None)]).unwrap();
    let c = db.claim(c.id).unwrap();
    assert_eq!(c.status, ClaimState::Submitted);
    assert_eq!(c.ns_reference, None);
}

#[test]
fn near_flag_is_keyed_by_trip_not_row_id() {
    let mut db = Db::open_in_memory().unwrap();
    let j = delayed_journey("D1");
    db.upsert_journey(&j).unwrap();
    db.set_ns_claim_near_keys(&[j.natural_key()]).unwrap();
    assert!(db.journey_rows(10).unwrap()[0].ns_claim_near);
    db.wipe_history().unwrap();
    // another trip may now get the same row id: it must not inherit the flag
    let other = on_time_journey();
    db.upsert_journey(&other).unwrap();
    assert!(!db.journey_rows(10).unwrap()[0].ns_claim_near);
    // the same trip fetched again is still flagged
    db.upsert_journey(&j).unwrap();
    let rows = db.journey_rows(10).unwrap();
    assert!(rows.iter().find(|r| r.journey.source_record_id == "D1").unwrap().ns_claim_near);
}

#[test]
fn old_near_setting_with_row_ids_is_migrated_to_keys() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.sqlite");
    let db = Db::open(&p).unwrap();
    let j = delayed_journey("D1");
    let (id, _) = db.upsert_journey(&j).unwrap();
    // simulate a database from before migration 0007
    db.conn.execute("DELETE FROM schema_migrations WHERE name='0007_mail_match_strength'", []).unwrap();
    db.conn.execute_batch("ALTER TABLE ns_mail DROP COLUMN match_kind; ALTER TABLE ns_mail DROP COLUMN stations_json; ALTER TABLE ns_mail DROP COLUMN hint_key;").unwrap();
    db.set_setting("ns_claim_near", &format!("[{id}]")).unwrap();
    drop(db);
    let db = Db::open(&p).unwrap();
    assert!(db.is_ns_claim_near(&j).unwrap());
}

#[test]
fn review_anyway_restores_the_state_before_dont_claim() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway::default();
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Ready);
    db.set_status_by_user(c.id, ClaimState::Dismissed).unwrap();
    db.set_status_by_user(c.id, ClaimState::NeedsReview).unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Ready, "READY stays READY");
    let h = db.events(c.id).unwrap();
    assert_eq!(h.last().unwrap().detail, "Door jou teruggezet / Restored by you");
}

#[test]
fn duplicate_claimed_elsewhere_entries_are_refused() {
    let db = Db::open_in_memory().unwrap();
    let d = chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
    db.add_external_claim(d, Some("Amersfoort"), Some("Hertogenbosch"), "x", None).unwrap();
    let err = db.add_external_claim(d, Some("amersfoort centraal"), Some("'s-Hertogenbosch"), "x", None).unwrap_err().to_string();
    assert!(err.contains(" / ") && err.contains("already"), "{err}");
    // a trip that already has a claim filed by this app
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let err = db.add_external_claim(d, Some("Amersfoort"), Some("Hertogenbosch"), "x", None).unwrap_err().to_string();
    assert!(err.contains("(Ingediend)") && err.contains("(Submitted)"), "{err}");
}

#[test]
fn history_notes_are_bilingual() {
    use ovrefund_core::db::history_detail;
    assert_eq!(history_detail("created"), "Aangemaakt / Created");
    assert_eq!(history_detail("not sent: x"), "Niet verzonden: x / Not sent: x");
    assert_eq!(
        history_detail("authorized; delay band 30 t/m 59 min"),
        "Toestemming gegeven; vertraging 30 t/m 59 min / Authorized; delay 30–59 min"
    );
    assert_eq!(history_detail("A / B"), "A / B", "already bilingual");
    assert_eq!(
        history_detail("estimated arrival delay 33 min (30 t/m 59 min)"),
        "estimated arrival delay 33 min (30 t/m 59 min)",
        "policy reasons: GUI"
    );
}

#[test]
fn newer_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.sqlite");
    let db = Db::open(&p).unwrap();
    db.conn.execute("INSERT INTO schema_migrations(name, applied_at) VALUES ('9999_future', 'x')", []).unwrap();
    drop(db);
    let e = Db::open(&p).err().expect("refused").to_string();
    assert!(e.contains("Installeer de nieuwste versie") && e.contains(" / "), "{e}");
    // a higher user_version is refused too
    let p2 = dir.path().join("u.sqlite");
    let db = Db::open(&p2).unwrap();
    db.conn.pragma_update(None, "user_version", 999).unwrap();
    drop(db);
    assert!(Db::open(&p2).is_err());
}

#[test]
fn login_problems_set_their_own_auth_state() {
    for (f, state, text) in [
        ((|| FetchError::AutoLoginFailed) as fn() -> FetchError, "autologin_failed", "NS-wachtwoord gewijzigd"),
        (|| FetchError::AccountMismatch, "account_mismatch", "ander NS-account dan eerder"),
        (|| FetchError::AccountUnreadable, "account_unreadable", "kon niet controleren met welk NS-account"),
        (|| FetchError::Blocked(BLOCKED_VERIFICATION.into()), "blocked: verificatiecode", "extra controle"),
        (|| FetchError::LoginRequired, "login_required", "moet opnieuw inloggen bij NS"),
    ] {
        let mut db = Db::open_in_memory().unwrap();
        let mut src = FixtureJourneySource { journeys: vec![], fail_with: Some(f) };
        let mut gw = FixtureClaimGateway::default();
        let s = run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
        assert_eq!(s.auth_state, state);
        assert!(s.errors[0].contains(text), "{:?}", s.errors);
        let id = JourneySource::id(&src).to_string();
        assert_eq!(db.source_status(&id).unwrap().unwrap().1.as_deref(), Some(state));
    }
}

#[test]
fn claim_list_status_and_expiring_soon() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    assert_eq!(db.get_setting(CLAIM_LIST_STATUS_KEY).unwrap().as_deref(), Some("ok"));
    // fictional trip of 28 Oct 2026 expires 28 Jan 2027: not within 14 days of 12 Nov
    assert_eq!(s.expiring_soon, 0);
    assert_eq!(db.expiring_soon(utc("2027-01-20T10:00:00Z"), Duration::days(14)).unwrap(), 1);
    assert_eq!(db.expiring_soon(utc("2027-01-29T10:00:00Z"), Duration::days(14)).unwrap(), 0, "expired is not 'soon'");
}

#[test]
fn exact_refund_below_minimum_is_not_claimable() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j = delayed_journey("CHEAP");
    j.fare_cents = Some(400); // 50% = EUR 2.00 < EUR 2.30
    let mut src = FixtureJourneySource { journeys: vec![j], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.submitted + s.candidates, 0);
    let r = &db.journey_rows(10).unwrap()[0];
    assert!(r.below_minimum);
    assert_eq!(r.reasons[0], ovrefund_core::eligibility::BELOW_MINIMUM_REASON);
    assert_eq!(r.confidence.as_deref(), Some("not_claimable_or_out_of_scope"));
    assert!(r.claim_status.is_none());
}

#[test]
fn rows_carry_carrier_page_and_possible_delay() {
    let db = Db::open_in_memory().unwrap();
    let mut other = delayed_journey("ARR");
    other.carrier_code = Some("12".into());
    other.carrier_name = Some("Arriva".into());
    other.check_in = utc("2026-10-20T08:00:00Z");
    other.check_out = Some(utc("2026-10-20T08:40:00Z"));
    db.upsert_journey(&other).unwrap();
    // two NS trips on the same route: 50 min (fast) and 100 min (waiting for timetable data)
    let mut fast = delayed_journey("FAST");
    fast.check_in = utc("2026-11-01T08:00:00Z");
    fast.check_out = Some(utc("2026-11-01T08:50:00Z"));
    let mut slow = delayed_journey("SLOW");
    slow.check_in = utc("2026-11-03T08:00:00Z");
    slow.check_out = Some(utc("2026-11-03T09:40:00Z"));
    let (_, _) = db.upsert_journey(&fast).unwrap();
    let (sid, _) = db.upsert_journey(&slow).unwrap();
    let pending = ScheduleEvidence {
        provider: "t".into(),
        historical: true,
        planned: None,
        taken: None,
        status: EvidenceStatus::Pending,
        notes: vec![],
        regular_arrival: None,
        regular_checked: false,
    };
    db.save_evidence(sid, &pending).unwrap();
    let rows = db.journey_rows(10).unwrap();
    let by = |id: &str| rows.iter().find(|r| r.journey.source_record_id == id).unwrap();
    assert_eq!(by("ARR").carrier_url.as_deref(), Some("https://www.arriva.nl"));
    assert_eq!(by("SLOW").carrier_url, None);
    assert_eq!(by("SLOW").possible_delay_min, Some(50));
    assert_eq!(by("FAST").possible_delay_min, None, "has no pending timetable data");
}

#[test]
fn subscription_history_groups_recent_products() {
    let db = Db::open_in_memory().unwrap();
    let now = utc("2026-11-12T10:00:00Z");
    for (id, day, sub, fee) in [
        ("S1", "2026-08-01T10:00:00Z", "DALKORT", 635),
        ("S2", "2026-09-10T10:00:00Z", "DALKORT,FLEXWVR", 4200),
        ("S3", "2026-11-01T10:00:00Z", "DALKORT,FLEXWVR", 4100),
        ("OLD", "2026-01-01T10:00:00Z", "FLEXAVR", 39995),
    ] {
        let mut j = delayed_journey(id);
        j.check_in = utc(day);
        j.check_out = None;
        j.subscription = Some(sub.into());
        j.subscription_fee_cents = Some(fee);
        db.upsert_journey(&j).unwrap();
    }
    let v = db.subscription_history(now).unwrap();
    assert_eq!(v["none"], false);
    assert_eq!(v["current"]["fee_cents"], 4100);
    assert_eq!(v["current"]["class"], 2);
    assert_eq!(v["current"]["parts"][1]["code"], "FLEXWVR");
    assert_eq!(v["current"]["parts"][1]["fractions"], serde_json::json!([12, 6]));
    let prev = v["previous"].as_array().unwrap();
    assert_eq!(prev.len(), 2, "older than 120 days is left out");
    assert_eq!(prev[0]["fee_cents"], 4200);
    assert_eq!(prev[1]["parts"][0]["kind"], "discount");
    assert!(prev[0]["until"].as_str().unwrap().starts_with("2026-09-10"));
    let empty = Db::open_in_memory().unwrap().subscription_history(now).unwrap();
    assert_eq!(empty["none"], true);
    assert!(empty["current"].is_null());
}

#[test]
fn truncated_backup_is_reported_as_damaged() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(&dir.path().join("live.sqlite")).unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1"), on_time_journey()], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let bk = dir.path().join("b.ovrbackup");
    db.backup_to(&bk, false).unwrap();
    let full = std::fs::read(&bk).unwrap();
    assert!(Db::check_backup_file(&bk, false).is_ok());
    for keep in [full.len() / 2, full.len() * 3 / 4, 4096 + 100] {
        let cut = dir.path().join(format!("cut-{keep}.ovrbackup"));
        std::fs::write(&cut, &full[..keep.min(full.len() - 1)]).unwrap();
        let e = Db::check_backup_file(&cut, false).unwrap_err().to_string();
        assert_eq!(e, db::RESTORE_DAMAGED_MSG, "cut at {keep}");
        let mut fresh = Db::open(&dir.path().join(format!("new-{keep}.sqlite"))).unwrap();
        assert_eq!(fresh.restore_from(&cut, false, false).unwrap_err().to_string(), db::RESTORE_DAMAGED_MSG);
    }
    // Some other SQLite file, or no database at all: not an OV Refund backup.
    let other = dir.path().join("other.sqlite");
    rusqlite::Connection::open(&other).unwrap().execute_batch("CREATE TABLE notes(x TEXT); INSERT INTO notes VALUES('a');").unwrap();
    assert_eq!(Db::check_backup_file(&other, false).unwrap_err().to_string(), db::RESTORE_INVALID_MSG);
    let junk = dir.path().join("junk.ovrbackup");
    std::fs::write(&junk, b"hello").unwrap();
    assert_eq!(Db::check_backup_file(&junk, false).unwrap_err().to_string(), db::RESTORE_INVALID_MSG);
}

#[test]
fn backup_restore_and_csv() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(&dir.path().join("live.sqlite")).unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1"), on_time_journey()], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    db.set_setting("mode", "automatic").unwrap();
    db.set_setting("ui_lang", "en").unwrap();
    db.add_external_claim(chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(), Some("Utrecht"), Some("Zwolle"), "other", None).unwrap();
    let csv = db.claims_csv(true).unwrap();
    assert!(csv.starts_with("datum;vertrek;van;naar"));
    assert!(csv.contains("2026-10-28;20:01;Amersfoort Centraal;'s-Hertogenbosch;33;Te claimen;"), "{csv}");
    assert!(db.claims_csv(false).unwrap().contains(";Claimable;"));

    let bk = dir.path().join("b.ovrbackup");
    db.set_setting("smtp", r#"{"host":"smtp.gmail.com","username":"me@example.org","to":"me@example.org"}"#).unwrap();
    db.set_setting("notify_email", "1").unwrap();
    db.set_setting("ns_account_email", "me@example.org").unwrap();
    db.set_setting("disclaimer_accepted", "1").unwrap();
    let run = db.start_run("manual", "test").unwrap();
    db.conn.execute("UPDATE runs SET error_summary='private error text' WHERE id=?1", [run]).unwrap();
    db.backup_to(&bk, false).unwrap();
    assert!(db.backup_to(&bk, false).is_err(), "never overwrites");
    let bytes = std::fs::read(&bk).unwrap();
    for private in [b"me@example.org".as_slice(), b"private error text"] {
        assert!(!bytes.windows(private.len()).any(|window| window == private));
    }
    {
        let b = rusqlite::Connection::open(&bk).unwrap();
        let errs: i64 = b.query_row("SELECT COUNT(*) FROM runs WHERE error_summary IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(errs, 0, "run error texts are not in a backup");
        let keys: Vec<String> = {
            let mut st = b.prepare("SELECT key FROM settings ORDER BY key").unwrap();
            let v = st.query_map([], |r| r.get(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            v
        };
        assert_eq!(keys, vec!["ns_claim_near".to_string(), "ui_lang".to_string()], "only allowlisted settings are in a backup");
        let n: i64 = b.query_row("SELECT COUNT(*) FROM settings WHERE value LIKE '%@%'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "no e-mail addresses in a backup");
    }
    // restore into a fresh ledger (on a copy, as the app does)
    let copy = dir.path().join("copy.sqlite");
    std::fs::copy(&bk, &copy).unwrap();
    let mut fresh = Db::open(&dir.path().join("new.sqlite")).unwrap();
    let st = fresh.restore_from(&copy, false, false).unwrap();
    assert_eq!(st.journeys, 2);
    assert_eq!(st.claims, 1);
    assert_eq!(st.kept_claims, 0);
    assert_eq!((st.from_backup_journeys, st.from_backup_claims), (2, 1), "counted from the backup file");
    assert_eq!(st.unassessed, 2, "open trips wait for the next check");
    assert_eq!(fresh.external_claims().unwrap().len(), 1);
    assert_eq!(fresh.get_setting("ui_lang").unwrap().as_deref(), Some("en"));
    assert_eq!(fresh.get_setting("mode").unwrap(), None);
    assert_eq!(fresh.all_claims().unwrap()[0].status, ClaimState::Ready);
    // Restored READY is a ledger state, never authority to submit without a new assessment.
    let restored = fresh.all_claims().unwrap().remove(0);
    assert!(fresh.assessment(restored.journey_id).unwrap().is_none());
    let error = submit_claim_at(&mut fresh, &mut gw, restored.id, AuthorizedMode::Automatic, None, t0()).unwrap_err();
    assert!(error.to_string() == NOT_ASSESSED_MSG, "{error}");
    for table in ["candidates", "schedules", "evidence_cache"] {
        let count: i64 = fresh.conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "restored {table} must be recomputed");
    }

    // append-only trigger is back
    assert!(fresh.conn.execute("DELETE FROM claim_events", []).is_err());
    // a non-empty ledger is never replaced without force
    std::fs::copy(&bk, &copy).unwrap();
    let e = fresh.restore_from(&copy, false, false).unwrap_err().to_string();
    assert!(e.contains(" / ") && e.contains("vervangen"), "{e}");
    assert!(fresh.restore_from(&copy, true, false).is_ok());
    // not a backup / newer backup
    let junk = dir.path().join("junk.ovrbackup");
    std::fs::write(&junk, b"hello").unwrap();
    assert!(fresh.restore_from(&junk, true, false).is_err());
    std::fs::copy(&bk, &copy).unwrap();
    rusqlite::Connection::open(&copy).unwrap().pragma_update(None, "user_version", 999).unwrap();
    assert!(fresh.restore_from(&copy, true, false).unwrap_err().to_string().contains("ouder dan de back-up"));
}

#[test]
fn restore_never_takes_sensitive_settings_even_from_a_crafted_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("a.sqlite")).unwrap();
    let bk = dir.path().join("b.ovrbackup");
    db.backup_to(&bk, false).unwrap();
    {
        // a hand-made "backup" with settings an attacker would like to plant
        let b = rusqlite::Connection::open(&bk).unwrap();
        for (k, v) in [
            ("smtp", r#"{"host":"evil.example","username":"x","to":"attacker@evil.example"}"#),
            ("notify_email", "1"),
            ("read_ns_mail", "1"),
            ("mode", "automatic"),
            ("auto_opt_in_at", "2026-01-01T00:00:00Z"),
            ("auto_unverified_override", "1"),
            ("other_service", "import_done"),
            ("disclaimer_accepted", "1"),
            ("wizard_done", "1"),
            ("onboarded", "1"),
            ("ns_account_email", "attacker@evil.example"),
            ("autologin_failures", "0"),
            ("claim_list_status", "ok"),
            ("lookback_days", "9999"),
            ("max_auto_submissions", "20"),
            ("interval_days", "1"),
            ("notify_windows", "0"),
        ] {
            b.execute("INSERT OR REPLACE INTO settings(key,value) VALUES(?1,?2)", [k, v]).unwrap();
        }
    }
    let mut fresh = Db::open(&dir.path().join("n.sqlite")).unwrap();
    fresh.set_setting("interval_days", "7").unwrap();
    fresh.restore_from(&bk, false, false).unwrap();
    let settings: Vec<(String, String)> = fresh.all_settings().unwrap();
    let keys: Vec<&str> = settings.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["interval_days", "notify_windows"], "{settings:?}");
    assert_eq!(fresh.get_setting("interval_days").unwrap().as_deref(), Some("7"), "local interval kept");
    // a view or unknown trigger in a backup is refused
    let bad = dir.path().join("bad.ovrbackup");
    db.backup_to(&bad, false).unwrap();
    rusqlite::Connection::open(&bad)
        .unwrap()
        .execute_batch("CREATE TRIGGER evil AFTER INSERT ON settings BEGIN DELETE FROM journeys; END;")
        .unwrap();
    let e = fresh.restore_from(&bad, true, false).unwrap_err().to_string();
    assert!(e.contains("geen back-up"), "{e}");
    // a demo backup is refused by the real app, accepted by the demo
    let demo = dir.path().join("demo.ovrbackup");
    db.backup_to(&demo, true).unwrap();
    let e = fresh.restore_from(&demo, true, false).unwrap_err().to_string();
    assert!(e.contains("demo"), "{e}");
    assert!(fresh.restore_from(&demo, true, true).is_ok());
}

#[test]
fn forced_restore_keeps_claims_that_reached_ns() {
    let dir = tempfile::tempdir().unwrap();
    // The backup: trip D1 still claimable (READY), and an "on time" trip.
    let mut old = Db::open(&dir.path().join("old.sqlite")).unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1"), on_time_journey()], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut old, &mut src, &mut gw, RunMode::Manual);
    let bk = dir.path().join("b.ovrbackup");
    old.backup_to(&bk, false).unwrap();
    // The current ledger: D1 was submitted since (reached NS), plus a "claimed elsewhere" entry
    // and an "NS may already have a claim" flag.
    let mut db = Db::open(&dir.path().join("live.sqlite")).unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    let r = submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).unwrap();
    assert_eq!(r.status, ClaimState::Submitted);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitted);
    db.add_external_claim(chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), Some("Utrecht"), Some("Zwolle"), "other", None).unwrap();
    db.set_ns_claim_near_keys(&["2026-10-01T08:00:00Z|8400621".to_string()]).unwrap();
    let events_before = db.events(c.id).unwrap().len();

    let st = db.restore_from(&bk, true, false).unwrap();
    assert_eq!(st.kept_claims, 1);
    assert_eq!(st.replaced_from_backup, 1, "the backup's READY claim for D1 is replaced");
    assert_eq!(st.kept_external, 1);
    assert_eq!(st.journeys, 2);
    let claims = db.all_claims().unwrap();
    assert_eq!(claims.len(), 1);
    let kept = &claims[0];
    assert_eq!(kept.status, ClaimState::Submitted, "a claim that reached NS is never turned back into READY");
    assert!(db.assessment(kept.journey_id).unwrap().is_some(), "kept claim assessment survives");
    assert!(kept.ns_reference.is_some());
    assert_eq!(db.events(kept.id).unwrap().len(), events_before, "history kept");
    assert_eq!(db.journey(kept.journey_id).unwrap().source_record_id, "D1");
    assert_eq!(db.external_claims().unwrap().len(), 1);
    assert!(db.ns_claim_near_keys().unwrap().contains("2026-10-01T08:00:00Z|8400621"), "flags merged");
    // kept rows keep counting as submitted here; restored rows never do
    let here: i64 = db.conn.query_row("SELECT submitted_here FROM claims WHERE id=?1", [kept.id], |r| r.get(0)).unwrap();
    assert_eq!(here, 1);
    // no deleted rows left in the WAL
    assert!(db.conn.execute("DELETE FROM claim_events", []).is_err(), "append-only trigger is back");
}

#[test]
fn restored_submissions_never_count_as_submitted_here() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(&dir.path().join("a.sqlite")).unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).unwrap();
    let bk = dir.path().join("b.ovrbackup");
    db.backup_to(&bk, false).unwrap();
    let mut fresh = Db::open(&dir.path().join("n.sqlite")).unwrap();
    fresh.restore_from(&bk, false, false).unwrap();
    let n: i64 = fresh.conn.query_row("SELECT COUNT(*) FROM claims WHERE submitted_here=1", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    let n: i64 = fresh.conn.query_row("SELECT COUNT(*) FROM claims WHERE status='SUBMITTED'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "the claim itself is restored (duplicate protection)");
}

#[test]
fn csv_cells_never_start_a_formula() {
    use ovrefund_core::db::csv_safe;
    for s in ["=1+1", "+31", "-5", "@SUM(A1)", " =cmd", "  @x", "\t=1", "\rx"] {
        assert!(csv_safe(s).starts_with('\''), "{s:?}");
    }
    for s in ["Utrecht Centraal", "'s-Hertogenbosch", "12,50", ""] {
        assert_eq!(csv_safe(s), s);
    }
}

#[test]
fn bilingual_details_split_in_two() {
    use ovrefund_core::db::bilingual_detail;
    assert_eq!(bilingual_detail("Niet verzonden: ", "Not sent: ", "x"), "Niet verzonden: x / Not sent: x");
    assert_eq!(bilingual_detail("Niet verzonden: ", "Not sent: ", "a / b"), "Niet verzonden: a / Not sent: b");
    let d = bilingual_detail("A: ", "B: ", "x / y / z");
    assert_eq!(d.matches(" / ").count(), 1, "{d}");
}

#[test]
fn oversized_restore_is_rejected_before_opening() {
    use ovrefund_core::db::{BACKUP_TOO_LARGE_MSG, MAX_BACKUP_BYTES};
    assert_eq!(MAX_BACKUP_BYTES, 20 * 1024 * 1024);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.ovrbackup");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_BACKUP_BYTES as u64 + 1).unwrap();
    let mut db = Db::open_in_memory().unwrap();
    assert_eq!(db.restore_from(&path, true, false).unwrap_err().to_string(), BACKUP_TOO_LARGE_MSG);
}

#[test]
fn receipt_and_status_are_atomic_when_storage_fails() {
    let mut db = Db::open_in_memory().unwrap();
    let j = delayed_journey("receipt");
    let (jid, _) = db.upsert_journey(&j).unwrap();
    let c = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
    let facts = build_facts(&j, DelayBand::Min30To59, j.check_in).unwrap();
    db.begin_submission(c.id, &facts, AuthorizedMode::Manual).unwrap();
    let before = db.events(c.id).unwrap().len();
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_receipt BEFORE UPDATE OF ns_reference ON claims BEGIN SELECT RAISE(ABORT, 'storage failure'); END;",
        )
        .unwrap();
    let receipt = SubmissionReceipt { reference: Some("NS-RECEIPT".into()), confirmed: true, detail: "Ontvangen / Received".into() };
    assert!(db.complete_submission(c.id, &receipt).is_err());
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitting);
    assert_eq!(db.events(c.id).unwrap().len(), before);
    assert!(db.claim(c.id).unwrap().submitted_at.is_none());
    db.conn.execute_batch("DROP TRIGGER fail_receipt").unwrap();
    let saved = db.complete_submission(c.id, &receipt).unwrap();
    assert_eq!(saved.status, ClaimState::Submitted);
    assert_eq!(saved.ns_reference.as_deref(), Some("NS-RECEIPT"));
    assert!(saved.submitted_at.is_some());
    assert!(db.complete_submission(c.id, &receipt).is_err());
    assert_eq!(db.events(c.id).unwrap().len(), before + 1);
}

#[test]
fn monthly_assessment_is_visible_before_next_month_and_submission_waits_until_end() {
    use ovrefund_core::schedule::MonthReady;
    use std::{cell::Cell, rc::Rc};
    struct Monthly {
        fixture: FixtureSchedule,
        completed: Rc<Cell<usize>>,
        fail_second: bool,
    }
    impl ScheduleProvider for Monthly {
        fn prepare(&mut self, _: &mut Db, _: &[Journey]) -> anyhow::Result<Vec<String>> {
            unreachable!()
        }
        fn evidence(&self, db: &Db, j: &Journey) -> anyhow::Result<ScheduleEvidence> {
            self.fixture.evidence(db, j)
        }
        fn prepare_incremental(&mut self, db: &mut Db, js: &[Journey], ready: &mut MonthReady<'_>) -> anyhow::Result<Vec<String>> {
            for (i, month) in ["2026-09", "2026-10"].into_iter().enumerate() {
                if i == 1 && self.fail_second {
                    anyhow::bail!("fixture interrupted after first month");
                }
                let shift = Duration::days(if i == 0 { -31 } else { 0 });
                let mut services = self.fixture.services.clone();
                for stop in services.iter_mut().flatten() {
                    stop.service_date += shift;
                    stop.dep_planned = stop.dep_planned.map(|t| t + shift);
                    stop.arr_planned = stop.arr_planned.map(|t| t + shift);
                }
                self.fixture.inner.ingest(db, month, &services, &js.iter().collect::<Vec<_>>())?;
                // Ingest alone caches evidence; the pipeline callback must also save the
                // public evidence, assessment and claim state before it returns.
                ready(db, self, month, i + 1, 2)?;
                let rows = db.journey_rows(10)?;
                let ready = rows.iter().filter(|r| r.claim_status.as_deref() == Some("READY")).count();
                assert_eq!(ready, i + 1);
                assert!(rows.iter().filter(|r| r.claim_status.is_some()).all(|r| r.delay_min == Some(33)));
                assert!(db.all_claims()?.iter().all(|c| c.status == ClaimState::Ready), "nothing submitted between months");
                self.completed.set(i + 1);
            }
            Ok(vec![])
        }
    }
    for fail_second in [false, true] {
        let mut db = Db::open_in_memory().unwrap();
        let october = delayed_journey("OCT");
        let mut september = delayed_journey("SEP");
        september.check_in -= Duration::days(31);
        september.check_out = september.check_out.map(|t| t - Duration::days(31));
        let mut source = FixtureJourneySource { journeys: vec![october, september], fail_with: None };
        let completed = Rc::new(Cell::new(0));
        let mut schedule = Monthly { fixture: FixtureSchedule::new(&mut db), completed: completed.clone(), fail_second };
        let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
        let before = Rc::new(Cell::new(0));
        let before_calls = before.clone();
        let mut options = opts(RunMode::Automatic);
        options.before_submit = Some(Box::new(move |_| {
            assert_eq!(completed.get(), 2, "submission checks only after all months");
            before_calls.set(before_calls.get() + 1);
            vec![]
        }));
        let summary = run(
            RunContext { db: &mut db, source: &mut source, schedule: &mut schedule, gateway: Some(&mut gateway), notifiers: vec![] },
            &options,
        )
        .unwrap();
        if fail_second {
            assert!(!summary.errors.is_empty());
            assert_eq!(before.get(), 0);
            assert!(gateway.submitted.is_empty());
            assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Ready, "first month survives interruption");
        } else {
            assert!(summary.errors.is_empty(), "{:?}", summary.errors);
            assert_eq!(summary.candidates, 0);
            assert_eq!(summary.submitted, 2);
            assert_eq!(before.get(), 1);
            assert_eq!(gateway.submitted.len(), 2);
        }
    }
}

#[test]
fn final_submission_boundary_refuses_expired_or_excluded_assessment() {
    for excluded in [false, true] {
        let mut db = Db::open_in_memory().unwrap();
        let mut source = FixtureJourneySource { journeys: vec![delayed_journey("final-gate")], fail_with: None };
        let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
        run_once(&mut db, &mut source, &mut gateway, RunMode::Manual);
        let claim = db.all_claims().unwrap().remove(0);
        if excluded {
            db.conn.execute("UPDATE candidates SET confidence='not_claimable_or_out_of_scope'", []).unwrap();
        } else {
            db.conn.execute("UPDATE candidates SET expires_at='2000-01-01T00:00:00Z'", []).unwrap();
        }
        let error = submit_claim_at(&mut db, &mut gateway, claim.id, AuthorizedMode::Manual, None, t0()).unwrap_err().to_string();
        assert!(error.contains(if excluded { "not claimable" } else { "expired" }), "{error}");
        assert!(gateway.submitted.is_empty());
        assert_eq!(db.claim(claim.id).unwrap().status, ClaimState::Ready);
        assert_eq!(db.claim(claim.id).unwrap().attempts, 0);
    }
}

#[test]
fn failed_claim_is_removed_from_open_counts_when_no_longer_eligible() {
    struct Excluded;
    impl ScheduleProvider for Excluded {
        fn prepare(&mut self, _: &mut Db, _: &[Journey]) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        fn evidence(&self, _: &Db, _: &Journey) -> anyhow::Result<ScheduleEvidence> {
            Ok(ScheduleEvidence {
                provider: "fixture".into(),
                historical: true,
                planned: None,
                taken: None,
                status: EvidenceStatus::Found,
                notes: vec![],
                regular_arrival: None,
                regular_checked: false,
            })
        }
    }
    let mut db = Db::open_in_memory().unwrap();
    let mut j = delayed_journey("failed-excluded");
    let (jid, _) = db.upsert_journey(&j).unwrap();
    let c = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
    let facts = build_facts(&j, DelayBand::Min30To59, j.check_in).unwrap();
    db.begin_submission(c.id, &facts, AuthorizedMode::Manual).unwrap();
    db.transition(c.id, Some(ClaimState::Submitting), ClaimState::Failed, "fixture", "not sent").unwrap();
    j.check_out = None;
    db.upsert_journey(&j).unwrap();
    let a = assess_and_track(&mut db, &Excluded, "fixture", jid, &j, &opts(RunMode::Manual)).unwrap();
    assert_eq!(a.confidence, Confidence::NotClaimableOrOutOfScope);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Dismissed);
}

#[test]
fn checkout_correction_invalidates_evidence_and_submission_facts_atomically() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j = delayed_journey("corrected");
    let mut src = FixtureJourneySource { journeys: vec![j.clone()], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let (jid, _) = db.upsert_journey(&j).unwrap();
    let c = db.claim_for_journey(jid).unwrap().unwrap();
    let original = db.cached_evidence(&j.fallback_key()).unwrap().unwrap();
    // Missing fields and changed record IDs do not invalidate unchanged evidence.
    let mut partial = j.clone();
    partial.check_out = None;
    partial.source_record_id = "corrected-alias".into();
    db.upsert_journey(&partial).unwrap();
    assert_eq!(db.cached_evidence(&j.fallback_key()).unwrap(), Some(original.clone()));
    j.check_out = j.check_out.map(|t| t - Duration::hours(1));
    // A failed correction leaves both the journey and evidence untouched.
    db.conn
        .execute_batch("CREATE TRIGGER refuse_invalidation BEFORE DELETE ON evidence_cache BEGIN SELECT RAISE(ABORT,'test'); END;")
        .unwrap();
    assert!(db.upsert_journey(&j).is_err());
    assert_ne!(db.journey(jid).unwrap().check_out, j.check_out);
    assert_eq!(db.cached_evidence(&j.fallback_key()).unwrap(), Some(original));
    db.conn.execute_batch("DROP TRIGGER refuse_invalidation").unwrap();
    db.upsert_journey(&j).unwrap();
    assert!(db.cached_evidence(&j.fallback_key()).unwrap().is_none());
    assert!(db.assessment(jid).unwrap().is_none());
    assert!(submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).is_err());
    assert!(gw.submitted.is_empty());
    // Reassessment identifies the earlier, on-time arrival instead of reusing +33.
    src.journeys = vec![j];
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Dismissed);
}

#[test]
fn manual_submission_cannot_bypass_the_minimum_age() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j = delayed_journey("too-recent");
    j.check_in = Utc::now() - Duration::hours(2);
    j.check_out = Some(j.check_in + Duration::hours(1));
    let (jid, _) = db.upsert_journey(&j).unwrap();
    db.save_assessment(
        jid,
        &Assessment {
            confidence: Confidence::NeedsReview,
            checkout_minus_scheduled_arrival_min: Some(40),
            estimated_arrival_delay_min: Some(40),
            delay_band: Some(DelayBand::Min30To59),
            scheduled_departure: Some(j.check_in),
            reasons: vec![],
            expires_at: Utc::now() + Duration::days(90),
        },
    )
    .unwrap();
    let c = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
    let mut gw = FixtureClaimGateway::default();
    let err = submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).unwrap_err();
    assert!(err.to_string().contains("24 hours"));
    assert_eq!(db.claim(c.id).unwrap().attempts, 0);
    assert!(gw.submitted.is_empty());
}

#[test]
fn an_arriva_claim_never_goes_to_ns_or_into_automatic_submission() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j = delayed_journey("arriva");
    j.carrier_code = Some("12".into());
    j.carrier_name = Some("Arriva".into());
    j.fare_cents = Some(1240);
    let (jid, _) = db.upsert_journey(&j).unwrap();
    // Even with evidence that would be automatic for NS, an Arriva trip is never queued or sent to NS.
    db.save_assessment(
        jid,
        &Assessment {
            confidence: Confidence::SupportedForAuto,
            checkout_minus_scheduled_arrival_min: Some(40),
            estimated_arrival_delay_min: Some(40),
            delay_band: Some(DelayBand::Min30To59),
            scheduled_departure: Some(j.check_in),
            reasons: vec![],
            expires_at: utc("2027-01-28T22:59:59Z"),
        },
    )
    .unwrap();
    let c = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
    assert!(auto_queue(&db, utc("2026-11-12T10:00:00Z")).unwrap().is_empty());
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let err = submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, utc("2026-11-12T10:00:00Z")).unwrap_err();
    assert!(err.to_string().contains("never goes through NS"), "{err}");
    assert!(gw.submitted.is_empty());
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Ready);
    // An uncertain Arriva claim does not pause NS's automatic submission.
    db.transition(c.id, Some(ClaimState::Ready), ClaimState::Submitting, "user", "test").unwrap();
    db.transition(c.id, Some(ClaimState::Submitting), ClaimState::Indeterminate, "gateway", "test").unwrap();
    assert_eq!(unresolved_ns_claims(&db).unwrap(), 0);
    // Arriva's compensation: a share of the trip's own fare (Arriva's region rule), not NS's.
    let row = db.journey_rows(10).unwrap().into_iter().find(|r| r.id == jid).unwrap();
    // (No Arriva region for this route: the strictest rule, from 30 minutes; 40 minutes = 50%.)
    assert_eq!(row.compensation_cents, Some(620));
    assert!(!row.below_minimum);
}

#[test]
fn an_arriva_vrij_trip_has_the_same_estimate_in_new_and_existing_assessments() {
    let db = Db::open_in_memory().unwrap();
    let mut trip = delayed_journey("arriva-vrij");
    trip.carrier_code = Some("12".into());
    trip.carrier_name = Some("Arriva".into());
    assert!(trip.is_arriva());
    trip.subscription = Some("FLEXDVR".into());
    trip.subscription_fee_cents = Some(4430);
    trip.invoiced = Some(true);
    let (id, _) = db.upsert_journey(&trip).unwrap();
    let mut assessment = Assessment {
        confidence: Confidence::NeedsReview,
        checkout_minus_scheduled_arrival_min: Some(40),
        estimated_arrival_delay_min: Some(40),
        delay_band: Some(DelayBand::Min30To59),
        scheduled_departure: Some(trip.check_in),
        reasons: vec![],
        expires_at: utc("2027-01-28T22:59:59Z"),
    };
    db.save_assessment(id, &assessment).unwrap();
    let stored: i64 = db.conn.query_row("SELECT compensation_cents FROM candidates WHERE journey_id=?1", [id], |r| r.get(0)).unwrap();
    assert_eq!(stored, 123);
    let row = db.journey_rows(1).unwrap().pop().unwrap();
    assert_eq!(row.compensation_cents, Some(123));
    assert_eq!(row.compensation_exact, Some(false));
    assert_eq!(
        row.compensation_basis.as_deref(),
        Some("Arriva: 50% van de ritprijs (€ 2,46); ritprijs volgens NS-regel: maandbedrag Dal Vrij (€ 44,30, volgens NS) ÷ 18")
    );
    assert!(!row.below_minimum);

    // An earlier version already assessed this trip, but had no compensation estimate.
    assessment.reasons.push("compensation amount unknown (fare not in travel history)".into());
    db.save_assessment(id, &assessment).unwrap();
    db.conn
        .execute(
            "UPDATE candidates SET compensation_cents=NULL, compensation_exact=NULL, compensation_basis=NULL WHERE journey_id=?1",
            [id],
        )
        .unwrap();
    let existing = db.journey_rows(1).unwrap().pop().unwrap();
    assert_eq!(existing.compensation_cents, row.compensation_cents);
    assert_eq!(existing.compensation_basis, row.compensation_basis);
    assert!(existing.reasons.is_empty());
}

#[test]
fn an_rrreis_trip_is_never_claimed_counted_or_sent() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j = delayed_journey("RRR1");
    j.carrier_code = Some("12".into());
    j.carrier_name = Some("RRReis".into());
    assert!(!j.is_ns_carrier() && !j.is_arriva());
    let mut src = FixtureJourneySource { journeys: vec![j], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let row = db.journey_rows(10).unwrap().into_iter().find(|r| r.journey.source_record_id == "RRR1").unwrap();
    // Kept and listed with its carrier, never a claim.
    assert_eq!(row.journey.carrier_name.as_deref(), Some("RRReis"));
    assert_eq!(row.claim_id, None);
    assert!(gw.submitted.is_empty());
    assert!(auto_queue(&db, utc("2026-11-12T10:00:00Z")).unwrap().is_empty());
    assert_eq!(unresolved_ns_claims(&db).unwrap(), 0);
    // Even a claim row created by hand never goes to NS.
    let (jid, _) = db.upsert_journey(&row.journey).unwrap();
    let c = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
    let err = submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, utc("2026-11-12T10:00:00Z")).unwrap_err();
    assert!(err.to_string().contains("never goes through NS"), "{err}");
    assert!(gw.submitted.is_empty());
}

#[test]
fn submitting_and_its_inflight_marker_are_written_together() {
    let mut db = Db::open_in_memory().unwrap();
    let (jid, _) = db.upsert_journey(&delayed_journey("inflight")).unwrap();
    let c = db.ensure_claim(jid, "fixture", ClaimState::Ready).unwrap();
    let key = format!("{INFLIGHT_KEY_PREFIX}{}", c.id);
    // A refused transition writes neither the status nor the marker.
    assert!(db.transition_and_set(c.id, Some(ClaimState::NeedsReview), ClaimState::Submitting, "user", "t", &key, "x").is_err());
    assert_eq!(db.get_setting(&key).unwrap(), None);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Ready);
    let refused =
        db.transition_and_set_checked(c.id, Some(ClaimState::Ready), ClaimState::Submitting, "user", "t", &key, "now|owner", |db| {
            assert!(!db.conn.is_autocommit());
            anyhow::bail!("fixture refusal")
        });
    assert!(refused.is_err());
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Ready);
    assert_eq!(db.get_setting(&key).unwrap(), None);
    db.transition_and_set(c.id, Some(ClaimState::Ready), ClaimState::Submitting, "user", "t", &key, "now|owner").unwrap();
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitting);
    assert_eq!(db.get_setting(&key).unwrap().as_deref(), Some("now|owner"));
}

#[test]
fn cards_and_journeys_per_coupling() {
    let db = Db::open_in_memory().unwrap();
    for (id, coupling, last4, at) in [
        ("a1", "coupling-a", "1234", "2026-03-01T08:00:00Z"),
        ("a2", "coupling-a", "1234", "2026-06-01T08:00:00Z"),
        ("b1", "coupling-b", "1234", "2026-06-02T08:00:00Z"),
        ("c1", "coupling-c", "9999", "2026-06-03T08:00:00Z"),
    ] {
        let mut j = delayed_journey(id);
        j.coupling_id = Some(coupling.into());
        j.card_last4 = Some(last4.into());
        j.check_in = utc(at);
        j.check_out = Some(utc(at) + Duration::minutes(50));
        db.upsert_journey(&j).unwrap();
    }
    let mut both = db.couplings_for_last4("1234").unwrap();
    both.sort();
    assert_eq!(both, vec!["coupling-a".to_string(), "coupling-b".to_string()], "same last four digits: ambiguous");
    assert_eq!(db.couplings_for_last4("9999").unwrap(), vec!["coupling-c".to_string()]);
    assert_eq!(db.journey_count_for_coupling("coupling-a", utc("2026-01-01T00:00:00Z"), utc("2026-12-31T00:00:00Z")).unwrap(), 2);
    assert_eq!(db.journey_count_for_coupling("coupling-a", utc("2026-05-01T00:00:00Z"), utc("2026-12-31T00:00:00Z")).unwrap(), 1);
}

#[test]
fn a_full_import_that_shrank_is_incomplete() {
    assert!(import_incomplete(15, 200), "NS's capped list of 15 against 200 known trips");
    assert!(import_incomplete(8, 10));
    assert!(!import_incomplete(9, 10), "90% is enough");
    assert!(!import_incomplete(200, 200));
    assert!(!import_incomplete(250, 200));
    assert!(!import_incomplete(0, 9), "too few known trips to judge");
}

#[test]
fn a_lost_lease_prevents_final_writes_and_notifications() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![], fail_with: None };
    let mut sched = FixtureSchedule::new(&mut db);
    let mut notifier = RecordingNotifier::default();
    let mut options = opts(RunMode::Manual);
    options.after_run = Some(Box::new(|db, _| {
        let owner = db.run_lock_owner().unwrap().unwrap();
        db.release_run_lock(&owner).unwrap();
        assert!(db.acquire_run_lock("replacement", Duration::minutes(30)).unwrap());
        vec![]
    }));
    let result =
        run(RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: None, notifiers: vec![&mut notifier] }, &options);
    assert!(result.is_err());
    let unfinished: i64 = db.conn.query_row("SELECT COUNT(*) FROM runs WHERE completed_at IS NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(unfinished, 1);
    assert!(notifier.sent.is_empty());
    assert_eq!(db.run_lock_owner().unwrap().as_deref(), Some("replacement"));
}

#[test]
fn route_refresh_migration_invalidates_open_facts_but_keeps_submission_history() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("migration-refresh")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap().remove(0);
    assert!(db.assessment(c.journey_id).unwrap().is_some());
    // Reapply the new migration to simulate upgrading a version-eight ledger.
    db.conn.execute("DELETE FROM schema_migrations WHERE name='0009_route_evidence_refresh'", []).unwrap();
    db.acquire_run_lock("older-app", Duration::minutes(30)).unwrap();
    assert!(db.migrate().is_err(), "a pending migration must wait for the older app's active run");
    assert!(db.assessment(c.journey_id).unwrap().is_some());
    db.release_run_lock("older-app").unwrap();
    db.migrate().unwrap();
    assert!(db.assessment(c.journey_id).unwrap().is_none());
    assert!(db.cached_evidence(&src.journeys[0].fallback_key()).unwrap().is_none());
    assert!(submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).is_err());
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitted);
    db.conn.execute("DELETE FROM schema_migrations WHERE name='0009_route_evidence_refresh'", []).unwrap();
    db.migrate().unwrap();
    assert!(db.assessment(c.journey_id).unwrap().is_some());
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Submitted);
    assert_eq!(db.claim(c.id).unwrap().attempts, 1);
}

#[test]
fn destination_corrections_keep_the_fallback_identity_current() {
    let db = Db::open_in_memory().unwrap();
    let mut j = delayed_journey("destination-corrected");
    let (id, _) = db.upsert_journey(&j).unwrap();
    j.destination = Some(StationRef { uic: "8400621".into(), name: "Utrecht Centraal".into() });
    assert_eq!(db.upsert_journey(&j).unwrap(), (id, false));
    j.source_record_id = "replacement-source-id".into();
    assert_eq!(db.upsert_journey(&j).unwrap(), (id, false), "a new NS record ID must not create a second trip after a correction");
    let stored: String = db.conn.query_row("SELECT fallback_key FROM journeys WHERE id=?1", [id], |r| r.get(0)).unwrap();
    assert_eq!(stored, j.fallback_key());
}

#[test]
fn route_refresh_preserves_closed_and_expired_history_and_only_fetches_open_months() {
    fn rows(db: &Db, table: &str, column: &str, key: rusqlite::types::Value) -> Vec<Vec<rusqlite::types::Value>> {
        let mut st = db.conn.prepare(&format!("SELECT * FROM {table} WHERE {column}=?1")).unwrap();
        let columns = st.column_count();
        st.query_map([key], |r| (0..columns).map(|i| r.get(i)).collect()).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }
    fn snapshot(db: &Db, id: i64, j: &Journey) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        vec![
            rows(db, "candidates", "journey_id", id.into()),
            rows(db, "schedules", "journey_id", id.into()),
            rows(db, "evidence_cache", "journey_key", j.fallback_key().into()),
        ]
    }
    for open_state in [Some(ClaimState::Candidate), Some(ClaimState::Ready), Some(ClaimState::NeedsReview), Some(ClaimState::Failed), None]
    {
        let mut db = Db::open_in_memory().unwrap();
        let now = Utc::now();
        let noon = now.date_naive().and_hms_opt(12, 0, 0).unwrap().and_utc();
        let cases = [
            (2, open_state),
            // Expired even though still within the suggested 100-day SQL margin.
            (95, Some(ClaimState::Candidate)),
            (35, Some(ClaimState::Dismissed)),
            (65, Some(ClaimState::ExternallyClaimed)),
            (190, None),
            (3, Some(ClaimState::Submitting)),
            (4, Some(ClaimState::Submitted)),
            (5, Some(ClaimState::Indeterminate)),
            (6, Some(ClaimState::Approved)),
            (7, Some(ClaimState::Rejected)),
            (8, Some(ClaimState::Paid)),
        ];
        let mut journeys = vec![];
        let mut ids = vec![];
        for (i, (age, state)) in cases.into_iter().enumerate() {
            let mut j = delayed_journey(&format!("refresh-{i}"));
            j.check_in = noon - Duration::days(age);
            j.check_out = Some(j.check_in + Duration::hours(2));
            let (id, _) = db.upsert_journey(&j).unwrap();
            if let Some(state) = state {
                db.ensure_claim(id, "fixture", state).unwrap();
            }
            let ev = ScheduleEvidence {
                provider: "fixture".into(),
                historical: true,
                planned: None,
                taken: None,
                status: EvidenceStatus::Found,
                notes: vec!["preserved delay history".into()],
                regular_arrival: None,
                regular_checked: true,
            };
            db.put_cached_evidence(&j.fallback_key(), &j.check_in.format("%Y-%m").to_string(), &ev).unwrap();
            db.save_evidence(id, &ev).unwrap();
            db.save_assessment(
                id,
                &Assessment {
                    confidence: Confidence::NeedsReview,
                    checkout_minus_scheduled_arrival_min: Some(33),
                    estimated_arrival_delay_min: Some(33),
                    delay_band: Some(DelayBand::Min30To59),
                    scheduled_departure: Some(j.check_in),
                    reasons: vec!["original assessment".into()],
                    expires_at: eligibility::expires_at(j.check_in, eligibility::ClaimWindow::default()),
                },
            )
            .unwrap();
            ids.push(id);
            journeys.push(j);
        }
        let before: Vec<_> = ids.iter().zip(&journeys).map(|(&id, j)| snapshot(&db, id, j)).collect();
        db.conn.execute("DELETE FROM schema_migrations WHERE name='0009_route_evidence_refresh'", []).unwrap();
        // A failure after candidates/schedules were deleted must roll back the entire hook.
        db.conn
            .execute_batch("CREATE TRIGGER refuse_refresh BEFORE DELETE ON evidence_cache BEGIN SELECT RAISE(ABORT,'fixture'); END;")
            .unwrap();
        assert!(db.migrate().is_err());
        for (i, (&id, j)) in ids.iter().zip(&journeys).enumerate() {
            assert_eq!(snapshot(&db, id, j), before[i]);
        }
        db.conn.execute_batch("DROP TRIGGER refuse_refresh").unwrap();
        assert_eq!(db.migrate().unwrap(), vec!["0009_route_evidence_refresh"]);
        assert!(snapshot(&db, ids[0], &journeys[0]).iter().all(Vec::is_empty));
        for (i, (&id, j)) in ids.iter().zip(&journeys).enumerate().skip(1) {
            assert_eq!(snapshot(&db, id, j), before[i]);
        }
        let mut provider = schedule::ArchiveScheduleProvider::new();
        let mut fetched = vec![];
        provider
            .prepare_with(
                &mut db,
                &journeys,
                |month, _, _| {
                    fetched.push(month.to_string());
                    Ok(Some((vec![], 0))) // Fake published archive, no network.
                },
                &mut |_, _, _, _, _| Ok(()),
            )
            .unwrap();
        assert_eq!(fetched, schedule::journey_months(&journeys[0]), "only the invalidated trip needs an archive");
        let mut offline = ovrefund_core::schedule::ArchiveScheduleProvider::new();
        offline.offline = true;
        let mut options = opts(RunMode::Manual);
        options.now = now;
        assess_and_track(&mut db, &offline, "fixture", ids[0], &journeys[0], &options).unwrap();
        assert!(db.assessment(ids[0]).unwrap().is_some());
        assert!(db.evidence_status(ids[0]).unwrap().is_some());
        assert!(db.cached_evidence(&journeys[0].fallback_key()).unwrap().is_some());
        assert_ne!(snapshot(&db, ids[0], &journeys[0]), before[0]);
        for (i, (&id, j)) in ids.iter().zip(&journeys).enumerate().skip(1) {
            assert_eq!(snapshot(&db, id, j), before[i], "prepare must preserve all other history");
        }
        assert!(db.migrate().unwrap().is_empty());
    }
}

#[test]
fn stale_manual_lease_never_starts_or_completes_submission() {
    let mut db = Db::open_in_memory().unwrap();
    let mut source = FixtureJourneySource { journeys: vec![delayed_journey("lease")], fail_with: None };
    let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut source, &mut gateway, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    db.acquire_run_lock("replacement", Duration::minutes(30)).unwrap();
    assert!(submit_claim_under_lock(&mut db, &mut gateway, c.id, AuthorizedMode::Manual, None, "old-owner", t0()).is_err());
    assert!(gateway.submitted.is_empty());
    assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Ready);

    struct ReplacingGateway {
        conn: rusqlite::Connection,
    }
    impl ClaimGateway for ReplacingGateway {
        fn id(&self) -> &str {
            "test"
        }
        fn verified_for_auto(&self) -> bool {
            true
        }
        fn preflight(&mut self, _: &ClaimFacts) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        fn list_remote_claims(&mut self) -> anyhow::Result<Option<Vec<RemoteClaim>>> {
            Ok(Some(vec![]))
        }
        fn submit(&mut self, _: &ClaimFacts) -> SubmitOutcome {
            self.conn.execute("UPDATE run_lock SET owner='replacement'", []).unwrap();
            SubmitOutcome::NotSent("test refusal".into())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock.sqlite");
    let mut db = Db::open(&path).unwrap();
    run_once(&mut db, &mut source, &mut gateway, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    db.acquire_run_lock("original", Duration::minutes(30)).unwrap();
    let mut replacing = ReplacingGateway { conn: rusqlite::Connection::open(&path).unwrap() };
    // The lease is lost during submit(): the outcome is still recorded first.
    let r = submit_claim_under_lock(&mut db, &mut replacing, c.id, AuthorizedMode::Manual, None, "original", t0()).unwrap();
    assert_eq!(r.status, ClaimState::Failed, "outcome recorded although the lease was lost afterwards");
    assert!(db.renew_run_lock("original", Duration::minutes(30)).is_err(), "the lock stays with its new owner");
}

#[test]
fn automatic_run_submits_every_qualifying_claim_with_a_pause_between() {
    let mut db = Db::open_in_memory().unwrap();
    let mut js = two_journeys_same_day();
    js[1].card_last4 = Some("1111".into());
    let mut source = FixtureJourneySource { journeys: js, fail_with: None };
    let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut schedule = FixtureSchedule::new(&mut db);
    let mut options = opts(RunMode::Automatic);
    options.auto_pause = std::time::Duration::from_millis(300);
    let started = std::time::Instant::now();
    let s = run(
        RunContext { db: &mut db, source: &mut source, schedule: &mut schedule, gateway: Some(&mut gateway), notifiers: vec![] },
        &options,
    )
    .unwrap();
    assert_eq!(s.submitted, 2, "no per-run cap: {s:?}");
    assert_eq!(gateway.submitted.len(), 2);
    assert!(started.elapsed() >= std::time::Duration::from_millis(300), "one pause between the two submissions");
}

#[test]
fn precondition_failure_does_not_starve_the_automatic_queue() {
    let mut db = Db::open_in_memory().unwrap();
    let mut js = two_journeys_same_day();
    js[1].card_last4 = Some("1111".into());
    let mut source = FixtureJourneySource { journeys: js, fail_with: None };
    let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut schedule = FixtureSchedule::new(&mut db);
    let mut options = opts(RunMode::Automatic);
    options.before_submit = Some(Box::new(|db| {
        db.conn
            .execute("UPDATE journeys SET coupling_id=NULL WHERE id=(SELECT journey_id FROM claims ORDER BY updated_at DESC LIMIT 1)", [])
            .unwrap();
        vec![]
    }));
    let s = run(
        RunContext { db: &mut db, source: &mut source, schedule: &mut schedule, gateway: Some(&mut gateway), notifiers: vec![] },
        &options,
    )
    .unwrap();
    assert_eq!(s.submitted, 1);
    assert_eq!((s.failed, s.refused), (0, 1), "refused before NS, counted apart");
    assert_eq!(gateway.submitted.len(), 1);
}

#[test]
fn undo_legacy_date_matched_mail_removes_false_reference_and_payout() {
    let mut db = Db::open_in_memory().unwrap();
    let (jid, _) = db.upsert_journey(&on_time_journey()).unwrap();
    let c = db.ensure_claim(jid, "external", ClaimState::Discovered).unwrap();
    db.mark_external(c.id, "ns-mail").unwrap();
    db.set_ns_reference(c.id, "mail-number").unwrap();
    db.set_paid_amount(c.id, Some(342)).unwrap();
    let c = db.undo_external(c.id, "user").unwrap();
    assert_eq!(c.status, ClaimState::NeedsReview);
    assert!(c.ns_reference.is_none());
    assert_eq!(db.paid_cents(c.id).unwrap(), None);
}

#[test]
fn manual_external_input_is_bounded_without_a_migration() {
    let db = Db::open_in_memory().unwrap();
    for (day, from) in [("1999-01-01", "Utrecht"), ("2026-10-28", "<script>"), ("2026-10-28", "123")] {
        assert!(db
            .add_external_claim(chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap(), Some(from), Some("Zwolle"), "user", None)
            .is_err());
    }
}

#[test]
fn matching_policy_update_invalidates_only_derived_cache_once() {
    let mut db = Db::open_in_memory().unwrap();
    let mut source = FixtureJourneySource { journeys: vec![delayed_journey("cache-version")], fail_with: None };
    let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut source, &mut gateway, RunMode::Automatic);
    let j = delayed_journey("cache-version");
    let evidence = db.cached_evidence(&j.fallback_key()).unwrap().unwrap();
    db.set_setting("matching_policy_version", "old").unwrap();
    db.refresh_matching_policy().unwrap();
    assert!(db.cached_evidence(&j.fallback_key()).unwrap().is_none());
    assert_eq!(db.all_claims().unwrap()[0].status, ClaimState::Submitted);
    db.put_cached_evidence(&j.fallback_key(), "2026-10", &evidence).unwrap();
    db.refresh_matching_policy().unwrap();
    assert!(db.cached_evidence(&j.fallback_key()).unwrap().is_some());
}

#[test]
fn uncertain_claims_and_a_missing_claim_list_explain_the_pause() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["unknown"], ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.auto_paused_code.as_deref(), Some("uncertain"), "{s:?}");
    assert_eq!(s.auto_paused_reason.as_deref(), Some(paused_uncertain_reason(1).as_str()));
    assert_eq!(paused_uncertain_reason(1), "Gepauzeerd: 1 claim met een onzekere uitkomst. / Paused: 1 claim with an uncertain outcome.");
    // The next run is paused for the same reason (nothing new is submitted).
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!((s.submitted, s.auto_paused_code.as_deref()), (0, Some("uncertain")));
    // Manual mode: nothing to explain.
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    assert_eq!(s.auto_paused_reason, None);

    struct NoList(FixtureClaimGateway);
    impl ClaimGateway for NoList {
        fn id(&self) -> &str {
            "fixture"
        }
        fn verified_for_auto(&self) -> bool {
            true
        }
        fn preflight(&mut self, f: &ClaimFacts) -> anyhow::Result<Vec<String>> {
            self.0.preflight(f)
        }
        fn submit(&mut self, f: &ClaimFacts) -> SubmitOutcome {
            self.0.submit(f)
        }
        fn list_remote_claims(&mut self) -> anyhow::Result<Option<Vec<RemoteClaim>>> {
            Ok(None)
        }
    }
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = NoList(FixtureClaimGateway::default());
    let mut sched = FixtureSchedule::new(&mut db);
    let s = run(
        RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: Some(&mut gw), notifiers: vec![] },
        &opts(RunMode::Automatic),
    )
    .unwrap();
    assert_eq!(s.auto_paused_code.as_deref(), Some("claim_list"));
    assert_eq!(s.auto_paused_reason.as_deref(), Some(PAUSED_CLAIM_LIST_REASON));
    assert!(gw.0.submitted.is_empty());
}

#[test]
fn stop_with_a_clock_set_ahead_never_blocks_later_runs() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    // Stops in the older formats (time only, or "<time>|<run id>") stop nothing.
    db.set_setting(AUTO_STOP_KEY, &(Utc::now() + Duration::days(1)).to_rfc3339()).unwrap();
    assert!(!auto_stop_requested(&db, "pid1-run-1").unwrap());
    db.set_setting(AUTO_STOP_KEY, &format!("{}|99", (Utc::now() + Duration::days(1)).to_rfc3339())).unwrap();
    assert_eq!(run_once(&mut db, &mut src, &mut gw, RunMode::Automatic).submitted, 1);
}

#[test]
fn stop_belongs_to_the_run_that_holds_the_lock() {
    let db = Db::open_in_memory().unwrap();
    // No check running: nothing to stop, nothing stored.
    request_auto_stop(&db, StopReason::User).unwrap();
    assert_eq!(db.get_setting(AUTO_STOP_KEY).unwrap(), None);
    // A running check (lock taken, e.g. while the NS window opens) is stopped ...
    assert!(db.acquire_run_lock("pid1-run-7", Duration::minutes(30)).unwrap());
    request_auto_stop(&db, StopReason::User).unwrap();
    assert!(auto_stop_requested(&db, "pid1-run-7").unwrap());
    // ... a later run is not, whatever its run id or the clock says.
    db.release_run_lock("pid1-run-7").unwrap();
    assert!(!auto_stop_requested(&db, "pid1-run-8").unwrap());
    assert!(!auto_stop_requested(&db, "pid1-run-77").unwrap(), "no prefix match");
    clear_auto_stop(&db).unwrap();
    assert_eq!(db.get_setting(AUTO_STOP_KEY).unwrap(), None);
}

#[test]
fn stop_before_a_history_wipe_or_restore_never_stops_later_runs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wipe-stop.sqlite");
    let mut db = Db::open(&path).unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway::default();
    // Some runs, a stop for a check that was running then, and the history wiped (run ids
    // start again at 1).
    for _ in 0..3 {
        run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    }
    assert!(db.acquire_run_lock("pid1-old", Duration::minutes(30)).unwrap());
    request_auto_stop(&db, StopReason::User).unwrap();
    db.release_run_lock("pid1-old").unwrap();
    assert!(db.get_setting(AUTO_STOP_KEY).unwrap().is_some());
    db.wipe_history().unwrap();
    assert_eq!(db.get_setting(AUTO_STOP_KEY).unwrap(), None, "wiped with the history");
    // Even a stop that survived (older version) does not stop the new runs.
    db.set_setting(AUTO_STOP_KEY, &format!("{}|owner:pid1-old", Utc::now().to_rfc3339())).unwrap();
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.run_id, 1, "run ids started again");
    assert_eq!(s.submitted, 1, "{s:?}");
    // A restore clears it as well.
    let backup = dir.path().join("backup.sqlite");
    db.backup_to(&backup, false).unwrap();
    db.set_setting(AUTO_STOP_KEY, &format!("{}|owner:pid1-old", Utc::now().to_rfc3339())).unwrap();
    db.restore_from(&backup, true, false).unwrap();
    assert_eq!(db.get_setting(AUTO_STOP_KEY).unwrap(), None, "cleared by the restore");
}

/// Source whose fetch is where the user presses Stop (through its own connection).
struct StopDuringFetch {
    inner: FixtureJourneySource,
    path: std::path::PathBuf,
}
impl JourneySource for StopDuringFetch {
    fn id(&self) -> &str {
        "fixture"
    }
    fn fetch_journeys(&mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Journey>, FetchError> {
        request_auto_stop(&Db::open(&self.path).unwrap(), StopReason::User).unwrap();
        self.inner.fetch_journeys(from, to)
    }
}

#[test]
fn stop_during_the_sync_ends_an_automatic_check_early() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync-stop.sqlite");
    let mut db = Db::open(&path).unwrap();
    let mut src =
        StopDuringFetch { inner: FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None }, path: path.clone() };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut sched = FixtureSchedule::new(&mut db);
    let mut n = RecordingNotifier::default();
    let s = run(
        RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: Some(&mut gw), notifiers: vec![&mut n] },
        &opts(RunMode::Automatic),
    )
    .unwrap();
    assert_eq!(s.submitted, 0);
    assert!(gw.submitted.is_empty());
    assert_eq!(s.journeys_seen, 2, "the fetched trips are kept");
    assert!(db.all_claims().unwrap().is_empty(), "nothing assessed after the stop");
    assert!(db.run_lock_owner().unwrap().is_none());
    // A manual check is not ended by a stop (Stop is about automatic submission).
    let manual = dir.path().join("manual.sqlite");
    let mut db = Db::open(&manual).unwrap();
    let mut src =
        StopDuringFetch { inner: FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None }, path: manual.clone() };
    let mut sched = FixtureSchedule::new(&mut db);
    let s = run(
        RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: Some(&mut gw), notifiers: vec![] },
        &opts(RunMode::Manual),
    )
    .unwrap();
    assert!(s.candidates > 0, "{s:?}");
}

#[test]
fn auto_phase_marker_is_set_while_submitting_and_cleared_after() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("phase.sqlite");
    let mut db = Db::open(&path).unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut o = opts(RunMode::Automatic);
    o.auto_pause = std::time::Duration::from_secs(30);
    let (tx, rx) = std::sync::mpsc::channel();
    let actor = second_actor(&path, move |db| {
        let active = auto_phase_active(db).unwrap();
        request_auto_stop(db, StopReason::User).unwrap();
        tx.send(active).unwrap();
    });
    let s = run_with(&mut db, &mut src, &mut gw, o);
    actor.join().unwrap();
    assert!(rx.recv().unwrap(), "in the pause between two claims the run is in its submission phase");
    assert_eq!(s.submitted, 1);
    assert!(!auto_phase_active(&db).unwrap());
    assert_eq!(db.get_setting(AUTO_PHASE_KEY).unwrap(), None, "marker removed with the lock");
    // A manual run never sets it.
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    assert_eq!(db.get_setting(AUTO_PHASE_KEY).unwrap(), None);
}

#[test]
fn run_uses_a_lock_its_caller_already_took() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    assert!(db.acquire_run_lock("pid1-run-7", Duration::minutes(30)).unwrap());
    let mut o = opts(RunMode::Automatic);
    o.lock_owner = Some("pid1-run-7".into());
    assert_eq!(run_with(&mut db, &mut src, &mut gw, o).submitted, 1);
    assert!(db.run_lock_owner().unwrap().is_none(), "released at the end");
    // Someone else's lock: refused.
    assert!(db.acquire_run_lock("pid1-other", Duration::minutes(30)).unwrap());
    let mut o = opts(RunMode::Automatic);
    o.lock_owner = Some("pid1-run-8".into());
    let mut sched = FixtureSchedule::new(&mut db);
    assert!(run(RunContext { db: &mut db, source: &mut src, schedule: &mut sched, gateway: Some(&mut gw), notifiers: vec![] }, &o).is_err());
}

#[test]
fn our_own_claim_listed_with_another_reference_format_does_not_hold_back_the_other_trip() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let claims = db.all_claims().unwrap();
    let journey_of = |db: &Db, id: i64| db.journey(db.claim(id).unwrap().journey_id).unwrap();
    let d1 = claims.iter().find(|c| journey_of(&db, c.id).source_record_id == "D1").unwrap().id;
    let d2 = claims.iter().find(|c| journey_of(&db, c.id).source_record_id == "D2").unwrap().id;
    submit_claim_at(&mut db, &mut gw, d1, AuthorizedMode::Automatic, None, t0()).unwrap();
    assert_eq!(db.claim(d1).unwrap().ns_reference.as_deref(), Some("FIXTURE-1"), "the reference our submission captured");
    let j2 = journey_of(&db, d2);
    let listed = |reference: Option<&str>| vec![remote(None, reference, Some((2026, 10, 28)), ClaimState::Submitted, None)];
    // NS lists our claim for D1 with its reference in another form: D2 is not "near" a claim.
    assert!(!remote_claim_near(&db, &listed(Some("fixture 1")), &j2).unwrap());
    assert!(same_reference("NS-0001234", "ns0001234") && !same_reference("", "") && !same_reference("NS-1", "NS-12"));
    // A reference that is not ours, or none at all: fail-safe, D2 waits for the user.
    assert!(remote_claim_near(&db, &listed(Some("NS-9999999")), &j2).unwrap());
    assert!(remote_claim_near(&db, &listed(None), &j2).unwrap());
}

#[test]
fn a_matching_ns_reference_with_a_conflicting_transaction_still_holds_the_claim() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let claim = db.claims_in(&[ClaimState::Ready]).unwrap().remove(0);
    let journey = db.journey(claim.journey_id).unwrap();
    db.set_ns_reference(claim.id, "fixture-ref").unwrap();
    let list = [
        remote(None, Some("fixture-ref"), None, ClaimState::Approved, None),
        remote(Some("other-trip"), Some("fixture ref"), None, ClaimState::Submitted, None),
    ];
    let (_, near) = reconcile_open(&mut db, &list).unwrap();
    assert_eq!(near, vec![journey.natural_key()]);
    assert_eq!(db.claim(claim.id).unwrap().status, ClaimState::Ready);
}

#[test]
fn claimed_elsewhere_entry_stored_without_the_lock_blocks_the_next_submission() {
    // The app stores a "claimed elsewhere" entry while a check is submitting (no lock needed):
    // the final check before the next submission refuses the matching trip.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("external.sqlite");
    let mut db = Db::open(&path).unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut o = opts(RunMode::Automatic);
    let j = delayed_journey("D1");
    let day = j.check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
    // Stored through another connection while the run holds its lock, after the trips were
    // assessed (they are still READY) and before the first submission.
    o.before_submit = Some(Box::new(move |_db: &mut Db| {
        let other = Db::open(&path).unwrap();
        assert!(!other.acquire_run_lock("pid1-gui-edit", Duration::minutes(2)).unwrap(), "the run holds the lock");
        other.add_external_claim(day, Some(&j.origin.name), Some(&j.destination.as_ref().unwrap().name), "other", None).unwrap();
        vec![]
    }));
    let s = run_with(&mut db, &mut src, &mut gw, o);
    assert!(gw.submitted.is_empty(), "matching trips are never sent: {s:?}");
    assert_eq!((s.submitted, s.failed, s.refused), (0, 0, 2), "{s:?}");
}

#[test]
fn an_interrupted_manual_retry_does_not_reset_the_automatic_attempt_limit() {
    let mut db = Db::open_in_memory().unwrap();
    let mut source = FixtureJourneySource { journeys: vec![delayed_journey("test-attempt-limit")], fail_with: None };
    let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut source, &mut gateway, RunMode::Manual);
    let id = db.claims_in(&[ClaimState::Ready]).unwrap()[0].id;
    for _ in 0..3 {
        gateway.next_outcomes.push("notsent");
        assert_eq!(submit_claim_at(&mut db, &mut gateway, id, AuthorizedMode::Manual, None, t0()).unwrap().status, ClaimState::Failed);
        approve_for_manual_submission(&mut db, id, false, false).unwrap();
    }
    assert_eq!(db.claim(id).unwrap().attempts, 3);
    assert!(auto_queue(&db, t0()).unwrap().is_empty());
    assert!(submit_claim_at(&mut db, &mut gateway, id, AuthorizedMode::Automatic, None, t0()).is_err());
}

#[test]
fn failed_retries_wait_behind_ready_claims_and_a_retry_not_sent_again_is_skipped() {
    let mut db = Db::open_in_memory().unwrap();
    let mut js = two_journeys_same_day();
    let mut j3 = delayed_journey("D3");
    j3.check_in += Duration::seconds(10);
    j3.card_last4 = Some("2222".into());
    js.push(j3);
    let mut src = FixtureJourneySource { journeys: js, fail_with: None };
    // First run: the first claim is not sent (a first attempt stops the run).
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent"], ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!((s.failed, s.submitted), (1, 0), "{s:?}");
    // Second run: READY claims come before the retry; the next READY claim is not sent either.
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent"], ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(db.claims_in(&[ClaimState::Failed]).unwrap().len(), 2);
    assert_eq!(db.claims_in(&[ClaimState::Ready]).unwrap().len(), 1);
    let queue = auto_queue(&db, utc("2026-11-12T10:00:00Z")).unwrap();
    assert_eq!(queue.iter().map(|(_, c)| c.status).collect::<Vec<_>>(), vec![ClaimState::Ready, ClaimState::Failed, ClaimState::Failed]);
    let ready = db.journey(queue[0].1.journey_id).unwrap().source_record_id;
    // Third run: the READY claim first; both retries are not sent again and are skipped, the run goes on.
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["ok", "notsent", "notsent"], ..Default::default() };
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!((s.submitted, s.failed), (1, 2), "{s:?}");
    assert_eq!(gw.submitted.len(), 1);
    assert_eq!(gw.submitted[0].source_record_id, ready, "the READY claim went first");
    assert!(gw.next_outcomes.is_empty(), "both retries were tried");
}

#[test]
fn stop_reaches_a_check_whose_lease_ran_out_while_its_process_runs() {
    let db = Db::open_in_memory().unwrap();
    // The lease ran out (e.g. after sleep), the check itself still runs.
    assert!(db.acquire_run_lock("pid1-run-9", Duration::minutes(-1)).unwrap());
    assert!(db.live_run_lock_owner().unwrap().is_none());
    request_auto_stop_with(&db, &|_| false, StopReason::User).unwrap();
    assert_eq!(db.get_setting(AUTO_STOP_KEY).unwrap(), None, "no live check: nothing stored");
    request_auto_stop_with(&db, &|o| o == "pid1-run-9", StopReason::User).unwrap();
    assert!(auto_stop_requested(&db, "pid1-run-9").unwrap());
}

#[test]
fn claimed_elsewhere_entry_needs_known_and_different_stations() {
    let db = Db::open_in_memory().unwrap();
    let d = chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
    let err = db.add_external_claim(d, Some("Atlantis"), Some("Zwolle"), "x", None).unwrap_err().to_string();
    assert!(err.starts_with("Vertrekstation niet gevonden.") && err.contains(" / Departure station not found."), "{err}");
    let err = db.add_external_claim(d, Some("Zwolle"), Some("Nergenshuizen"), "x", None).unwrap_err().to_string();
    assert!(err.starts_with("Aankomststation niet gevonden."), "{err}");
    let err = db.add_external_claim(d, Some("Zwolle"), Some("zwolle"), "x", None).unwrap_err().to_string();
    assert!(err.starts_with("Vertrek- en aankomststation zijn hetzelfde."), "{err}");
    // Short names that match trips are accepted ("Leiden" → Leiden Centraal, "Den Bosch").
    db.add_external_claim(d, Some("Leiden"), Some("Den Bosch"), "x", None).unwrap();
    db.add_external_claim(d, Some("Leiden"), Some("Leiden Lammenschans"), "x", None).unwrap();
}

#[test]
fn a_stop_names_why_automatic_submission_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stop-reason.sqlite");
    let mut db = Db::open(&path).unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut o = opts(RunMode::Automatic);
    o.auto_pause = std::time::Duration::from_secs(30);
    // The user logs out during the pause.
    let actor = second_actor(&path, |db| request_auto_stop(db, StopReason::LoggedOut).unwrap());
    let s = run_with(&mut db, &mut src, &mut gw, o);
    actor.join().unwrap();
    assert_eq!(gw.submitted.len(), 1, "no new submission after the stop");
    assert_eq!(s.auto_paused_code.as_deref(), Some("stopped"), "{s:?}");
    assert_eq!(s.auto_paused_reason.as_deref(), Some("Gestopt: je bent uitgelogd bij NS. / Stopped: you logged out of NS."));
    // Every reason has its own bilingual text; an older stop without a reason is the user's.
    for r in [StopReason::User, StopReason::LoggedOut, StopReason::BankRemoved, StopReason::AutoOff, StopReason::Erased] {
        assert!(r.reason().starts_with("Gestopt") && r.reason().contains(" / Stopped"), "{r:?}");
    }
    assert_eq!(StopReason::User.reason(), PAUSED_STOPPED_REASON);
    db.set_setting(AUTO_STOP_KEY, "2026-11-12T10:00:00Z|owner:pid1-run-3").unwrap();
    assert_eq!(auto_stop_reason(&db, "pid1-run-3").unwrap(), Some(StopReason::User));
    assert_eq!(auto_stop_reason(&db, "pid1-run-4").unwrap(), None);
    db.set_setting(AUTO_STOP_KEY, "2026-11-12T10:00:00Z|reason:bank|owner:pid1-run-3").unwrap();
    assert_eq!(auto_stop_reason(&db, "pid1-run-3").unwrap(), Some(StopReason::BankRemoved));
}

#[test]
fn a_refused_claim_is_marked_until_a_later_attempt_passes_the_final_check() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut o = opts(RunMode::Automatic);
    // The trip loses its linked card before the submission: refused by the final check.
    o.before_submit = Some(Box::new(|db| {
        db.conn.execute("UPDATE journeys SET coupling_id=NULL", []).unwrap();
        vec![]
    }));
    let s = run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!((s.submitted, s.failed, s.refused), (0, 0, 1), "{s:?}");
    let c = db.all_claims().unwrap()[0].clone();
    assert!(auto_refused(&db, c.id).unwrap());
    // The refusal is a typed error, also for a manual submission.
    let e = submit_claim_at(&mut db, &mut gw, c.id, AuthorizedMode::Manual, None, t0()).unwrap_err();
    assert!(e.downcast_ref::<Refusal>().is_some(), "{e}");
    // The card is back: the next check submits it and the mark is gone.
    db.conn.execute("UPDATE journeys SET coupling_id='00000000-0000-0000-0000-000000000000'", []).unwrap();
    let s = run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    assert_eq!(s.submitted, 1, "{s:?}");
    assert!(!auto_refused(&db, c.id).unwrap());
}

#[test]
fn an_urgent_retry_goes_before_new_claims() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: two_journeys_same_day(), fail_with: None };
    // The first claim is not sent: FAILED (a retry), the other stays READY.
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent"], ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let failed = db.claims_in(&[ClaimState::Failed]).unwrap()[0].clone();
    let now = t0();
    let order = |db: &Db| auto_queue(db, now).unwrap().iter().map(|(_, c)| c.status).collect::<Vec<_>>();
    assert_eq!(order(&db), vec![ClaimState::Ready, ClaimState::Failed], "not urgent: READY first");
    // The retry's window ends within 4 days: it goes first.
    db.conn
        .execute(
            "UPDATE candidates SET expires_at=?1 WHERE journey_id=?2",
            ((now + Duration::days(URGENT_DAYS) - Duration::hours(1)).to_rfc3339(), failed.journey_id),
        )
        .unwrap();
    assert_eq!(order(&db), vec![ClaimState::Failed, ClaimState::Ready]);
}

#[test]
fn check_now_reports_claims_left_for_the_scheduled_check_and_claims_near_an_ns_claim() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let c = db.all_claims().unwrap()[0].clone();
    let exp = db.assessment(c.journey_id).unwrap().unwrap().3;
    let now = exp - Duration::days(20);
    let mut o = opts(RunMode::Automatic);
    o.now = now;
    o.clock = Some(Box::new(move || now));
    o.auto_only_due_within = Some(Duration::days(14));
    let s = run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!((s.submitted, s.auto_later, s.near_skipped), (0, 1, 0), "{s:?}");
    // Within the window, but NS lists a claim around that day: not submitted, counted apart.
    let mut gw = FixtureClaimGateway {
        verified: true,
        remote: vec![remote(None, Some("ELSEWHERE"), Some((2026, 10, 28)), ClaimState::Submitted, None)],
        ..Default::default()
    };
    let now = exp - Duration::days(3);
    let mut o = opts(RunMode::Automatic);
    o.now = now;
    o.clock = Some(Box::new(move || now));
    o.auto_only_due_within = Some(Duration::days(14));
    let s = run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!((s.submitted, s.auto_later, s.near_skipped), (0, 0, 1), "{s:?}");
    assert!(gw.submitted.is_empty());
}

#[test]
fn recording_login_state_cannot_interleave_with_a_logout() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("ledger.sqlite");
    let mut db = Db::open(&path).unwrap();
    let other = Db::open(&path).unwrap();
    other.conn.busy_timeout(std::time::Duration::ZERO).unwrap();
    let checked = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = checked.clone();
    let mut o = opts(RunMode::Manual);
    o.same_session = Some(Box::new(move |_: &Db| {
        // A logout in another process cannot slip between checking the epoch and writing "ok".
        assert!(other.set_setting("session_epoch", "logged-out").is_err());
        seen.set(true);
        true
    }));
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let result = run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!(result.auth_state, "ok");
    assert!(checked.get());
    // Once recording finishes, the logout succeeds and remains the last state.
    let other = Db::open(&path).unwrap();
    other.set_setting("session_epoch", "logged-out").unwrap();
    other.upsert_source("fixture", "mijnns", false, "disconnected").unwrap();
    assert_eq!(db.source_status("fixture").unwrap().unwrap().1.as_deref(), Some("disconnected"));
}

#[test]
fn a_run_from_before_a_logout_never_records_the_login_state() {
    let mut db = Db::open_in_memory().unwrap();
    db.upsert_source("fixture", "mijnns", false, "disconnected").unwrap();
    for fail in [None, Some((|| FetchError::LoginRequired) as fn() -> FetchError)] {
        let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: fail };
        let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
        let mut o = opts(RunMode::Manual);
        o.same_session = Some(Box::new(|_: &Db| false));
        run_with(&mut db, &mut src, &mut gw, o);
        let st = db.source_status("fixture").unwrap().unwrap();
        assert_eq!((st.0, st.1.as_deref()), (false, Some("disconnected")), "{fail:?}");
    }
    // The same session: recorded as before.
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    let mut o = opts(RunMode::Manual);
    o.same_session = Some(Box::new(|_: &Db| true));
    run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!(db.source_status("fixture").unwrap().unwrap().1.as_deref(), Some("ok"));
}

#[test]
fn claimed_elsewhere_entry_counts_only_open_trips_it_matches_itself() {
    let mut db = Db::open_in_memory().unwrap();
    let mut j2 = delayed_journey("D2");
    j2.check_in += Duration::hours(1);
    j2.origin = StationRef { uic: "8400319".into(), name: "'s-Hertogenbosch".into() };
    j2.destination = Some(StationRef { uic: "8400055".into(), name: "Amersfoort Centraal".into() });
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1"), j2], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let day = delayed_journey("D1").check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
    // The return trip's entry does not count the outward trip.
    let back = db.add_external_claim(day, Some("'s-Hertogenbosch"), Some("Amersfoort"), "other", None).unwrap();
    assert_eq!(db.open_trips_matching_external(back).unwrap().len(), 1);
    let out = db.add_external_claim(day, Some("Amersfoort"), Some("'s-Hertogenbosch"), "other", None).unwrap();
    assert_eq!(db.open_trips_matching_external(out).unwrap().len(), 1);
    // Once marked, a trip is no longer counted.
    db.apply_external_claims("fixture", day).unwrap();
    assert_eq!(db.open_trips_matching_external(out).unwrap().len(), 0);
    assert_eq!(db.open_trips_matching_external(back).unwrap().len(), 0);
    assert_eq!(db.open_trips_matching_external(9999).unwrap().len(), 0);
}

#[test]
fn claimed_elsewhere_names_resolve_to_one_station_and_match_by_station() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    let mut gw = FixtureClaimGateway { verified: true, ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Manual);
    let day = delayed_journey("D1").check_in.with_timezone(&chrono_tz::Europe::Amsterdam).date_naive();
    // One or two letters, or a part of a word, never stand for a station.
    for (o, d) in [("L", "U"), ("Ut", "Zwolle"), ("Amers", "'s-Hertogenbosch")] {
        let e = db.add_external_claim(day, Some(o), Some(d), "x", None).unwrap_err().to_string();
        assert!(e.contains("niet gevonden"), "{o} → {d}: {e}");
    }
    // The same station written two ways.
    let e = db.add_external_claim(day, Some("Utrecht C"), Some("Utrecht"), "x", None).unwrap_err().to_string();
    assert!(e.starts_with("Vertrek- en aankomststation zijn hetzelfde."), "{e}");
    // Stored and shown with the full station names; the trip is matched by its stations.
    let id = db.add_external_claim(day, Some("amersfoort c"), Some("Den Bosch"), "x", None).unwrap();
    let row = db.external_claims().unwrap().into_iter().find(|r| r.0 == id).unwrap();
    assert_eq!((row.2.as_deref(), row.3.as_deref()), (Some("Amersfoort Centraal"), Some("'s-Hertogenbosch")));
    let trips = db.open_trips_matching_external(id).unwrap();
    assert_eq!(trips.len(), 1);
    assert_eq!(trips[0].origin.name, "Amersfoort Centraal");
    // Written another way, it is the same entry.
    let e = db.add_external_claim(day, Some("Amersfoort"), Some("'s-Hertogenbosch"), "x", None).unwrap_err().to_string();
    assert!(e.starts_with("Deze reis staat al in je lijst"), "{e}");
    db.delete_external_claim(id).unwrap();
    // An entry stored before the stations were kept (typed text, no stations) still matches as before.
    db.conn
        .execute(
            "INSERT INTO external_claims(travel_date,origin_text,dest_text,source,note,created_at) VALUES(?1,'Amers','Hertog','x',NULL,?2)",
            (day.to_string(), chrono::Utc::now().to_rfc3339()),
        )
        .unwrap();
    assert!(db.has_external_claim(&delayed_journey("D1")).unwrap());
}

#[test]
fn an_automatic_retry_refused_by_the_final_checks_stays_failed_with_the_reason() {
    let mut db = Db::open_in_memory().unwrap();
    let mut src = FixtureJourneySource { journeys: vec![delayed_journey("D1")], fail_with: None };
    // First check: NS conclusively did not receive the claim (FAILED, retryable).
    let mut gw = FixtureClaimGateway { verified: true, next_outcomes: vec!["notsent"], ..Default::default() };
    run_once(&mut db, &mut src, &mut gw, RunMode::Automatic);
    let c = db.all_claims().unwrap()[0].clone();
    assert_eq!(c.status, ClaimState::Failed);
    // Second check: the claim is queued for a retry (FAILED -> READY), then the final checks
    // refuse it (its delay band disappeared right before sending). Nothing reaches NS.
    let mut o = opts(RunMode::Automatic);
    o.auto_still_allowed = Some(Box::new(|db: &Db| {
        db.conn.execute("UPDATE candidates SET delay_band=NULL", []).unwrap();
        true
    }));
    let s = run_with(&mut db, &mut src, &mut gw, o);
    assert_eq!(gw.submitted.len(), 0, "nothing sent");
    assert_eq!(s.refused, 1, "{s:?}");
    let row = db.claim(c.id).unwrap();
    assert_eq!(row.status, ClaimState::Failed, "the refused retry is FAILED again, never left READY");
    let last = db.events(c.id).unwrap().pop().unwrap();
    assert_eq!((last.from_status.as_deref(), last.to_status.as_str()), (Some("READY"), "FAILED"));
    assert!(
        last.detail.starts_with("Niet verzonden: ") && last.detail.contains("Not sent: The delay of this trip is not clear enough."),
        "{}",
        last.detail
    );
}
