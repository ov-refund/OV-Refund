//! Historical train archive (Rijden de Treinen open data, CC BY 4.0).
//!
//! Monthly gzip CSV: `https://opendata.rijdendetreinen.nl/public/services/services-YYYY-MM.csv.gz`.
//! One row per stop; rows of one service are consecutive. We stream-parse and keep
//! only services touching stations the user actually travelled from/to, so the
//! local cache stays small (a few MB) while still allowing 1-transfer routing.
use crate::model::ArchiveStop;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use flate2::read::GzDecoder;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

pub const ARCHIVE_BASE: &str = "https://opendata.rijdendetreinen.nl/public/services";
pub const PROVIDER: &str = "rijdendetreinen-archive";
pub const ATTRIBUTION: &str = "Train data: Rijden de Treinen (rijdendetreinen.nl), CC BY 4.0";

pub fn month_key(d: NaiveDate) -> String {
    d.format("%Y-%m").to_string()
}

pub fn month_url(month: &str) -> String {
    format!("{ARCHIVE_BASE}/services-{month}.csv.gz")
}

/// Open a month archive as a stream (gzip bytes straight from HTTPS; nothing is written to disk).
/// `None` when the month has not been published yet.
pub fn open_month(month: &str) -> Result<Option<Box<dyn Read + Send>>> {
    validate_month(month)?;
    let url = month_url(month);
    match ureq::get(&url).timeout(std::time::Duration::from_secs(600)).call() {
        Ok(r) => Ok(Some(Box::new(r.into_reader().take(500 * 1024 * 1024)))),
        Err(ureq::Error::Status(404, _)) | Err(ureq::Error::Status(403, _)) => Ok(None),
        Err(e) => Err(e).context("archive download failed"),
    }
}

/// All archive columns of some trains, as published, plus when the month file was downloaded.
pub struct MonthRows {
    /// Per (service date, train number): one object per stop, column name to value, exactly as in the CSV.
    pub services: HashMap<(NaiveDate, String), Vec<serde_json::Value>>,
    pub url: String,
    /// Time the download finished.
    pub retrieved: DateTime<Utc>,
    /// The server's `Last-Modified` header of the month file, when sent.
    pub last_modified: Option<String>,
}

/// Rows of the `wanted` (service date, train number) pairs from a month CSV (gzip or plain). Stops
/// reading after the last wanted date (rows are sorted by date).
pub fn select_services<R: Read>(
    reader: R,
    gz: bool,
    wanted: &HashSet<(NaiveDate, String)>,
) -> Result<HashMap<(NaiveDate, String), Vec<serde_json::Value>>> {
    let reader: Box<dyn Read> = if gz { Box::new(GzDecoder::new(reader)) } else { Box::new(reader) };
    let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_reader(reader);
    let headers = rdr.headers()?.clone();
    let col = |name: &str| headers.iter().position(|h| h == name).with_context(|| format!("archive column missing: {name}"));
    let (c_date, c_num) = (col("Service:Date")?, col("Service:Train number")?);
    let last = wanted.iter().map(|w| w.0).max();
    let mut out: HashMap<(NaiveDate, String), Vec<serde_json::Value>> = HashMap::new();
    let mut rec = csv::StringRecord::new();
    while rdr.read_record(&mut rec)? {
        let Ok(date) = NaiveDate::parse_from_str(&rec[c_date], "%Y-%m-%d") else { continue };
        if last.is_some_and(|l| date > l) {
            break;
        }
        let key = (date, rec[c_num].to_string());
        if wanted.contains(&key) {
            let row = serde_json::Value::Object(headers.iter().zip(rec.iter()).map(|(h, v)| (h.to_string(), v.into())).collect());
            out.entry(key).or_default().push(row);
        }
    }
    Ok(out)
}

/// Downloads one month file and keeps the rows of the `wanted` trains. `None` when the month is not
/// published.
pub fn month_rows(month: &str, wanted: &HashSet<(NaiveDate, String)>) -> Result<Option<MonthRows>> {
    validate_month(month)?;
    let url = month_url(month);
    let resp = match ureq::get(&url).timeout(std::time::Duration::from_secs(600)).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) | Err(ureq::Error::Status(403, _)) => return Ok(None),
        Err(e) => return Err(e).context("archive download failed"),
    };
    let last_modified = resp.header("Last-Modified").map(str::to_string);
    let services = select_services(resp.into_reader().take(500 * 1024 * 1024), true, wanted)?;
    Ok(Some(MonthRows { services, url, retrieved: Utc::now(), last_modified }))
}

/// One train on one service date, downloaded now. `None` when the month is not published or the
/// train is not in it.
pub fn service_record(date: NaiveDate, train_number: &str) -> Result<Option<(Vec<serde_json::Value>, MonthRows)>> {
    let key = (date, train_number.to_string());
    let Some(mut m) = month_rows(&month_key(date), &HashSet::from([key.clone()]))? else { return Ok(None) };
    Ok(m.services.remove(&key).map(|rows| (rows, m)))
}

