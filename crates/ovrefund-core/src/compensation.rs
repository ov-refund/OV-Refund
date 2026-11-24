//! Expected compensation, following NS's "Vergoedingentabel geld terug bij vertraging"
//! (https://www.ns.nl/service-en-contact/geld-terug/vergoedingen-geld-terug-bij-vertraging):
//! * trip paid per ride (incl. subscription discount): 1/2 (30-59 min) or the whole fare (60+);
//! * trip free under a "Vrij" subscription: a fraction of that product's monthly price
//!   (Weekend Vrij 1/12 | 1/6, Dal Vrij 1/36 | 1/18, Altijd Vrij 1/42 | 1/21), using the price
//!   that was valid on the travel date (assets/compensation_rules.json, with sources);
//! * nothing below EUR 2.30.
//!
//! Order of evidence: exact fare → NS rule × the monthly fee NS lists on the trip itself →
//! NS rule × dated price list → what NS actually paid (gross) for a comparable trip → `None`
//! ("unknown"). Never invented.
//!
//! NS rounds to the nearest cent. The trip's monthly fee takes precedence over the public price list.
use crate::model::{DelayBand, Journey};
use chrono::{Duration, NaiveDate, Utc};
use chrono_tz::Europe::Amsterdam;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Compensation {
    pub cents: i64,
    /// true = exact per NS rule from the trip's own fare; false = estimate
    pub exact: bool,
    pub basis: String,
}

pub const MIN_PAYOUT_CENTS: i64 = 230;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VrijProduct {
    WeekendVrij,
    DalVrij,
    AltijdVrij,
}

impl VrijProduct {
    pub fn key(&self) -> &'static str {
        match self {
            Self::WeekendVrij => "weekend_vrij",
            Self::DalVrij => "dal_vrij",
            Self::AltijdVrij => "altijd_vrij",
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::WeekendVrij => "Weekend Vrij",
            Self::DalVrij => "Dal Vrij",
            Self::AltijdVrij => "Altijd Vrij",
        }
    }
}

/// Tokens of NS's subscription description ("DALKORT,FLEXWVR" -> ["DALKORT", "FLEXWVR"]):
/// comma-separated, trimmed, upper case, empty ones dropped.
pub fn subscription_tokens(sub: &str) -> Vec<String> {
    sub.split(',').map(|t| t.trim().to_uppercase()).filter(|t| !t.is_empty()).collect()
}

/// Recognise the free-travel product in NS's subscription description (e.g. "DALKORT,FLEXWVR"),
/// by exact token only (a substring such as "WVR" inside an unknown code never counts).
/// "DALKORT" is Dal Voordeel (a discount, trips are paid) and deliberately not matched.
pub fn vrij_product(sub: &str) -> Option<VrijProduct> {
    let tokens = subscription_tokens(sub);
    [("FLEXWVR", VrijProduct::WeekendVrij), ("FLEXDVR", VrijProduct::DalVrij), ("FLEXAVR", VrijProduct::AltijdVrij)]
        .into_iter()
        .find(|(code, _)| tokens.iter().any(|t| t == code))
        .map(|(_, p)| p)
}

/// One product in a subscription description, for "Jouw gegevens".
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubPart {
    /// NS's own code, e.g. "FLEXWVR".
    pub code: String,
    pub nl: String,
    pub en: String,
    /// "free" (trips cost nothing), "discount" (trips are paid at a discount) or "unknown".
    pub kind: &'static str,
    /// NS's refund fractions of the monthly price (divisor 30-59 min, divisor 60+ min), free products only.
    pub fractions: Option<[i64; 2]>,
    /// Code-to-product mapping for NS transaction product tokens.
    pub verified: bool,
}

