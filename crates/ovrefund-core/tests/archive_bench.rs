//! Offline real-month benchmark. No network or persistent database writes.
#[allow(dead_code)]
#[path = "../src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../src/schedule.rs"]
mod schedule;
use anyhow::Result;
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use model::{Journey, StationRef};
pub use ovrefund_core::{db, eligibility, matcher, model, progress, stations};
use schedule::{ArchiveScheduleProvider, ScheduleProvider};
use std::{collections::HashSet, fs::File, time::Instant};

fn trip(a: &str, b: &str, day: u32, hour: u32) -> Journey {
    let station = |code| {
        let s = stations::STATIONS.by_code(code).unwrap();
        StationRef { uic: s.uic.clone(), name: s.name.clone() }
    };
    let ci = Amsterdam.with_ymd_and_hms(2026, 9, day, hour, 0, 0).unwrap().with_timezone(&Utc);
    Journey {
        source_id: "offline-bench".into(),
        source_record_id: format!("{a}-{b}-{ci}"),
        coupling_id: None,
        card_last4: None,
        check_in: ci,
        check_out: Some(ci + Duration::minutes(150)),
        origin: station(a),
        destination: Some(station(b)),
        carrier_code: Some("4".into()),
        carrier_name: Some("NS".into()),
        product_name: None,
        travel_class: Some(2),
        fare_cents: None,
        subscription: None,
        subscription_fee_cents: None,
        invoiced: Some(true),
        payload_sha256: String::new(),
    }
}

#[test]
#[ignore = "requires target/bench/services-2026-09.csv.gz; offline CPU benchmark"]
fn real_month_fresh_path() -> Result<()> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench/services-2026-09.csv.gz");
    // Codes verified directly against Stop:Station code/name in the supplied CSV.
    let journeys = vec![
        trip("LEDN", "ASDZ", 3, 8),
        trip("UT", "ZL", 7, 17),
        trip("EHV", "NM", 10, 8),
        trip("DT", "LEDN", 14, 17),
        trip("NM", "LEDN", 18, 8),
        trip("LEDN", "ASDZ", 22, 17),
        trip("UT", "ZL", 26, 8),
        trip("NM", "LEDN", 29, 17),
    ];
    let stations: HashSet<_> = ["LEDN", "ASDZ", "UT", "ZL", "EHV", "NM", "DT"].into_iter().map(str::to_owned).collect();
    let dates: HashSet<_> = (1..=30).map(|d| NaiveDate::from_ymd_opt(2026, 9, d).unwrap()).collect();
    let start = Instant::now();
    let mut services = Vec::new();
    archive::scan_until(File::open(&path)?, true, &stations, dates.iter().max().copied(), |s| {
        if dates.contains(&s[0].service_date) {
            services.push(s.to_vec());
        }
        Ok(())
    })?;
    let stream = start.elapsed();
    let rows: usize = services.iter().map(Vec::len).sum();
    println!("stream+filter={stream:?}, services={}, rows={rows}", services.len());
    let provider = ArchiveScheduleProvider::new();
    let fresh = db::Db::open_in_memory()?;
    let refs: Vec<_> = journeys.iter().collect();
    let start = Instant::now();
    provider.ingest(&fresh, "2026-09", &services, &refs)?;
    let evidence = start.elapsed();
    println!("fresh evidence (8)={evidence:?}; total fresh={:?}", stream + evidence);
    // Exercise the production batch path, replacing only the HTTP source with a local reader.
    let mut prepared = db::Db::open_in_memory()?;
    let mut provider = ArchiveScheduleProvider::new();
    let start = Instant::now();
    provider.prepare_with(
        &mut prepared,
        &journeys,
        |_, st, dates| {
            let mut services = Vec::new();
            archive::scan_until(File::open(&path)?, true, st, dates.iter().max().copied(), |s| {
                if dates.contains(&s[0].service_date) {
                    services.push(s.to_vec());
                }
                Ok(())
            })?;
            Ok(Some((services, 0)))
        },
        &mut |_, _, _, _, _| Ok(()),
    )?;
    println!("production prepare total={:?}", start.elapsed());
    for j in &journeys {
        assert_eq!(provider.evidence(&fresh, j)?, provider.evidence(&prepared, j)?);
    }
    Ok(())
}
