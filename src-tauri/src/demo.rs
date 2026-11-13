//! **DEMO MODE** (`ov-refund --demo`): a safe sandbox to try the whole app.
//! - separate, throw-away data folder (%TEMP%\OVRefund-demo, wiped on every demo start);
//! - separate Credential Manager namespace (`OVRefund-demo`);
//! - example journeys + matching example timetable data (all invented);
//! - a simulated NS: claims are "accepted" with a DEMO reference, never sent anywhere;
//! - no Task Scheduler changes, no e-mail, no network access to NS.
//!
//! Developer-only (`--features devtools`); the UI shows a permanent DEMO banner.
use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use ovrefund_core::db::Db;
use ovrefund_core::matcher::Service;
use ovrefund_core::model::*;
use ovrefund_core::pipeline::{ClaimGateway, RemoteClaim, SubmitOutcome};
use ovrefund_core::schedule::{ArchiveScheduleProvider, ScheduleProvider};
use std::sync::atomic::{AtomicBool, Ordering};

static DEMO: AtomicBool = AtomicBool::new(false);

/// Optional second sandbox for automated UI tests (developer builds only): `OVREFUND_DEMO_INSTANCE=x`
/// gives a separate data folder and credential namespace, so tests never disturb the normal demo.
pub fn instance_suffix() -> &'static str {
    static S: once_cell::sync::Lazy<String> = once_cell::sync::Lazy::new(|| {
        std::env::var("OVREFUND_DEMO_INSTANCE")
            .ok()
            .filter(|s| !s.is_empty() && s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(|s| format!("-{s}"))
            .unwrap_or_default()
    });
    &S
}

