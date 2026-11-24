//! Opt-in NETWORK verification, never run by normal CI:
//! cargo test -p ovrefund-core --test archive_live -- --ignored --nocapture
//! Test B additionally needs OVREFUND_VERIFY_DB pointing to a COPY of the current app DB.
//! No archive files, evidence caches, or database changes are persisted.

// Include the production sources to access cfg(test)/pub(crate) hooks without exposing
// verification controls in the shipped library. Their ordinary unit tests remain offline.
#[allow(dead_code)]
#[path = "../src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../src/schedule.rs"]
mod schedule;
pub use ovrefund_core::{db, eligibility, matcher, model, progress, stations};

use anyhow::{bail, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use db::Db;
use model::{Journey, ScheduleEvidence, StationRef};
use rusqlite::{Connection, OpenFlags};
use schedule::{ArchiveScheduleProvider, ScheduleProvider, VerificationPath};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

const MONTHS: [&str; 5] = ["2026-03", "2026-04", "2026-09", "2026-10", "2026-11"];
const ROUTES: [(&str, &str); 5] = [("LEDN", "ASDZ"), ("UT", "ZL"), ("EHV", "NM"), ("DT", "LEDN"), ("ASD", "RTD")];

fn station(code: &str) -> StationRef {
    let s = stations::STATIONS.by_code(code).expect("embedded synthetic station");
    StationRef { uic: s.uic.clone(), name: s.name.clone() }
}

fn trip(a: &str, b: &str, ci: DateTime<Utc>, co: DateTime<Utc>) -> Journey {
    Journey {
        source_id: "archive-live-synthetic".into(),
        source_record_id: format!("{a}-{b}-{ci}"),
        coupling_id: None,
        card_last4: None,
        check_in: ci,
        check_out: Some(co),
        origin: station(a),
        destination: Some(station(b)),
        carrier_code: Some("4".into()),
        carrier_name: Some("NS".into()),
        product_name: None,
        travel_class: Some(2),
        fare_cents: Some(1500),
        subscription: None,
        subscription_fee_cents: None,
        invoiced: Some(true),
        payload_sha256: String::new(),
    }
}

fn local(d: NaiveDate, h: u32, m: u32) -> DateTime<Utc> {
    Amsterdam.from_local_datetime(&d.and_hms_opt(h, m, 0).unwrap()).single().expect("unambiguous synthetic time").with_timezone(&Utc)
}

fn hard_cases(month: &str) -> Vec<(String, Journey)> {
    let first = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").unwrap();
    let last = (first + Duration::days(32)).with_day(1).unwrap().pred_opt().unwrap();
    let mut cases = vec![];
    for (r, (a, b)) in ROUTES.into_iter().enumerate() {
        for (label, day, h, m, minutes) in [
            ("first-0015", 1, 0, 15, 75),
            ("previous-service-day", 15, 0, 30, 60),
            ("same-day-am", 15, 8, 0, 75),
            ("same-day-pm", 15, 17, 0, 90),
            ("adjacent-day", 16, 8, 0, 75),
            ("two-days-apart", 18, 8, 0, 75),
            ("first-day", 1, 12, 0, 75),
            ("last-day", last.day(), 12, 0, 75),
            ("month-crossing", last.day(), 23, 40, 40),
        ] {
            let ci = local(first.with_day(day).unwrap(), h, m);
            cases.push((format!("{label}-route{}", r + 1), trip(a, b, ci, ci + Duration::minutes(minutes))));
        }
    }
    let dst = match month {
        "2026-03" => Some((29, 1, 45)),
        "2026-10" => Some((25, 1, 30)),
        _ => None,
    };
    if let Some((day, h, m)) = dst {
        let d = first.with_day(day).unwrap();
        cases.push(("DST-night".into(), trip("UT", "ZL", local(d, h, m), local(d, 3, 30))));
    }
    cases
}

// Select from a streaming full-month discovery scan, rather than assuming a disruption
// date or a diverted train exists. All Dutch stations are eligible for discovered routes.
fn discover(month: &str) -> Result<Option<Vec<(String, Journey)>>> {
    let all: HashSet<_> =
        include_str!("../assets/stations-nl.csv").lines().skip(1).filter_map(|l| l.split(',').nth(1)).map(str::to_owned).collect();
    let mut delayed = None;
    let mut diverted = vec![];
    let mut diverted_services = HashSet::new();
    let bytes = archive::verify_full_month(month, &all, |service| {
        if delayed.is_some() && diverted.len() == 8 {
            return Ok(());
        }
        for (i, from) in service.iter().enumerate() {
            if from.company != "NS" || stations::STATIONS.by_code(&from.station_code).is_none() {
                continue;
            }
            let Some(dep) = from.dep_planned else { continue };
            for to in &service[i + 1..] {
                if stations::STATIONS.by_code(&to.station_code).is_none() || from.station_code == to.station_code {
                    continue;
                }
                let Some(arr) = to.arr_planned.filter(|a| *a > dep) else { continue };
                let ci = dep - Duration::minutes(3);
                if archive::month_key(ci.with_timezone(&Amsterdam).date_naive()) != month {
                    continue;
                }
                if delayed.is_none() && to.arr_delay_min.is_some_and(|d| d >= 30) && !to.arr_cancelled && !from.dep_cancelled {
                    // Long checkout, 90 minutes beyond planned arrival, on a real delayed route.
                    delayed = Some(trip(&from.station_code, &to.station_code, ci, arr + Duration::minutes(90)));
                }
                if diverted.len() < 8
                    && from.train_number.len() == 6
                    && from.train_number.starts_with("70")
                    && diverted_services.insert(from.service_id)
                {
                    diverted.push(trip(&from.station_code, &to.station_code, ci, to.arr_actual().unwrap_or(arr) + Duration::minutes(2)));
                }
            }
        }
        Ok(())
    })?;
    let Some(bytes) = bytes else { return Ok(None) };
    println!("discovery {month}: {:.2} MB (excluded from comparison totals)", mb(bytes));
    let mut cases = vec![];
    if let Some(j) = delayed {
        cases.push(("observed-delay-long-checkout".into(), j));
    } else {
        bail!("no usable >=30 minute arrival delay found in required archive {month}");
    }
    if diverted.is_empty() {
        println!("{month}: no usable 70xxxx service found (full archive scanned)");
    }
    // A few distinct services avoid relying on one diverted train winning over
    // all competing regular trains in the full-month route matcher.
    cases.extend(diverted.into_iter().enumerate().map(|(i, j)| (format!("observed-70xxxx-{}", i + 1), j)));
    Ok(Some(cases))
}

struct Run {
    evidence: Vec<ScheduleEvidence>,
    bytes: BTreeMap<String, u64>,
}

fn run(journeys: &[Journey], path: VerificationPath) -> Result<Run> {
    let mut db = Db::open_in_memory()?;
    let mut provider = ArchiveScheduleProvider::new();
    let bytes = provider.verify_prepare(&mut db, journeys, path)?;
    // Missing archives must never turn into a successful Pending-vs-Pending comparison.
    anyhow::ensure!(provider.pending_months.is_empty(), "an archive became unavailable during comparison");
    let evidence = journeys.iter().map(|j| provider.evidence(&db, j)).collect::<Result<_>>()?;
    Ok(Run { evidence, bytes })
}

fn print_months(old: &Run, new: &Run) -> Result<()> {
    println!("archive month | legacy MB | new MB");
    for (month, legacy) in &old.bytes {
        let early_stop = new.bytes.get(month).copied().unwrap_or(0);
        println!("{month} | {:.2} | {:.2}", mb(*legacy), mb(early_stop));
        anyhow::ensure!(early_stop <= *legacy, "{month}: early-stop bytes exceed legacy month");
    }
    anyhow::ensure!(new.bytes.keys().all(|m| old.bytes.contains_key(m)), "unexpected early-stop archive month");
    Ok(())
}

fn snapshot(j: &Journey, evidence: &ScheduleEvidence) -> Value {
    let assessment = eligibility::assess(&eligibility::PolicyInput {
        journey: j,
        evidence,
        now: j.check_in + Duration::days(2),
        window: Default::default(),
        already_claimed: false,
    });
    let compensation = ovrefund_core::compensation::expected(j, assessment.delay_band, None);
    serde_json::json!({ "evidence": evidence, "assessment": assessment, "compensation": compensation })
}

// Only field paths are returned; never format compared values, even on failure.
fn difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    if a == b {
        return None;
    }
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys()) {
                let field = format!("{path}.{key}");
                match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => {
                        if let Some(d) = difference(a, b, &field) {
                            return Some(d);
                        }
                    }
                    _ => return Some(field),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                if let Some(d) = difference(a, b, &format!("{path}[{i}]")) {
                    return Some(d);
                }
            }
        }
        _ => {}
    }
    Some(path.into())
}

