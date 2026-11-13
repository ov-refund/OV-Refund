//! Eligibility policy (see SECURITY.md). Produces a bounded, explainable assessment.
//! Only `SupportedForAuto` may ever be submitted without a human reviewing it.
use crate::model::{Assessment, Confidence, DelayBand, EvidenceStatus, Journey, ScheduleEvidence};
use chrono::{DateTime, Duration, Months, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;

/// Observed `mijnns-claim-api/settings.gtbvClaimPeriod` on 2026-11-12: min 24 h, max 3 months.
#[derive(Debug, Clone, Copy)]
pub struct ClaimWindow {
    pub min_age_hours: i64,
    pub max_age_months: u32,
}

impl Default for ClaimWindow {
    fn default() -> Self {
        Self { min_age_hours: 24, max_age_months: 3 }
    }
}

/// Margin (minutes) below the 30-minute screening threshold where a trip can still be
/// reviewed. Evidence-backed delays of at least 30 minutes use the normal claim path.
pub const BOUNDARY_MARGIN_MIN: i64 = 2;
/// Screening threshold for the check-out proxy.
pub const PROXY_SCREEN_MIN: i64 = 30;

/// NS's own page about delay refunds (also covers Intercity Direct over the high-speed line).
pub const NS_REFUND_PAGE: &str = "https://www.ns.nl/service-en-contact/geld-terug/geld-terug-bij-vertraging";
/// Reason (stable text, translated by the GUI) for a route over the high-speed line; followed by
/// "; see <NS_REFUND_PAGE>".
pub const HSL_REASON: &str = "high-speed line (different thresholds/form)";
/// Reason (stable text, translated by the GUI) for a route with a replacement bus.
/// Late check-out while the trains ran on time (0 min or earlier).
pub const LATE_CHECKOUT_ON_TIME_REASON: &str = "late check-out but the trains ran on time; not automatically claimable";
/// The historical timetable explains the replacement train used for the delay estimate.
pub const CANCELLED_REPLACEMENT_REASON: &str = "planned train was cancelled; replacement train matches check-in/check-out";
pub const MISSED_CONNECTION_REASON: &str = "connection was missed; replacement train matches check-in/check-out";
pub const REPLACEMENT_BUS_REASON: &str = "replacement bus instead of a train on this route — check the times yourself";
/// Reason (stable text, translated by the GUI) when the exact refund is below NS's EUR 2.30 minimum.
pub const BELOW_MINIMUM_REASON: &str = "refund below €2.30: NS does not pay this out";

pub struct PolicyInput<'a> {
    pub journey: &'a Journey,
    pub evidence: &'a ScheduleEvidence,
    pub now: DateTime<Utc>,
    pub window: ClaimWindow,
    /// Already claimed via this app or externally (blocks everything).
    pub already_claimed: bool,
}

pub fn expires_at(check_in: DateTime<Utc>, w: ClaimWindow) -> DateTime<Utc> {
    let local = check_in.with_timezone(&Amsterdam).date_naive();
    let end = local.checked_add_months(Months::new(w.max_age_months)).unwrap_or(local);
    Amsterdam
        .from_local_datetime(&end.and_hms_opt(23, 59, 59).unwrap())
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or(check_in + Duration::days(90))
}

pub fn assess(inp: &PolicyInput) -> Assessment {
    let mut a = assess_inner(inp);
    // Past the claim window nothing is claimable, whatever the evidence says.
    if inp.now > a.expires_at && a.confidence != Confidence::NotClaimableOrOutOfScope {
        a.confidence = Confidence::NotClaimableOrOutOfScope;
        a.reasons.insert(0, "claim window (3 months) has passed — missed claim".into());
    }
    a
}

