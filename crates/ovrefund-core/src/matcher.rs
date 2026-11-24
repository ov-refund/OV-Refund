//! Route matching against the historical archive.
//!
//! * **Planned route (P)** — what the timetable valid on the travel date offered
//!   a traveller who checked in at `check_in`: earliest *planned* arrival at the
//!   destination among direct or 1-transfer routes whose first train had a planned
//!   departure no earlier than `check_in - CHECKIN_GRACE` and had not actually left
//!   before the check-in (or was cancelled). Cancelled trains stay eligible for P:
//!   a cancellation is exactly what causes delay.
//! * **Taken route (T)** — the non-cancelled route whose *actual* times are
//!   consistent with check-in and check-out: departs after check-in, arrives
//!   before check-out. Prefer the planned route when it fits, otherwise the earliest
//!   fitting arrival. A different route needs a matching cancellation or broken connection
//!   to qualify for the normal claim path; other ambiguous matches require review.
//!
//! Arrival delay = actual arrival of T − planned arrival of P. This mirrors NS's
//! rule (arrival at destination vs. the timetable) and is evidence-based, unlike
//! the check-out proxy.
use crate::model::{ArchiveStop, Leg, Route};
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, HashSet};

/// Travellers often check in just after the planned departure of a train that is running late.
pub const CHECKIN_GRACE_MIN: i64 = 5;
/// Maximum wait between check-in and the planned departure we consider.
pub const MAX_WAIT_MIN: i64 = 90;
/// Planned minimum transfer time.
pub const MIN_TRANSFER_MIN: i64 = 2;
/// Maximum transfer wait considered for a connection.
pub const MAX_TRANSFER_MIN: i64 = 75;
/// Taken-route arrival may be at most this much before check-out to be "consistent".
pub const MAX_CHECKOUT_LAG_MIN: i64 = 20;

pub type Service = Vec<ArchiveStop>;

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub planned: Option<Route>,
    pub taken: Option<Route>,
    pub notes: Vec<String>,
}

impl MatchResult {
    pub fn delay_minutes(&self) -> Option<i64> {
        let (p, t) = (self.planned.as_ref()?, self.taken.as_ref()?);
        Some((t.arr_actual() - p.arr_planned()).num_seconds().div_euclid(60))
    }
}

fn leg(svc: &Service, i: usize, j: usize) -> Option<Leg> {
    let (a, b) = (&svc[i], &svc[j]);
    Some(Leg {
        train_number: a.train_number.clone(),
        train_type: a.train_type.clone(),
        company: a.company.clone(),
        from_code: a.station_code.clone(),
        to_code: b.station_code.clone(),
        dep_planned: a.dep_planned?,
        dep_actual: a.dep_actual()?,
        arr_planned: b.arr_planned?,
        arr_actual: b.arr_actual()?,
        cancelled: a.dep_cancelled || b.arr_cancelled || svc[i..=j].iter().any(|s| s.dep_cancelled && s.arr_cancelled),
    })
}

/// All legs from station `from` to station `to` in a service (first matching pair).
fn legs_between(svc: &Service, from: &str, to: &str) -> Option<Leg> {
    let i = svc.iter().position(|s| s.station_code == from)?;
    let j = svc.iter().skip(i + 1).position(|s| s.station_code == to)? + i + 1;
    leg(svc, i, j)
}

/// Enumerate direct and 1-transfer routes from `a` to `b`.
pub fn enumerate_routes(services: &[Service], a: &str, b: &str, window_start: DateTime<Utc>, window_end: DateTime<Utc>) -> Vec<Route> {
    let services: Vec<_> = services.iter().collect();
    enumerate_routes_refs(&services, a, b, window_start, window_end)
}

