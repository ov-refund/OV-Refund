//! `ScheduleProvider` backed by the Rijden de Treinen historical archive.
//!
//! Lean by design: a month file is streamed from the download straight into the
//! parser (never written to disk), the user's journeys of that month are matched
//! in memory, and only the per-journey evidence (planned/taken trains, ~1 KB) is
//! stored. Raw timetable data is discarded immediately.
use crate::archive;
use crate::db::Db;
use crate::matcher::{self, Service};
use crate::model::{EvidenceStatus, Journey, ScheduleEvidence};
use crate::stations::STATIONS;
use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;

pub type MonthReady<'a> = dyn FnMut(&mut Db, &dyn ScheduleProvider, &str, usize, usize) -> Result<()> + 'a;

pub trait ScheduleProvider {
    /// Make sure evidence for these journeys is available locally (may download).
    fn prepare(&mut self, db: &mut Db, journeys: &[Journey]) -> Result<Vec<String>>;
    /// Providers with monthly batches call this only after evidence has been persisted.
    fn prepare_incremental(&mut self, db: &mut Db, journeys: &[Journey], _ready: &mut MonthReady<'_>) -> Result<Vec<String>> {
        self.prepare(db, journeys)
    }
    fn evidence(&self, db: &Db, j: &Journey) -> Result<ScheduleEvidence>;
}

pub struct ArchiveScheduleProvider {
    /// Months known to be unpublished during this run.
    pub pending_months: HashSet<String>,
    /// Offline mode for tests: never download.
    pub offline: bool,
}

impl Default for ArchiveScheduleProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveScheduleProvider {
    pub fn new() -> Self {
        Self { pending_months: HashSet::new(), offline: false }
    }

    /// Match `journeys` of `month` against already-parsed services and store their evidence.
    pub fn ingest(&self, db: &Db, month: &str, services: &[Service], journeys: &[&Journey]) -> Result<usize> {
        let index = ServiceIndex::new(services);
        let mut n = 0;
        for j in journeys.iter().filter(|j| journey_month(j) == month) {
            let Some((a, b)) = codes(j) else { continue };
            let ev = journey_evidence(&index, j, &a, &b);
            db.put_cached_evidence(&j.fallback_key(), month, &ev)?;
            n += 1;
        }
        Ok(n)
    }
}

// Built once per monthly batch, including any previous-month carry. Keep input
// service order so equal-scoring routes retain exactly the legacy tie breaking.
struct ServiceIndex<'a> {
    services: &'a [Service],
    by_station_date: BTreeMap<(NaiveDate, &'a str), Vec<usize>>,
}

impl<'a> ServiceIndex<'a> {
    fn new(services: &'a [Service]) -> Self {
        let mut by_station_date: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for (i, s) in services.iter().enumerate() {
            for stop in s {
                let ids = by_station_date.entry((s[0].service_date, stop.station_code.as_str())).or_default();
                if ids.last() != Some(&i) {
                    ids.push(i);
                }
            }
        }
        Self { services, by_station_date }
    }

