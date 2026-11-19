//! Rough progress of a check (0-100 % plus a short stage label), for the GUI's progress ring.
//! The app installs a sink for the duration of a run; without a sink, reports are ignored.
use std::sync::Mutex;

/// Monthly metadata stays structured all the way to the GUI event boundary.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Progress {
    pub pct: u8,
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    /// Seconds left in the pause between two automatic submissions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    #[serde(skip)]
    pub month_done: bool,
    /// An automatic submission reached NS: the GUI refreshes the claim states.
    #[serde(skip)]
    pub claim_done: bool,
}

type Sink = Box<dyn Fn(&Progress) + Send>;
static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Install (Some) or remove (None) the progress sink.
pub fn set_sink(sink: Option<Sink>) {
    if let Ok(mut s) = SINK.lock() {
        *s = sink;
    }
}

/// Report progress; `pct` is clamped to 0..=100.
pub fn report(pct: u8, stage: &str) {
    emit(Progress { pct: pct.min(100), stage: stage.into(), ..blank() });
}

fn blank() -> Progress {
    Progress { pct: 0, stage: String::new(), month: None, index: None, total: None, seconds: None, month_done: false, claim_done: false }
}

/// Automatic submission `index` of `total` (1-based) starts.
pub fn submission(pct: u8, index: usize, total: usize) {
    emit(Progress { pct: pct.min(100), stage: "submit".into(), index: Some(index), total: Some(total), ..blank() });
}

/// Automatic submission `index` of `total` reached NS.
pub fn claim_done(pct: u8, index: usize, total: usize) {
    emit(Progress { pct: pct.min(100), stage: "submit".into(), index: Some(index), total: Some(total), claim_done: true, ..blank() });
}

/// Pause before automatic submission `index` of `total`, `seconds` left.
pub fn pause(pct: u8, index: usize, total: usize, seconds: u64) {
    emit(Progress { pct: pct.min(100), stage: "pause".into(), index: Some(index), total: Some(total), seconds: Some(seconds), ..blank() });
}

pub fn timetable(pct: u8, month: &str, index: usize, total: usize, month_done: bool) {
    emit(Progress {
        pct: pct.min(100),
        stage: "timetable".into(),
        month: Some(month.into()),
        index: Some(index),
        total: Some(total),
        month_done,
        ..blank()
    });
}

fn emit(progress: Progress) {
    if let Ok(s) = SINK.lock() {
        if let Some(f) = s.as_ref() {
            f(&progress);
        }
    }
}

/// Map step `i` of `n` into the range `from..to` (both in %).
pub fn within(from: u8, to: u8, i: usize, n: usize) -> u8 {
    if n == 0 {
        return to;
    }
    let span = to.saturating_sub(from) as f64;
    (from as f64 + span * (i.min(n) as f64 / n as f64)).round() as u8
}

#[cfg(test)]
mod tests {
    #[test]
    fn within_maps_steps_into_range() {
        assert_eq!(super::within(30, 80, 0, 4), 30);
        assert_eq!(super::within(30, 80, 2, 4), 55);
        assert_eq!(super::within(30, 80, 4, 4), 80);
        assert_eq!(super::within(30, 80, 9, 4), 80);
        assert_eq!(super::within(30, 80, 0, 0), 80);
    }
}
