//! Refund rules of a carrier as data: who it applies to, the minimum delay and refund bands per
//! region, the delay choices of the carrier's claim form, the claim period and the attachments.
//! Self-contained on purpose (no app state, no I/O) so it can become a per-carrier profile.
//! Only Arriva is described here; NS keeps its own rules in `eligibility`.
use chrono::{Months, NaiveDate};

/// A refund band: delays from `min` up to and including `max` minutes (`None` = no upper limit).
pub struct RefundBand {
    pub min: u32,
    pub max: Option<u32>,
    /// Refund on pay-as-you-go travel (saldo), in percent of the fare.
    pub percent_of_fare: u8,
    /// Fixed refund for a student travel product, in cents.
    pub student_product_cents: i64,
}

/// One choice in the claim form's delay list: delays from `min` up to and including `max` minutes.
pub struct DelayOption {
    pub min: u32,
    pub max: Option<u32>,
    /// Exactly as the form shows it.
    pub label: &'static str,
}

pub struct RegionRule {
    /// The region as named on the terms page and in the claim form.
    pub region: &'static str,
    pub min_delay: u32,
    pub bands: &'static [RefundBand],
}

pub struct Attachment {
    pub id: &'static str,
    pub nl: &'static str,
    pub en: &'static str,
}

pub struct CarrierPolicy {
    pub id: &'static str,
    pub name: &'static str,
    /// Carrier codes / names in travel-history records that this policy applies to.
    pub carrier_codes: &'static [&'static str],
    pub carrier_names: &'static [&'static str],
    /// Travel mode the rules cover.
    pub mode: &'static str,
    pub source_url: &'static str,
    pub form_url: &'static str,
    /// Date the terms page and the form were read.
    pub source_checked: &'static str,
    /// A claim must be filed within this many months after the trip.
    pub claim_months: u32,
    pub regions: &'static [RegionRule],
    pub form_delay_options: &'static [DelayOption],
    pub required_attachments: &'static [Attachment],
    /// Situations in which the carrier pays nothing (terms page, abridged).
    pub exclusions: &'static [&'static str],
}

/// Outcome of applying a policy to one trip.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    pub eligible: bool,
    /// Region whose rule was applied; `None` when the region was unknown and the strictest rule was used.
    pub region: Option<&'static str>,
    pub min_delay: u32,
    pub refund_percent: Option<u8>,
    /// The form's delay choice for this delay (also for a trip that does not qualify).
    pub form_option: Option<&'static str>,
    pub deadline: Option<NaiveDate>,
    pub reason_nl: String,
    pub reason_en: String,
}

fn within(min: u32, max: Option<u32>, minutes: u32) -> bool {
    minutes >= min && max.is_none_or(|m| minutes <= m)
}

