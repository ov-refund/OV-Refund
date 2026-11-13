//! Station lookup (UIC code ↔ Rijden de Treinen station code).
//!
//! Data: Rijden de Treinen station list (CC0), `stations-2023-09-nl.csv`, which
//! the publisher extracted from the NS API. Embedded at build time.
use once_cell::sync::Lazy;
use std::collections::HashMap;

static RAW: &str = include_str!("../assets/stations-nl.csv");

#[derive(Debug, Clone)]
pub struct Station {
    pub code: String,
    pub uic: String,
    pub name: String,
}

pub struct StationIndex {
    by_uic: HashMap<String, Station>,
    by_code: HashMap<String, Station>,
    /// Short, medium and long names of every station, with its UIC code (for typed station names).
    names: Vec<(String, String)>,
}

pub static STATIONS: Lazy<StationIndex> = Lazy::new(|| StationIndex::from_csv(RAW).expect("embedded station CSV parses"));

impl StationIndex {
    pub fn from_csv(raw: &str) -> anyhow::Result<Self> {
        let mut rdr = csv::Reader::from_reader(raw.as_bytes());
        let mut by_uic = HashMap::new();
        let mut by_code = HashMap::new();
        let mut names = vec![];
        for rec in rdr.records() {
            let rec = rec?;
            names.extend([3, 4, 5].iter().map(|&i| (rec[i].to_string(), rec[2].to_string())).filter(|(n, _)| !n.trim().is_empty()));
            // id,code,uic,name_short,name_medium,name_long,...
            let st = Station { code: rec[1].to_string(), uic: rec[2].to_string(), name: rec[5].to_string() };
            by_uic.insert(st.uic.clone(), st.clone());
            by_code.insert(st.code.clone(), st);
        }
        Ok(Self { by_uic, by_code, names })
    }

    /// Does a typed station name (e.g. "Leiden", "Den Bosch", "Amersfoort C") match a known station?
    pub fn knows(&self, typed: &str) -> bool {
        self.names.iter().any(|(n, _)| crate::db::station_matches(typed, n))
    }

    /// The stations a typed name stands for: its short, medium or long name (case and
    /// abbreviations like "C"/"CS" ignored, no partial words), or else a city name for its
    /// "Centraal" station ("Leiden" → Leiden Centraal). One station: unambiguous.
    pub fn resolve(&self, typed: &str) -> Vec<&Station> {
        let t = crate::db::station_words(typed);
        if t.is_empty() {
            return vec![];
        }
        let pick = |alias: bool| {
            let mut uics: Vec<&str> = self
                .names
                .iter()
                .filter(|(n, _)| {
                    let mut w = crate::db::station_words(n);
                    if alias {
                        if w.last().map(String::as_str) != Some("centraal") {
                            return false;
                        }
                        w.pop();
                    }
                    w == t
                })
                .map(|(_, u)| u.as_str())
                .collect();
            uics.sort();
            uics.dedup();
            uics.into_iter().filter_map(|u| self.by_uic.get(u)).collect::<Vec<_>>()
        };
        let exact = pick(false);
        if exact.is_empty() {
            pick(true)
        } else {
            exact
        }
    }

    pub fn by_uic(&self, uic: &str) -> Option<&Station> {
        self.by_uic.get(uic)
    }

    pub fn by_code(&self, code: &str) -> Option<&Station> {
        self.by_code.get(code)
    }

    pub fn len(&self) -> usize {
        self.by_uic.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_uic.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_stations_resolve() {
        assert_eq!(STATIONS.by_uic("8400055").unwrap().code, "AMF");
        assert_eq!(STATIONS.by_uic("8400319").unwrap().code, "HT");
        assert_eq!(STATIONS.by_uic("8400058").unwrap().code, "ASD");
        assert!(STATIONS.len() > 380);
    }

    #[test]
    fn typed_station_names_resolve() {
        for typed in ["Leiden", "Leiden Centraal", "Den Bosch", "'s-Hertogenbosch", "Amersfoort C", "utrecht"] {
            assert!(STATIONS.knows(typed), "{typed}");
        }
        for typed in ["Atlantis", "Leidenx", "Utrecht Maan"] {
            assert!(!STATIONS.knows(typed), "{typed}");
        }
    }

    #[test]
    fn a_typed_name_resolves_to_one_station_or_none() {
        let one = |typed: &str| {
            let r = STATIONS.resolve(typed);
            assert_eq!(r.len(), 1, "{typed}: {r:?}");
            r[0].name.clone()
        };
        assert_eq!(one("Leiden"), "Leiden Centraal");
        assert_eq!(one("leiden centraal"), "Leiden Centraal");
        assert_eq!(one("Utrecht C"), "Utrecht Centraal");
        assert_eq!(one("Utrecht"), "Utrecht Centraal");
        assert_eq!(one("Den Bosch"), "'s-Hertogenbosch");
        assert_eq!(one("Amersfoort CS"), "Amersfoort Centraal");
        assert_eq!(one("Leiden Lammenschans"), "Leiden Lammenschans");
        // No prefixes of words: one or two letters never pick a station.
        for typed in ["L", "U", "Ut", "Leid", "Utrecht Ov", "", "Atlantis"] {
            assert!(STATIONS.resolve(typed).is_empty(), "{typed}");
        }
    }
}