fn assess_inner(inp: &PolicyInput) -> Assessment {
    let j = inp.journey;
    let ev = inp.evidence;
    let mut reasons: Vec<String> = Vec::new();
    let expires = expires_at(j.check_in, inp.window);
    let planned = ev.planned.as_ref();
    let taken = planned
        .filter(|p| {
            j.check_out.is_some_and(|co| {
                !p.legs.iter().any(|l| l.cancelled)
                    && p.legs[0].dep_actual >= j.check_in - Duration::seconds(30)
                    && p.arr_actual() <= co + Duration::minutes(1)
                    && p.legs.windows(2).all(|w| w[1].dep_actual >= w[0].arr_actual + Duration::minutes(1))
            })
        })
        .or(ev.taken.as_ref());

    let proxy = match (j.check_out, planned) {
        (Some(co), Some(p)) => Some((co - p.arr_planned()).num_minutes()),
        _ => None,
    };
    let delay = match (planned, taken) {
        (Some(p), Some(t)) => Some((t.arr_actual() - p.arr_planned()).num_seconds().div_euclid(60)),
        _ => None,
    };
    let mk = |confidence: Confidence, reasons: Vec<String>, band: Option<DelayBand>| Assessment {
        confidence,
        checkout_minus_scheduled_arrival_min: proxy,
        estimated_arrival_delay_min: delay,
        delay_band: band,
        scheduled_departure: planned.map(|p| p.dep_planned()),
        reasons,
        expires_at: expires,
    };
    let out = Confidence::NotClaimableOrOutOfScope;

    // Hard exclusions first.
    if inp.already_claimed {
        return mk(out, vec!["already claimed (this app or externally)".into()], None);
    }
    // Expired trips are still evaluated (to show missed claims) but never claimable.
    let expired = inp.now > expires;
    if !j.is_ns_carrier() {
        return mk(out, vec![format!("carrier is not NS ({})", j.carrier_name.as_deref().unwrap_or("unknown"))], None);
    }
    let Some(_co) = j.check_out else {
        return mk(out, vec!["no check-out recorded (use NS 'vergeten uit te checken' instead)".into()], None);
    };
    match &j.destination {
        Some(d) if d.uic == j.origin.uic => return mk(out, vec!["check-in and check-out at the same station".into()], None),
        None => return mk(out, vec!["no destination station".into()], None),
        _ => {}
    }

    // Evidence availability.
    match ev.status {
        _ if expired && planned.is_none() => return mk(out, vec!["outside the 3-month claim window".into()], None),
        EvidenceStatus::Pending => {
            return mk(
                Confidence::NeedsReview,
                vec!["historical timetable for this month not yet published; will re-check automatically".into()],
                None,
            )
        }
        EvidenceStatus::UnknownStation => return mk(Confidence::NeedsReview, vec!["station not in timetable data".into()], None),
        EvidenceStatus::NoRoute | EvidenceStatus::NotApplicable if planned.is_none() => {
            return mk(Confidence::NeedsReview, vec!["no matching scheduled route found; check manually".into()], None)
        }
        _ => {}
    }
    if !ev.historical {
        return mk(Confidence::NeedsReview, vec!["schedule is not the historical timetable for the travel date".into()], None);
    }
    reasons.extend(ev.notes.iter().cloned());

    // Adjusted timetable (planned works): NS has paid such trips even when the diverted train was on
    // time (observed in practice). The archive only knows the adjusted timetable -> let the user judge.
    // Compared against the regular timetable (same weekday, a week or two away) when known.
    let diverted = planned.map(|p| p.diverted()).unwrap_or(false);
    if diverted && delay.map(|d| d < 30).unwrap_or(true) {
        let arrived = taken.map(|t| t.arr_actual()).or(j.check_out);
        let vs_regular = match (ev.regular_arrival, arrived) {
            (Some(reg), Some(arr)) => Some((arr - reg).num_minutes()),
            _ => None,
        };
        match vs_regular {
            Some(m) if m < 30 - BOUNDARY_MARGIN_MIN => {
                // Not late against the regular timetable either: not a claim.
                reasons.insert(0, "adjusted timetable: also less than 30 minutes late against the normal timetable".into());
                return mk(out, reasons, None);
            }
            Some(m) => {
                let mut r = vec![format!(
                    "timetable was adjusted that day; against the regular timetable you arrived about {m} min later — NS may compensate this, check yourself"
                )];
                r.extend(reasons.iter().cloned());
                return mk(Confidence::NeedsReview, r, DelayBand::from_minutes(m));
            }
            None if proxy.map(|p| p >= 0).unwrap_or(false) => {
                let mut r = vec!["timetable was adjusted that day (diverted trains, planned works); NS may compensate against the regular timetable — check yourself".to_string()];
                r.extend(reasons.iter().cloned());
                return mk(Confidence::NeedsReview, r, delay.and_then(DelayBand::from_minutes));
            }
            None => {}
        }
    }

    // Fast exit for clearly on-time trips.
    if proxy.map(|p| p < PROXY_SCREEN_MIN - BOUNDARY_MARGIN_MIN).unwrap_or(false)
        && delay.map(|d| d < PROXY_SCREEN_MIN - BOUNDARY_MARGIN_MIN).unwrap_or(true)
    {
        reasons.insert(0, "arrived on time or with minor delay".into());
        return mk(out, reasons, None);
    }

    let Some(d) = delay else {
        reasons.insert(0, "check-out suggests a delay but the train actually taken could not be identified".into());
        return mk(Confidence::NeedsReview, reasons, proxy.and_then(DelayBand::from_minutes));
    };
    if d < PROXY_SCREEN_MIN {
        if d >= PROXY_SCREEN_MIN - BOUNDARY_MARGIN_MIN || proxy.unwrap_or(0) >= PROXY_SCREEN_MIN {
            // Review, not "not claimable": the late check-out may come from a disruption the
            // timetable data does not show.
            reasons.insert(
                0,
                if d <= 0 {
                    LATE_CHECKOUT_ON_TIME_REASON.to_string()
                } else {
                    format!("late check-out but trains were only {d} min late; not automatically claimable")
                },
            );
            return mk(Confidence::NeedsReview, reasons, None);
        }
        reasons.insert(0, format!("estimated arrival delay {d} min (< 30)"));
        return mk(out, reasons, None);
    }
    let band = DelayBand::from_minutes(d);
    let mut auto_ok = true;
    let (p, t) = (planned.unwrap(), taken.unwrap());
    if t.legs.iter().any(|l| l.cancelled) {
        auto_ok = false;
        reasons.push("matched arrival uses a cancelled train; no operated replacement confirmed".into());
    }
    if !p.all_ns() || !t.all_ns() {
        auto_ok = false;
        reasons.push("route includes a non-NS train".into());
    }
    if p.uses_hsl() || t.uses_hsl() {
        auto_ok = false;
        reasons.push(format!("{HSL_REASON}; see {NS_REFUND_PAGE}"));
    }
    // Replacement buses ("Stopbus i.p.v. trein", "Snelbus ...") in the archive: NS still pays
    // delay refunds, but the times are less reliable and planned works may apply.
    if p.legs.iter().chain(t.legs.iter()).any(|l| l.train_type.to_lowercase().contains("bus")) {
        auto_ok = false;
        reasons.push(REPLACEMENT_BUS_REASON.into());
    }
    if p.legs.iter().any(|l| l.cancelled) {
        // A cancellation supported by the archive explains a later replacement train. It
        // follows the normal claim path; NS assesses the request. Other evidence gates remain.
        reasons.push(CANCELLED_REPLACEMENT_REASON.into());
    }
    if ev.notes.iter().any(|n| n.contains("uncertain")) {
        auto_ok = false;
    }
    // The delay must be explained by the planned journey going wrong: the same route, a
    // cancellation, the planned train itself arriving 30+ min late, or a broken connection.
    // Otherwise the traveller may simply have chosen a later train (not a delay).
    let same_route = p.legs.len() == t.legs.len()
        && p.legs.iter().zip(&t.legs).all(|(a, b)| a.train_number == b.train_number && a.dep_planned == b.dep_planned);
    if j.check_out.is_some_and(|co| (co - t.arr_actual()).num_minutes() > crate::matcher::MAX_CHECKOUT_LAG_MIN) {
        auto_ok = false;
        reasons.push("check-out well after the earliest fitting arrival — train identification uncertain".into());
    }
    let planned_late = (p.arr_actual() - p.arr_planned()).num_minutes() >= 30;
    let cancelled = p.legs.iter().any(|l| l.cancelled);
    let broken = p.legs.windows(2).any(|w| w[1].dep_actual < w[0].arr_actual + chrono::Duration::minutes(1));
    if !same_route && !cancelled {
        if broken {
            reasons.push(MISSED_CONNECTION_REASON.into());
        } else {
            auto_ok = false;
            reasons.push("a different train was selected — train identification uncertain".into());
        }
    }
    if !(same_route || planned_late || cancelled || broken) {
        auto_ok = false;
        reasons.push(
            "a later train than planned was taken while the planned journey ran (nearly) on time — check whether there was a disruption"
                .into(),
        );
    }
    if inp.now - j.check_out.unwrap_or(j.check_in).max(j.check_in) < Duration::hours(inp.window.min_age_hours) {
        auto_ok = false;
        reasons.push("NS accepts claims only 24 h after travel".into());
    }
    if j.fare_cents.is_none() {
        reasons.push("compensation amount unknown (fare not in travel history)".into());
    }
    reasons.insert(0, format!("estimated arrival delay {d} min ({})", band.map(|b| b.label_nl()).unwrap_or("-")));
    if expired {
        reasons.insert(0, "claim window (3 months) has passed — missed claim".into());
        return mk(out, reasons, band);
    }
    let conf = if auto_ok { Confidence::SupportedForAuto } else { Confidence::NeedsReview };
    mk(conf, reasons, band)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn journey(ci: &str, co: Option<&str>, carrier: &str) -> Journey {
        Journey {
            source_id: "fixture".into(),
            source_record_id: "X1".into(),
            coupling_id: Some("c".into()),
            card_last4: Some("0000".into()),
            check_in: utc(ci),
            check_out: co.map(utc),
            origin: StationRef { uic: "8400080".into(), name: "Amersfoort".into() },
            destination: Some(StationRef { uic: "8400319".into(), name: "'s-Hertogenbosch".into() }),
            carrier_code: Some(carrier.into()),
            carrier_name: None,
            product_name: None,
            travel_class: Some(2),
            fare_cents: None,
            subscription: None,
            subscription_fee_cents: None,
            invoiced: None,
            payload_sha256: String::new(),
        }
    }

    fn leg(num: &str, company: &str, dp: &str, ap: &str, arr_delay: i64, cancelled: bool) -> Leg {
        Leg {
            train_number: num.into(),
            train_type: "Intercity".into(),
            company: company.into(),
            from_code: "AMF".into(),
            to_code: "HT".into(),
            dep_planned: utc(dp),
            dep_actual: utc(dp),
            arr_planned: utc(ap),
            arr_actual: utc(ap) + Duration::minutes(arr_delay),
            cancelled,
        }
    }

    fn ev(p: Leg, t: Leg) -> ScheduleEvidence {
        ScheduleEvidence {
            provider: "test".into(),
            historical: true,
            planned: Some(Route { legs: vec![p] }),
            taken: Some(Route { legs: vec![t] }),
            status: EvidenceStatus::Found,
            notes: vec![],
            regular_arrival: None,
            regular_checked: false,
        }
    }

    fn run(j: &Journey, e: &ScheduleEvidence, now: &str) -> Assessment {
        assess(&PolicyInput { journey: j, evidence: e, now: utc(now), window: ClaimWindow::default(), already_claimed: false })
    }

    const DP: &str = "2026-10-28T19:06:00Z";
    const AP: &str = "2026-10-28T21:05:00Z";
    const NOW: &str = "2026-11-12T10:00:00Z";

    #[test]
    fn planned_late_train_cannot_be_inflated_by_a_later_arrival() {
        let p = leg("1", "NS", "2026-10-28T09:00:00Z", "2026-10-28T10:00:00Z", 35, false);
        let t = leg("2", "NS", "2026-10-28T10:01:00Z", "2026-10-28T11:01:00Z", 0, false);
        let j = journey("2026-10-28T08:58:00Z", Some("2026-10-28T11:02:00Z"), "4");
        // Also protects pre-existing cached evidence which chose the later train.
        let a = run(&j, &ev(p, t), NOW);
        assert_eq!(a.estimated_arrival_delay_min, Some(35));
        assert_eq!(a.delay_band, Some(DelayBand::Min30To59));
        assert_eq!(a.confidence, Confidence::NeedsReview);
        assert!(a.reasons.iter().any(|r| r.contains("earliest fitting")));
    }

    #[test]
    fn minimum_age_is_measured_after_checkout() {
        let p = leg("1", "NS", DP, AP, 35, false);
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:42:00Z"), "4");
        let evidence = ev(p.clone(), p);
        let a = run(&j, &evidence, "2026-10-29T20:01:00Z");
        assert_eq!(a.confidence, Confidence::NeedsReview);
        assert!(a.reasons.iter().any(|r| r.contains("24 h")));
    }

    #[test]
    fn later_train_by_choice_is_not_auto() {
        // planned 19:06 arrives on time; traveller took a train 45 minutes later
        let p = leg("1", "NS", DP, AP, 0, false);
        let t = leg("2", "NS", "2026-10-28T19:51:00Z", "2026-10-28T21:50:00Z", 0, false);
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:52:00Z"), "4");
        let a = run(&j, &ev(p, t), NOW);
        assert_eq!(a.confidence, Confidence::NeedsReview, "{:?}", a.reasons);
    }

    #[test]
    fn choosing_a_later_connection_is_not_automatic_evidence() {
        let first = leg("1", "NS", "2026-10-28T19:06:00Z", "2026-10-28T19:25:00Z", 0, false);
        let planned = leg("2", "NS", "2026-10-28T19:30:00Z", AP, 0, false);
        let later = leg("3", "NS", "2026-10-28T20:10:00Z", "2026-10-28T21:45:00Z", 0, false);
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:47:00Z"), "4");
        let mut evidence = ev(planned.clone(), later.clone());
        evidence.planned = Some(Route { legs: vec![first.clone(), planned] });
        evidence.taken = Some(Route { legs: vec![first, later] });
        let a = run(&j, &evidence, NOW);
        assert_eq!(a.estimated_arrival_delay_min, Some(0));
        assert_eq!(a.confidence, Confidence::NeedsReview);
        assert!(a.reasons.iter().any(|r| r == LATE_CHECKOUT_ON_TIME_REASON), "{:?}", a.reasons);
    }

    #[test]
    fn adjusted_timetable_uses_the_regular_timetable() {
        // diverted train (70xxxx), on time against the adjusted timetable
        let p = leg("700123", "NS", DP, AP, 0, false);
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:08:00Z"), "4");
        // regular timetable would have arrived 5 min earlier -> not a claim
        let mut e = ev(p.clone(), p.clone());
        e.regular_arrival = Some(utc(AP) - Duration::minutes(5));
        e.regular_checked = true;
        assert_eq!(run(&j, &e, NOW).confidence, Confidence::NotClaimableOrOutOfScope);
        // regular timetable 32 min earlier -> worth checking (NS has paid such cases)
        e.regular_arrival = Some(utc(AP) - Duration::minutes(32));
        let a = run(&j, &e, NOW);
        assert_eq!(a.confidence, Confidence::NeedsReview);
        assert_eq!(a.delay_band, Some(DelayBand::Min30To59));
        // no baseline known -> still left to the user
        e.regular_arrival = None;
        assert_eq!(run(&j, &e, NOW).confidence, Confidence::NeedsReview);
    }

    #[test]
    fn regular_timetable_threshold_uses_the_shared_margin() {
        let p = leg("700123", "NS", DP, AP, 6, false);
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:13:00Z"), "4");
        for regular_delay in [26, 27, 28, 29, 30, 32] {
            let mut e = ev(p.clone(), p.clone());
            e.regular_checked = true;
            e.regular_arrival = Some(utc(AP) + Duration::minutes(6 - regular_delay));
            let a = run(&j, &e, NOW);
            if regular_delay < 30 - BOUNDARY_MARGIN_MIN {
                assert_eq!(a.confidence, Confidence::NotClaimableOrOutOfScope);
                assert_eq!(a.delay_band, None);
                assert!(a.reasons[0].starts_with("adjusted timetable: also less than 30"));
            } else {
                assert_eq!(a.confidence, Confidence::NeedsReview);
            }
        }
    }

    #[test]
    fn near_thirty_below_threshold_is_review() {
        for delay in [26, 27, 28, 29] {
            let p = leg("123", "NS", DP, AP, delay, false);
            let co = (utc(AP) + Duration::minutes(delay)).to_rfc3339();
            let j = journey("2026-10-28T19:01:00Z", Some(&co), "4");
            assert_eq!(
                run(&j, &ev(p.clone(), p), NOW).confidence,
                if delay >= 28 { Confidence::NeedsReview } else { Confidence::NotClaimableOrOutOfScope }
            );
        }
    }

    #[test]
    fn long_delay_is_supported() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T22:15:00Z"), "4");
        let a = run(&j, &ev(leg("1", "NS", DP, AP, 65, false), leg("1", "NS", DP, AP, 65, false)), NOW);
        assert_eq!(a.confidence, Confidence::SupportedForAuto, "{:?}", a.reasons);
        assert_eq!(a.delay_band, Some(DelayBand::Min60Plus));
        assert_eq!(a.scheduled_departure, Some(utc(DP)));
    }

    #[test]
    fn short_delay_band() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:45:00Z"), "4");
        let a = run(&j, &ev(leg("1", "NS", DP, AP, 38, false), leg("1", "NS", DP, AP, 38, false)), NOW);
        assert_eq!(a.confidence, Confidence::SupportedForAuto);
        assert_eq!(a.delay_band, Some(DelayBand::Min30To59));
    }

    #[test]
    fn on_time_is_not_claimable() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:07:00Z"), "4");
        let a = run(&j, &ev(leg("1", "NS", DP, AP, 0, false), leg("1", "NS", DP, AP, 0, false)), NOW);
        assert_eq!(a.confidence, Confidence::NotClaimableOrOutOfScope);
    }

    #[test]
    fn late_checkout_on_time_train_needs_review() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:50:00Z"), "4");
        let a = run(&j, &ev(leg("1", "NS", DP, AP, 2, false), leg("1", "NS", DP, AP, 2, false)), NOW);
        assert_eq!(a.confidence, Confidence::NeedsReview);
        assert_eq!(a.checkout_minus_scheduled_arrival_min, Some(45));
        assert!(a.reasons[0].contains("only 2 min late"), "{:?}", a.reasons);
        let a = run(&j, &ev(leg("1", "NS", DP, AP, 0, false), leg("1", "NS", DP, AP, 0, false)), NOW);
        assert_eq!(a.confidence, Confidence::NeedsReview);
        assert_eq!(a.reasons[0], LATE_CHECKOUT_ON_TIME_REASON, "never 'only 0 min late'");
    }

    #[test]
    fn evidence_backed_delays_at_and_above_thirty_use_the_normal_claim_path() {
        for delay in [30, 31, 32, 36, 58, 59, 60, 61, 62, 90] {
            let co = (utc(AP) + Duration::minutes(delay + 2)).to_rfc3339();
            let j = journey("2026-10-28T19:01:00Z", Some(&co), "4");
            let p = leg("1", "NS", DP, AP, delay, false);
            let a = run(&j, &ev(p.clone(), p), NOW);
            assert_eq!(a.confidence, Confidence::SupportedForAuto, "{delay}: {:?}", a.reasons);
            assert_eq!(a.delay_band, DelayBand::from_minutes(delay));
        }
    }

    #[test]
    fn cancelled_planned_train_uses_next_train_delay() {
        // planned train cancelled; next train an hour later arrives on time -> 60 min delay
        let p = leg("1", "NS", DP, AP, 0, true);
        let t = leg("2", "NS", "2026-10-28T20:06:00Z", "2026-10-28T22:05:00Z", 3, false);
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T22:10:00Z"), "4");
        let a = run(&j, &ev(p, t), NOW);
        assert_eq!(a.estimated_arrival_delay_min, Some(63));
        assert_eq!(a.confidence, Confidence::SupportedForAuto, "a matched cancellation explains the replacement train");
        assert_eq!(a.delay_band, Some(DelayBand::Min60Plus));
        assert!(a.reasons.iter().any(|r| r.contains("cancelled")));
        assert!(a.reasons.iter().any(|r| r == CANCELLED_REPLACEMENT_REASON), "{:?}", a.reasons);
        assert!(!a.reasons.iter().any(|r| r.contains("a different train was selected")), "{:?}", a.reasons);
    }

    #[test]
    fn matched_cancellation_is_claimable_but_missing_or_uncertain_replacement_is_not_automatic() {
        let p = leg("1", "NS", DP, AP, 0, true);
        let t = leg("2", "NS", "2026-10-28T19:36:00Z", "2026-10-28T21:35:00Z", 6, false);
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:43:00Z"), "4");
        let evidence = ev(p, t);
        let a = run(&j, &evidence, NOW);
        assert_eq!(a.estimated_arrival_delay_min, Some(36));
        assert_eq!(a.confidence, Confidence::SupportedForAuto);
        for variant in 0..4 {
            let mut weaker = evidence.clone();
            match variant {
                0 => weaker.taken = None,
                1 => weaker.notes.push("replacement train identification uncertain".into()),
                2 => weaker.historical = false,
                _ => weaker.taken.as_mut().unwrap().legs[0].cancelled = true,
            }
            assert_eq!(run(&j, &weaker, NOW).confidence, Confidence::NeedsReview, "variant {variant}");
        }
    }

    #[test]
    fn a_delayed_first_leg_explains_the_missed_connection_and_replacement() {
        let first = leg("1", "NS", "2026-10-28T19:06:00Z", "2026-10-28T19:25:00Z", 10, false);
        let onward = leg("2", "NS", "2026-10-28T19:30:00Z", AP, 0, false);
        let later = leg("3", "NS", "2026-10-28T20:10:00Z", "2026-10-28T21:45:00Z", 0, false);
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T21:47:00Z"), "4");
        let mut evidence = ev(onward.clone(), later.clone());
        evidence.planned = Some(Route { legs: vec![first.clone(), onward] });
        evidence.taken = Some(Route { legs: vec![first, later] });
        let a = run(&j, &evidence, NOW);
        assert_eq!(a.estimated_arrival_delay_min, Some(40));
        assert_eq!(a.confidence, Confidence::SupportedForAuto, "{:?}", a.reasons);
        assert!(a.reasons.iter().any(|r| r == MISSED_CONNECTION_REASON));
    }

    #[test]
    fn non_ns_carrier_out_of_scope() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T22:15:00Z"), "12");
        let a = run(&j, &ev(leg("1", "NS", DP, AP, 65, false), leg("1", "NS", DP, AP, 65, false)), NOW);
        assert_eq!(a.confidence, Confidence::NotClaimableOrOutOfScope);
    }

    #[test]
    fn replacement_bus_needs_review() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T22:15:00Z"), "4");
        let mut l = leg("1", "NS", DP, AP, 65, false);
        l.train_type = "Stopbus i.p.v. trein".into();
        let a = run(&j, &ev(l.clone(), l), NOW);
        assert_eq!(a.confidence, Confidence::NeedsReview);
        assert!(a.reasons.iter().any(|r| r.contains("replacement bus")));
    }

    #[test]
    fn non_ns_leg_needs_review() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T22:15:00Z"), "4");
        let a = run(&j, &ev(leg("1", "Arriva", DP, AP, 65, false), leg("1", "Arriva", DP, AP, 65, false)), NOW);
        assert_eq!(a.confidence, Confidence::NeedsReview);
    }

    #[test]
    fn no_checkout_out_of_scope() {
        let j = journey("2026-10-28T19:01:00Z", None, "4");
        let a = run(&j, &ev(leg("1", "NS", DP, AP, 65, false), leg("1", "NS", DP, AP, 65, false)), NOW);
        assert_eq!(a.confidence, Confidence::NotClaimableOrOutOfScope);
    }

    #[test]
    fn expired_window() {
        let j = journey("2026-07-01T19:01:00Z", Some("2026-07-01T22:15:00Z"), "4");
        let a = run(&j, &ev(leg("1", "NS", DP, AP, 65, false), leg("1", "NS", DP, AP, 65, false)), NOW);
        assert_eq!(a.confidence, Confidence::NotClaimableOrOutOfScope);
        assert_eq!(a.estimated_arrival_delay_min, Some(65), "missed claims keep their delay");
        assert!(a.reasons[0].contains("missed"));
    }

    #[test]
    fn expiry_boundary_is_end_of_local_day_three_months_later() {
        let e = expires_at(utc("2026-08-31T22:30:00Z"), ClaimWindow::default()); // 1 Sep local
        assert_eq!(e, utc("2026-12-01T22:59:59Z"));
    }

    #[test]
    fn pending_evidence_needs_review() {
        let j = journey("2026-11-05T19:01:00Z", Some("2026-11-05T22:15:00Z"), "4");
        let e = ScheduleEvidence {
            provider: "t".into(),
            historical: true,
            planned: None,
            taken: None,
            status: EvidenceStatus::Pending,
            notes: vec![],
            regular_arrival: None,
            regular_checked: false,
        };
        let a = run(&j, &e, NOW);
        assert_eq!(a.confidence, Confidence::NeedsReview);
    }

    #[test]
    fn non_historical_schedule_never_auto() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T22:15:00Z"), "4");
        let mut e = ev(leg("1", "NS", DP, AP, 65, false), leg("1", "NS", DP, AP, 65, false));
        e.historical = false;
        assert_eq!(run(&j, &e, NOW).confidence, Confidence::NeedsReview);
    }

    #[test]
    fn already_claimed_blocks() {
        let j = journey("2026-10-28T19:01:00Z", Some("2026-10-28T22:15:00Z"), "4");
        let e = ev(leg("1", "NS", DP, AP, 65, false), leg("1", "NS", DP, AP, 65, false));
        let a = assess(&PolicyInput { journey: &j, evidence: &e, now: utc(NOW), window: ClaimWindow::default(), already_claimed: true });
        assert_eq!(a.confidence, Confidence::NotClaimableOrOutOfScope);
    }
}