pub fn enable() {
    DEMO.store(true, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("OVRefund-demo{}", instance_suffix()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("OVREFUND_DATA_DIR", &dir);
    // No e-mail address is preset: the demo user enters their own in the demo's e-mail step
    // (credentials live in the separate `OVRefund-demo` namespace).
}

pub fn is_demo() -> bool {
    DEMO.load(Ordering::SeqCst)
}

pub const DEMO_EMAIL: &str = "demo@example.com";
const CARD: &str = "3528000000001234";
const COUPLING: &str = "00000000-0000-4000-8000-00000000demo";

struct Trip {
    days_ago: i64,
    from: (&'static str, &'static str, &'static str), // (code, uic, name)
    to: (&'static str, &'static str, &'static str),
    dep: (u32, u32),
    minutes: i64,
    planned_delay: i64,        // arrival delay of the planned train
    cancelled: bool,           // planned train cancelled
    taken_offset: Option<i64>, // minutes later the taken train departed (if not the planned one)
    taken_delay: i64,
    fare: Option<i64>,
    note: &'static str,
    regular_minutes: Option<i64>, // adjusted timetable: regular journey duration
    missing_checkout: bool,
    other_carrier: bool,
    history: Option<ClaimState>,
}

const UT: (&str, &str, &str) = ("UT", "8400621", "Utrecht Centraal");
const ASD: (&str, &str, &str) = ("ASD", "8400058", "Amsterdam Centraal");
const RTD: (&str, &str, &str) = ("RTD", "8400530", "Rotterdam Centraal");
const EHV: (&str, &str, &str) = ("EHV", "8400206", "Eindhoven Centraal");
const GVC: (&str, &str, &str) = ("GVC", "8400282", "Den Haag Centraal");
const LEDN: (&str, &str, &str) = ("LEDN", "8400390", "Leiden Centraal");
const HT: (&str, &str, &str) = ("HT", "8400319", "'s-Hertogenbosch");
const AMF: (&str, &str, &str) = ("AMF", "8400055", "Amersfoort Centraal");

// Codes, UICs and names from the embedded stations-nl.csv (no external lookup).
const HLM: (&str, &str, &str) = ("HLM", "8400285", "Haarlem");
const GD: (&str, &str, &str) = ("GD", "8400258", "Gouda");
const ZL: (&str, &str, &str) = ("ZL", "8400747", "Zwolle");
const ZP: (&str, &str, &str) = ("ZP", "8400744", "Zutphen");
const AH: (&str, &str, &str) = ("AH", "8400071", "Arnhem Centraal");

fn trips() -> Vec<Trip> {
    vec![
        Trip {
            days_ago: 2,
            from: UT,
            to: ASD,
            dep: (8, 12),
            minutes: 27,
            planned_delay: 0,
            cancelled: false,
            taken_offset: None,
            taken_delay: 0,
            fare: Some(940),
            note: "on time",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 3,
            from: RTD,
            to: ASD,
            dep: (17, 5),
            minutes: 72,
            planned_delay: 42,
            cancelled: false,
            taken_offset: None,
            taken_delay: 42,
            fare: Some(1820),
            note: "30-59",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 9,
            from: ASD,
            to: EHV,
            dep: (18, 0),
            minutes: 80,
            planned_delay: 68,
            cancelled: false,
            taken_offset: None,
            taken_delay: 68,
            fare: Some(2290),
            note: "60+",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 10,
            from: GVC,
            to: UT,
            dep: (10, 45),
            minutes: 38,
            planned_delay: 31,
            cancelled: false,
            taken_offset: None,
            taken_delay: 31,
            fare: Some(1350),
            note: "borderline",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 12,
            from: LEDN,
            to: UT,
            dep: (16, 20),
            minutes: 47,
            planned_delay: 0,
            cancelled: true,
            taken_offset: Some(30),
            taken_delay: 6,
            fare: Some(1270),
            note: "cancelled",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 18,
            from: UT,
            to: HT,
            dep: (12, 3),
            minutes: 28,
            planned_delay: 0,
            cancelled: false,
            taken_offset: Some(45),
            taken_delay: 0,
            fare: Some(1080),
            note: "later train by choice",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 5,
            from: AMF,
            to: UT,
            dep: (9, 2),
            minutes: 15,
            planned_delay: 2,
            cancelled: false,
            taken_offset: None,
            taken_delay: 2,
            fare: Some(570),
            note: "small delay",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 99,
            from: EHV,
            to: UT,
            dep: (19, 10),
            minutes: 48,
            planned_delay: 46,
            cancelled: false,
            taken_offset: None,
            taken_delay: 46,
            fare: Some(1770),
            note: "expired",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 1,
            from: ASD,
            to: HLM,
            dep: (21, 35),
            minutes: 18,
            planned_delay: 0,
            cancelled: false,
            taken_offset: None,
            taken_delay: 0,
            fare: Some(510),
            note: "evening on time",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 4,
            from: GD,
            to: GVC,
            dep: (7, 52),
            minutes: 20,
            planned_delay: 7,
            cancelled: false,
            taken_offset: None,
            taken_delay: 7,
            fare: Some(630),
            note: "commute small delay",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 6,
            from: UT,
            to: AH,
            dep: (17, 23),
            minutes: 36,
            planned_delay: 15,
            cancelled: false,
            taken_offset: None,
            taken_delay: 15,
            fare: Some(1230),
            note: "commute fifteen minutes",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 14,
            from: HLM,
            to: ASD,
            dep: (8, 10),
            minutes: 18,
            planned_delay: 29,
            cancelled: false,
            taken_offset: None,
            taken_delay: 29,
            fare: Some(510),
            note: "just below threshold",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 20,
            from: AH,
            to: UT,
            dep: (7, 32),
            minutes: 36,
            planned_delay: 38,
            cancelled: false,
            taken_offset: None,
            taken_delay: 38,
            fare: Some(1230),
            note: "morning 30-59",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 25,
            from: UT,
            to: ZL,
            dep: (18, 18),
            minutes: 52,
            planned_delay: 52,
            cancelled: false,
            taken_offset: None,
            taken_delay: 52,
            fare: Some(1770),
            note: "evening 30-59",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 28,
            from: ZL,
            to: UT,
            dep: (20, 20),
            minutes: 52,
            planned_delay: 76,
            cancelled: false,
            taken_offset: None,
            taken_delay: 76,
            fare: Some(1770),
            note: "evening 60+",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 32,
            from: UT,
            to: GD,
            dep: (11, 18),
            minutes: 30,
            planned_delay: 0,
            cancelled: false,
            taken_offset: None,
            taken_delay: 0,
            fare: Some(790),
            note: "adjusted below regular threshold",
            regular_minutes: Some(20),
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 39,
            from: AH,
            to: UT,
            dep: (13, 32),
            minutes: 76,
            planned_delay: 0,
            cancelled: false,
            taken_offset: None,
            taken_delay: 0,
            fare: Some(1230),
            note: "adjusted review against regular timetable",
            regular_minutes: Some(36),
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 45,
            from: HT,
            to: EHV,
            dep: (8, 8),
            minutes: 20,
            planned_delay: 0,
            cancelled: false,
            taken_offset: None,
            taken_delay: 0,
            fare: Some(680),
            note: "missing checkout",
            regular_minutes: None,
            missing_checkout: true,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 49,
            from: ZL,
            to: ZP,
            dep: (14, 20),
            minutes: 48,
            planned_delay: 8,
            cancelled: false,
            taken_offset: None,
            taken_delay: 8,
            fare: Some(1740),
            note: "regional carrier",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: true,
            history: None,
        },
        Trip {
            days_ago: 55,
            from: ASD,
            to: UT,
            dep: (0, 24),
            minutes: 48,
            planned_delay: 5,
            cancelled: false,
            taken_offset: None,
            taken_delay: 5,
            fare: Some(940),
            note: "after midnight",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        Trip {
            days_ago: 60,
            from: UT,
            to: ASD,
            dep: (8, 12),
            minutes: 27,
            planned_delay: 43,
            cancelled: false,
            taken_offset: None,
            taken_delay: 43,
            fare: Some(940),
            note: "submitted history",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: Some(ClaimState::Submitted),
        },
        Trip {
            days_ago: 64,
            from: GVC,
            to: UT,
            dep: (17, 25),
            minutes: 38,
            planned_delay: 47,
            cancelled: false,
            taken_offset: None,
            taken_delay: 47,
            fare: Some(1350),
            note: "approved history",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: Some(ClaimState::Approved),
        },
        Trip {
            days_ago: 69,
            from: EHV,
            to: UT,
            dep: (18, 10),
            minutes: 48,
            planned_delay: 73,
            cancelled: false,
            taken_offset: None,
            taken_delay: 73,
            fare: Some(1770),
            note: "paid long delay history",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: Some(ClaimState::Paid),
        },
        Trip {
            days_ago: 73,
            from: ASD,
            to: UT,
            dep: (19, 10),
            minutes: 27,
            planned_delay: 39,
            cancelled: false,
            taken_offset: None,
            taken_delay: 39,
            fare: Some(940),
            note: "paid short delay history",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: Some(ClaimState::Paid),
        },
        Trip {
            days_ago: 77,
            from: UT,
            to: HT,
            dep: (7, 33),
            minutes: 28,
            planned_delay: 41,
            cancelled: false,
            taken_offset: None,
            taken_delay: 41,
            fare: Some(1080),
            note: "rejected history",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: Some(ClaimState::Rejected),
        },
        Trip {
            days_ago: 81,
            from: AH,
            to: UT,
            dep: (16, 32),
            minutes: 36,
            planned_delay: 44,
            cancelled: false,
            taken_offset: None,
            taken_delay: 44,
            fare: Some(1230),
            note: "uncertain history",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: Some(ClaimState::Indeterminate),
        },
        Trip {
            days_ago: 85,
            from: RTD,
            to: UT,
            dep: (18, 5),
            minutes: 37,
            planned_delay: 48,
            cancelled: false,
            taken_offset: None,
            taken_delay: 48,
            fare: Some(1270),
            note: "claimed elsewhere history",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: Some(ClaimState::ExternallyClaimed),
        },
        Trip {
            days_ago: 88,
            from: UT,
            to: LEDN,
            dep: (12, 10),
            minutes: 47,
            planned_delay: 35,
            cancelled: false,
            taken_offset: None,
            taken_delay: 35,
            fare: Some(1270),
            note: "not claiming history",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: Some(ClaimState::Dismissed),
        },
        Trip {
            days_ago: 98,
            from: UT,
            to: AH,
            dep: (9, 23),
            minutes: 36,
            planned_delay: 67,
            cancelled: false,
            taken_offset: None,
            taken_delay: 67,
            fare: Some(1230),
            note: "expired long delay",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
        // DEMO only: claimable, and its claim window ends within 14 days, so "Controleer nu" with
        // automatic submission on also submits (and can be stopped) in the demo.
        Trip {
            days_ago: 82,
            from: GD,
            to: UT,
            dep: (17, 48),
            minutes: 21,
            planned_delay: 44,
            cancelled: false,
            taken_offset: None,
            taken_delay: 44,
            fare: Some(860),
            note: "deadline within 14 days",
            regular_minutes: None,
            missing_checkout: false,
            other_carrier: false,
            history: None,
        },
    ]
}

fn local(date: NaiveDate, h: u32, m: u32) -> DateTime<Utc> {
    Amsterdam.from_local_datetime(&date.and_hms_opt(h, m, 0).unwrap()).earliest().unwrap().with_timezone(&Utc)
}

#[allow(clippy::too_many_arguments)]
fn stop(
    sid: i64,
    seq: i64,
    date: NaiveDate,
    num: &str,
    code: &str,
    arr: Option<(DateTime<Utc>, i64)>,
    dep: Option<(DateTime<Utc>, i64)>,
    cancelled: bool,
) -> ArchiveStop {
    ArchiveStop {
        service_id: sid,
        service_date: date,
        train_type: "Intercity".into(),
        company: "NS".into(),
        train_number: num.into(),
        station_code: code.into(),
        stop_seq: seq,
        arr_planned: arr.map(|a| a.0),
        arr_delay_min: arr.map(|a| a.1),
        arr_cancelled: cancelled,
        dep_planned: dep.map(|d| d.0),
        dep_delay_min: dep.map(|d| d.1),
        dep_cancelled: cancelled,
    }
}

/// The demo user's subscription (switchable in the DEMO banner).
pub fn subscription() -> String {
    crate::runner::open_db().ok().and_then(|db| db.get_setting("demo_subscription").ok().flatten()).unwrap_or_else(|| "none".into())
}

/// NS-style subscription code + monthly fee for a demo product.
fn sub_code(p: &str) -> Option<(&'static str, i64)> {
    Some(match p {
        "dal_voordeel" => ("DALKORT", 635),
        "weekend_vrij" => ("DALKORT,FLEXWVR", 4100),
        "dal_vrij" => ("FLEXDVR", 12795),
        "altijd_vrij" => ("FLEXAVR", 39995),
        _ => return None,
    })
}

/// Example journeys + the example timetable services that explain them.
pub fn data(now: DateTime<Utc>) -> (Vec<Journey>, Vec<Service>) {
    data_with_subscription(now, &subscription())
}

fn data_with_subscription(now: DateTime<Utc>, sub: &str) -> (Vec<Journey>, Vec<Service>) {
    let mut js = vec![];
    let mut services = vec![];
    for (k, t) in trips().into_iter().enumerate() {
        let mut date = (now - Duration::days(t.days_ago)).with_timezone(&Amsterdam).date_naive();
        if k == 2 || k == 3 {
            // two delayed trips on a Saturday, so Weekend Vrij's free-trip rule can be demonstrated
            use chrono::Datelike;
            while date.weekday() != chrono::Weekday::Sat {
                date = date.pred_opt().unwrap();
            }
        }
        let dep = local(date, t.dep.0, t.dep.1);
        let arr = dep + Duration::minutes(t.minutes);
        let base = 10_000 + k as i64 * 10;
        // Adjusted trains use the archive's 70xxxx convention. A service one week
        // earlier supplies the regular baseline through the real archive matcher.
        let number = if t.regular_minutes.is_some() { 700000 + k * 2 } else { 3000 + k * 2 };
        if let Some(minutes) = t.regular_minutes {
            let d = dep - Duration::days(7);
            let baseline_date = d.with_timezone(&Amsterdam).date_naive();
            services.push(vec![
                stop(base + 2, 1, baseline_date, &format!("{}", 3000 + k * 2), t.from.0, None, Some((d, 0)), false),
                stop(
                    base + 2,
                    2,
                    baseline_date,
                    &format!("{}", 3000 + k * 2),
                    t.to.0,
                    Some((d + Duration::minutes(minutes), 0)),
                    None,
                    false,
                ),
            ]);
        }
        // planned train
        services.push(vec![
            stop(base, 1, date, &number.to_string(), t.from.0, None, Some((dep, 0)), t.cancelled),
            stop(base, 2, date, &number.to_string(), t.to.0, Some((arr, t.planned_delay)), None, t.cancelled),
        ]);
        // later train (taken) if any
        let (taken_dep, taken_arr) = match t.taken_offset {
            Some(off) => {
                let d2 = dep + Duration::minutes(off);
                let a2 = d2 + Duration::minutes(t.minutes);
                services.push(vec![
                    stop(base + 1, 1, date, &format!("{}", 3001 + k * 2), t.from.0, None, Some((d2, 0)), false),
                    stop(base + 1, 2, date, &format!("{}", 3001 + k * 2), t.to.0, Some((a2, t.taken_delay)), None, false),
                ]);
                (d2, a2 + Duration::minutes(t.taken_delay))
            }
            None => (dep, arr + Duration::minutes(t.taken_delay)),
        };
        let _ = taken_dep;
        if t.other_carrier {
            for service in services.iter_mut().filter(|s| s[0].service_id == base) {
                for stop in service {
                    stop.company = "Arriva".into();
                    stop.train_type = "Stoptrein".into();
                }
            }
        }
        js.push(Journey {
            source_id: "demo".into(),
            source_record_id: format!("DEMO-{k:03}"),
            coupling_id: Some(COUPLING.into()),
            card_last4: Some(CARD[CARD.len() - 4..].into()),
            check_in: dep - Duration::minutes(6),
            check_out: (!t.missing_checkout).then_some(taken_arr + Duration::minutes(2)),
            origin: StationRef { uic: t.from.1.into(), name: t.from.2.into() },
            destination: Some(StationRef { uic: t.to.1.into(), name: t.to.2.into() }),
            carrier_code: Some(if t.other_carrier { "ARR" } else { NS_CARRIER_CODE }.into()),
            carrier_name: Some(if t.other_carrier { "Arriva" } else { "Nederlandse Spoorwegen" }.into()),
            product_name: Some("Voorbeeld-reisproduct".into()),
            travel_class: Some(2),
            fare_cents: demo_fare(sub, t.fare, dep),
            subscription: sub_code(sub).map(|c| c.0.to_string()),
            subscription_fee_cents: sub_code(sub).map(|c| c.1),
            invoiced: Some(true),
            payload_sha256: format!("demo-{k}-{}", t.note),
        });
    }
    (js, services)
}

/// What the trip costs under the demo subscription (None = free under a Vrij product).
fn demo_fare(sub: &str, fare: Option<i64>, dep: DateTime<Utc>) -> Option<i64> {
    use chrono::{Datelike, Timelike, Weekday};
    let l = dep.with_timezone(&Amsterdam);
    let weekend = matches!(l.weekday(), Weekday::Sat | Weekday::Sun);
    let off_peak = weekend || !((6..9).contains(&l.hour()) || (16..18).contains(&l.hour()));
    match sub {
        "weekend_vrij" if weekend => None,
        "weekend_vrij" | "dal_voordeel" if off_peak => fare.map(|f| f * 60 / 100),
        "dal_vrij" if off_peak => None,
        "altijd_vrij" => None,
        _ => fare,
    }
}

pub struct DemoSource;
impl ovrefund_core::pipeline::JourneySource for DemoSource {
    fn id(&self) -> &str {
        "mijnns"
    }
    fn fetch_journeys(
        &mut self,
        _from: DateTime<Utc>,
        _to: DateTime<Utc>,
    ) -> std::result::Result<Vec<Journey>, ovrefund_core::pipeline::FetchError> {
        Ok(data(Utc::now()).0)
    }
}

pub struct DemoSchedule(pub ArchiveScheduleProvider);
impl ScheduleProvider for DemoSchedule {
    fn prepare(&mut self, db: &mut Db, journeys: &[Journey]) -> Result<Vec<String>> {
        let services = data(Utc::now()).1;
        let refs: Vec<&Journey> = journeys.iter().collect();
        let mut months: Vec<String> =
            journeys.iter().map(|j| ovrefund_core::archive::month_key(j.check_in.with_timezone(&Amsterdam).date_naive())).collect();
        months.sort();
        months.dedup();
        for m in months {
            self.0.ingest(db, &m, &services, &refs)?;
        }
        Ok(vec!["demo timetable data".into()])
    }
    fn evidence(&self, db: &Db, j: &Journey) -> Result<ScheduleEvidence> {
        self.0.evidence(db, j)
    }
}

/// Seed fictional claim history once, after the first scan has assessed the trips.
/// Later scans preserve user changes and reconcile Submitted -> Approved -> Paid.
fn seed_history(db: &mut Db, schedule: &DemoSchedule, journeys: &[Journey], now: DateTime<Utc>) -> Result<bool> {
    if db.get_setting("demo_history_seeded")?.as_deref() == Some("1") {
        return Ok(false);
    }
    use ovrefund_core::pipeline::{assess_and_track, build_facts, RunOptions};
    let scenarios = trips();
    for j in journeys {
        let Some(t) = j.source_record_id.strip_prefix("DEMO-").and_then(|s| s.parse::<usize>().ok()).and_then(|k| scenarios.get(k)) else {
            continue;
        };
        let Some(target) = t.history else { continue };
        let (jid, _) = db.upsert_journey(j)?;
        if db.claim_for_journey(jid)?.map(|c| c.status != ClaimState::Ready).unwrap_or(false) {
            continue;
        }
        let a = assess_and_track(db, schedule, "demo", jid, j, &RunOptions { now, ..Default::default() })?;
        let Some(c) = db.claim_for_journey(jid)? else { continue };
        match target {
            ClaimState::ExternallyClaimed => {
                db.mark_external(c.id, "demo")?;
            }
            ClaimState::Dismissed => {
                db.set_status_by_user(c.id, target)?;
            }
            _ => {
                // These fixtures have definite delays; journal the same facts and submission
                // boundary as a normal claim. No production gateway or network is involved.
                let facts = build_facts(j, a.delay_band.unwrap(), a.scheduled_departure.unwrap()).unwrap();
                db.begin_submission(c.id, &facts, AuthorizedMode::Manual)?;
                if target == ClaimState::Indeterminate {
                    db.transition(c.id, Some(ClaimState::Submitting), target, "demo", "DEMO: acknowledgement uncertain")?;
                    continue;
                }
                if let SubmitOutcome::Confirmed(receipt) = DemoGateway.submit(&facts) {
                    db.complete_submission(c.id, &receipt)?;
                }
                if matches!(target, ClaimState::Approved | ClaimState::Paid) {
                    db.transition(c.id, Some(ClaimState::Submitted), ClaimState::Approved, "demo", "DEMO: approved")?;
                    db.set_paid_amount(c.id, ovrefund_core::compensation::expected(j, a.delay_band, None).map(|c| c.cents))?;
                    if target == ClaimState::Paid {
                        db.transition(c.id, Some(ClaimState::Approved), target, "demo", "DEMO: paid")?;
                    }
                } else if target == ClaimState::Rejected {
                    db.transition(c.id, Some(ClaimState::Submitted), target, "demo", "DEMO: rejected")?;
                    db.set_paid_amount(c.id, Some(0))?;
                }
            }
        }
    }
    db.set_setting("demo_history_seeded", "1")?;
    Ok(true)
}

/// Simulated NS: accepts every claim with a DEMO reference; a later check shows it as approved.
pub struct DemoGateway;
impl ClaimGateway for DemoGateway {
    fn id(&self) -> &str {
        "demo"
    }
    fn verified_for_auto(&self) -> bool {
        true
    }
    fn preflight(&mut self, f: &ClaimFacts) -> Result<Vec<String>> {
        Ok(vec![
            "DEMO: er wordt geen NS-website geopend / DEMO: no NS website is opened".into(),
            format!(
                "Inchecken en uitchecken: {ci} – {co} / Check-in and check-out: {ci} – {co}",
                ci = crate::fmt_local(f.check_in),
                co = crate::fmt_local(f.check_out)
            ),
            format!("Geplande vertrektijd {t} / Scheduled departure {t}", t = f.scheduled_departure_local),
            format!("Vertraging: {} / Delay: {}", f.delay_band.label_nl(), f.delay_band.label_en()),
        ])
    }
    fn submit(&mut self, f: &ClaimFacts) -> SubmitOutcome {
        SubmitOutcome::Confirmed(SubmissionReceipt {
            reference: Some(format!(
                "DEMO-{}",
                f.source_record_id.chars().skip(f.source_record_id.chars().count().saturating_sub(3)).collect::<String>()
            )),
            confirmed: true,
            detail: "DEMO: gesimuleerde bevestiging van NS (er is niets verzonden) / DEMO: simulated NS acknowledgement (nothing was sent)"
                .into(),
        })
    }
    fn list_remote_claims(&mut self) -> Result<Option<Vec<RemoteClaim>>> {
        // Everything "submitted" in the demo is reported back as approved on the next check.
        demo_remote_claims(&mut crate::runner::open_db()?, Utc::now())
    }
}

fn demo_remote_claims(db: &mut Db, now: DateTime<Utc>) -> Result<Option<Vec<RemoteClaim>>> {
    // Called after timetable preparation and assessment, just like a normal NS status check.
    // Keep the initial history visible; subsequent checks advance it as usual.
    let journeys: Vec<_> = db.journeys_since(now - Duration::days(100))?.into_iter().map(|(_, j)| j).collect();
    if seed_history(db, &DemoSchedule(ArchiveScheduleProvider::new()), &journeys, now)? {
        return Ok(Some(vec![]));
    }
    let mut out = vec![];
    for c in db.claims_in(&[ClaimState::Submitted, ClaimState::Approved])? {
        let j = db.journey(c.journey_id)?;
        let paid = c.status == ClaimState::Approved;
        let amount: Option<i64> =
            db.conn.query_row("SELECT compensation_cents FROM candidates WHERE journey_id=?1", [c.journey_id], |r| r.get(0)).ok().flatten();
        out.push(RemoteClaim {
            reference: c.ns_reference.clone(),
            travel_date: None,
            origin_uic: None,
            destination_uic: None,
            transaction_id: Some(j.source_record_id),
            state: Some(if paid { ClaimState::Paid } else { ClaimState::Approved }),
            raw_status: if paid { "Uitbetaald (demo)".into() } else { "Toegekend (demo)".into() },
            amount_cents: amount,
        });
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovrefund_core::eligibility::{assess, ClaimWindow, PolicyInput};
    use ovrefund_core::pipeline::{assess_and_track, reconcile, RunOptions};

    fn prepared(now: DateTime<Utc>, sub: &str) -> (Db, DemoSchedule, Vec<Journey>) {
        let (js, services) = data_with_subscription(now, sub);
        let db = Db::open_in_memory().unwrap();
        let sched = DemoSchedule(ArchiveScheduleProvider::new());
        let refs: Vec<_> = js.iter().collect();
        for j in &js {
            db.upsert_journey(j).unwrap();
            let month = ovrefund_core::archive::month_key(j.check_in.with_timezone(&Amsterdam).date_naive());
            sched.0.ingest(&db, &month, &services, &refs).unwrap();
        }
        (db, sched, js)
    }

    #[test]
    fn demo_receipts_accept_unicode_record_ids() {
        let now = "2026-11-13T12:00:00Z".parse().unwrap();
        let (_, _, js) = prepared(now, "none");
        let mut facts = ovrefund_core::pipeline::build_facts(&js[0], DelayBand::Min30To59, js[0].check_in).unwrap();
        for id in ["", "a", "train-\u{00e9}\u{4e2d}\u{1f686}"] {
            facts.source_record_id = id.into();
            let SubmitOutcome::Confirmed(receipt) = DemoGateway.submit(&facts) else { panic!("demo receipt missing") };
            assert!(receipt.reference.unwrap().starts_with("DEMO-"));
        }
    }

    #[test]
    fn demo_scenarios_classify_as_intended() {
        // Exercise calendar boundaries and both Amsterdam daylight-saving transitions.
        for date in ["2026-11-13T12:00:00Z", "2026-04-02T12:00:00Z", "2027-01-03T12:00:00Z"] {
            let now = date.parse().unwrap();
            let (db, sched, js) = prepared(now, "none");
            use Confidence::*;
            let expected = [
                NotClaimableOrOutOfScope,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                NeedsReview,
                NotClaimableOrOutOfScope,
                NotClaimableOrOutOfScope,
                NotClaimableOrOutOfScope,
                NotClaimableOrOutOfScope,
                NotClaimableOrOutOfScope,
                NeedsReview,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                NotClaimableOrOutOfScope,
                NeedsReview,
                NotClaimableOrOutOfScope,
                NotClaimableOrOutOfScope,
                NotClaimableOrOutOfScope,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                SupportedForAuto,
                NotClaimableOrOutOfScope,
                SupportedForAuto,
            ];
            for (j, expected) in js.iter().zip(expected) {
                let ev = sched.evidence(&db, j).unwrap();
                let a = assess(&PolicyInput { journey: j, evidence: &ev, now, window: ClaimWindow::default(), already_claimed: false });
                assert_eq!(a.confidence, expected, "{date}: {}", j.payload_sha256);
            }
            for k in [15, 16] {
                let ev = sched.evidence(&db, &js[k]).unwrap();
                assert!(ev.planned.unwrap().diverted());
                assert!(ev.regular_arrival.is_some());
            }
        }
    }

    #[test]
    fn a_cancelled_demo_trip_moves_from_existing_review_to_the_normal_queue() {
        use ovrefund_core::fixtures::FixtureClaimGateway;
        use ovrefund_core::pipeline::{auto_queue, submit_claim_at};
        let now = "2026-11-13T12:00:00Z".parse().unwrap();
        let (mut db, sched, js) = prepared(now, "none");
        let j = &js[4]; // Leiden -> Utrecht: cancelled planned train, matched replacement, +36.
        let (jid, _) = db.upsert_journey(j).unwrap();
        let mut previous = assess(&PolicyInput {
            journey: j,
            evidence: &sched.evidence(&db, j).unwrap(),
            now,
            window: ClaimWindow::default(),
            already_claimed: false,
        });
        previous.confidence = Confidence::NeedsReview;
        db.save_assessment(jid, &previous).unwrap();
        let c = db.ensure_claim(jid, "fixture", ClaimState::NeedsReview).unwrap();
        let a = assess_and_track(&mut db, &sched, "fixture", jid, j, &RunOptions { now, ..Default::default() }).unwrap();
        assert_eq!(a.estimated_arrival_delay_min, Some(36));
        assert_eq!(db.claim(c.id).unwrap().status, ClaimState::Ready);
        assert!(auto_queue(&db, now).unwrap().iter().any(|(_, queued)| queued.id == c.id));
        let mut gateway = FixtureClaimGateway { verified: true, ..Default::default() };
        let submitted = submit_claim_at(&mut db, &mut gateway, c.id, AuthorizedMode::Automatic, None, now).unwrap();
        assert_eq!(submitted.status, ClaimState::Submitted);
        assert_eq!(gateway.submitted.len(), 1);
    }

    #[test]
    fn seeded_demo_populates_tabs_received_money_and_preserves_history() {
        for date in ["2026-11-13T12:00:00Z", "2026-04-02T12:00:00Z", "2027-01-03T12:00:00Z"] {
            let now = date.parse().unwrap();
            for sub in ["none", "dal_voordeel", "weekend_vrij", "dal_vrij", "altijd_vrij"] {
                let (mut db, sched, js) = prepared(now, sub);
                assert!((24..=30).contains(&js.len()));
                for j in &js {
                    let (id, _) = db.upsert_journey(j).unwrap();
                    assess_and_track(&mut db, &sched, "demo", id, j, &RunOptions { now, ..Default::default() }).unwrap();
                }
                assert!(demo_remote_claims(&mut db, now).unwrap().unwrap().is_empty());
                let rows = db.journey_rows(100).unwrap();
                // Match the UI tabs: open READY / review, blocking states, and expired delayed rows.
                assert!(rows.iter().any(|r| r.claim_status.as_deref() == Some("READY")), "{sub}: Te claimen");
                assert!(rows.iter().any(|r| r.claim_status.as_deref() == Some("NEEDS_REVIEW")), "{sub}: Beoordelen");
                assert!(rows.iter().any(|r| r.claim_status.as_deref() == Some("SUBMITTED")), "{sub}: Geclaimd");
                assert!(
                    rows.iter()
                        .any(|r| r.expires_at.as_ref().unwrap().parse::<DateTime<Utc>>().unwrap() < now && r.delay_min.unwrap_or(0) >= 30),
                    "{sub}: Verlopen"
                );
                let totals = db.totals().unwrap();
                assert!(totals["paid"]["cents"].as_i64().unwrap() > 0, "{sub}: Ontvangen");
                for days in [7, 30, 365] {
                    assert!(rows.iter().any(|r| r.journey.check_in >= now - Duration::days(days)));
                }
                for state in [
                    ClaimState::Submitted,
                    ClaimState::Approved,
                    ClaimState::Paid,
                    ClaimState::Rejected,
                    ClaimState::Indeterminate,
                    ClaimState::ExternallyClaimed,
                    ClaimState::Dismissed,
                ] {
                    assert!(!db.claims_in(&[state]).unwrap().is_empty(), "{sub}: {state:?}");
                }
                for c in db.all_claims().unwrap() {
                    let events = db.events(c.id).unwrap();
                    for pair in events.windows(2) {
                        let from = ClaimState::parse(&pair[0].to_status).unwrap();
                        let to = ClaimState::parse(&pair[1].to_status).unwrap();
                        assert!(from.can_transition_to(to));
                    }
                }
                let ready = db.claims_in(&[ClaimState::Ready]).unwrap()[0].id;
                db.set_status_by_user(ready, ClaimState::Dismissed).unwrap();
                assert!(!seed_history(&mut db, &sched, &js, now).unwrap());
                assert_eq!(db.claim(ready).unwrap().status, ClaimState::Dismissed);
                // The same automatic-check path advances historical claims and records money.
                for _ in 0..2 {
                    let remote = demo_remote_claims(&mut db, now).unwrap().unwrap();
                    reconcile(&mut db, &remote).unwrap();
                }
                assert!(db.claims_in(&[ClaimState::Submitted, ClaimState::Approved]).unwrap().is_empty());
                assert_eq!(db.claims_in(&[ClaimState::Indeterminate]).unwrap().len(), 1);
                assert!(db.totals().unwrap()["paid"]["cents"].as_i64().unwrap() > totals["paid"]["cents"].as_i64().unwrap());
            }
        }
    }

    #[test]
    fn demo_stations_and_special_journeys_are_consistent() {
        use chrono::Timelike;
        use ovrefund_core::stations::STATIONS;
        for t in trips() {
            for (code, uic, name) in [t.from, t.to] {
                let station = STATIONS.by_uic(uic).unwrap();
                assert_eq!(station.code, code);
                assert_eq!(station.name, name);
            }
        }
        let (_, _, js) = prepared("2026-11-13T12:00:00Z".parse().unwrap(), "none");
        assert_eq!(js.iter().filter(|j| j.check_out.is_none()).count(), 1);
        assert_eq!(js.iter().filter(|j| !j.is_ns_carrier()).count(), 1);
        assert!(js.iter().any(|j| j.check_in.with_timezone(&Amsterdam).hour() == 0));
    }

    #[test]
    fn demo_has_one_claimable_trip_due_within_fourteen_days() {
        use ovrefund_core::pipeline::auto_queue;
        for date in ["2026-11-13T12:00:00Z", "2026-04-02T12:00:00Z", "2027-01-03T12:00:00Z", "2026-03-01T12:00:00Z"] {
            let now: DateTime<Utc> = date.parse().unwrap();
            let (mut db, sched, js) = prepared(now, "none");
            for j in &js {
                let (id, _) = db.upsert_journey(j).unwrap();
                assess_and_track(&mut db, &sched, "demo", id, j, &RunOptions { now, ..Default::default() }).unwrap();
            }
            // As the first demo check does: the example claim history.
            assert!(seed_history(&mut db, &sched, &js, now).unwrap());
            let due: Vec<_> = auto_queue(&db, now).unwrap().into_iter().filter(|(exp, _)| *exp <= now + Duration::days(14)).collect();
            assert_eq!(due.len(), 1, "{date}");
            assert_eq!(db.journey(due[0].1.journey_id).unwrap().payload_sha256, format!("demo-{}-deadline within 14 days", js.len() - 1));
        }
    }
}

pub fn subscription_summary(product: &str) -> serde_json::Value {
    let current = sub_code(product).map(|(code, fee)| {
        serde_json::json!({
            "parts": ovrefund_core::compensation::describe_subscription(code), "fee_cents": fee, "class": 2
        })
    });
    serde_json::json!({"none": current.is_none(), "current": current, "previous": []})
}

#[cfg(test)]
mod subscription_summary_tests {
    #[test]
    fn selector_none_does_not_keep_a_previous_subscription() {
        assert!(!super::subscription_summary("altijd_vrij")["current"].is_null());
        let none = super::subscription_summary("none");
        assert!(none["current"].is_null());
        assert_eq!(none["none"], true);
        assert_eq!(none["previous"], serde_json::json!([]));
    }
}
