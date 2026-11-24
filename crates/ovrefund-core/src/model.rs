//! Domain types shared by every adapter.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// NS carrier code as observed in `mijnns-transaction-api` (`journey.carrier.code`).
pub const NS_CARRIER_CODE: &str = "4";

/// Is a trip with this carrier code and name an Arriva trip (see [`Journey::is_arriva`])? Used
/// where only the stored carrier columns are at hand (e.g. inside a database transaction).
pub fn is_arriva_carrier(code: Option<&str>, name: Option<&str>) -> bool {
    code != Some(NS_CARRIER_CODE) && crate::carrier_policy::ARRIVA.matches(code, name)
}

/// One check-in/check-out journey from a travel-history source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journey {
    /// Source adapter id, e.g. `mijnns`.
    pub source_id: String,
    /// Provider record id (Mijn NS transaction id, e.g. `HB-ROR…`).
    pub source_record_id: String,
    /// Mijn NS coupling id (needed for the journey-linked claim form route).
    pub coupling_id: Option<String>,
    /// Card fingerprint: last 4 digits only, never the full number.
    pub card_last4: Option<String>,
    pub check_in: DateTime<Utc>,
    pub check_out: Option<DateTime<Utc>>,
    pub origin: StationRef,
    pub destination: Option<StationRef>,
    pub carrier_code: Option<String>,
    pub carrier_name: Option<String>,
    pub product_name: Option<String>,
    pub travel_class: Option<u8>,
    /// Fare in euro cents when the source states one. `None` means unknown, not free.
    pub fare_cents: Option<i64>,
    pub subscription: Option<String>,
    /// Monthly fee of the subscription this trip was travelled on (cents), as stated by NS.
    #[serde(default)]
    pub subscription_fee_cents: Option<i64>,
    /// Whether NS has invoiced the trip (amounts are only final after invoicing).
    #[serde(default)]
    pub invoiced: Option<bool>,
    /// SHA-256 of the canonicalised source record (provenance, change detection).
    pub payload_sha256: String,
}

impl Journey {
    /// Deterministic fallback key used when a source record id is unstable.
    pub fn fallback_key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.card_last4.as_deref().unwrap_or("-"),
            self.check_in.format("%Y-%m-%dT%H:%M:%SZ"),
            self.origin.uic,
            self.destination.as_ref().map(|d| d.uic.as_str()).unwrap_or("-")
        )
    }

    /// Stable identity of a trip that survives a history wipe and re-fetch (row ids do not):
    /// check-in time + origin station.
    pub fn natural_key(&self) -> String {
        format!("{}|{}", self.check_in.format("%Y-%m-%dT%H:%M:%SZ"), self.origin.uic)
    }

    pub fn is_ns_carrier(&self) -> bool {
        self.carrier_code.as_deref() == Some(NS_CARRIER_CODE)
    }

    /// An Arriva train trip: checked with Arriva's own rules and claimed through Arriva's form
    /// (manually, by the user), never through NS.
    pub fn is_arriva(&self) -> bool {
        is_arriva_carrier(self.carrier_code.as_deref(), self.carrier_name.as_deref())
    }

    /// Arriva's region for this trip (from its stations), see [`crate::carrier_policy::arriva_region`].
    pub fn arriva_region(&self) -> Option<&'static str> {
        crate::carrier_policy::arriva_region(&self.origin.name, self.destination.as_ref().map(|d| d.name.as_str()))
    }

    /// Where to ask for a refund for a trip with another carrier (None for NS trips).
    pub fn carrier_url(&self) -> Option<&'static str> {
        if self.is_ns_carrier() {
            return None;
        }
        carrier_url(self.carrier_name.as_deref().unwrap_or(""))
    }
}