fn enumerate_routes_refs(services: &[&Service], a: &str, b: &str, window_start: DateTime<Utc>, window_end: DateTime<Utc>) -> Vec<Route> {
    let mut routes = Vec::new();
    let in_window = |l: &Leg| l.dep_planned >= window_start && l.dep_planned <= window_end;
    // direct
    for s in services {
        if let Some(l) = legs_between(s, a, b) {
            if in_window(&l) {
                routes.push(Route { legs: vec![l] });
            }
        }
    }
    // Index second legs once, in service order. Preserve legs_between's first
    // matching pair even for services visiting a station more than once.
    let mut second: HashMap<&str, Vec<(&Service, Leg)>> = HashMap::new();
    for &s in services {
        if !s.iter().any(|stop| stop.station_code == b) {
            continue;
        }
        let mut seen = HashSet::new();
        for stop in s {
            let x = stop.station_code.as_str();
            if seen.insert(x) {
                if let Some(l) = legs_between(s, x, b) {
                    second.entry(x).or_default().push((s, l));
                }
            }
        }
    }
    // one transfer at X: s1 a->X, s2 X->b
    for &s1 in services {
        let Some(ia) = s1.iter().position(|s| s.station_code == a) else { continue };
        if s1[ia].dep_planned.map(|d| d < window_start || d > window_end).unwrap_or(true) {
            continue;
        }
        // A faster connection can leave a train that also calls at the destination.
        // Only change before reaching B: travelling past it would invent a detour.
        let end = s1.iter().skip(ia + 1).position(|s| s.station_code == b).map(|i| ia + 1 + i).unwrap_or(s1.len());
        for k in ia + 1..end {
            let x = &s1[k].station_code;
            let Some(l1) = leg(s1, ia, k) else { continue };
            let Some(connections) = second.get(x.as_str()) else { continue };
            for (s2, l2) in connections {
                if std::ptr::eq(s1, *s2) {
                    continue;
                }
                let gap = l2.dep_planned - l1.arr_planned;
                if gap >= Duration::minutes(MIN_TRANSFER_MIN) && gap <= Duration::minutes(MAX_TRANSFER_MIN) {
                    routes.push(Route { legs: vec![l1.clone(), l2.clone()] });
                }
            }
        }
    }
    routes
}

fn actual_connections_ok(r: &Route) -> bool {
    r.legs.windows(2).all(|w| w[1].dep_actual >= w[0].arr_actual + Duration::minutes(1))
}

pub fn match_journey(services: &[Service], a: &str, b: &str, check_in: DateTime<Utc>, check_out: Option<DateTime<Utc>>) -> MatchResult {
    let services: Vec<_> = services.iter().collect();
    match_journey_refs(&services, a, b, check_in, check_out)
}