    fn select(&self, dates: &[NaiveDate], a: &str, b: &str) -> Vec<&'a Service> {
        let mut ids = Vec::new();
        for &date in dates {
            for station in [a, b] {
                if let Some(found) = self.by_station_date.get(&(date, station)) {
                    ids.extend_from_slice(found);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().map(|i| &self.services[i]).collect()
    }
}

fn journey_evidence(index: &ServiceIndex<'_>, j: &Journey, a: &str, b: &str) -> ScheduleEvidence {
    let relevant = index.select(&service_dates(j), a, b);
    let m = matcher::match_journey_refs(&relevant, a, b, j.check_in, j.check_out);
    let mut ev = base(if m.planned.is_some() { EvidenceStatus::Found } else { EvidenceStatus::NoRoute });
    if m.planned.as_ref().map(|p| p.diverted()).unwrap_or(false) {
        ev.regular_arrival = regular_arrival(index, a, b, j);
        ev.regular_checked = true;
    }
    ev.planned = m.planned;
    ev.taken = m.taken;
    ev.notes = m.notes;
    if j.check_out.map(|co| co - j.check_in > Duration::hours(6)).unwrap_or(false) {
        ev.notes.push("journey longer than 6 h; check data".into());
    }
    ev
}

fn base(status: EvidenceStatus) -> ScheduleEvidence {
    ScheduleEvidence {
        provider: archive::PROVIDER.into(),
        historical: true,
        planned: None,
        taken: None,
        status,
        notes: vec![],
        regular_arrival: None,
        regular_checked: false,
    }
}

/// Weeks around the travel date used as the regular-timetable baseline.
const BASELINE_WEEKS: [i64; 4] = [-1, 1, -2, 2];

/// Planned arrival in the regular timetable: the same check-in time on the same weekday a week or
/// two away (within the same month's data), on a day without diverted trains on that route.
fn regular_arrival(index: &ServiceIndex<'_>, a: &str, b: &str, j: &Journey) -> Option<DateTime<Utc>> {
    for w in BASELINE_WEEKS {
        let Some(ci) = shift_local(j.check_in, 7 * w) else { continue };
        let d = ci.with_timezone(&Amsterdam).date_naive();
        let dates = [d.pred_opt().unwrap_or(d), d];
        let svc = index.select(&dates, a, b);
        if let Some(p) = matcher::match_journey_refs(&svc, a, b, ci, None).planned.filter(|p| !p.diverted()) {
            if let Some(arrival) = shift_local(p.arr_planned(), -7 * w) {
                return Some(arrival);
            }
        }
    }
    None
}

// Baselines use the same Amsterdam wall-clock time. Ambiguous/nonexistent DST times
// cannot identify a unique train and are skipped.
fn shift_local(t: DateTime<Utc>, days: i64) -> Option<DateTime<Utc>> {
    let local = t.with_timezone(&Amsterdam).naive_local().checked_add_signed(Duration::days(days))?;
    Amsterdam.from_local_datetime(&local).single().map(|t| t.with_timezone(&Utc))
}

/// Service dates to keep while streaming a month: the travel dates, plus the baseline weeks.
fn wanted_dates(j: &Journey) -> Vec<NaiveDate> {
    let mut v = service_dates(j);
    for w in BASELINE_WEEKS {
        if let Some(ci) = shift_local(j.check_in, 7 * w) {
            let mut baseline = j.clone();
            baseline.check_in = ci;
            v.extend(service_dates(&baseline));
        }
    }
    v
}

fn service_dates(j: &Journey) -> Vec<NaiveDate> {
    let d = j.check_in.with_timezone(&Amsterdam).date_naive();
    // Trains after midnight belong to the previous service date. Retain the
    // conservative legacy selection until real evidence verifies that services
    // continuing past 04:00 cannot affect matching (including long journeys).
    vec![d.pred_opt().unwrap_or(d), d]
}

fn journey_month(j: &Journey) -> String {
    archive::month_key(j.check_in.with_timezone(&Amsterdam).date_naive())
}

/// Archive months a journey needs: its own month, plus the previous one for a check-in before
/// 04:00 on the 1st (a train after midnight belongs to the previous day's service, which is in
/// the previous month's file).
pub fn journey_months(j: &Journey) -> Vec<String> {
    use chrono::{Datelike, Timelike};
    let t = j.check_in.with_timezone(&Amsterdam);
    let own = journey_month(j);
    if t.day() == 1 && t.hour() < 4 {
        let prev = t.date_naive().pred_opt().map(archive::month_key);
        prev.into_iter().chain(std::iter::once(own)).collect()
    } else {
        vec![own]
    }
}

fn prev_month(month: &str) -> Option<String> {
    let d = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").ok()?;
    d.pred_opt().map(archive::month_key)
}

fn codes(j: &Journey) -> Option<(String, String)> {
    let a = STATIONS.by_uic(&j.origin.uic)?.code.clone();
    let b = STATIONS.by_uic(&j.destination.as_ref()?.uic)?.code.clone();
    Some((a, b))
}

impl ScheduleProvider for ArchiveScheduleProvider {
    fn prepare(&mut self, db: &mut Db, journeys: &[Journey]) -> Result<Vec<String>> {
        self.prepare_incremental(db, journeys, &mut |_, _, _, _, _| Ok(()))
    }

    fn prepare_incremental(&mut self, db: &mut Db, journeys: &[Journey], ready: &mut MonthReady<'_>) -> Result<Vec<String>> {
        self.prepare_with(db, journeys, |month, stations, dates| archive::download_month(month, stations, dates, true), ready)
    }

    fn evidence(&self, db: &Db, j: &Journey) -> Result<ScheduleEvidence> {
        if codes(j).is_none() {
            return Ok(base(EvidenceStatus::UnknownStation));
        }
        Ok(db.cached_evidence(&j.fallback_key())?.unwrap_or_else(|| base(EvidenceStatus::Pending)))
    }
}

impl ArchiveScheduleProvider {
    pub(crate) fn prepare_with(
        &mut self,
        db: &mut Db,
        journeys: &[Journey],
        mut fetch: impl FnMut(&str, &HashSet<String>, &HashSet<NaiveDate>) -> Result<Option<(Vec<Service>, u64)>>,
        ready: &mut MonthReady<'_>,
    ) -> Result<Vec<String>> {
        // Only journeys without stored evidence need work.
        let mut by_month: BTreeMap<String, Vec<&Journey>> = BTreeMap::new();
        for j in journeys {
            if codes(j).is_none() {
                continue;
            }
            // Cached evidence is final, except adjusted-timetable trips cached before the
            // regular-timetable baseline existed (looked up once more).
            if let Some(ev) = db.cached_evidence(&j.fallback_key())? {
                if ev.regular_checked || !ev.planned.as_ref().map(|p| p.diverted()).unwrap_or(false) {
                    continue;
                }
            }
            for m in journey_months(j) {
                by_month.entry(m).or_default().push(j);
            }
        }
        let mut log = vec![];
        let n_months = by_month.len();
        // Preserve the latest selected date's services, exactly as in the full-month flow.
        let mut carry: Option<(String, Vec<Service>)> = None;
        for (mi, (month, js)) in by_month.into_iter().enumerate() {
            crate::progress::timetable(crate::progress::within(35, 80, mi, n_months), &month, mi + 1, n_months, false);
            let stations = js.iter().filter_map(|j| codes(j)).flat_map(|(a, b)| [a, b]).collect();
            let dates: HashSet<_> = js.iter().flat_map(|j| wanted_dates(j)).filter(|d| archive::month_key(*d) == month).collect();
            let result = if self.offline { None } else { fetch(&month, &stations, &dates)? };
            if let Some((mut services, bytes)) = result {
                // When the month file was downloaded (shown on delay proofs made from it).
                db.set_setting(&archive::retrieved_key(&month), &chrono::Utc::now().to_rfc3339())?;
                let last = services.iter().map(|s| s[0].service_date).max();
                let next = services.iter().filter(|s| Some(s[0].service_date) == last).cloned().collect();
                if let Some((cm, cs)) = carry.take() {
                    if prev_month(&month).as_deref() == Some(cm.as_str()) {
                        services.extend(cs);
                    }
                }
                carry = Some((month.clone(), next));
                let n = self.ingest(db, &month, &services, &js)?;
                let day = dates.iter().max().expect("month has wanted dates").format("%d");
                log.push(format!("archive {month}: {:.1} MB read (stopped after {day})", bytes as f64 / 1_000_000.0));
                log.push(format!(
                    "archive {month}: evidence for {n} journeys from {} relevant services (raw data discarded)",
                    services.len()
                ));
            } else {
                self.pending_months.insert(month.clone());
                log.push(format!("archive {month} not yet published"));
            }
            ready(db, self, &month, mi + 1, n_months)?;
            crate::progress::timetable(crate::progress::within(35, 80, mi + 1, n_months), &month, mi + 1, n_months, false);
        }
        Ok(log)
    }
}

/// Stream helper used by tests to feed a plain CSV.
pub fn parse_services<R: Read>(r: R, gz: bool, stations: &HashSet<String>) -> Result<Vec<Service>> {
    let mut v = vec![];
    archive::scan_archive(r, gz, stations, |s| {
        v.push(s.to_vec());
        Ok(())
    })?;
    Ok(v)
}

// The integration harness includes this module so these hooks need no public API or feature.
#[cfg(test)]
#[derive(Clone, Copy)]
#[allow(dead_code)] // Used by the source-including integration target.
pub(crate) enum VerificationPath {
    Legacy,
    EarlyStop,
}

#[cfg(test)]
impl ArchiveScheduleProvider {
    #[allow(dead_code)] // Used by the source-including integration target.
    pub(crate) fn verify_prepare(&mut self, db: &mut Db, journeys: &[Journey], path: VerificationPath) -> Result<BTreeMap<String, u64>> {
        let mut bytes = BTreeMap::new();
        self.prepare_with(
            db,
            journeys,
            |month, stations, dates| {
                let result = archive::download_month(month, stations, dates, matches!(path, VerificationPath::EarlyStop))?;
                if let Some((_, count)) = &result {
                    bytes.insert(month.to_string(), *count);
                }
                Ok(result)
            },
            &mut |_, _, _, _, _| Ok(()),
        )?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod month_tests {
    use super::*;
    use crate::model::StationRef;

    fn j(ci: &str) -> Journey {
        Journey {
            source_id: "t".into(),
            source_record_id: "x".into(),
            coupling_id: None,
            card_last4: None,
            check_in: DateTime::parse_from_rfc3339(ci).unwrap().with_timezone(&Utc),
            check_out: None,
            origin: StationRef { uic: "1".into(), name: "A".into() },
            destination: None,
            carrier_code: Some("4".into()),
            carrier_name: None,
            product_name: None,
            travel_class: None,
            fare_cents: None,
            subscription: None,
            subscription_fee_cents: None,
            invoiced: None,
            payload_sha256: String::new(),
        }
    }

    #[test]
    fn wanted_dates_preserve_midnight_and_regular_timetable_baseline() {
        let trip = j("2026-09-15T00:30:00Z");
        let dates = wanted_dates(&trip);
        for day in [1, 7, 8, 14, 15, 21, 22, 28, 29] {
            assert!(dates.contains(&NaiveDate::from_ymd_opt(2026, 9, day).unwrap()));
        }
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()));
    }

    #[test]
    fn midnight_on_the_first_also_needs_the_previous_month() {
        // 00:30 local on 1 Dec 2026 (UTC+1) -> November's file has the 30 Nov service
        assert_eq!(journey_months(&j("2026-11-30T23:30:00Z")), vec!["2026-11".to_string(), "2026-12".to_string()]);
        // 1 Jan: previous month is December of the year before
        assert_eq!(journey_months(&j("2026-12-31T23:10:00Z")), vec!["2026-12".to_string(), "2027-01".to_string()]);
        // 04:30 on the 1st, or any other day: own month only
        assert_eq!(journey_months(&j("2026-12-01T03:30:00Z")), vec!["2026-12".to_string()]);
        assert_eq!(journey_months(&j("2026-12-02T00:30:00Z")), vec!["2026-12".to_string()]);
        assert_eq!(prev_month("2027-01").as_deref(), Some("2026-12"));
    }

    fn trip(ci: &str, delay: i64) -> Journey {
        let mut trip = j(ci);
        trip.origin = StationRef { uic: "8400055".into(), name: "Amersfoort Centraal".into() };
        trip.destination = Some(StationRef { uic: "8400319".into(), name: "'s-Hertogenbosch".into() });
        trip.check_out = Some(trip.check_in + Duration::minutes(65 + delay));
        trip
    }

    fn train(j: &Journey, number: &str, duration: i64, delay: i64, cancelled: bool) -> Service {
        ["AMF", "HT"]
            .iter()
            .enumerate()
            .map(|(i, code)| crate::model::ArchiveStop {
                service_id: j.check_in.timestamp(),
                service_date: j.check_in.with_timezone(&Amsterdam).date_naive(),
                train_type: "IC".into(),
                company: "NS".into(),
                train_number: number.into(),
                station_code: (*code).into(),
                stop_seq: i as i64,
                arr_planned: (i == 1).then_some(j.check_in + Duration::minutes(5 + duration)),
                dep_planned: (i == 0).then_some(j.check_in + Duration::minutes(5)),
                arr_delay_min: Some(delay),
                dep_delay_min: Some(0),
                arr_cancelled: cancelled,
                dep_cancelled: cancelled,
            })
            .collect()
    }

    // Reference is the original single-pass monthly flow, including its latest-date carry.
    fn assert_equivalent(journeys: &[Journey], data: &[Service]) -> Vec<(String, HashSet<NaiveDate>)> {
        let mut db = Db::open_in_memory().unwrap();
        let single = Db::open_in_memory().unwrap();
        let mut provider = ArchiveScheduleProvider::new();
        let select = |month: &str, stations: &HashSet<String>, dates: &HashSet<NaiveDate>| -> Vec<Service> {
            data.iter()
                .filter(|s| {
                    archive::month_key(s[0].service_date) == month
                        && dates.contains(&s[0].service_date)
                        && s.iter().any(|stop| stations.contains(&stop.station_code))
                })
                .cloned()
                .collect()
        };
        let mut requests = vec![];
        provider
            .prepare_with(
                &mut db,
                journeys,
                |month, stations, dates| {
                    requests.push((month.to_string(), dates.clone()));
                    Ok(Some((select(month, stations, dates), dates.len() as u64 * 1_000_000)))
                },
                &mut |_, _, _, _, _| Ok(()),
            )
            .unwrap();
        // Every downloaded month records when it was fetched.
        for (month, _) in &requests {
            let at = db.get_setting(&archive::retrieved_key(month)).unwrap().expect("retrieval time stored");
            assert!(DateTime::parse_from_rfc3339(&at).is_ok(), "{at}");
        }
        let mut months: BTreeMap<String, Vec<&Journey>> = BTreeMap::new();
        for j in journeys {
            for month in journey_months(j) {
                months.entry(month).or_default().push(j);
            }
        }
        let mut carry: Option<(String, Vec<Service>)> = None;
        for (month, js) in months {
            let stations = js.iter().filter_map(|j| codes(j)).flat_map(|(a, b)| [a, b]).collect();
            let dates = js.iter().flat_map(|j| wanted_dates(j)).collect();
            let mut services = select(&month, &stations, &dates);
            let last = services.iter().map(|s| s[0].service_date).max();
            let next = services.iter().filter(|s| Some(s[0].service_date) == last).cloned().collect();
            if let Some((cm, cs)) = carry.take() {
                if prev_month(&month).as_deref() == Some(cm.as_str()) {
                    services.extend(cs);
                }
            }
            carry = Some((month.clone(), next));
            provider.ingest(&single, &month, &services, &js).unwrap();
        }
        for j in journeys {
            let actual = provider.evidence(&db, j).unwrap();
            let expected = provider.evidence(&single, j).unwrap();
            assert_eq!(actual, expected, "{}", j.check_in);
            let assess = |ev: &ScheduleEvidence| {
                crate::eligibility::assess(&crate::eligibility::PolicyInput {
                    journey: j,
                    evidence: ev,
                    now: j.check_in + Duration::days(2),
                    window: Default::default(),
                    already_claimed: false,
                })
            };
            assert_eq!(serde_json::to_value(assess(&actual)).unwrap(), serde_json::to_value(assess(&expected)).unwrap());
        }
        // Final evidence also remains cached on a second run, with no requests.
        provider.prepare_with(&mut db, journeys, |_, _, _| panic!("final evidence must be cached"), &mut |_, _, _, _, _| Ok(())).unwrap();
        requests
    }

    #[test]
    fn service_index_matches_legacy_selection_in_input_order() {
        let first = trip("2026-10-25T08:00:00Z", 0);
        let previous = trip("2026-10-24T08:00:00Z", 0);
        let mut repeated = train(&first, "123", 60, 0, false);
        repeated.push(repeated[0].clone());
        // Deliberately unsorted, with repeated stations and identical services.
        let services = vec![repeated.clone(), train(&previous, "321", 25, 0, false), repeated];
        let index = ServiceIndex::new(&services);
        let date = first.check_in.date_naive();
        for dates in [vec![date], service_dates(&first), vec![date, date], vec![]] {
            for a in ["AMF", "HT", "missing"] {
                for b in ["AMF", "HT", "missing"] {
                    let expected: Vec<_> = services
                        .iter()
                        .filter(|s| dates.contains(&s[0].service_date) && s.iter().any(|x| x.station_code == a || x.station_code == b))
                        .map(|s| s as *const Service)
                        .collect();
                    let actual: Vec<_> = index.select(&dates, a, b).into_iter().map(|s| s as *const Service).collect();
                    assert_eq!(actual, expected, "{dates:?}: {a} -> {b}");
                }
            }
        }
    }

    #[test]
    fn monthly_evidence_equals_full_month_for_mixed_multi_day_journeys() {
        let delays = [0, 45, 75, 29, 30, 59, 60, 0, 0, 35, 65];
        let journeys: Vec<_> =
            delays.iter().enumerate().map(|(i, delay)| trip(&format!("2026-09-{:02}T{:02}:00:00Z", i + 10, i + 6), *delay)).collect();
        let mut data = vec![];
        for (i, j) in journeys.iter().enumerate() {
            if i == 8 {
                continue;
            } // Unmatched on travel date, despite a baseline route.
            let diverted = i >= 7;
            data.push(train(j, if diverted { "700123" } else { "123" }, 60, delays[i], i == 6));
            if i == 6 {
                let mut later = train(j, "124", 120, 0, false);
                later[0].dep_planned = later[0].dep_planned.map(|t| t + Duration::minutes(30));
                data.push(later);
            }
            let shifted = trip(&(j.check_in - Duration::days(7)).to_rfc3339(), 0);
            data.push(train(&shifted, "321", 25, 0, false));
        }
        // A regular route exists only on baseline dates for this unmatched journey.
        data.push(train(&trip(&(journeys[8].check_in - Duration::days(7)).to_rfc3339(), 0), "321", 25, 0, false));
        data.sort_by_key(|s| s[0].service_date);
        let requests = assert_equivalent(&journeys, &data);
        assert_eq!(requests.len(), 1);
        let expected: HashSet<_> = journeys.iter().flat_map(wanted_dates).filter(|d| archive::month_key(*d) == "2026-09").collect();
        assert_eq!(requests[0].1, expected);
        // Ordinary on-time, delayed, near-threshold, cancelled and unmatched trips
        // individually make only one monthly request, even when baseline routes exist.
        for j in &journeys[..7] {
            assert_eq!(assert_equivalent(std::slice::from_ref(j), &data).len(), 1);
        }
        assert_eq!(assert_equivalent(&journeys[8..9], &data).len(), 1);
        for i in [0, 1, 2, 3, 4, 5, 7, 9, 10] {
            let ev = journey_evidence(&ServiceIndex::new(&data), &journeys[i], "AMF", "HT");
            assert_eq!((ev.taken.unwrap().arr_actual() - ev.planned.unwrap().arr_planned()).num_minutes(), delays[i]);
        }
        let ev = journey_evidence(&ServiceIndex::new(&data), &journeys[7], "AMF", "HT");
        assert!(ev.regular_arrival.is_some());
        assert_ne!(ev.regular_arrival, ev.planned.as_ref().map(|p| p.arr_planned()));
    }

    #[test]
    fn monthly_preserves_midnight_and_baseline_carry() {
        let early = trip("2026-09-11T00:00:00+02:00", 0);
        let adjusted = trip("2026-10-02T00:00:00+02:00", 0);
        let overnight = trip("2026-10-01T00:30:00+02:00", 0);
        let mut regular = train(&trip("2026-09-25T00:00:00+02:00", 0), "321", 25, 0, false);
        // September's latest available selected date is a baseline day, not a travel day.
        regular[0].service_date = NaiveDate::from_ymd_opt(2026, 9, 24).unwrap();
        regular[1].service_date = regular[0].service_date;
        let mut midnight = train(&overnight, "123", 60, 0, false);
        for stop in &mut midnight {
            stop.service_date = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        }
        let mut data = vec![train(&early, "123", 60, 0, false), regular, midnight, train(&adjusted, "700123", 60, 0, false)];
        data.sort_by_key(|s| s[0].service_date);
        let requests = assert_equivalent(&[early.clone(), adjusted.clone()], &data);
        assert!(requests.iter().any(|(month, dates)| month == "2026-09" && dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 24).unwrap())));
        assert_equivalent(&[early, overnight, adjusted], &data);
    }

