//! OV Refund core library. No GUI, no browser — pure logic + storage.
#![allow(clippy::type_complexity)] // small tuple rows from SQLite queries
pub mod archive;
pub mod compensation;
pub mod db;
pub mod eligibility;
pub mod email;
pub mod fixtures;
pub mod matcher;
pub mod model;
pub mod ns_mail;
pub mod ns_parse;
pub mod pipeline;
pub mod progress;
pub mod redact;
pub mod schedule;
pub mod stations;

/// First 8 hex characters of SHA-256 of `s` (stable short id, e.g. for per-user task names).
pub fn short_hash(s: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))[..8].to_string()
}