impl CarrierPolicy {
    /// By name when NS gives one: exactly a listed name, or a listed name followed by more words
    /// ("Arriva Personenvervoer"); another name (e.g. a carrier that took over a line, such as
    /// "RRReis") never matches, whatever its code. Without a name: by carrier code.
    pub fn matches(&self, code: Option<&str>, name: Option<&str>) -> bool {
        match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => {
                let n = n.to_lowercase();
                self.carrier_names.iter().any(|x| {
                    let x = x.to_lowercase();
                    n == x || n.strip_prefix(&x).is_some_and(|rest| rest.starts_with(' '))
                })
            }
            None => code.is_some_and(|c| self.carrier_codes.iter().any(|x| x.eq_ignore_ascii_case(c.trim()))),
        }
    }
    /// Refund for a trip on pay-as-you-go travel: (cents, percent of the fare), when the delay
    /// qualifies in `region` and the fare is known.
    pub fn refund_cents(&self, region: Option<&str>, delay_minutes: i64, fare_cents: Option<i64>) -> Option<(i64, u8)> {
        let (rule, _) = self.rule(region);
        let minutes = delay_minutes.clamp(0, i64::from(u32::MAX)) as u32;
        let band = rule.bands.iter().find(|b| minutes >= rule.min_delay && within(b.min, b.max, minutes))?;
        let fare = fare_cents.filter(|f| *f > 0)?;
        Some(((i128::from(fare) * i128::from(band.percent_of_fare) / 100) as i64, band.percent_of_fare))
    }
    /// The rule of `region`; an unknown region gets the strictest rule (highest minimum delay), only to
    /// judge whether the trip can qualify anywhere. Such a trip is never claimed in the app: Arriva's
    /// form needs the region, so the user submits it at Arriva (see `arriva_claim::blocker`).
    pub fn rule(&self, region: Option<&str>) -> (&RegionRule, bool) {
        if let Some(r) = region.and_then(|n| self.regions.iter().find(|r| r.region.eq_ignore_ascii_case(n.trim()))) {
            return (r, true);
        }
        (self.regions.iter().max_by_key(|r| r.min_delay).expect("policy has regions"), false)
    }
    /// Last day a claim for a trip on `travel` is accepted.
    pub fn deadline(&self, travel: NaiveDate) -> Option<NaiveDate> {
        travel.checked_add_months(Months::new(self.claim_months))
    }
    pub fn form_delay_option(&self, minutes: u32) -> Option<&'static str> {
        self.form_delay_options.iter().find(|o| within(o.min, o.max, minutes)).map(|o| o.label)
    }
    /// The whole policy for display (the app's Arriva view lists its rules and source).
    pub fn to_json(&self) -> serde_json::Value {
        let range = |min: u32, max: Option<u32>| serde_json::json!({"min": min, "max": max});
        serde_json::json!({
            "id": self.id, "name": self.name, "carrier_codes": self.carrier_codes, "carrier_names": self.carrier_names,
            "mode": self.mode, "source_url": self.source_url, "form_url": self.form_url, "source_checked": self.source_checked,
            "claim_months": self.claim_months,
            "regions": self.regions.iter().map(|r| serde_json::json!({
                "region": r.region, "min_delay": r.min_delay,
                "bands": r.bands.iter().map(|b| serde_json::json!({"range": range(b.min, b.max), "percent_of_fare": b.percent_of_fare, "student_product_cents": b.student_product_cents})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "form_delay_options": self.form_delay_options.iter().map(|o| serde_json::json!({"range": range(o.min, o.max), "label": o.label})).collect::<Vec<_>>(),
            "required_attachments": self.required_attachments.iter().map(|a| serde_json::json!({"id": a.id, "nl": a.nl, "en": a.en})).collect::<Vec<_>>(),
            "exclusions": self.exclusions,
        })
    }
    pub fn assess(&self, region: Option<&str>, delay_minutes: i64, travel: NaiveDate, today: NaiveDate) -> Assessment {
        let (rule, known) = self.rule(region);
        let minutes = delay_minutes.clamp(0, i64::from(u32::MAX)) as u32;
        let band = rule.bands.iter().find(|b| minutes >= rule.min_delay && within(b.min, b.max, minutes));
        let deadline = self.deadline(travel);
        let expired = deadline.is_some_and(|d| today > d);
        let where_nl = if known { format!("in {}", rule.region) } else { "(regio onbekend)".into() };
        let where_en = if known { format!("in {}", rule.region) } else { "(region unknown)".into() };
        let date = |d: Option<NaiveDate>| d.map(|d| d.format("%d-%m-%Y").to_string()).unwrap_or_default();
        let (eligible, nl, en) = match band {
            None => (
                false,
                format!("{} vergoedt {where_nl} vanaf {} minuten vertraging; deze rit had +{minutes} min.", self.name, rule.min_delay),
                format!("{} pays {where_en} from {} minutes of delay; this trip had +{minutes} min.", self.name, rule.min_delay),
            ),
            Some(_) if expired => (
                false,
                format!("Aanvraagtermijn verlopen: {} vergoedt binnen {} maanden (tot {}).", self.name, self.claim_months, date(deadline)),
                format!("Claim period over: {} pays within {} months (until {}).", self.name, self.claim_months, date(deadline)),
            ),
            Some(b) => (
                true,
                format!("+{minutes} min {where_nl}: {}% van de ritprijs; aanvragen tot {}.", b.percent_of_fare, date(deadline)),
                format!("+{minutes} min {where_en}: {}% of the fare; claim until {}.", b.percent_of_fare, date(deadline)),
            ),
        };
        Assessment {
            eligible,
            region: known.then_some(rule.region),
            min_delay: rule.min_delay,
            refund_percent: band.map(|b| b.percent_of_fare),
            form_option: self.form_delay_option(minutes),
            deadline,
            reason_nl: nl,
            reason_en: en,
        }
    }
}

const BANDS_15: &[RefundBand] = &[
    RefundBand { min: 15, max: Some(59), percent_of_fare: 50, student_product_cents: 232 },
    RefundBand { min: 60, max: None, percent_of_fare: 100, student_product_cents: 464 },
];
const BANDS_30: &[RefundBand] = &[
    RefundBand { min: 30, max: Some(59), percent_of_fare: 50, student_product_cents: 232 },
    RefundBand { min: 60, max: None, percent_of_fare: 100, student_product_cents: 464 },
];

/// Arriva trains, "Geld terug bij vertraging" (terms page per region, read 2026-11-20). Noord-Brabant
/// is left out: Arriva runs no trains there and its table covers buses.
pub const ARRIVA: CarrierPolicy = CarrierPolicy {
    id: "arriva",
    name: "Arriva",
    carrier_codes: &["ARR", "12"],
    carrier_names: &["Arriva"],
    mode: "train",
    source_url: "https://www.arriva.nl/vragen-contact/contact-met-arriva/een-vergoeding-aanvragen/",
    form_url: "https://klantenservice.arriva.nl/geldterug",
    source_checked: "2026-11-20",
    claim_months: 3,
    regions: &[
        RegionRule { region: "Limburg", min_delay: 15, bands: BANDS_15 },
        RegionRule { region: "Gelderland", min_delay: 30, bands: BANDS_30 },
        RegionRule { region: "Overijssel", min_delay: 30, bands: BANDS_30 },
        RegionRule { region: "Drenthe", min_delay: 30, bands: BANDS_30 },
        RegionRule { region: "Groningen", min_delay: 30, bands: BANDS_30 },
        RegionRule { region: "Fryslân", min_delay: 30, bands: BANDS_30 },
    ],
    // The form's list: "15 - 30" and "30 - 59" share 30; the terms put 30 in the 30-59 band.
    form_delay_options: &[
        DelayOption { min: 15, max: Some(29), label: "15 - 30 minuten" },
        DelayOption { min: 30, max: Some(59), label: "30 - 59 minuten" },
        DelayOption { min: 60, max: None, label: "60+ minuten" },
    ],
    required_attachments: &[
        Attachment { id: "travel", nl: "Reisbewijs (check-in en check-out)", en: "Travel proof (check-in and check-out)" },
        Attachment { id: "delay", nl: "Vertragingsbewijs (Rijden de Treinen open data)", en: "Delay proof (Rijden de Treinen open data)" },
    ],
    exclusions: &[
        "vertraging bij een andere vervoerder dan Arriva",
        "langere reistijd vooraf aangekondigd (werk aan het spoor)",
        "abonnement op OV-pas, betaalpas of OV-chipkaart zonder in- of uitchecken",
        "overmacht, zoals extreme weersomstandigheden of een stroomstoring",
        "vertraging met de bus",
    ],
};

/// Arriva train stations by region (Arriva's train concession of the line, which is the region of
/// the terms and of the claim form; not always the province: the Maaslijn from Nijmegen is
/// Limburg). `true` marks a hub that other carriers or other Arriva regions also serve; its region
/// is used only when the other end of the trip is not in the table. Names as NS writes them,
/// compared without case and accents. A station that is not listed gives no region, and then
/// the strictest rule applies (see [`CarrierPolicy::rule`]).
const ARRIVA_STATIONS: &[(&str, &str, bool)] = &[
    // Limburg: Maaslijn (Nijmegen - Venlo - Roermond), Heuvellandlijn, Sittard - Heerlen, Roermond - Maastricht.
    ("Nijmegen", "Limburg", true),
    ("Nijmegen Heyendaal", "Limburg", false),
    ("Mook-Molenhoek", "Limburg", false),
    ("Cuijk", "Limburg", false),
    ("Boxmeer", "Limburg", false),
    ("Vierlingsbeek", "Limburg", false),
    ("Venray", "Limburg", false),
    ("Horst-Sevenum", "Limburg", false),
    ("Blerick", "Limburg", false),
    ("Venlo", "Limburg", false),
    ("Tegelen", "Limburg", false),
    ("Reuver", "Limburg", false),
    ("Swalmen", "Limburg", false),
    ("Roermond", "Limburg", false),
    ("Echt", "Limburg", false),
    ("Susteren", "Limburg", false),
    ("Sittard", "Limburg", false),
    ("Geleen-Lutterade", "Limburg", false),
    ("Geleen Oost", "Limburg", false),
    ("Beek-Elsloo", "Limburg", false),
    ("Bunde", "Limburg", false),
    ("Spaubeek", "Limburg", false),
    ("Schinnen", "Limburg", false),
    ("Nuth", "Limburg", false),
    ("Hoensbroek", "Limburg", false),
    ("Maastricht", "Limburg", false),
    ("Maastricht Noord", "Limburg", false),
    ("Maastricht Randwyck", "Limburg", false),
    ("Meerssen", "Limburg", false),
    ("Houthem-St. Gerlach", "Limburg", false),
    ("Valkenburg", "Limburg", false),
    ("Schin op Geul", "Limburg", false),
    ("Klimmen-Ransdaal", "Limburg", false),
    ("Voerendaal", "Limburg", false),
    ("Heerlen", "Limburg", false),
    ("Heerlen Woonboulevard", "Limburg", false),
    ("Landgraaf", "Limburg", false),
    ("Eygelshoven", "Limburg", false),
    ("Eygelshoven Markt", "Limburg", false),
    ("Chevremont", "Limburg", false),
    ("Kerkrade Centrum", "Limburg", false),
    // Gelderland: Arnhem - Winterswijk, Zutphen - Winterswijk, Arnhem - Tiel.
    ("Arnhem Centraal", "Gelderland", true),
    ("Zutphen", "Gelderland", true),
    ("Arnhem Velperpoort", "Gelderland", false),
    ("Westervoort", "Gelderland", false),
    ("Duiven", "Gelderland", false),
    ("Zevenaar", "Gelderland", false),
    ("Didam", "Gelderland", false),
    ("Wehl", "Gelderland", false),
    ("Doetinchem", "Gelderland", false),
    ("Doetinchem De Huet", "Gelderland", false),
    ("Gaanderen", "Gelderland", false),
    ("Terborg", "Gelderland", false),
    ("Varsseveld", "Gelderland", false),
    ("Aalten", "Gelderland", false),
    ("Lichtenvoorde-Groenlo", "Gelderland", false),
    ("Winterswijk", "Gelderland", false),
    ("Winterswijk West", "Gelderland", false),
    ("Vorden", "Gelderland", false),
    ("Ruurlo", "Gelderland", false),
    ("Arnhem Zuid", "Gelderland", false),
    ("Elst", "Gelderland", false),
    ("Zetten-Andelst", "Gelderland", false),
    ("Hemmen-Dodewaard", "Gelderland", false),
    ("Opheusden", "Gelderland", false),
    ("Kesteren", "Gelderland", false),
    ("Tiel Passewaaij", "Gelderland", false),
    ("Tiel", "Gelderland", true),
    // Overijssel and Drenthe: Zwolle - Emmen, Almelo - Mariënberg.
    ("Zwolle", "Overijssel", true),
    ("Almelo", "Overijssel", true),
    ("Dalfsen", "Overijssel", false),
    ("Ommen", "Overijssel", false),
    ("Mariënberg", "Overijssel", false),
    ("Hardenberg", "Overijssel", false),
    ("Gramsbergen", "Overijssel", false),
    ("Vriezenveen", "Overijssel", false),
    ("Daarlerveen", "Overijssel", false),
    ("Vroomshoop", "Overijssel", false),
    ("Coevorden", "Drenthe", false),
    ("Dalen", "Drenthe", false),
    ("Nieuw Amsterdam", "Drenthe", false),
    ("Emmen Zuid", "Drenthe", false),
    ("Emmen", "Drenthe", false),
    // Groningen and Fryslân: the northern lines.
    ("Groningen", "Groningen", true),
    ("Groningen Noord", "Groningen", false),
    ("Groningen Europapark", "Groningen", false),
    ("Sauwerd", "Groningen", false),
    ("Winsum", "Groningen", false),
    ("Baflo", "Groningen", false),
    ("Warffum", "Groningen", false),
    ("Usquert", "Groningen", false),
    ("Uithuizen", "Groningen", false),
    ("Uithuizermeeden", "Groningen", false),
    ("Roodeschool", "Groningen", false),
    ("Eemshaven", "Groningen", false),
    ("Bedum", "Groningen", false),
    ("Stedum", "Groningen", false),
    ("Loppersum", "Groningen", false),
    ("Appingedam", "Groningen", false),
    ("Delfzijl West", "Groningen", false),
    ("Delfzijl", "Groningen", false),
    ("Kropswolde", "Groningen", false),
    ("Martenshoek", "Groningen", false),
    ("Hoogezand-Sappemeer", "Groningen", false),
    ("Sappemeer Oost", "Groningen", false),
    ("Zuidbroek", "Groningen", false),
    ("Veendam", "Groningen", false),
    ("Scheemda", "Groningen", false),
    ("Winschoten", "Groningen", false),
    ("Bad Nieuweschans", "Groningen", false),
    ("Zuidhorn", "Groningen", false),
    ("Grijpskerk", "Groningen", false),
    ("Leeuwarden", "Fryslân", true),
    ("Leeuwarden Camminghaburen", "Fryslân", false),
    ("Buitenpost", "Fryslân", false),
    ("De Westereen", "Fryslân", false),
    ("Feanwâlden", "Fryslân", false),
    ("Hurdegaryp", "Fryslân", false),
    ("Deinum", "Fryslân", false),
    ("Dronryp", "Fryslân", false),
    ("Franeker", "Fryslân", false),
    ("Harlingen", "Fryslân", false),
    ("Harlingen Haven", "Fryslân", false),
    ("Mantgum", "Fryslân", false),
    ("Sneek Noord", "Fryslân", false),
    ("Sneek", "Fryslân", false),
    ("IJlst", "Fryslân", false),
    ("Workum", "Fryslân", false),
    ("Hindeloopen", "Fryslân", false),
    ("Koudum-Molkwerum", "Fryslân", false),
    ("Stavoren", "Fryslân", false),
];

/// Lower case without accents and spacing differences ("Feanwâlden" = "feanwalden").
fn station_key(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'Á' | 'Â' => 'a',
            'é' | 'è' | 'ê' | 'ë' | 'É' => 'e',
            'í' | 'ï' | 'î' => 'i',
            'ó' | 'ô' | 'ö' => 'o',
            'ú' | 'û' | 'ü' => 'u',
            c => c.to_ascii_lowercase(),
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn station_region(name: &str) -> Option<(&'static str, bool)> {
    let k = station_key(name);
    let k = if k == "arnhem" { "arnhem centraal".to_string() } else { k };
    ARRIVA_STATIONS.iter().find(|(n, _, _)| station_key(n) == k).map(|(_, r, hub)| (*r, *hub))
}

/// The Arriva region of a trip from its stations, the same in both directions. When the two
/// stations belong to different regions the strictest rule applies (the region with the highest
/// minimum delay; on a tie a station only Arriva's line in one region serves, then the name).
/// `None` when neither station is known.
pub fn arriva_region(origin: &str, destination: Option<&str>) -> Option<&'static str> {
    let known: Vec<(&'static str, bool)> = [Some(origin), destination].into_iter().flatten().filter_map(station_region).collect();
    known
        .iter()
        .max_by(|(a, a_hub), (b, b_hub)| {
            let min = |r: &str| ARRIVA.rule(Some(r)).0.min_delay;
            min(a).cmp(&min(b)).then(b_hub.cmp(a_hub)).then(b.cmp(a))
        })
        .map(|(r, _)| *r)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    #[test]
    fn limburg_pays_from_15_minutes() {
        let a = ARRIVA.assess(Some("Limburg"), 24, d("2026-10-29"), d("2026-11-20"));
        assert!(a.eligible);
        assert_eq!((a.region, a.min_delay, a.refund_percent), (Some("Limburg"), 15, Some(50)));
        assert_eq!(a.form_option, Some("15 - 30 minuten"));
        assert_eq!(a.deadline, Some(d("2027-01-29")));
        let short = ARRIVA.assess(Some("Limburg"), 14, d("2026-10-29"), d("2026-11-20"));
        assert!(!short.eligible);
        assert!(short.reason_en.contains("from 15 minutes"));
        assert_eq!(ARRIVA.assess(Some("Limburg"), 60, d("2026-10-29"), d("2026-11-20")).refund_percent, Some(100));
    }
    #[test]
    fn other_regions_pay_from_30_minutes() {
        for r in ["Gelderland", "Overijssel", "Drenthe", "Groningen", "Fryslân"] {
            assert!(!ARRIVA.assess(Some(r), 29, d("2026-10-29"), d("2026-11-20")).eligible, "{r}");
            let a = ARRIVA.assess(Some(r), 30, d("2026-10-29"), d("2026-11-20"));
            assert!(a.eligible && a.refund_percent == Some(50) && a.form_option == Some("30 - 59 minuten"), "{r}");
        }
    }
    #[test]
    fn unknown_region_uses_the_strictest_rule() {
        let a = ARRIVA.assess(None, 24, d("2026-10-29"), d("2026-11-20"));
        assert!(!a.eligible);
        assert_eq!((a.region, a.min_delay), (None, 30));
        assert!(!ARRIVA.assess(Some("Noord-Brabant"), 24, d("2026-10-29"), d("2026-11-20")).eligible);
    }
    #[test]
    fn claim_period_is_three_months() {
        assert!(ARRIVA.assess(Some("Limburg"), 24, d("2026-10-29"), d("2027-01-29")).eligible);
        let late = ARRIVA.assess(Some("Limburg"), 24, d("2026-10-29"), d("2027-01-30"));
        assert!(!late.eligible);
        assert!(late.reason_en.starts_with("Claim period over"));
        // Month ends clamp: 30 November + 3 months = 28 February.
        assert_eq!(ARRIVA.deadline(d("2026-11-30")), Some(d("2027-02-28")));
    }
    #[test]
    fn form_delay_choices_cover_every_qualifying_delay() {
        assert_eq!(ARRIVA.form_delay_option(14), None);
        assert_eq!(ARRIVA.form_delay_option(15), Some("15 - 30 minuten"));
        assert_eq!(ARRIVA.form_delay_option(29), Some("15 - 30 minuten"));
        assert_eq!(ARRIVA.form_delay_option(30), Some("30 - 59 minuten"));
        assert_eq!(ARRIVA.form_delay_option(59), Some("30 - 59 minuten"));
        assert_eq!(ARRIVA.form_delay_option(60), Some("60+ minuten"));
        assert_eq!(ARRIVA.form_delay_option(400), Some("60+ minuten"));
        for r in ARRIVA.regions {
            for m in r.min_delay..200 {
                assert!(ARRIVA.form_delay_option(m).is_some(), "{} {m}", r.region);
            }
        }
    }
    #[test]
    fn carrier_matching() {
        assert!(ARRIVA.matches(Some("ARR"), None));
        assert!(ARRIVA.matches(None, Some("arriva")));
        assert!(!ARRIVA.matches(Some("NS"), Some("NS")));
        assert!(ARRIVA.matches(Some("12"), Some("Arriva Personenvervoer")));
        assert!(!ARRIVA.matches(None, Some("Arrivals")));
        // Another carrier's name decides, even with a code Arriva used.
        assert!(!ARRIVA.matches(Some("12"), Some("RRReis")));
        assert!(!ARRIVA.matches(None, Some("RRReis")));
        assert!(!ARRIVA.matches(Some("ARR"), Some("Keolis")));
        assert!(ARRIVA.matches(Some("12"), Some("  ")));
        assert!(!ARRIVA.matches(Some("4"), Some("Nederlandse Spoorwegen")));
    }
    #[test]
    fn region_from_the_route() {
        assert_eq!(arriva_region("Venlo", Some("Nijmegen")), Some("Limburg"));
        assert_eq!(arriva_region("Nijmegen", Some("Venray")), Some("Limburg"));
        assert_eq!(arriva_region("Arnhem Centraal", Some("Winterswijk")), Some("Gelderland"));
        assert_eq!(arriva_region("Arnhem", Some("Doetinchem")), Some("Gelderland"));
        assert_eq!(arriva_region("Zwolle", Some("Emmen")), Some("Drenthe"));
        assert_eq!(arriva_region("Zwolle", Some("Ommen")), Some("Overijssel"));
        assert_eq!(arriva_region("Groningen", Some("Feanwalden")), Some("Fryslân"));
        assert_eq!(arriva_region("Groningen", Some("Leeuwarden")), Some("Fryslân"), "two hubs, same rule: by name");
        // The same region in both directions.
        for (a, b) in
            [("Venlo", "Nijmegen"), ("Zwolle", "Emmen"), ("Groningen", "Leeuwarden"), ("Groningen", "Feanwalden"), ("Arnhem", "Doetinchem")]
        {
            assert_eq!(arriva_region(a, Some(b)), arriva_region(b, Some(a)), "{a} - {b}");
        }
        // Different regions with different rules: the strictest (30 minutes, not Limburg's 15).
        assert_eq!(arriva_region("Nijmegen", Some("Arnhem Centraal")), Some("Gelderland"));
        assert_eq!(arriva_region("Arnhem Centraal", Some("Nijmegen")), Some("Gelderland"));
        assert_eq!(arriva_region("Utrecht Centraal", Some("Amersfoort Centraal")), None);
        assert_eq!(arriva_region("Utrecht Centraal", None), None);
    }
    #[test]
    fn refund_from_the_fare() {
        assert_eq!(ARRIVA.refund_cents(Some("Limburg"), 24, Some(1240)), Some((620, 50)));
        assert_eq!(ARRIVA.refund_cents(Some("Limburg"), 75, Some(1240)), Some((1240, 100)));
        assert_eq!(ARRIVA.refund_cents(Some("Limburg"), i64::MAX, Some(i64::MAX)), Some((i64::MAX, 100)));
        assert_eq!(ARRIVA.assess(Some("Limburg"), i64::from(u32::MAX) + 1, d("2026-10-01"), d("2026-10-02")).refund_percent, Some(100));
        assert_eq!(ARRIVA.refund_cents(Some("Gelderland"), 24, Some(1240)), None);
        assert_eq!(ARRIVA.refund_cents(Some("Limburg"), 24, None), None);
    }
}