/// Settings key under which the time of the last download of a month file is kept.
pub fn retrieved_key(month: &str) -> String {
    format!("archive_retrieved:{month}")
}

struct Counted<R> {
    reader: R,
    bytes: Arc<AtomicU64>,
}
impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// One streaming GET; only selected services survive parsing. No raw bytes are cached.
pub(crate) fn download_month(
    month: &str,
    stations: &HashSet<String>,
    dates: &HashSet<NaiveDate>,
    early_stop: bool,
) -> Result<Option<(Vec<Vec<ArchiveStop>>, u64)>> {
    let Some(reader) = open_month(month)? else { return Ok(None) };
    let bytes = Arc::new(AtomicU64::new(0));
    let reader = Counted { reader, bytes: bytes.clone() };
    let mut services = vec![];
    let last = early_stop.then(|| dates.iter().max().copied()).flatten();
    scan_until(reader, true, stations, last, |s| {
        if dates.contains(&s[0].service_date) {
            services.push(s.to_vec());
        }
        Ok(())
    })?;
    Ok(Some((services, bytes.load(Ordering::Relaxed))))
}

#[cfg(test)]
#[allow(dead_code)] // Used by the source-including live integration harness.
pub(crate) fn verify_full_month(
    month: &str,
    stations: &HashSet<String>,
    sink: impl FnMut(&[ArchiveStop]) -> Result<()>,
) -> Result<Option<u64>> {
    let Some(reader) = open_month(month)? else { return Ok(None) };
    let bytes = Arc::new(AtomicU64::new(0));
    scan_archive(Counted { reader, bytes: bytes.clone() }, true, stations, sink)?;
    Ok(Some(bytes.load(Ordering::Relaxed)))
}

/// Stream-parse a (gzipped) archive, calling `sink` with every stop of every
/// service that touches at least one station in `stations`.
pub fn scan_archive<R: Read>(
    reader: R,
    gz: bool,
    stations: &HashSet<String>,
    sink: impl FnMut(&[ArchiveStop]) -> Result<()>,
) -> Result<usize> {
    scan_until(reader, gz, stations, None, sink)
}