/// Explain NS's subscription description token by token. Known codes only; anything else is
/// "unknown" (never guessed). Traject Vrij has no confirmed code yet, so it shows as unknown.
pub fn describe_subscription(desc: &str) -> Vec<SubPart> {
    subscription_tokens(desc)
        .into_iter()
        .map(|code| {
            let (nl, en, kind, product, verified): (&str, &str, &'static str, Option<VrijProduct>, bool) = match code.as_str() {
                // Product mapping used by the NS transaction parser.
                "DALKORT" => ("Dal Voordeel", "Off-Peak Discount (Dal Voordeel)", "discount", None, true),
                // Product mapping used by the NS transaction parser.
                "FLEXWVR" => ("Weekend Vrij", "Weekend Free (Weekend Vrij)", "free", Some(VrijProduct::WeekendVrij), true),
                // Same product naming scheme.
                "FLEXDVR" => ("Dal Vrij", "Off-Peak Free (Dal Vrij)", "free", Some(VrijProduct::DalVrij), false),
                "FLEXAVR" => ("Altijd Vrij", "Always Free (Altijd Vrij)", "free", Some(VrijProduct::AltijdVrij), false),
                _ => ("Onbekend abonnement", "Unknown subscription", "unknown", None, false),
            };
            let fractions = product.and_then(|p| Some([fraction(p, DelayBand::Min30To59)?, fraction(p, DelayBand::Min60Plus)?]));
            SubPart { code, nl: nl.into(), en: en.into(), kind, fractions, verified }
        })
        .collect()
}

#[derive(Deserialize)]
struct Rules {
    fractions: std::collections::HashMap<String, serde_json::Value>,
    monthly_prices: Vec<PriceRow>,
}

#[derive(Deserialize, Clone)]
struct PriceRow {
    product: String,
    class: u8,
    from: NaiveDate,
    cents: i64,
    source: String,
}

static RULES: Lazy<Rules> = Lazy::new(|| serde_json::from_str(include_str!("../assets/compensation_rules.json")).expect("rules JSON"));

/// (divisor 30-59, divisor 60+) for a product.
fn fraction(p: VrijProduct, band: DelayBand) -> Option<i64> {
    let v = RULES.fractions.get(p.key())?.as_array()?;
    let i = if band == DelayBand::Min30To59 { 0 } else { 1 };
    v.get(i)?.as_i64()
}

/// Monthly price valid on `date` (2nd class fallback when no 1st-class price is known; the
/// returned class says which one was used).
pub fn monthly_price(p: VrijProduct, class: u8, date: NaiveDate) -> Option<(i64, String, u8)> {
    let pick = |c: u8| {
        RULES.monthly_prices.iter().filter(|r| r.product == p.key() && r.class == c && r.from <= date).max_by_key(|r| r.from).cloned()
    };
    let r = pick(class).or_else(|| pick(2))?;
    Some((r.cents, r.source, r.class))
}

/// Easter Sunday (Gregorian calendar; anonymous Gregorian algorithm, Meeus/Jones/Butcher).
pub fn easter_sunday(year: i32) -> NaiveDate {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    NaiveDate::from_ymd_opt(year, month as u32, day as u32).expect("valid Easter date")
}

/// A Dutch public holiday on which NS's "weekend and public holidays" / off-peak rules apply all
/// day. This list is used to establish free travel under a Vrij subscription. NS sells
/// Weekend Vrij as free travel "in het weekend en op feestdagen" and counts "zaterdag, zondag en
/// feestdagen" as off-peak all day; NS publishes no list of those holidays with the products, so
/// the usual NS list is used: Nieuwjaarsdag, Eerste en Tweede Paasdag, Koningsdag (27 April, 26
/// April when the 27th is a Sunday), Hemelvaartsdag, Eerste en Tweede Pinksterdag, Eerste en
/// Tweede Kerstdag. Goede Vrijdag and Bevrijdingsdag (5 May) are not national days off at NS and
/// are left out (no NS source names them).
pub fn dutch_public_holiday(d: NaiveDate) -> bool {
    use chrono::{Datelike, Weekday};
    let y = d.year();
    let easter = easter_sunday(y);
    let kings = NaiveDate::from_ymd_opt(y, 4, 27).expect("valid date");
    let kings = if kings.weekday() == Weekday::Sun { kings - Duration::days(1) } else { kings };
    let fixed = [(1, 1), (12, 25), (12, 26)].iter().any(|&(m, day)| d.month() == m && d.day() == day);
    fixed || d == kings || [0, 1, 39, 49, 50].iter().any(|&n| d == easter + Duration::days(n))
}

