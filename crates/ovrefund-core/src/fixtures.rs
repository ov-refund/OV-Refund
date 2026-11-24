//! **FIXTURE / MOCK adapters — for tests and the demo mode only.**
//! They never touch NS. The app shows a permanent "DEMO DATA" banner when they
//! are in use and never falls back to them in production.
use crate::model::*;
use crate::pipeline::*;
use anyhow::Result;
use chrono::{DateTime, Utc};

pub struct FixtureJourneySource {
    pub journeys: Vec<Journey>,
    pub fail_with: Option<fn() -> FetchError>,
}

impl JourneySource for FixtureJourneySource {
    fn id(&self) -> &str {
        "fixture"
    }
    fn fetch_journeys(&mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> std::result::Result<Vec<Journey>, FetchError> {
        if let Some(f) = self.fail_with {
            return Err(f());
        }
        Ok(self.journeys.iter().filter(|j| j.check_in >= from && j.check_in <= to).cloned().collect())
    }
}

#[derive(Default)]
pub struct FixtureClaimGateway {
    pub verified: bool,
    pub submitted: Vec<ClaimFacts>,
    pub next_outcomes: Vec<&'static str>, // "ok" | "notsent" | "unknown" | "panic"
    pub remote: Vec<RemoteClaim>,
}

impl ClaimGateway for FixtureClaimGateway {
    fn id(&self) -> &str {
        "fixture"
    }
    fn verified_for_auto(&self) -> bool {
        self.verified
    }
    fn preflight(&mut self, _facts: &ClaimFacts) -> Result<Vec<String>> {
        Ok(vec!["fixture form ok".into()])
    }
    fn submit(&mut self, facts: &ClaimFacts) -> SubmitOutcome {
        let o = if self.next_outcomes.is_empty() { "ok" } else { self.next_outcomes.remove(0) };
        match o {
            "notsent" => SubmitOutcome::NotSent("fixture validation error".into()),
            "unknown" => {
                self.submitted.push(facts.clone());
                SubmitOutcome::Unknown("fixture timeout after click".into())
            }
            "panic" => panic!("simulated crash during submission"),
            _ => {
                self.submitted.push(facts.clone());
                SubmitOutcome::Confirmed(SubmissionReceipt {
                    reference: Some(format!("FIXTURE-{}", self.submitted.len())),
                    confirmed: true,
                    detail: "fixture acknowledgement".into(),
                })
            }
        }
    }
    fn list_remote_claims(&mut self) -> Result<Option<Vec<RemoteClaim>>> {
        Ok(Some(self.remote.clone()))
    }
}

#[derive(Default)]
pub struct RecordingNotifier {
    pub sent: Vec<RunSummary>,
    pub fail: bool,
}

impl Notifier for RecordingNotifier {
    fn channel(&self) -> &str {
        "test"
    }
    fn notify(&mut self, summary: &RunSummary, _lines: &[String]) -> Result<()> {
        if self.fail {
            anyhow::bail!("simulated notification failure");
        }
        self.sent.push(summary.clone());
        Ok(())
    }
}