    #[test]
    fn regular_baseline_keeps_local_times_across_both_dst_changes() {
        for (travel, baseline) in
            [("2026-10-18T08:00:00+02:00", "2026-10-25T08:00:00+01:00"), ("2026-03-22T08:00:00+01:00", "2026-03-29T08:00:00+02:00")]
        {
            let adjusted = trip(travel, 0);
            let regular = trip(baseline, 0);
            let data = vec![train(&adjusted, "700123", 60, 0, false), train(&regular, "123", 25, 0, false)];
            let evidence = journey_evidence(&ServiceIndex::new(&data), &adjusted, "AMF", "HT");
            assert_eq!(evidence.regular_arrival, Some(adjusted.check_in + Duration::minutes(30)));
            assert!(wanted_dates(&adjusted).contains(&regular.check_in.with_timezone(&Amsterdam).date_naive()));
        }
        // A midnight baseline retains its own local service date after the autumn change.
        let adjusted = trip("2026-10-18T00:30:00+02:00", 0);
        let baseline = shift_local(adjusted.check_in, 14).unwrap();
        assert_eq!(baseline.with_timezone(&Amsterdam).date_naive().to_string(), "2026-11-01");
        assert!(wanted_dates(&adjusted).contains(&NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()));
        assert!(shift_local(trip("2026-03-22T02:30:00+01:00", 0).check_in, 7).is_none());
        assert!(shift_local(trip("2026-10-18T02:30:00+02:00", 0).check_in, 7).is_none());
    }