/// Is the check-in inside the product's free-travel window? (Weekend Vrij: Fri 18:00 - Mon 04:00;
/// Dal Vrij: weekdays before 06:30, 09:00-16:00, after 18:30, and all weekend; Altijd Vrij: always.)
/// A Dutch public holiday ([`dutch_public_holiday`]) counts as a weekend day: from 00:00 until
/// 04:00 the next morning (like the weekend, which ends on Monday at 04:00).
fn covers(p: VrijProduct, j: &Journey) -> bool {
    use chrono::{Datelike, Timelike, Weekday};
    let t = j.check_in.with_timezone(&Amsterdam);
    let m = t.hour() * 60 + t.minute();
    let wd = t.weekday();
    let day = t.date_naive();
    let holiday = dutch_public_holiday(day) || (m < 4 * 60 && day.pred_opt().is_some_and(dutch_public_holiday));
    let weekend =
        holiday || matches!(wd, Weekday::Sat | Weekday::Sun) || (wd == Weekday::Fri && m >= 18 * 60) || (wd == Weekday::Mon && m < 4 * 60);
    match p {
        VrijProduct::AltijdVrij => true,
        VrijProduct::WeekendVrij => weekend,
        VrijProduct::DalVrij => weekend || m < 6 * 60 + 30 || (9 * 60..16 * 60).contains(&m) || m >= 18 * 60 + 30,
    }
}

/// NS rounds the fraction to the nearest cent (41.00 / 12 = 3.4167 -> 3.42).
fn div_round(price: i64, div: i64) -> i64 {
    ((i128::from(price) * 2 + i128::from(div)) / (i128::from(div) * 2)) as i64
}