fn mb(n: u64) -> f64 {
    n as f64 / 1_000_000.0
}

#[test]
#[ignore = "NETWORK: downloads real monthly archives; maintainer opt-in only"]
fn a_synthetic_hard_trips_on_real_archives() -> Result<()> {
    println!("{}", archive::ATTRIBUTION);
    let mut cases = vec![];
    for month in MONTHS {
        match discover(month)? {
            Some(mut selected) => {
                cases.extend(hard_cases(month));
                cases.append(&mut selected);
            }
            None if month == "2026-11" => println!("{month}: not published; skipped"),
            None => bail!("required archive {month} is unavailable"),
        }
    }
    let journeys: Vec<_> = cases.iter().map(|(_, j)| j.clone()).collect();
    let old = run(&journeys, VerificationPath::Legacy)?;
    let new = run(&journeys, VerificationPath::EarlyStop)?;
    print_months(&old, &new)?;
    for month in MONTHS {
        let diverted_cases: Vec<_> = cases
            .iter()
            .enumerate()
            .filter(|(_, (label, j))| {
                label.starts_with("observed-70xxxx") && archive::month_key(j.check_in.with_timezone(&Amsterdam).date_naive()) == month
            })
            .collect();
        anyhow::ensure!(
            diverted_cases.is_empty()
                || diverted_cases.iter().any(|(i, _)| old.evidence[*i].planned.as_ref().is_some_and(|p| p.diverted())),
            "{month}: discovered diverted trains did not exercise the baseline path"
        );
    }
    println!("case | month | equal (all journeys batched together)");
    let mut unequal = 0;
    for (i, (label, j)) in cases.iter().enumerate() {
        let month = archive::month_key(j.check_in.with_timezone(&Amsterdam).date_naive());
        let diff = difference(&snapshot(j, &old.evidence[i]), &snapshot(j, &new.evidence[i]), "result");
        println!("{label} | {month} | {}", if diff.is_none() { "yes" } else { "no" });
        if let Some(field) = diff {
            unequal += 1;
            println!("case {label}: {field}");
        }
    }
    println!(
        "totals (including preceding-month carry): legacy {:.2} MB, new {:.2} MB",
        mb(old.bytes.values().sum()),
        mb(new.bytes.values().sum())
    );
    anyhow::ensure!(unequal == 0, "{unequal} synthetic cases differ");
    Ok(())
}