/// Match borrowed services without copying their stops.
pub fn match_journey_refs(
    services: &[&Service],
    a: &str,
    b: &str,
    check_in: DateTime<Utc>,
    check_out: Option<DateTime<Utc>>,
) -> MatchResult {
    let mut notes = Vec::new();
    let window_start = check_in - Duration::minutes(CHECKIN_GRACE_MIN);
    let window_end = check_in + Duration::minutes(MAX_WAIT_MIN);
    // Taken trains can depart a lot later than planned (delays): widen for T.
    let routes = enumerate_routes_refs(services, a, b, window_start, window_end + Duration::minutes(120));

    // Planned: first leg must not have *actually* left before check-in (unless cancelled).
    let planned = routes
        .iter()
        .filter(|r| r.dep_planned() <= window_end)
        .filter(|r| r.legs[0].cancelled || r.legs[0].dep_actual >= check_in - Duration::seconds(30))
        .min_by_key(|r| (r.arr_planned(), r.legs.len(), r.dep_planned()))
        .cloned();

    let taken = check_out.and_then(|co| {
        let fits = |r: &&Route| {
            !r.legs.iter().any(|l| l.cancelled)
                && r.legs[0].dep_actual >= check_in - Duration::seconds(30)
                && r.arr_actual() <= co + Duration::minutes(1)
                && actual_connections_ok(r)
        };
        if let Some(p) = planned.as_ref().filter(|p| fits(p)) {
            return Some(p.clone());
        }
        routes
            .iter()
            .filter(fits)
            .filter(|r| r.legs[0].dep_actual >= check_in - Duration::seconds(30))
            .filter(|r| r.arr_actual() <= co + Duration::minutes(1))
            .filter(|r| actual_connections_ok(r))
            .min_by_key(|r| (r.arr_actual(), r.legs.len()))
            .cloned()
    });

    if planned.is_none() {
        notes.push("no planned direct or 1-transfer route found in archive".into());
    }
    if let (Some(t), Some(co)) = (&taken, check_out) {
        let lag = (co - t.arr_actual()).num_minutes();
        if lag > MAX_CHECKOUT_LAG_MIN {
            notes.push(format!("check-out {lag} min after the matched arrival; train identification uncertain"));
        }
    } else if check_out.is_some() && planned.is_some() {
        notes.push("no realised route consistent with check-in/check-out".into());
    }
    MatchResult { planned, taken, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::scan_archive;
    use chrono::TimeZone;
    use std::collections::HashSet;

    pub fn sample_services() -> Vec<Service> {
        let st: HashSet<String> = ["AMF", "HT"].iter().map(|s| s.to_string()).collect();
        let mut v = vec![];
        scan_archive(include_str!("../tests/fixtures/archive_sample.csv").as_bytes(), false, &st, |s| {
            v.push(s.to_vec());
            Ok(())
        })
        .unwrap();
        v
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn indexed_routes_match_unindexed_order_connections_and_repeated_stations() {
        let template = &sample_services()[0];
        let mut services = Vec::new();
        for i in 0..32 {
            let mut s = template.clone();
            // Repeated stations, direct alternatives and both legs of transfers.
            let codes = match i % 4 {
                0 => ["A", "X", "X", "B"],
                1 => ["X", "Y", "B", "X"],
                2 => ["A", "Y", "X", "Y"],
                _ => ["Y", "B", "A", "B"],
            };
            s.truncate(4);
            while s.len() < 4 {
                s.push(template[0].clone());
            }
            for (k, stop) in s.iter_mut().enumerate() {
                stop.station_code = codes[k].into();
                stop.service_id = i;
                let t = utc("2026-10-28T08:00:00Z") + Duration::minutes(i * 5 + k as i64 * 10);
                stop.arr_planned = Some(t);
                stop.dep_planned = Some(t + Duration::minutes(2));
                stop.arr_cancelled = i % 7 == 0;
                stop.dep_cancelled = i % 7 == 0;
            }
            services.push(s);
        }
        for a in ["A", "X", "Y", "B", "missing"] {
            for b in ["A", "X", "Y", "B", "missing"] {
                let start = utc("2026-10-28T08:00:00Z");
                let end = start + Duration::hours(2);
                assert_eq!(
                    serde_json::to_value(enumerate_routes(&services, a, b, start, end)).unwrap(),
                    serde_json::to_value(unindexed_enumerate_routes(&services, a, b, start, end)).unwrap(),
                    "{a} -> {b}"
                );
            }
        }
    }

    fn unindexed_enumerate_routes(
        services: &[Service],
        a: &str,
        b: &str,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
    ) -> Vec<Route> {
        let mut routes = Vec::new();
        let in_window = |l: &Leg| l.dep_planned >= window_start && l.dep_planned <= window_end;
        // direct
        for s in services {
            if let Some(l) = legs_between(s, a, b) {
                if in_window(&l) {
                    routes.push(Route { legs: vec![l] });
                }
            }
        }
        // one transfer at X: s1 a→X, s2 X→b
        for s1 in services {
            let Some(ia) = s1.iter().position(|s| s.station_code == a) else { continue };
            if s1[ia].dep_planned.map(|d| d < window_start || d > window_end).unwrap_or(true) {
                continue;
            }
            let end = s1.iter().skip(ia + 1).position(|s| s.station_code == b).map(|i| ia + 1 + i).unwrap_or(s1.len());
            for k in ia + 1..end {
                let x = &s1[k].station_code;
                let Some(l1) = leg(s1, ia, k) else { continue };
                for s2 in services {
                    if std::ptr::eq(s1, s2) {
                        continue;
                    }
                    let Some(l2) = legs_between(s2, x, b) else { continue };
                    let gap = l2.dep_planned - l1.arr_planned;
                    if gap >= Duration::minutes(MIN_TRANSFER_MIN) && gap <= Duration::minutes(MAX_TRANSFER_MIN) {
                        routes.push(Route { legs: vec![l1.clone(), l2] });
                    }
                }
            }
        }
        routes
    }
    #[test]
    fn express_transfer_can_beat_a_direct_train() {
        let template = sample_services()[0][0].clone();
        let start = utc("2026-10-28T08:00:00Z");
        let service = |codes: &[&str], minutes: &[i64], number: &str| -> Service {
            codes
                .iter()
                .zip(minutes)
                .map(|(code, min)| {
                    let mut stop = template.clone();
                    stop.station_code = (*code).into();
                    stop.train_number = number.into();
                    stop.arr_planned = Some(start + Duration::minutes(*min));
                    stop.dep_planned = stop.arr_planned;
                    stop.arr_delay_min = Some(0);
                    stop.dep_delay_min = Some(0);
                    stop
                })
                .collect()
        };
        let services = vec![
            service(&["A", "X", "B"], &[5, 20, 90], "1"),
            service(&["X", "B"], &[25, 45], "2"),
            service(&["B", "Y", "B"], &[95, 100, 120], "3"),
        ];
        let routes = enumerate_routes(&services, "A", "B", start, start + Duration::hours(1));
        assert!(routes.iter().all(|r| r.legs.len() == 1 || r.legs[0].to_code != "B"));
        let matched = match_journey(&services, "A", "B", start, Some(start + Duration::minutes(46)));
        assert_eq!(matched.planned.as_ref().unwrap().legs.len(), 2);
        assert_eq!(matched.planned.as_ref().unwrap().arr_planned(), start + Duration::minutes(45));
        assert_eq!(matched.delay_minutes(), Some(0));
    }

    #[test]
    fn on_time_trip_in_fixture_archive() {
        // Synthetic traveller on IC 28569 (real archive data: +5 at 's-Hertogenbosch) on the DST-end day.
        let m = match_journey(&sample_services(), "AMF", "HT", utc("2026-10-28T19:31:00+01:00"), Some(utc("2026-10-28T21:43:00+01:00")));
        let p = m.planned.as_ref().unwrap();
        assert_eq!(p.legs[0].train_number, "28569");
        assert_eq!(p.dep_planned(), utc("2026-10-28T19:36:00+01:00"));
        assert_eq!(m.taken.as_ref().unwrap().legs[0].train_number, "28569");
        assert_eq!(m.delay_minutes(), Some(5));
    }

    #[test]
    fn planned_train_fits_before_later_arrival() {
        let mut planned = sample_services()[0].clone();
        planned.truncate(2);
        for (i, stop) in planned.iter_mut().enumerate() {
            stop.station_code = if i == 0 { "A" } else { "B" }.into();
            stop.dep_planned = Some(utc("2026-10-28T09:00:00Z"));
            stop.arr_planned = Some(utc("2026-10-28T10:00:00Z"));
            stop.dep_delay_min = Some(0);
            stop.arr_delay_min = Some(35);
            stop.dep_cancelled = false;
            stop.arr_cancelled = false;
        }
        let mut later = planned.clone();
        for stop in &mut later {
            stop.train_number = "later".into();
            stop.dep_planned = Some(utc("2026-10-28T10:01:00Z"));
            stop.arr_planned = Some(utc("2026-10-28T11:01:00Z"));
            stop.arr_delay_min = Some(0);
        }
        let m = match_journey(&[planned, later], "A", "B", utc("2026-10-28T08:58:00Z"), Some(utc("2026-10-28T11:02:00Z")));
        assert_eq!(m.delay_minutes(), Some(35));
        assert!(m.notes.iter().any(|n| n.contains("27 min") && n.contains("uncertain")));
    }

    #[test]
    fn delayed_trip_amersfoort_den_bosch() {
        // Synthetic traveller on IC 28571 (real archive data: +33 at 's-Hertogenbosch).
        let m = match_journey(&sample_services(), "AMF", "HT", utc("2026-10-28T20:01:00+01:00"), Some(utc("2026-10-28T22:41:00+01:00")));
        assert_eq!(m.planned.as_ref().unwrap().legs[0].train_number, "28571");
        assert_eq!(m.delay_minutes(), Some(33));
    }

    #[test]
    fn late_checkout_without_late_train_is_flagged() {
        // On-time train but traveller checked out 45 min after arrival.
        let m = match_journey(&sample_services(), "AMF", "HT", utc("2026-10-28T19:31:00+01:00"), Some(utc("2026-10-28T22:25:00+01:00")));
        assert!(m.notes.iter().any(|n| n.contains("uncertain")) || m.delay_minutes().unwrap() >= 30);
    }

    #[test]
    fn checkin_after_train_actually_left_picks_next() {
        // 28569 left AMF 19:36; checking in 19:39 means the next train (28571, 20:06) is planned.
        let m = match_journey(&sample_services(), "AMF", "HT", utc("2026-10-28T19:39:00+01:00"), Some(utc("2026-10-28T22:15:00+01:00")));
        assert_eq!(m.planned.as_ref().unwrap().legs[0].train_number, "28571");
    }

    #[test]
    fn no_route_for_unknown_station() {
        let m = match_journey(&sample_services(), "AMF", "XXX", Utc.with_ymd_and_hms(2026, 10, 28, 19, 0, 0).unwrap(), None);
        assert!(m.planned.is_none());
    }
}