/// Public web page of a non-NS carrier, by the carrier name in NS's travel history. The refund
/// page itself when its exact address is known (NS International), otherwise the carrier's
/// homepage, where its own "geld terug bij vertraging" rules are linked. Unknown carrier: None.
pub fn carrier_url(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    const MAP: [(&str, &str); 14] = [
        ("ns international", "https://www.nsinternational.com/nl/na-de-reis/compensatie-bij-vertraging"),
        ("blauwnet", "https://www.blauwnet.nl"),
        ("breng", "https://www.breng.nl"),
        ("arriva", "https://www.arriva.nl"),
        ("keolis", "https://www.keolis.nl"),
        ("qbuzz", "https://www.qbuzz.nl"),
        ("connexxion", "https://www.connexxion.nl"),
        ("ebs", "https://www.ebs-ov.nl"),
        ("rrreis", "https://www.rrreis.nl"),
        ("u-ov", "https://www.u-ov.info"),
        ("gvb", "https://www.gvb.nl"),
        ("ret", "https://www.ret.nl"),
        ("htm", "https://www.htm.nl"),
        ("hermes", "https://www.hermes.nl"),
    ];
    // Whole words only ("ret" must not match "arretje"); the first (most specific) entry wins.
    let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric() && c != '-').filter(|w| !w.is_empty()).collect();
    MAP.iter()
        .find(|(k, _)| {
            let kw: Vec<&str> = k.split(' ').collect();
            words.windows(kw.len()).any(|w| w == kw.as_slice())
        })
        .map(|(_, u)| *u)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationRef {
    pub uic: String,
    pub name: String,
}

/// One stop of a train service in the historical archive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveStop {
    pub service_id: i64,
    pub service_date: chrono::NaiveDate,
    pub train_type: String,
    pub company: String,
    pub train_number: String,
    pub station_code: String,
    pub stop_seq: i64,
    pub arr_planned: Option<DateTime<Utc>>,
    pub arr_delay_min: Option<i64>,
    pub arr_cancelled: bool,
    pub dep_planned: Option<DateTime<Utc>>,
    pub dep_delay_min: Option<i64>,
    pub dep_cancelled: bool,
}

impl ArchiveStop {
    pub fn arr_actual(&self) -> Option<DateTime<Utc>> {
        self.arr_planned.and_then(|t| chrono::Duration::try_minutes(self.arr_delay_min.unwrap_or(0)).and_then(|d| t.checked_add_signed(d)))
    }
    pub fn dep_actual(&self) -> Option<DateTime<Utc>> {
        self.dep_planned.and_then(|t| chrono::Duration::try_minutes(self.dep_delay_min.unwrap_or(0)).and_then(|d| t.checked_add_signed(d)))
    }
}

/// A leg of a matched rail route (one train between two stations).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leg {
    pub train_number: String,
    pub train_type: String,
    pub company: String,
    pub from_code: String,
    pub to_code: String,
    pub dep_planned: DateTime<Utc>,
    pub dep_actual: DateTime<Utc>,
    pub arr_planned: DateTime<Utc>,
    pub arr_actual: DateTime<Utc>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Route {
    #[serde(deserialize_with = "nonempty_legs")]
    pub legs: Vec<Leg>,
}

fn nonempty_legs<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<Leg>, D::Error> {
    let legs = Vec::<Leg>::deserialize(de)?;
    if legs.is_empty() {
        return Err(serde::de::Error::custom("Route zonder treinen. / Route has no trains."));
    }
    Ok(legs)
}

impl Route {
    pub fn dep_planned(&self) -> DateTime<Utc> {
        self.legs[0].dep_planned
    }
    pub fn arr_planned(&self) -> DateTime<Utc> {
        self.legs.last().unwrap().arr_planned
    }
    pub fn arr_actual(&self) -> DateTime<Utc> {
        self.legs.last().unwrap().arr_actual
    }
    /// Trains of an adjusted timetable (planned works) carry 70xxxx numbers.
    pub fn diverted(&self) -> bool {
        self.legs.iter().any(|l| l.train_number.len() >= 6 && l.train_number.starts_with("70"))
    }
    pub fn all_ns(&self) -> bool {
        self.legs.iter().all(|l| l.company == "NS")
    }
    pub fn uses_hsl(&self) -> bool {
        self.legs
            .iter()
            .any(|l| l.train_type.to_lowercase().contains("intercity direct") || l.train_type.to_lowercase().contains("eurostar"))
    }
}