/// Once NS has invoiced the trip. NS invoices monthly, so a covered trip older than
/// 60 days without a fare is treated as invoiced when the history has no explicit status.
fn invoiced_free(product: VrijProduct, j: &Journey) -> bool {
    j.invoiced.unwrap_or_else(|| Utc::now() - j.check_in > Duration::days(60) && covers(product, j))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VrijFeeSource {
    Ns,
    PriceList { source: String, class: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VrijTripValue {
    pub cents: i64,
    pub product: VrijProduct,
    pub divisor: i64,
    pub monthly_cents: i64,
    pub source: VrijFeeSource,
}

/// The NS monthly fee divided by the 60+ minute refund divisor for a confirmed free trip.
pub fn vrij_trip_value(j: &Journey) -> Option<VrijTripValue> {
    if !matches!(j.fare_cents, None | Some(0)) {
        return None;
    }
    let product = vrij_product(j.subscription.as_deref()?)?;
    if !covers(product, j) || !invoiced_free(product, j) {
        return None;
    }
    let divisor = fraction(product, DelayBand::Min60Plus).filter(|d| *d > 0)?;
    let (monthly_cents, source) = if let Some(fee) = j.subscription_fee_cents.filter(|f| *f > 0) {
        (fee, VrijFeeSource::Ns)
    } else {
        let date = j.check_in.with_timezone(&Amsterdam).date_naive();
        let (price, source, class) = monthly_price(product, j.travel_class.unwrap_or(2), date)?;
        (price, VrijFeeSource::PriceList { source, class })
    };
    let cents = div_round(monthly_cents, divisor);
    (cents > 0).then_some(VrijTripValue { cents, product, divisor, monthly_cents, source })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrivaFare {
    TravelHistory(i64),
    Vrij(VrijTripValue),
}

pub fn effective_arriva_fare(j: &Journey) -> Option<ArrivaFare> {
    j.fare_cents.filter(|f| *f > 0).map(ArrivaFare::TravelHistory).or_else(|| vrij_trip_value(j).map(ArrivaFare::Vrij))
}

impl ArrivaFare {
    pub fn cents(&self) -> i64 {
        match self {
            Self::TravelHistory(cents) => *cents,
            Self::Vrij(value) => value.cents,
        }
    }

    pub fn compensation(&self, j: &Journey, delay: i64) -> Option<Compensation> {
        let fare = self.cents();
        let (cents, pct) = crate::carrier_policy::ARRIVA.refund_cents(j.arriva_region(), delay, Some(fare))?;
        let mut basis = format!("Arriva: {pct}% van de ritprijs ({})", eur(fare));
        let exact = match self {
            Self::TravelHistory(_) => true,
            Self::Vrij(value) => {
                let monthly = eur(value.monthly_cents);
                let price = match &value.source {
                    VrijFeeSource::Ns => format!("{monthly}, volgens NS"),
                    VrijFeeSource::PriceList { source, class } => {
                        let host = source.split('/').nth(2).unwrap_or("NS");
                        let note = if *class == 2 && j.travel_class == Some(1) { " voor een reis in de 1e klas" } else { "" };
                        format!("{monthly} op de reisdatum, {class}e klas{note}; bron {host}")
                    }
                };
                basis.push_str(&format!(
                    "; ritprijs volgens NS-regel: maandbedrag {} ({price}) ÷ {}",
                    value.product.name(),
                    value.divisor
                ));
                false
            }
        };
        Some(Compensation { cents, exact, basis })
    }
}

fn floor(c: i64) -> i64 {
    if c < MIN_PAYOUT_CENTS {
        0
    } else {
        c
    }
}

/// `observed_gross`: what NS actually paid (gross, before any third-party commission) for a
/// free trip on the same product and band, closest in time — used when no dated price is known.
/// "€ 12,30": the app's money format (decimal comma in both languages).
pub fn eur(cents: i64) -> String {
    format!("€ {},{:02}", cents / 100, cents % 100)
}

pub fn expected(j: &Journey, band: Option<DelayBand>, observed_gross: Option<i64>) -> Option<Compensation> {
    let band = band?;
    if let Some(fare) = j.fare_cents.filter(|f| *f > 0) {
        let c = if band == DelayBand::Min30To59 { fare / 2 } else { fare };
        let part = if band == DelayBand::Min30To59 { "50%" } else { "100%" };
        return Some(Compensation { cents: floor(c), exact: true, basis: format!("{part} van de ritprijs ({})", eur(fare)) });
    }
    let product = j.subscription.as_deref().and_then(vrij_product)?;
    if !invoiced_free(product, j) {
        return None;
    }
    let date = j.check_in.with_timezone(&Amsterdam).date_naive();
    let div = fraction(product, band)?;
    let class = j.travel_class.unwrap_or(2);
    // NS's own monthly amount for this trip (what NS pays a fraction of).
    if let Some(fee) = j.subscription_fee_cents.filter(|f| *f > 0) {
        return Some(Compensation {
            cents: floor(div_round(fee, div)),
            exact: false,
            basis: format!("NS-regel: 1/{div} van je maandbedrag {} ({}, volgens NS)", product.name(), eur(fee)),
        });
    }
    if let Some((price, source, used)) = monthly_price(product, class, date) {
        let host = source.split('/').nth(2).unwrap_or("NS").to_string();
        let note = if used != class { ", prijs 2e klas: 1e klas is hoger" } else { "" };
        return Some(Compensation {
            cents: floor(div_round(price, div)),
            exact: false,
            basis: format!("NS-regel: 1/{div} van de maandprijs {} ({} op de reisdatum{note}; bron {host})", product.name(), eur(price)),
        });
    }
    if let Some(obs) = observed_gross.filter(|o| *o > 0) {
        return Some(Compensation {
            cents: floor(obs),
            exact: false,
            basis: format!("wat NS betaalde voor een vergelijkbare reis rond die tijd ({})", eur(obs)),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn easter_and_dutch_public_holidays() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(easter_sunday(2024), d(2024, 3, 31));
        assert_eq!(easter_sunday(2025), d(2025, 4, 20));
        assert_eq!(easter_sunday(2026), d(2026, 4, 5));
        assert_eq!(easter_sunday(2027), d(2027, 3, 28));
        assert_eq!(easter_sunday(2038), d(2038, 4, 25));
        assert_eq!(easter_sunday(2285), d(2285, 3, 22));
        for h in [
            d(2026, 1, 1),
            d(2026, 4, 5),
            d(2026, 4, 6),
            d(2026, 4, 27),
            d(2026, 5, 14),
            d(2026, 5, 24),
            d(2026, 5, 25),
            d(2026, 12, 25),
            d(2026, 12, 26),
            // Koningsdag on Saturday 26 April when the 27th is a Sunday.
            d(2025, 4, 26),
        ] {
            assert!(dutch_public_holiday(h), "{h}");
        }
        for n in [d(2026, 4, 3), d(2026, 5, 5), d(2025, 4, 27), d(2026, 4, 28), d(2026, 5, 15), d(2026, 12, 24)] {
            assert!(!dutch_public_holiday(n), "{n}");
        }
    }

    #[test]
    fn a_public_holiday_counts_as_weekend_for_weekend_and_dal_vrij() {
        let trip = |s: &str| Journey {
            check_in: chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc),
            ..j(None, None, None, None, "2026-05-14T08:00:00+02:00")
        };
        // Hemelvaartsdag (Thursday 14 May 2026) at 08:00: a peak hour on a normal Thursday.
        assert!(covers(VrijProduct::WeekendVrij, &trip("2026-05-14T08:00:00+02:00")));
        assert!(covers(VrijProduct::DalVrij, &trip("2026-05-14T08:00:00+02:00")));
        // Until 04:00 the next morning, not later.
        assert!(covers(VrijProduct::WeekendVrij, &trip("2026-05-15T03:30:00+02:00")));
        assert!(!covers(VrijProduct::WeekendVrij, &trip("2026-05-15T08:00:00+02:00")));
        assert!(!covers(VrijProduct::DalVrij, &trip("2026-05-15T08:00:00+02:00")));
        // A normal Wednesday.
        assert!(!covers(VrijProduct::WeekendVrij, &trip("2026-05-13T08:00:00+02:00")));
    }

    fn j(fare: Option<i64>, sub: Option<&str>, fee: Option<i64>, invoiced: Option<bool>, date: &str) -> Journey {
        Journey {
            source_id: "t".into(),
            source_record_id: "x".into(),
            coupling_id: None,
            card_last4: None,
            check_in: chrono::DateTime::parse_from_rfc3339(date).unwrap().with_timezone(&Utc),
            check_out: None,
            origin: StationRef { uic: "1".into(), name: "A".into() },
            destination: None,
            carrier_code: Some("4".into()),
            carrier_name: None,
            product_name: None,
            travel_class: Some(2),
            fare_cents: fare,
            subscription: sub.map(str::to_string),
            subscription_fee_cents: fee,
            invoiced,
            payload_sha256: String::new(),
        }
    }

    #[test]
    fn arriva_vrij_fares_match_both_ns_fractions_to_within_a_cent() {
        for (sub, product, monthly, divisor, cents) in [
            ("FLEXWVR", VrijProduct::WeekendVrij, 4206, 6, 701),
            ("FLEXDVR", VrijProduct::DalVrij, 10818, 18, 601),
            ("FLEXAVR", VrijProduct::AltijdVrij, 42021, 21, 2001),
        ] {
            let trip = j(None, Some(sub), Some(monthly), Some(true), "2026-10-17T17:30:00Z");
            let value = vrij_trip_value(&trip).unwrap();
            assert_eq!(value, VrijTripValue { cents, product, divisor, monthly_cents: monthly, source: VrijFeeSource::Ns });
            let fare = effective_arriva_fare(&trip).unwrap();
            for (delay, band, arriva_cents, rounding_difference) in [
                (45, DelayBand::Min30To59, cents / 2, 1),
                (65, DelayBand::Min60Plus, cents, 0),
            ] {
                let arriva = fare.compensation(&trip, delay).unwrap();
                let ns = expected(&trip, Some(band), None).unwrap();
                assert_eq!(arriva.cents, arriva_cents, "{sub} at {delay} min");
                assert_eq!(arriva.cents.abs_diff(ns.cents), rounding_difference, "{sub} at {delay} min");
            }
        }
    }

    #[test]
    fn arriva_vrij_uses_the_dated_price_and_records_the_class_used() {
        let mut trip = j(None, Some("FLEXWVR"), Some(0), Some(true), "2025-12-06T17:00:00Z");
        let fare = effective_arriva_fare(&trip).unwrap();
        assert_eq!(fare.cents(), 618);
        let basis = fare.compensation(&trip, 65).unwrap().basis;
        assert_eq!(
            basis,
            "Arriva: 100% van de ritprijs (€ 6,18); ritprijs volgens NS-regel: maandbedrag Weekend Vrij (€ 37,10 op de reisdatum, 2e klas; bron www.iamexpat.nl) ÷ 6"
        );
        trip.travel_class = Some(1);
        let value = vrij_trip_value(&trip).unwrap();
        assert_eq!(value.cents, 618);
        assert!(matches!(value.source, VrijFeeSource::PriceList { class: 2, .. }));
        assert!(ArrivaFare::Vrij(value).compensation(&trip, 65).unwrap().basis.contains("2e klas voor een reis in de 1e klas"));
        trip.subscription_fee_cents = Some(4206);
        let value = vrij_trip_value(&trip).unwrap();
        assert_eq!((value.cents, value.source), (701, VrijFeeSource::Ns));
    }

    #[test]
    fn arriva_vrij_needs_a_covered_invoiced_trip_and_a_known_monthly_fee() {
        let mut trip = j(None, Some("FLEXDVR"), Some(4430), Some(true), "2026-10-13T08:00:00+02:00");
        assert!(vrij_trip_value(&trip).is_none(), "outside the free window, even when invoiced");
        trip.subscription = Some("FLEXAVR".into());
        trip.check_in = Utc::now() - Duration::days(10);
        trip.invoiced = Some(false);
        assert!(vrij_trip_value(&trip).is_none());
        trip.invoiced = None;
        assert!(vrij_trip_value(&trip).is_none(), "too young to infer invoicing");
        trip.check_in = Utc::now() - Duration::days(61);
        assert!(vrij_trip_value(&trip).is_some(), "infer invoicing for an older covered trip");
        trip.invoiced = Some(false);
        assert!(vrij_trip_value(&trip).is_none(), "an explicit status takes precedence");
        trip.invoiced = Some(true);
        trip.subscription = Some("UNKNOWNFLEXAVR".into());
        assert!(vrij_trip_value(&trip).is_none());
        trip.subscription = Some("FLEXAVR".into());
        trip.check_in = j(None, None, None, None, "2025-06-01T10:00:00Z").check_in;
        trip.subscription_fee_cents = None;
        assert!(vrij_trip_value(&trip).is_none(), "no fee or dated price for Altijd Vrij");
        trip.subscription_fee_cents = Some(1);
        assert!(vrij_trip_value(&trip).is_none(), "rounds to zero");
    }

    #[test]
    fn arriva_uses_the_recorded_fare_first_and_treats_zero_as_missing() {
        let mut trip = j(None, Some("FLEXDVR"), Some(4430), Some(true), "2026-10-17T17:30:00Z");
        let value = vrij_trip_value(&trip).unwrap();
        assert_eq!(value.cents, 246);
        trip.fare_cents = Some(0);
        assert_eq!(vrij_trip_value(&trip), Some(value));
        trip.fare_cents = Some(1240);
        assert!(vrij_trip_value(&trip).is_none());
        assert_eq!(effective_arriva_fare(&trip), Some(ArrivaFare::TravelHistory(1240)));
        trip.fare_cents = Some(-1);
        assert!(vrij_trip_value(&trip).is_none());
    }

    #[test]
    fn extreme_monthly_fee_does_not_overflow() {
        assert_eq!(super::div_round(i64::MAX, 12), 768_614_336_404_564_651);
    }

    #[test]
    fn delay_band_wire_names_used_by_the_ui() {
        // src/main.ts (review dialog) compares against these exact serialised names.
        assert_eq!(serde_json::to_string(&DelayBand::Min60Plus).unwrap(), "\"min60_plus\"");
        assert_eq!(serde_json::to_string(&DelayBand::Min30To59).unwrap(), "\"min30_to59\"");
        assert_eq!(DelayBand::Min60Plus.as_str(), "60_plus");
    }

    #[test]
    fn paid_trip_half_or_full() {
        let x = expected(&j(Some(1240), None, None, Some(true), "2026-10-10T10:00:00Z"), Some(DelayBand::Min30To59), None).unwrap();
        assert_eq!((x.cents, x.exact), (620, true));
        assert_eq!(
            expected(&j(Some(1240), None, None, Some(true), "2026-10-10T10:00:00Z"), Some(DelayBand::Min60Plus), None).unwrap().cents,
            1240
        );
    }

    #[test]
    fn below_minimum_pays_nothing() {
        assert_eq!(
            expected(&j(Some(400), None, None, Some(true), "2026-10-10T10:00:00Z"), Some(DelayBand::Min30To59), None).unwrap().cents,
            0
        );
    }

    #[test]
    fn weekend_vrij_uses_the_trips_own_monthly_fee() {
        // Fictional fee: rounding to the nearest cent.
        let x =
            expected(&j(None, Some("DALKORT,FLEXWVR"), Some(4100), Some(true), "2026-10-17T17:30:00Z"), Some(DelayBand::Min30To59), None)
                .unwrap();
        assert_eq!(x.cents, 342);
        assert!(x.basis.contains("41.00") || x.basis.contains("41,00"), "{}", x.basis);
        // Fictional fee: 4206 / 12 = 350.5 cents rounds up to 351.
        let y =
            expected(&j(None, Some("FLEXWVR"), Some(4206), Some(true), "2026-05-17T18:00:00Z"), Some(DelayBand::Min30To59), None).unwrap();
        assert_eq!(y.cents, 351);
    }

    #[test]
    fn weekend_vrij_without_fee_uses_dated_price() {
        // Published list price fallback; 2025 price applies to a 2025 trip (60+: 1/6)
        let x = expected(&j(None, Some("DALKORT,FLEXWVR"), None, Some(true), "2026-10-24T17:00:00Z"), Some(DelayBand::Min30To59), None)
            .unwrap();
        assert_eq!(x.cents, 329);
        let y = expected(&j(None, Some("FLEXWVR"), None, Some(true), "2025-12-06T17:00:00Z"), Some(DelayBand::Min60Plus), None).unwrap();
        assert_eq!(y.cents, 618);
    }

    #[test]
    fn observed_payout_when_no_fee_or_price() {
        // No fee on the trip and no dated price (Altijd Vrij in 2025): what NS paid for a similar trip
        let x =
            expected(&j(None, Some("FLEXAVR"), None, Some(true), "2025-06-01T10:00:00Z"), Some(DelayBand::Min30To59), Some(900)).unwrap();
        assert_eq!(x.cents, 900);
        // ...and nothing known at all: unknown, never invented
        assert!(expected(&j(None, Some("FLEXAVR"), None, Some(true), "2025-06-01T10:00:00Z"), Some(DelayBand::Min30To59), None).is_none());
        // with the trip's own fee: 1/42 of it
        let z = expected(&j(None, Some("FLEXAVR"), Some(38000), Some(true), "2025-06-01T10:00:00Z"), Some(DelayBand::Min30To59), Some(900))
            .unwrap();
        assert_eq!(z.cents, 905);
    }

    #[test]
    fn dal_voordeel_is_not_a_vrij_product() {
        assert_eq!(vrij_product("DALKORT"), None);
        assert_eq!(vrij_product("DALKORT,FLEXWVR"), Some(VrijProduct::WeekendVrij));
        assert_eq!(vrij_product(" dalkort , flexdvr "), Some(VrijProduct::DalVrij));
        assert_eq!(vrij_product("FLEXAVR"), Some(VrijProduct::AltijdVrij));
    }

    #[test]
    fn vrij_product_needs_an_exact_token() {
        // substrings of unknown codes never count (formerly "AVR"/"DVR"/"WVR" anywhere matched)
        for s in ["TRAJAVR", "XWVRX", "FLEXWVR2", "DVR-KORTING", "", "Weekend Vrij"] {
            assert_eq!(vrij_product(s), None, "{s}");
        }
    }

    #[test]
    fn subscription_is_described_per_token() {
        let p = describe_subscription("DALKORT,FLEXWVR,ABCDEF");
        assert_eq!(p.len(), 3);
        assert_eq!((p[0].code.as_str(), p[0].kind, p[0].verified, p[0].fractions), ("DALKORT", "discount", true, None));
        assert_eq!((p[1].nl.as_str(), p[1].kind, p[1].verified, p[1].fractions), ("Weekend Vrij", "free", true, Some([12, 6])));
        assert_eq!((p[2].kind, p[2].verified, p[2].fractions), ("unknown", false, None));
        let d = describe_subscription("FLEXDVR, FLEXAVR");
        assert_eq!((d[0].fractions, d[0].verified), (Some([36, 18]), false));
        assert_eq!((d[1].fractions, d[1].verified), (Some([42, 21]), false));
        assert!(describe_subscription(" , ").is_empty());
    }

    #[test]
    fn not_invoiced_or_unknown_is_none() {
        assert!(expected(&j(None, Some("FLEXWVR"), Some(4100), Some(false), "2026-11-08T10:00:00Z"), Some(DelayBand::Min30To59), None)
            .is_none());
        assert!(expected(&j(None, None, None, Some(true), "2026-10-10T10:00:00Z"), Some(DelayBand::Min30To59), None).is_none());
        assert!(expected(&j(Some(1000), None, None, Some(true), "2026-10-10T10:00:00Z"), None, None).is_none());
    }

    #[test]
    fn missing_price_outside_the_free_window_is_unknown() {
        // Weekend Vrij, Wednesday morning, old trip without fare, invoiced flag unknown:
        // that trip was paid, its price is just missing -> no Vrij estimate.
        assert!(expected(&j(None, Some("FLEXWVR"), Some(4100), None, "2025-06-04T07:00:00Z"), Some(DelayBand::Min30To59), None).is_none());
        // Saturday: inside the window -> free, estimate from the dated price
        assert!(expected(&j(None, Some("FLEXWVR"), Some(4100), None, "2025-06-07T10:00:00Z"), Some(DelayBand::Min30To59), None).is_some());
    }

    #[test]
    fn first_class_fallback_is_labelled() {
        let mut x = j(None, Some("FLEXWVR"), None, Some(true), "2025-06-07T10:00:00Z");
        x.travel_class = Some(1);
        assert!(expected(&x, Some(DelayBand::Min30To59), None).unwrap().basis.contains("2e klas"));
    }

    #[test]
    fn every_price_row_has_a_source() {
        assert!(RULES.monthly_prices.iter().all(|r| r.source.starts_with("https://")));
    }
}