fn read_journeys(path: &std::path::Path) -> Result<Vec<Journey>> {
    // No Db::open: that migrates/writes. No URI flags, backup, WAL changes or temp files.
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.pragma_update(None, "query_only", true)?;
    let mut stmt = conn.prepare("SELECT * FROM journeys ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut journeys = vec![];
    while let Some(r) = rows.next()? {
        let ci: String = r.get("check_in_utc")?;
        let co: Option<String> = r.get("check_out_utc")?;
        let dest: Option<String> = r.get("dest_uic")?;
        journeys.push(Journey {
            source_id: r.get("source_id")?,
            source_record_id: r.get("source_record_id")?,
            coupling_id: r.get("coupling_id")?,
            card_last4: r.get("card_last4")?,
            check_in: DateTime::parse_from_rfc3339(&ci)?.with_timezone(&Utc),
            check_out: co.map(|v| DateTime::parse_from_rfc3339(&v).map(|t| t.with_timezone(&Utc))).transpose()?,
            origin: StationRef { uic: r.get("origin_uic")?, name: r.get("origin_name")? },
            destination: dest
                .map(|uic| -> rusqlite::Result<_> {
                    Ok(StationRef { uic, name: r.get::<_, Option<String>>("dest_name")?.unwrap_or_default() })
                })
                .transpose()?,
            carrier_code: r.get("carrier_code")?,
            carrier_name: r.get("carrier_name")?,
            product_name: r.get("product_name")?,
            travel_class: r.get("travel_class")?,
            fare_cents: r.get("fare_cents")?,
            subscription: r.get("subscription")?,
            subscription_fee_cents: r.get("subscription_fee_cents")?,
            invoiced: r.get::<_, Option<i64>>("invoiced")?.map(|v| v != 0),
            payload_sha256: r.get("payload_sha256")?,
        });
    }
    Ok(journeys)
}

fn published(month: &str) -> Result<bool> {
    match ureq::head(&archive::month_url(month)).set("Accept-Encoding", "identity").timeout(std::time::Duration::from_secs(600)).call() {
        Ok(_) => Ok(true),
        Err(ureq::Error::Status(403 | 404, _)) => Ok(false),
        Err(_) => bail!("archive publication check failed"),
    }
}

#[test]
#[ignore = "NETWORK + PRIVATE COPY: set OVREFUND_VERIFY_DB; prints no journey values"]
fn b_real_journeys_from_read_only_copy() -> Result<()> {
    let Some(path) = std::env::var_os("OVREFUND_VERIFY_DB") else {
        println!("Test B skipped: set OVREFUND_VERIFY_DB to a COPY of the app database");
        return Ok(());
    };
    // Suppress error chains too: SQLite and timestamp parse errors may contain private data.
    let journeys = read_journeys(std::path::Path::new(&path)).map_err(|_| anyhow::anyhow!("cannot read verification database copy"))?;
    let mut publication = BTreeMap::new();
    let mut selected = vec![];
    for (index, j) in journeys.into_iter().enumerate() {
        let mut available = true;
        for month in schedule::journey_months(&j) {
            let available_month = match publication.get(&month) {
                Some(v) => *v,
                None => {
                    let v = published(&month)?;
                    publication.insert(month, v);
                    v
                }
            };
            available &= available_month;
        }
        if available {
            selected.push((index + 1, j));
        }
    }
    let journeys: Vec<_> = selected.iter().map(|(_, j)| j.clone()).collect();
    let old = run(&journeys, VerificationPath::Legacy).map_err(|_| anyhow::anyhow!("legacy verification failed (details suppressed)"))?;
    let new =
        run(&journeys, VerificationPath::EarlyStop).map_err(|_| anyhow::anyhow!("early-stop verification failed (details suppressed)"))?;
    print_months(&old, &new)?;
    let mut equal = 0;
    for (i, (index, j)) in selected.iter().enumerate() {
        match difference(&snapshot(j, &old.evidence[i]), &snapshot(j, &new.evidence[i]), "result") {
            None => {
                equal += 1;
                println!("journey {index}: equal yes");
            }
            Some(field) => println!("journey {index}: equal no, field {field}"),
        }
    }
    println!(
        "journeys compared: {}, equal: {equal}, legacy MB: {:.2}, new MB: {:.2}",
        journeys.len(),
        mb(old.bytes.values().sum()),
        mb(new.bytes.values().sum())
    );
    anyhow::ensure!(equal == journeys.len(), "real journey verification differs (see field names above)");
    Ok(())
}

// These checks run offline: exercise DST construction, field-only diagnostics and
// the real SQLite loader, independently of archive publication or network access.
#[test]
fn harness_dates_privacy_and_read_only_loader() -> Result<()> {
    for (month, elapsed) in [("2026-03", 45), ("2026-10", 180)] {
        let cases = hard_cases(month);
        let (_, j) = cases.iter().find(|(label, _)| label == "DST-night").unwrap();
        assert_eq!((j.check_out.unwrap() - j.check_in).num_minutes(), elapsed);
    }
    let a = serde_json::json!({"evidence": {"notes": ["PRIVATE-A"]}});
    let b = serde_json::json!({"evidence": {"notes": ["PRIVATE-B"]}});
    assert_eq!(difference(&a, &b, "result").as_deref(), Some("result.evidence.notes[0]"));
    assert!(difference(&a, &a, "result").is_none());

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("copy.sqlite");
    let (_, j) = hard_cases("2026-09").remove(0);
    {
        let db = Db::open(&path)?;
        db.upsert_source(&j.source_id, "fixture", false, "offline")?;
        db.upsert_journey(&j)?;
    }
    let before = std::fs::read(&path)?;
    assert_eq!(read_journeys(&path)?, vec![j]);
    assert_eq!(std::fs::read(&path)?, before);
    // RAII removes the complete temporary directory, including SQLite sidecars.
    dir.close()?;
    Ok(())
}
