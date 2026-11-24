//! Developer tool: assess a captured Mijn NS transactions body (local file) against
//! the historical archive. Prints dates, stations and delays only.
//!   cargo run -p ovrefund-core --example analyze -- <body.json> <archive-cache-dir> [coupling-id]
use chrono::Utc;
use chrono_tz::Europe::Amsterdam;
use ovrefund_core::db::Db;
use ovrefund_core::eligibility::{assess, ClaimWindow, PolicyInput};
use ovrefund_core::ns_parse::parse_transactions;
use ovrefund_core::schedule::{ArchiveScheduleProvider, ScheduleProvider};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let body = std::fs::read_to_string(&args[1])?;
    let rep = parse_transactions(&body, args.get(3).map(|s| s.as_str()))?;
    let now = Utc::now();
    let window = ClaimWindow::default();
    let recent: Vec<_> = rep.journeys.into_iter().filter(|j| now - j.check_in < chrono::Duration::days(100)).collect();
    let mut db = Db::open_in_memory()?;
    let _ = &args[2];
    let mut sched = ArchiveScheduleProvider::new();
    let ns: Vec<_> = recent.iter().filter(|j| j.is_ns_carrier() && j.check_out.is_some()).cloned().collect();
    for l in sched.prepare(&mut db, &ns)? {
        eprintln!("{l}");
    }
    println!("{:<17} {:<40} {:>6} {:>6}  {:<30} reason", "check-in (local)", "route", "proxy", "delay", "confidence");
    for j in &recent {
        let ev = sched.evidence(&db, j)?;
        let a = assess(&PolicyInput { journey: j, evidence: &ev, now, window, already_claimed: false });
        let route = format!("{} → {}", j.origin.name, j.destination.as_ref().map(|d| d.name.as_str()).unwrap_or("?"));
        let trains =
            ev.taken.as_ref().map(|t| t.legs.iter().map(|l| l.train_number.clone()).collect::<Vec<_>>().join("+")).unwrap_or_default();
        println!(
            "{:<17} {:<40} {:>6} {:>6}  {:<30} {} {}",
            j.check_in.with_timezone(&Amsterdam).format("%Y-%m-%d %H:%M"),
            route.chars().take(40).collect::<String>(),
            a.checkout_minus_scheduled_arrival_min.map(|v| v.to_string()).unwrap_or("-".into()),
            a.estimated_arrival_delay_min.map(|v| v.to_string()).unwrap_or("-".into()),
            a.confidence.as_str(),
            a.reasons.first().cloned().unwrap_or_default(),
            if trains.is_empty() { String::new() } else { format!("[{trains}]") }
        );
        if a.estimated_arrival_delay_min.unwrap_or(0) >= 25 || std::env::var("VERBOSE").is_ok() {
            if let Some(p) = &ev.planned {
                println!("      planned: {}", fmt_route(p));
            }
            if let Some(t) = &ev.taken {
                println!("      taken:   {}", fmt_route(t));
            }
            println!(
                "      check-out: {}  notes: {:?}",
                j.check_out.map(|c| c.with_timezone(&Amsterdam).format("%H:%M:%S").to_string()).unwrap_or_default(),
                ev.notes
            );
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn fmt_route(r: &ovrefund_core::model::Route) -> String {
    r.legs
        .iter()
        .map(|l| {
            format!(
                "{} {} {} {}→{} dep {}({:+}) arr {}({:+}){}",
                l.company,
                l.train_type,
                l.train_number,
                l.from_code,
                l.to_code,
                l.dep_planned.with_timezone(&Amsterdam).format("%H:%M"),
                (l.dep_actual - l.dep_planned).num_minutes(),
                l.arr_planned.with_timezone(&Amsterdam).format("%H:%M"),
                (l.arr_actual - l.arr_planned).num_minutes(),
                if l.cancelled { " CANCELLED" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}