/// Historical schedule evidence for one journey.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleEvidence {
    pub provider: String,
    /// True only when the timetable is the one valid on the travel date (never today's timetable).
    pub historical: bool,
    /// The route the traveller would have taken per the timetable, given the check-in time.
    pub planned: Option<Route>,
    /// The route most consistent with the realised train times and the check-out time.
    pub taken: Option<Route>,
    /// Status when no evidence could be produced.
    pub status: EvidenceStatus,
    pub notes: Vec<String>,
    /// Adjusted-timetable days only: planned arrival of the same trip in the REGULAR timetable
    /// (same weekday, a week or two earlier/later), shifted to the travel date.
    #[serde(default)]
    pub regular_arrival: Option<DateTime<Utc>>,
    /// The regular-timetable baseline was looked up (found or not).
    #[serde(default)]
    pub regular_checked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Found,
    /// Archive for this month not yet published; retry on a later run.
    Pending,
    NoRoute,
    UnknownStation,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    NotClaimableOrOutOfScope,
    NeedsReview,
    SupportedForAuto,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::SupportedForAuto => "supported_for_auto",
            Confidence::NeedsReview => "needs_review",
            Confidence::NotClaimableOrOutOfScope => "not_claimable_or_out_of_scope",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "supported_for_auto" => Some(Self::SupportedForAuto),
            "needs_review" => Some(Self::NeedsReview),
            "not_claimable_or_out_of_scope" => Some(Self::NotClaimableOrOutOfScope),
            _ => None,
        }
    }
}

/// NS form radio choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelayBand {
    /// "30 t/m 59 min"
    Min30To59,
    /// "60 minuten of meer"
    Min60Plus,
}

impl DelayBand {
    pub fn from_minutes(m: i64) -> Option<Self> {
        match m {
            30..=59 => Some(Self::Min30To59),
            m if m >= 60 => Some(Self::Min60Plus),
            _ => None,
        }
    }
    pub fn label_nl(&self) -> &'static str {
        match self {
            Self::Min30To59 => "30 t/m 59 min",
            Self::Min60Plus => "60 minuten of meer",
        }
    }
    pub fn label_en(&self) -> &'static str {
        match self {
            Self::Min30To59 => "30–59 min",
            Self::Min60Plus => "60 min or more",
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Min30To59 => "30_59",
            Self::Min60Plus => "60_plus",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "30_59" => Some(Self::Min30To59),
            "60_plus" => Some(Self::Min60Plus),
            _ => None,
        }
    }
}

/// Result of the eligibility policy for one journey.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    pub confidence: Confidence,
    /// Screening proxy: check-out minus scheduled arrival (NOT the actual train delay).
    pub checkout_minus_scheduled_arrival_min: Option<i64>,
    /// Evidence-based arrival delay: actual arrival of taken route minus planned arrival.
    pub estimated_arrival_delay_min: Option<i64>,
    pub delay_band: Option<DelayBand>,
    /// Scheduled departure time (for the NS form field "Vertrektijd volgens dienstregeling").
    pub scheduled_departure: Option<DateTime<Utc>>,
    pub reasons: Vec<String>,
    pub expires_at: DateTime<Utc>,
}

/// Claim lifecycle (see SECURITY.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaimState {
    Discovered,
    Candidate,
    NeedsReview,
    Ready,
    Submitting,
    Submitted,
    Indeterminate,
    Failed,
    Approved,
    Rejected,
    Paid,
    /// Claimed outside this app (e.g. via another service) — blocks any new claim.
    ExternallyClaimed,
    /// User explicitly decided not to claim.
    Dismissed,
}

