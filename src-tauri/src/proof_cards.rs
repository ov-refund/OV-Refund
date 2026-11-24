//! Evidence cards for a provider claim, rendered as HTML and captured as PNG in a hidden window
//! (see arriva.rs). Pure functions: every value shown comes from the given record; nothing is
//! estimated or filled in. Light, neutral styling (no operator or NS branding).
use anyhow::{anyhow, Result};
use chrono::{DateTime, FixedOffset};
use serde_json::Value;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const STYLE: &str = r#"
*{box-sizing:border-box}html,body{margin:0;background:#fbf7ef}
body{font:14px/1.45 "Segoe UI Variable Text","Segoe UI",system-ui,sans-serif;color:#1d2433;font-feature-settings:"tnum" 1;padding:24px;width:840px}
#card{background:#fff;border:1px solid #e6dece;border-radius:14px;box-shadow:0 1px 2px rgba(40,32,10,.06),0 4px 16px rgba(40,32,10,.06);overflow:hidden}
.top{display:flex;justify-content:space-between;align-items:flex-start;gap:16px;padding:20px 24px 16px;border-bottom:1px solid #efe9dd}
.kicker{margin:0 0 4px;font-size:12px;font-weight:650;letter-spacing:.06em;text-transform:uppercase;color:#62666d}
h1{margin:0;font-size:22px;line-height:1.25}
.sub{margin:4px 0 0;color:#414a5a}
.chip{display:inline-block;padding:3px 10px;border-radius:999px;font-size:12px;font-weight:700;white-space:nowrap}
.chip.demo{background:#fff2ce;color:#6b4100;border:1px solid #dfb850}
.chip.op{background:#e3f3e9;color:#15563b}
.facts{display:grid;grid-template-columns:repeat(4,1fr);gap:0;border-bottom:1px solid #efe9dd}
.facts div{padding:12px 24px;border-right:1px solid #efe9dd}.facts div:last-child{border-right:0}
.facts b{display:block;font-size:12px;font-weight:600;color:#62666d}.facts span{font-size:15px;font-weight:600}
.callout{margin:16px 24px 0;padding:12px 16px;border-radius:10px;background:#fdecea;color:#7a1b12;border:1px solid #f3c2bc;font-weight:600}
.callout.ok{background:#eef7f1;color:#15563b;border-color:#cfe8d8}
table{width:calc(100% - 48px);margin:16px 24px 8px;border-collapse:collapse}
th{font-size:12px;font-weight:600;color:#62666d;text-align:left;padding:6px 8px;border-bottom:1px solid #e6dece}
td{padding:7px 8px;border-bottom:1px solid #efe9dd;vertical-align:top}
td.n,th.n{text-align:right}
tr.hl td{background:#fff6d9}tr.hl td:first-child{box-shadow:inset 3px 0 0 #c41e12}
.late{color:#c41e12;font-weight:700}.ontime{color:#17703b}.muted{color:#62666d}
.tag{display:inline-block;white-space:nowrap;margin-left:6px;padding:1px 7px;border-radius:999px;font-size:11px;font-weight:700;background:#fdecea;color:#b42318}
.tag.leg{background:#eef2fb;color:#1f4f9c}
.trip{display:grid;grid-template-columns:1fr auto 1fr;gap:16px;align-items:center;padding:20px 24px}
.trip .st b{display:block;font-size:12px;font-weight:600;color:#62666d}.trip .st strong{display:block;font-size:20px}.trip .st span{font-size:15px}
.trip .arrow{color:#62666d;font-size:22px}
.source{margin:12px 0 0;padding:14px 24px 18px;border-top:1px solid #efe9dd;background:#f7f2e8;font-size:12px;color:#414a5a}
.facts + .source{margin-top:0;border-top:0}
.source p{margin:2px 0}.source b{color:#1d2433}
"#;

fn page(body: &str) -> String {
    format!("<!doctype html><html lang=\"nl\"><head><meta charset=\"utf-8\"><style>{STYLE}</style></head><body><div id=\"card\">{body}</div></body></html>")
}

fn hm(t: &DateTime<FixedOffset>) -> String {
    t.format("%H:%M").to_string()
}

/// One trip from a travel history (demo data in the demo).
pub struct TravelTrip {
    pub date: String,
    pub from: String,
    pub to: String,
    pub check_in: DateTime<FixedOffset>,
    pub check_out: DateTime<FixedOffset>,
    pub operator: String,
    pub product: String,
    pub travel_class: Option<u8>,
    pub fare_cents: Option<i64>,
    /// e.g. "Demogegevens (fictief) / Demo data (fictional)".
    pub source: String,
    pub demo: bool,
}

/// Demo stand-in for a travel-history screenshot: one simulated history row with the same content
/// as a real row (date, check-in and check-out, stations, operator, fare). Neutral styling, no NS
/// branding, and marked as demo data.
pub fn demo_history_row(t: &TravelTrip) -> String {
    let fare = t.fare_cents.map(|c| format!("€ {},{:02}", c / 100, c % 100)).unwrap_or_else(|| "—".into());
    let class = t.travel_class.map(|c| format!("{c}e klas")).unwrap_or_default();
    let mins = (t.check_out - t.check_in).num_minutes();
    format!(
        r#"<!doctype html><html lang="nl"><head><meta charset="utf-8"><style>{STYLE}{ROW_STYLE}</style></head><body style="width:760px"><div id="card">
<div class="banner">{demo}<span>Gesimuleerde reishistorie-rij, geen schermafbeelding van een echte reis / Simulated travel-history row, not a screenshot of a real trip</span></div>
<div class="day">{date}</div>
<div class="row"><div class="times"><strong>{cin}</strong><span>{cout}</span></div>
<div class="line"><i></i><b></b><i></i></div>
<div class="where"><strong>{from}</strong><span>{to}</span></div>
<div class="meta"><span>{op} · {product}{class_sep}{class}</span><span>{mins} min</span></div>
<div class="fare">{fare}</div></div>
<div class="src">Bron / Source: {source}</div>
</div></body></html>"#,
        demo = if t.demo { r#"<span class="chip demo">DEMODATA / DEMO DATA</span>"# } else { "" },
        date = esc(&t.date),
        cin = hm(&t.check_in),
        cout = hm(&t.check_out),
        from = esc(&t.from),
        to = esc(&t.to),
        op = esc(&t.operator),
        product = esc(&t.product),
        class_sep = if class.is_empty() { "" } else { " · " },
        class = esc(&class),
        source = esc(&t.source),
    )
}
const ROW_STYLE: &str = r#"
#card{border-radius:10px}
.banner{display:flex;gap:10px;align-items:center;padding:10px 16px;background:#fff8e6;border-bottom:1px solid #efe2bd;font-size:12px;color:#6b4100}
.day{padding:10px 16px 6px;font-size:13px;font-weight:650;color:#414a5a;border-bottom:1px solid #efe9dd}
.row{display:grid;grid-template-columns:64px 18px 1fr auto auto;gap:12px;align-items:center;padding:14px 16px}
.times,.where{display:flex;flex-direction:column;gap:14px}.times strong,.times span{font-size:15px;font-weight:600}
.where strong,.where span{font-size:15px;font-weight:600}
.line{display:flex;flex-direction:column;align-items:center;height:52px}.line i{width:9px;height:9px;border-radius:50%;border:2px solid #5b6472}.line b{flex:1;width:2px;background:#c9ced6}
.meta{display:flex;flex-direction:column;gap:14px;text-align:right;color:#414a5a;font-size:13px}
.fare{font-size:16px;font-weight:700;min-width:80px;text-align:right}
.src{padding:8px 16px 10px;border-top:1px solid #efe9dd;font-size:12px;color:#62666d;background:#faf7f0}
"#;

/// Caption under a travel-history screenshot: the full OV-chipkaart number (as stored by the app),
/// the travel date and the route. Shown in its own strip below the (masked) screenshot.
pub struct Caption {
    pub card_number: Option<String>,
    pub date: String,
    pub route: String,
    pub demo: bool,
}

/// Groups a card number in fours ("3528 0000 0000 1234").
pub fn card_groups(n: &str) -> String {
    let digits: Vec<char> = n.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    digits.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

/// The caption strip as a page whose `#card` is exactly `width` CSS pixels wide.
pub fn caption_strip(c: &Caption, width: u32) -> String {
    let card = c.card_number.as_deref().map(card_groups).unwrap_or_else(|| "niet beschikbaar / not available".into());
    format!(
        r#"<!doctype html><html lang="nl"><head><meta charset="utf-8"><style>{STYLE}
body{{padding:0;width:{width}px}}#card{{width:{width}px;border-radius:0;border:0;box-shadow:none;border-top:3px solid #1d2433;background:#f4f6f9}}
.cap{{display:flex;flex-wrap:wrap;gap:6px 22px;padding:10px 14px 4px}}.cap b{{display:block;font-size:11px;font-weight:600;color:#62666d}}.cap span{{font-size:14px;font-weight:650}}
.cap .num span{{font-size:16px;letter-spacing:.04em}}.note{{padding:2px 14px 9px;font-size:11px;color:#62666d}}</style></head>
<body><div id="card"><div class="cap"><div class="num"><b>OV-chipkaartnummer / OV-chipkaart number</b><span>{card}</span></div>
<div><b>Reisdatum / Travel date</b><span>{date}</span></div><div><b>Traject / Route</b><span>{route}</span></div>{demo}</div>
<div class="note">Bijschrift toegevoegd door OV Refund. Andere reizen en persoonsgegevens zijn grijs afgedekt (verborgen); verder is de afbeelding niet bewerkt. / Caption added by OV Refund. Other trips and personal details are covered in grey (verborgen); the image is otherwise unedited.</div></div></body></html>"#,
        card = esc(&card),
        date = esc(&c.date),
        route = esc(&c.route),
        demo = if c.demo { r#"<div><b>&nbsp;</b><span class="chip demo">DEMODATA / DEMO DATA</span></div>"# } else { "" },
    )
}

fn decode_rgba(png_bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let mut d = png::Decoder::new(std::io::Cursor::new(png_bytes));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info()?;
    let mut buf = vec![0; r.output_buffer_size().ok_or_else(|| anyhow!("Afbeelding te groot. / Image too large."))?];
    let info = r.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err(anyhow!("Onverwacht afbeeldingsformaat. / Unexpected image format.")),
    };
    Ok((info.width, info.height, rgba))
}

/// Places `bottom` under `top` (white fill where widths differ). The pixels of both stay as they were.
pub fn stack_png(top: &[u8], bottom: &[u8]) -> Result<Vec<u8>> {
    let (tw, th, t) = decode_rgba(top)?;
    let (bw, bh, b) = decode_rgba(bottom)?;
    let w = tw.max(bw);
    let mut out = Vec::with_capacity((w * (th + bh) * 4) as usize);
    for (iw, ih, img) in [(tw, th, &t), (bw, bh, &b)] {
        for y in 0..ih as usize {
            out.extend_from_slice(&img[y * iw as usize * 4..(y + 1) * iw as usize * 4]);
            out.extend(std::iter::repeat_n(255u8, (w - iw) as usize * 4));
        }
    }
    let mut png_out = vec![];
    {
        let mut e = png::Encoder::new(&mut png_out, w, th + bh);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header()?;
        wr.write_image_data(&out)?;
        wr.finish()?;
    }
    Ok(png_out)
}

/// Where an open-data record came from.
pub struct RdtSource {
    pub dataset: String,
    pub url: String,
    /// Local time of the download, or the failure text.
    pub retrieved: String,
    /// The server's Last-Modified date of the file, when known.
    pub file_version: Option<String>,
}

fn field<'a>(r: &'a Value, k: &str) -> &'a str {
    r[k].as_str().unwrap_or("")
}
fn time(r: &Value, k: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(field(r, k)).ok()
}
fn delay(r: &Value, k: &str) -> Option<i64> {
    field(r, k).parse().ok()
}
fn cell(planned: Option<DateTime<FixedOffset>>, d: Option<i64>, cancelled: bool) -> (String, String, String) {
    match planned {
        None => ("—".into(), "—".into(), String::new()),
        Some(_) if cancelled => (hm(&planned.unwrap()), "—".into(), r#"<span class="tag">vervallen / cancelled</span>"#.into()),
        Some(p) => {
            let actual = d
                .and_then(chrono::Duration::try_minutes)
                .and_then(|offset| p.checked_add_signed(offset))
                .map(|time| hm(&time))
                .unwrap_or_else(|| "—".into());
            let dl = match d {
                Some(0) => r#"<span class="ontime">0</span>"#.to_string(),
                Some(d) => format!(r#"<span class="late">{d:+}</span>"#),
                None => "—".into(),
            };
            (hm(&p), actual, dl)
        }
    }
}

fn departure_matches(row: &Value, code: &str, planned: chrono::DateTime<chrono::Utc>) -> bool {
    field(row, "Stop:Station code") == code && time(row, "Stop:Departure time").is_some_and(|t| t == planned)
}

/// Select the unique archived service containing both ends of the matched leg.
pub fn service_for_leg(rows: &[Value], leg: &ovrefund_core::model::Leg) -> Option<Vec<Value>> {
    let ids: std::collections::BTreeSet<_> = rows
        .iter()
        .filter(|r| departure_matches(r, &leg.from_code, leg.dep_planned))
        .map(|r| field(r, "Service:RDT-ID"))
        .filter(|id| !id.is_empty())
        .collect();
    let mut matches = ids.into_iter().filter_map(|id| {
        let service: Vec<_> = rows.iter().filter(|r| field(r, "Service:RDT-ID") == id).collect();
        let unique_stop = |code: &str| {
            let mut stops = service.iter().filter(|r| field(r, "Stop:Station code") == code);
            let stop = stops.next()?;
            stops.next().is_none().then_some(*stop)
        };
        let board = unique_stop(&leg.from_code)?;
        let alight = unique_stop(&leg.to_code)?;
        if !departure_matches(board, &leg.from_code, leg.dep_planned)
            || time(alight, "Stop:Arrival time").map(|t| t.to_utc()) != Some(leg.arr_planned)
        {
            return None;
        }
        Some(service.into_iter().cloned().collect())
    });
    let service = matches.next()?;
    matches.next().is_none().then_some(service)
}

/// A delay card from Rijden de Treinen service-archive rows of one train (one row per stop).
/// `board`/`alight` are the station codes of the claimed trip; the arrival delay at `alight` is the
/// delay that applies.
pub fn delay_card(rows: &[Value], board: &str, alight: &str, src: &RdtSource) -> Result<String> {
    let first = rows.first().ok_or_else(|| anyhow!("Geen treinregistratie. / No train record."))?;
    let train = field(first, "Service:Train number");
    let date = field(first, "Service:Date");
    let company = field(first, "Service:Company");
    let kind = field(first, "Service:Type");
    let service_id = field(first, "Service:RDT-ID");
    let route = format!("{} \u{2192} {}", field(first, "Stop:Station name"), field(rows.last().unwrap(), "Stop:Station name"));
    let mut body = String::new();
    let mut changes = vec![];
    let (mut in_trip, mut applies) = (false, None);
    for r in rows {
        let code = field(r, "Stop:Station code");
        let name = field(r, "Stop:Station name");
        if code == board {
            in_trip = true;
        }
        let arr_c = field(r, "Stop:Arrival cancelled") == "true";
        let dep_c = field(r, "Stop:Departure cancelled") == "true";
        let (pa, aa, da) = cell(time(r, "Stop:Arrival time"), delay(r, "Stop:Arrival delay"), arr_c);
        let (pd, ad, dd) = cell(time(r, "Stop:Departure time"), delay(r, "Stop:Departure delay"), dep_c);
        let mut tags = String::new();
        if code == board {
            tags.push_str(r#"<span class="tag leg">instap / boarding</span>"#);
        }
        if code == alight {
            tags.push_str(r#"<span class="tag leg">uitstap / alighting</span>"#);
            applies = delay(r, "Stop:Arrival delay").map(|d| (name.to_string(), d, arr_c));
        }
        if arr_c || dep_c {
            changes.push(format!("{name}: vervallen / cancelled"));
        }
        if field(r, "Stop:Platform change") == "true" {
            changes.push(format!(
                "{name}: spoor / platform {} \u{2192} {}",
                field(r, "Stop:Planned platform"),
                field(r, "Stop:Actual platform")
            ));
        }
        let platform = if field(r, "Stop:Platform change") == "true" {
            format!("{} \u{2192} {}", esc(field(r, "Stop:Planned platform")), esc(field(r, "Stop:Actual platform")))
        } else {
            esc(field(r, "Stop:Actual platform"))
        };
        let hl = if in_trip && (code == board || code == alight) { " class=\"hl\"" } else { "" };
        body.push_str(&format!(
            "<tr{hl}><td>{}{tags}</td><td class=\"n\">{pa}</td><td class=\"n\">{aa}</td><td class=\"n\">{da}</td><td class=\"n\">{pd}</td><td class=\"n\">{ad}</td><td class=\"n\">{dd}</td><td class=\"n muted\">{platform}</td></tr>",
            esc(name)
        ));
        if code == alight {
            in_trip = false;
        }
    }
    if field(first, "Service:Completely cancelled") == "true" {
        changes.insert(0, "Hele rit vervallen / whole service cancelled".into());
    } else if field(first, "Service:Partly cancelled") == "true" && changes.is_empty() {
        changes.push("Deels vervallen / partly cancelled".into());
    }
    let callout = match applies {
        Some((st, d, false)) => {
            format!(r#"<div class="callout">Aankomstvertraging in {st}: {d} minuten · Arrival delay at {st}: {d} min</div>"#, st = esc(&st))
        }
        Some((st, _, true)) => {
            format!(r#"<div class="callout">Aankomst in {st} vervallen · Arrival at {st} cancelled</div>"#, st = esc(&st))
        }
        None => String::new(),
    };
    let changes = if changes.is_empty() {
        r#"<p class="muted" style="margin:0 24px">Geen uitval of spoorwijziging geregistreerd. / No cancellation or platform change recorded.</p>"#.to_string()
    } else {
        format!(r#"<p style="margin:0 24px"><b>Wijzigingen / Changes:</b> {}</p>"#, esc(&changes.join("; ")))
    };
    let max = field(first, "Service:Maximum delay");
    Ok(page(&format!(
        r#"<div class="top"><div><p class="kicker">Vertragingsbewijs / Delay record</p><h1>Trein / Train {train} · {date}</h1><p class="sub">{route} · {kind}</p></div><div><span class="chip op">{company}</span></div></div>
<div class="facts"><div><b>Treinnummer / Train</b><span>{train}</span></div><div><b>Datum / Date</b><span>{date}</span></div><div><b>Vervoerder / Operator</b><span>{company}</span></div><div><b>Max. vertraging / Max. delay</b><span>{max} min</span></div></div>
{callout}
<table><thead><tr><th>Station</th><th class="n">Aank. gepland<br>Arr. planned</th><th class="n">Aank. werkelijk<br>Arr. actual</th><th class="n">Vertr. (min)<br>Delay</th><th class="n">Vertr. gepland<br>Dep. planned</th><th class="n">Vertr. werkelijk<br>Dep. actual</th><th class="n">Vertr. (min)<br>Delay</th><th class="n">Spoor<br>Platform</th></tr></thead><tbody>{body}</tbody></table>
{changes}
<div class="source"><p><b>Bron / Source: Rijden de Treinen, open data, CC BY 4.0</b> (creativecommons.org/licenses/by/4.0)</p>
<p>Dataset: {dataset} · {url}</p>
<p>RDT service-id: {service_id} · trein / train {train} · {date}</p>
<p>Opgehaald / Retrieved: {retrieved}{version}</p>
<p>Kaart gemaakt door OV Refund uit dit openbare record; werkelijke tijd = geplande tijd + geregistreerde vertraging. / Card generated by OV Refund from this public record; actual time = planned time + recorded delay.</p></div>"#,
        train = esc(train),
        date = esc(date),
        route = esc(&route),
        kind = esc(kind),
        company = esc(company),
        max = esc(max),
        dataset = esc(&src.dataset),
        url = esc(&src.url),
        service_id = esc(service_id),
        retrieved = esc(&src.retrieved),
        version = src.file_version.as_deref().map(|v| format!(" · bestandsversie / file version: {}", esc(v))).unwrap_or_default(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    const RAW: &str = include_str!("../assets/arriva/real-delayed-service.json");
    #[test]
    fn delay_card_uses_only_record_values() {
        let rows: Vec<Value> = serde_json::from_str(RAW).unwrap();
        let src =
            RdtSource { dataset: "services-2026-10.csv.gz".into(), url: "https://x".into(), retrieved: "r".into(), file_version: None };
        let html = delay_card(&rows, "VL", "NM", &src).unwrap();
        assert!(html.contains("Trein / Train 732284 · 2026-10-29"));
        assert!(html.contains("Arrival delay at Nijmegen: 24 min"));
        // Nijmegen: planned 00:22 + 24 min = 00:46.
        assert!(html.contains(r#"<td class="n">00:22</td><td class="n">00:46</td>"#));
        assert!(html.contains("Rijden de Treinen, open data, CC BY 4.0"));
        assert!(html.contains("19491673"));
        assert!(html.contains("No cancellation or platform change recorded"));
        assert_eq!(html.matches("class=\"hl\"").count(), 2);
    }
    #[test]
    fn departure_picks_the_planned_service() {
        let rows: Vec<Value> = serde_json::from_str(RAW).unwrap();
        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc);
        let departs_at = |code, planned| rows.iter().any(|r| departure_matches(r, code, planned));
        assert!(departs_at("VL", t("2026-10-29T23:29:00+01:00")));
        assert!(!departs_at("VL", t("2026-10-29T23:30:00+01:00")));
        assert!(!departs_at("NM", t("2026-10-29T23:29:00+01:00")));
    }
    #[test]
    fn delay_proof_uses_one_service_even_when_the_train_number_repeats() {
        let mut rows: Vec<Value> = serde_json::from_str(RAW).unwrap();
        let mut other = rows[0].clone();
        other["Service:RDT-ID"] = "fixture-other-service".into();
        rows.insert(0, other);
        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc);
        let dep = t("2026-10-29T23:29:00+01:00");
        let arr = t("2026-10-30T00:22:00+01:00");
        let leg = ovrefund_core::model::Leg {
            train_number: "732284".into(),
            train_type: "Fixture".into(),
            company: "Arriva".into(),
            from_code: "VL".into(),
            to_code: "NM".into(),
            dep_planned: dep,
            arr_planned: arr,
            dep_actual: dep,
            arr_actual: arr,
            cancelled: false,
        };
        let service = service_for_leg(&rows, &leg).unwrap();
        assert_eq!(service.len(), rows.len() - 1);
        assert!(service.iter().all(|row| row["Service:RDT-ID"] == "19491673"));
        let bad = ovrefund_core::model::Leg { arr_planned: arr + chrono::Duration::minutes(1), ..leg };
        assert!(service_for_leg(&rows, &bad).is_none());
    }
    #[test]
    fn delay_card_reports_recorded_changes() {
        let mut rows: Vec<Value> = serde_json::from_str(RAW).unwrap();
        rows[4]["Stop:Platform change"] = "true".into();
        rows[4]["Stop:Actual platform"] = "2".into();
        rows[5]["Stop:Arrival cancelled"] = "true".into();
        let src = RdtSource { dataset: "d".into(), url: "u".into(), retrieved: "r".into(), file_version: None };
        let html = delay_card(&rows, "VL", "NM", &src).unwrap();
        assert!(html.contains("Boxmeer: spoor / platform"));
        assert!(html.contains("Cuijk: vervallen / cancelled"));
    }
    #[test]
    fn demo_history_row_is_marked_and_escaped() {
        let t = TravelTrip {
            date: "29-10-2026".into(),
            from: "Venlo".into(),
            to: "<Nijmegen>".into(),
            check_in: DateTime::parse_from_rfc3339("2026-10-29T23:25:00+01:00").unwrap(),
            check_out: DateTime::parse_from_rfc3339("2026-10-30T00:49:00+01:00").unwrap(),
            operator: "Arriva".into(),
            product: "p".into(),
            travel_class: Some(2),
            fare_cents: Some(1240),
            source: "Demogegevens (fictief) / Demo data (fictional)".into(),
            demo: true,
        };
        let html = demo_history_row(&t);
        assert!(html.contains("&lt;Nijmegen&gt;"));
        assert!(html.contains("€ 12,40"));
        assert!(html.contains("Bron / Source: Demogegevens"));
        assert!(html.contains("84 min"));
        assert!(html.contains("DEMODATA / DEMO DATA"));
        assert!(!html.to_lowercase().contains("ns.nl") && !html.contains("Mijn NS"));
    }
    #[test]
    fn caption_shows_the_full_card_number() {
        let c = Caption {
            card_number: Some("3528000000001234".into()),
            date: "29-10-2026".into(),
            route: "Venlo - Nijmegen".into(),
            demo: false,
        };
        let html = caption_strip(&c, 380);
        assert!(html.contains("OV-chipkaartnummer / OV-chipkaart number"));
        assert!(html.contains("3528 0000 0000 1234"));
        assert!(html.contains("width:380px"));
        assert!(!html.contains("DEMO DATA"));
        let none = caption_strip(&Caption { card_number: None, ..c }, 380);
        assert!(none.contains("niet beschikbaar / not available"));
    }
    fn png_of(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = vec![];
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().unwrap();
        wr.write_image_data(&rgba.repeat((w * h) as usize)).unwrap();
        wr.finish().unwrap();
        out
    }
    #[test]
    fn stacking_keeps_the_screenshot_pixels() {
        let top = png_of(4, 3, [10, 20, 30, 255]);
        let bottom = png_of(2, 2, [200, 0, 0, 255]);
        let (w, h, px) = decode_rgba(&stack_png(&top, &bottom).unwrap()).unwrap();
        assert_eq!((w, h), (4, 5));
        // The top three rows are the screenshot, unchanged.
        assert!(px[..4 * 3 * 4].chunks(4).all(|p| p == [10, 20, 30, 255]));
        // Caption row: its own pixels, white where it is narrower.
        assert_eq!(&px[4 * 3 * 4..4 * 3 * 4 + 16], &[200, 0, 0, 255, 200, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255]);
    }
}