    #[test]
    fn monthly_fetches_local_midnight_baseline_even_without_other_trips() {
        let adjusted = trip("2026-10-18T00:30:00+02:00", 0);
        let other = trip("2026-10-16T08:00:00+02:00", 0);
        let shifted = trip(&shift_local(adjusted.check_in, 14).unwrap().to_rfc3339(), 0);
        assert_eq!(shifted.check_in.with_timezone(&Amsterdam).date_naive().to_string(), "2026-11-01");
        let mut regular = train(&shifted, "123", 25, 0, false);
        // After-midnight service from October 31 must be retained for the local
        // November 1 baseline, even without another trip requesting that date.
        for stop in &mut regular {
            stop.service_date = NaiveDate::from_ymd_opt(2026, 10, 31).unwrap();
        }
        let mut data = vec![train(&adjusted, "700123", 60, 0, false), regular];
        data.sort_by_key(|s| s[0].service_date);
        assert_equivalent(&[adjusted.clone(), other], &data);
        let requests = assert_equivalent(std::slice::from_ref(&adjusted), &data);
        assert!(requests[0].1.contains(&NaiveDate::from_ymd_opt(2026, 10, 31).unwrap()));
        let evidence = journey_evidence(&ServiceIndex::new(&data), &adjusted, "AMF", "HT");
        assert_eq!(evidence.regular_arrival, Some(adjusted.check_in + Duration::minutes(30)));
    }
}