impl ClaimState {
    pub const ALL: [ClaimState; 13] = [
        Self::Discovered,
        Self::Candidate,
        Self::NeedsReview,
        Self::Ready,
        Self::Submitting,
        Self::Submitted,
        Self::Indeterminate,
        Self::Failed,
        Self::Approved,
        Self::Rejected,
        Self::Paid,
        Self::ExternallyClaimed,
        Self::Dismissed,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Discovered => "DISCOVERED",
            Self::Candidate => "CANDIDATE",
            Self::NeedsReview => "NEEDS_REVIEW",
            Self::Ready => "READY",
            Self::Submitting => "SUBMITTING",
            Self::Submitted => "SUBMITTED",
            Self::Indeterminate => "INDETERMINATE",
            Self::Failed => "FAILED",
            Self::Approved => "APPROVED",
            Self::Rejected => "REJECTED",
            Self::Paid => "PAID",
            Self::ExternallyClaimed => "EXTERNALLY_CLAIMED",
            Self::Dismissed => "DISMISSED",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// States in which a live request may already have reached NS. Any of these
    /// blocks a new submission for the same journey.
    pub fn blocks_new_submission(&self) -> bool {
        matches!(
            self,
            Self::Submitting
                | Self::Submitted
                | Self::Indeterminate
                | Self::Approved
                | Self::Rejected
                | Self::Paid
                | Self::ExternallyClaimed
        )
    }

    /// Allowed transitions. Anything not listed is rejected by the ledger.
    pub fn can_transition_to(&self, to: ClaimState) -> bool {
        use ClaimState::*;
        matches!(
            (self, to),
            (Discovered, Candidate | NeedsReview | Dismissed | ExternallyClaimed)
                | (Candidate, NeedsReview | Ready | Dismissed | ExternallyClaimed | Candidate)
                | (NeedsReview, Ready | Dismissed | ExternallyClaimed | NeedsReview | Candidate)
                | (Ready, Submitting | NeedsReview | Dismissed | ExternallyClaimed | Candidate)
                | (Submitting, Submitted | Indeterminate | Failed)
                // Failed = conclusively NOT received by NS (e.g. client-side validation error before submit).
                | (Failed, Ready | NeedsReview | Dismissed | ExternallyClaimed)
                // Indeterminate is only resolved by reconciliation or explicit user confirmation.
                | (Indeterminate, Submitted | Failed | Approved | Rejected)
                | (Submitted, Approved | Rejected | Paid)
                | (Approved, Paid)
                // NS reversed a rejection (objection), learned from an NS e-mail with the exact
                // application number, NS's claim list (exact match) or the user.
                | (Rejected, Approved | Paid)
                | (Dismissed, Candidate | NeedsReview | Ready | ExternallyClaimed)
                // undo "claimed elsewhere" (never sent by this app; see Db::undo_external)
                | (ExternallyClaimed, NeedsReview)
        )
    }
}

/// Facts that will be entered into the NS form. Stored verbatim in the ledger before submission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimFacts {
    pub source_record_id: String,
    pub coupling_id: String,
    pub check_in: DateTime<Utc>,
    pub check_out: DateTime<Utc>,
    pub origin_name: String,
    pub destination_name: String,
    /// Local time HH:MM (Europe/Amsterdam) for the form's "Vertrektijd volgens dienstregeling".
    pub scheduled_departure_local: String,
    pub delay_band: DelayBand,
    pub reached_destination: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizedMode {
    /// User reviewed and confirmed this exact claim.
    Manual,
    /// Submitted by an opted-in unattended run.
    Automatic,
}

impl AuthorizedMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmissionReceipt {
    /// NS reference when shown; `None` if not observed.
    pub reference: Option<String>,
    pub confirmed: bool,
    /// Redacted acknowledgement text / status metadata.
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: i64,
    pub mode: String,
    pub journeys_seen: usize,
    pub journeys_new: usize,
    pub candidates: usize,
    pub needs_review: usize,
    pub submitted: usize,
    pub failed: usize,
    /// Automatic submissions refused by the final check before sending (nothing reached NS),
    /// e.g. evidence no longer strong enough. Counted apart from `failed`.
    #[serde(default)]
    pub refused: usize,
    /// Automatic check from the app: claims that qualify for automatic submission but wait for
    /// a scheduled check (their claim window ends later than the urgent window).
    #[serde(default)]
    pub auto_later: usize,
    /// Claims not submitted automatically because NS shows a claim around the same date.
    #[serde(default)]
    pub near_skipped: usize,
    pub indeterminate: usize,
    pub approved_since_last: usize,
    pub pending_evidence: usize,
    /// Open claims (claimable / to review / not sent) whose 3-month claim window ends within 14 days.
    #[serde(default)]
    pub expiring_soon: usize,
    pub auth_state: String,
    pub errors: Vec<String>,
    /// Automatic submission is on, but this run did not submit because of a gate ("NL / EN").
    /// `None` when nothing paused it (or automatic submission is off).
    #[serde(default)]
    pub auto_paused_reason: Option<String>,
    /// The same as a code: "uncertain" (claims with an uncertain outcome), "claim_list" (NS claim
    /// list not available), "other_service", "service_unanswered", "bank", "unverified".
    #[serde(default)]
    pub auto_paused_code: Option<String>,
}

#[cfg(test)]
mod carrier_tests {
    #[test]
    fn carrier_pages() {
        use super::carrier_url as u;
        assert_eq!(u("NS International"), Some("https://www.nsinternational.com/nl/na-de-reis/compensatie-bij-vertraging"));
        assert_eq!(u("Arriva"), Some("https://www.arriva.nl"));
        assert_eq!(u("Arriva Breng"), Some("https://www.breng.nl"));
        assert_eq!(u("Keolis / Blauwnet"), Some("https://www.blauwnet.nl"));
        assert_eq!(u("QBUZZ"), Some("https://www.qbuzz.nl"));
        assert_eq!(u("Arretje"), None);
        assert_eq!(u(""), None);
    }
}