/// Dates are sorted ascending. Drop the entire reader at the first later row,
/// after flushing the last complete wanted service (including all its stops).
pub(crate) fn scan_until<R: Read>(
    reader: R,
    gz: bool,
    stations: &HashSet<String>,
    last: Option<NaiveDate>,
    mut sink: impl FnMut(&[ArchiveStop]) -> Result<()>,
) -> Result<usize> {
    let reader: Box<dyn Read> = if gz { Box::new(GzDecoder::new(reader)) } else { Box::new(reader) };
    let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_reader(reader);
    let headers = rdr.headers()?.clone();
    let col =
        |name: &str| -> Result<usize> { headers.iter().position(|h| h == name).with_context(|| format!("archive column missing: {name}")) };
    let c_sid = col("Service:RDT-ID")?;
    let c_date = col("Service:Date")?;
    let c_type = col("Service:Type")?;
    let c_comp = col("Service:Company")?;
    let c_num = col("Service:Train number")?;
    let c_stop = col("Stop:RDT-ID")?;
    let c_code = col("Stop:Station code")?;
    let c_arr = col("Stop:Arrival time")?;
    let c_arrd = col("Stop:Arrival delay")?;
    let c_arrc = col("Stop:Arrival cancelled")?;
    let c_dep = col("Stop:Departure time")?;
    let c_depd = col("Stop:Departure delay")?;
    let c_depc = col("Stop:Departure cancelled")?;

    let mut current: Vec<ArchiveStop> = Vec::new();
    let mut touches = false;
    let mut kept = 0usize;
    let mut flush = |cur: &mut Vec<ArchiveStop>, touches: &mut bool, kept: &mut usize| -> Result<()> {
        if *touches && !cur.is_empty() {
            sink(cur)?;
            *kept += 1;
        }
        cur.clear();
        *touches = false;
        Ok(())
    };
    let mut rec = csv::StringRecord::new();
    while rdr.read_record(&mut rec)? {
        let date = NaiveDate::parse_from_str(&rec[c_date], "%Y-%m-%d")?;
        if last.is_some_and(|last| date > last) {
            break;
        }
        let sid: i64 = rec[c_sid].parse().context("invalid archive service ID")?;
        if current.first().map(|s| (s.service_id, s.service_date)) != Some((sid, date)) {
            flush(&mut current, &mut touches, &mut kept)?;
        }
        let code = rec[c_code].to_string();
        if stations.contains(&code) {
            touches = true;
        }
        current.push(ArchiveStop {
            service_id: sid,
            service_date: date,
            train_type: rec[c_type].to_string(),
            company: rec[c_comp].to_string(),
            train_number: rec[c_num].to_string(),
            station_code: code,
            stop_seq: rec[c_stop].parse().unwrap_or(0),
            arr_planned: parse_ts(&rec[c_arr]),
            arr_delay_min: rec[c_arrd].parse().ok(),
            arr_cancelled: &rec[c_arrc] == "true",
            dep_planned: parse_ts(&rec[c_dep]),
            dep_delay_min: rec[c_depd].parse().ok(),
            dep_cancelled: &rec[c_depc] == "true",
        });
    }
    flush(&mut current, &mut touches, &mut kept)?;
    Ok(kept)
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

/// Validate that a month string is `YYYY-MM`.
pub fn validate_month(m: &str) -> Result<()> {
    if NaiveDate::parse_from_str(&format!("{m}-01"), "%Y-%m-%d").is_err() {
        bail!("invalid month {m}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const SAMPLE: &str = include_str!("../tests/fixtures/archive_sample.csv");

    #[test]
    fn selects_wanted_trains_with_all_columns_and_stops_after_the_last_date() {
        let csv = "Service:Date,Service:Train number,Stop:Station code,Stop:Arrival delay\n\
                   2026-10-28,100,VL,1\n2026-10-29,732284,VL,\n2026-10-29,732284,NM,24\n2026-10-29,999,NM,3\n2026-10-30,732284,VL,0\nbroken";
        let d = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        let wanted = HashSet::from([(d, "732284".to_string())]);
        let got = select_services(csv.as_bytes(), false, &wanted).unwrap();
        let rows = &got[&(d, "732284".to_string())];
        assert_eq!(got.len(), 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["Stop:Station code"], "NM");
        assert_eq!(rows[1]["Stop:Arrival delay"], "24");
    }
    #[test]
    fn keeps_only_services_touching_stations() {
        let st: HashSet<String> = ["NM".to_string()].into();
        let mut services = vec![];
        let n = scan_archive(SAMPLE.as_bytes(), false, &st, |s| {
            services.push(s.to_vec());
            Ok(())
        })
        .unwrap();
        assert!(n >= 1);
        assert!(services.iter().all(|s| s.iter().any(|x| x.station_code == "NM")));
        let ic = services.iter().find(|s| s[0].train_number == "28569").unwrap();
        assert_eq!(ic.len(), 15, "all stops of a touching service are kept");
        assert_eq!(ic.iter().find(|s| s.station_code == "ZP").unwrap().dep_delay_min, Some(3));
    }
    #[test]
    fn malformed_service_ids_cannot_merge_unrelated_trains() {
        let malformed = SAMPLE.replace("19461215,", "invalid,").replace("19461216,", "invalid,");
        let stations = ["AMF".into(), "HT".into()].into();
        let mut delivered = false;
        assert!(scan_archive(malformed.as_bytes(), false, &stations, |_| {
            delivered = true;
            Ok(())
        })
        .is_err());
        assert!(!delivered);
    }

    #[test]
    fn early_stop_flushes_complete_services_and_drops_reader() {
        use std::cell::Cell;
        use std::rc::Rc;
        struct Reader {
            data: std::io::Cursor<Vec<u8>>,
            read: Rc<Cell<usize>>,
            dropped: Rc<Cell<bool>>,
        }
        impl Read for Reader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let len = buf.len().min(64);
                let n = self.data.read(&mut buf[..len])?;
                self.read.set(self.read.get() + n);
                Ok(n)
            }
        }
        impl Drop for Reader {
            fn drop(&mut self) {
                self.dropped.set(true);
            }
        }
        let (header, rows) = SAMPLE.split_once('\n').unwrap();
        let csv = format!(
            "{header}\n{}{}{}",
            rows.replace("2026-10-28", "2026-10-01"),
            rows.replace("2026-10-28", "2026-10-02"),
            rows.repeat(100)
        );
        let stations = ["NM".to_string()].into();
        let last = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        let mut expected = vec![];
        scan_archive(csv.as_bytes(), false, &stations, |s| {
            if s[0].service_date <= last {
                expected.push(s.to_vec());
            }
            Ok(())
        })
        .unwrap();
        assert!(!expected.is_empty());
        let count = Rc::new(Cell::new(0));
        let dropped = Rc::new(Cell::new(false));
        let reader = Reader { data: std::io::Cursor::new(csv.clone().into_bytes()), read: count.clone(), dropped: dropped.clone() };
        let mut actual = vec![];
        scan_until(reader, false, &stations, Some(last), |s| {
            actual.push(s.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(actual, expected);
        assert!(dropped.get());
        assert!(count.get() < csv.len() / 10, "later dates must remain unread");
        // The same boundary must work through a single gzip member.
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(csv.as_bytes()).unwrap();
        let gz = gz.finish().unwrap();
        actual.clear();
        scan_until(gz.as_slice(), true, &stations, Some(last), |s| {
            actual.push(s.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(actual, expected);
    }
}
