//! Parser for the Mijn NS transaction-history JSON, as observed on 2026-11-12
//! (`mijnns-transaction-api/couplings/{id}/transactions`). See
//! `docs/integration-inventory.md`. Unknown fields are ignored; required fields
//! missing → record skipped with a reason (never guessed).
use crate::model::{Journey, StationRef};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
struct CouplingTransactions {
    #[serde(rename = "tokenId")]
    _token_id: Option<String>,
    transactions: TransactionPage,
}

#[derive(Debug, Deserialize)]
struct TransactionPage {
    #[serde(default)]
    transactions: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Tx {
    id: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "tokenValue")]
    token_value: Option<String>,
    product: Option<Product>,
    #[serde(rename = "travelClass")]
    travel_class: Option<u8>,
    amount: Option<i64>,
    #[serde(rename = "isInvoiced")]
    is_invoiced: Option<bool>,
    journey: Option<TxJourney>,
}

#[derive(Debug, Deserialize)]
struct Product {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TxJourney {
    departure: Option<Event>,
    arrival: Option<Event>,
    carrier: Option<Carrier>,
    subscription: Option<Subscription>,
}

#[derive(Debug, Deserialize)]
struct Event {
    timestamp: Option<DateTime<Utc>>,
    location: Option<Location>,
}

#[derive(Debug, Deserialize)]
struct Location {
    name: Option<String>,
    #[serde(rename = "uicCode")]
    uic_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Carrier {
    code: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Subscription {
    description: Option<String>,
    monthlyfee: Option<i64>,
}

#[derive(Debug, Default)]
pub struct ParseReport {
    pub journeys: Vec<Journey>,
    pub skipped: Vec<String>,
}

/// Parse one captured response body. `coupling_id` comes from the request URL.
pub fn parse_transactions(body: &str, coupling_id: Option<&str>) -> anyhow::Result<ParseReport> {
    let pages: Vec<CouplingTransactions> = serde_json::from_str(body)?;
    let mut out = ParseReport::default();
    for page in pages {
        for raw in page.transactions.transactions {
            match parse_one(&raw, coupling_id) {
                Ok(Some(j)) => out.journeys.push(j),
                Ok(None) => {}
                Err(e) => out.skipped.push(e),
            }
        }
    }
    Ok(out)
}

fn parse_one(raw: &serde_json::Value, coupling_id: Option<&str>) -> Result<Option<Journey>, String> {
    let tx: Tx = serde_json::from_value(raw.clone()).map_err(|_| "unparseable transaction".to_string())?;
    if tx.kind.as_deref() != Some("JOURNEY") {
        return Ok(None);
    }
    let j = tx.journey.ok_or_else(|| "JOURNEY without journey block".to_string())?;
    let dep = j.departure.ok_or_else(|| "no departure".to_string())?;
    let check_in = dep.timestamp.ok_or_else(|| "no check-in time".to_string())?;
    let origin = station(dep.location).ok_or_else(|| "no origin station".to_string())?;
    let (check_out, destination) = match j.arrival {
        Some(a) => (a.timestamp, station(a.location)),
        None => (None, None),
    };
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(raw).unwrap_or_default());
    Ok(Some(Journey {
        source_id: "mijnns".into(),
        source_record_id: tx.id,
        coupling_id: coupling_id.map(str::to_string),
        card_last4: tx.token_value.as_deref().and_then(last4),
        check_in,
        check_out,
        origin,
        destination,
        carrier_code: j.carrier.as_ref().and_then(|c| c.code.clone()),
        carrier_name: j.carrier.as_ref().and_then(|c| c.description.clone()),
        product_name: tx.product.and_then(|p| p.name),
        travel_class: tx.travel_class,
        fare_cents: tx.amount.filter(|a| *a > 0),
        subscription_fee_cents: j.subscription.as_ref().and_then(|s| s.monthlyfee),
        subscription: j.subscription.and_then(|s| s.description),
        invoiced: tx.is_invoiced,
        payload_sha256: hex::encode(hasher.finalize()),
    }))
}

fn station(loc: Option<Location>) -> Option<StationRef> {
    let l = loc?;
    Some(StationRef { uic: l.uic_code?, name: l.name.unwrap_or_default() })
}

fn last4(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    (digits.len() >= 4).then(|| digits[digits.len() - 4..].to_string())
}

/// Extract the coupling id from a transactions request URL.
pub fn coupling_id_from_url(url: &str) -> Option<String> {
    let re = regex::Regex::new(r"/couplings/([0-9a-fA-F-]{36})/transactions").ok()?;
    re.captures(url).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/mijnns_transactions_sanitized.json");

    #[test]
    fn parse_errors_do_not_contain_transaction_values() {
        for raw in [
            serde_json::json!({"id":"private-trip", "type":"JOURNEY"}),
            serde_json::json!({"id":"private-trip", "type":"JOURNEY", "journey":{"departure":{}}}),
            serde_json::json!({"id":{"payload":"private-trip"}, "type":"JOURNEY"}),
        ] {
            let error = parse_one(&raw, None).unwrap_err();
            assert!(!error.contains("private-trip") && !error.contains("payload"), "{error}");
        }
    }

    #[test]
    fn parses_sanitized_fixture() {
        let r = parse_transactions(FIXTURE, Some("00000000-0000-0000-0000-000000000000")).unwrap();
        assert_eq!(r.journeys.len(), 3, "skipped: {:?}", r.skipped);
        let j = &r.journeys[0];
        assert_eq!(j.origin.uic, "8400750");
        assert_eq!(j.destination.as_ref().unwrap().uic, "8400320");
        assert_eq!(j.card_last4.as_deref(), Some("0000"));
        assert!(j.is_ns_carrier());
        assert_eq!(j.fare_cents, None, "null amount must stay unknown");
        // no check-out record
        assert!(r.journeys[2].check_out.is_none());
        // non-NS carrier
        assert!(!r.journeys[1].is_ns_carrier());
    }

    #[test]
    fn coupling_from_url() {
        let u = "https://gateway.apiportal.ns.nl/mijnns-transaction-api/couplings/12345678-aaaa-bbbb-cccc-1234567890ab/transactions?from=x";
        assert_eq!(coupling_id_from_url(u).unwrap(), "12345678-aaaa-bbbb-cccc-1234567890ab");
    }
}
