//! Optional SMTP summary email (TLS only). The password comes from the OS
//! credential store via the caller; nothing is hard-coded here.
use crate::model::RunSummary;
use anyhow::{Context, Result};
use lettre::message::{Attachment, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    /// "starttls" (587) or "tls" (465)
    pub security: String,
    pub username: String,
    pub from: String,
    pub to: String,
}

impl SmtpConfig {
    pub fn is_configured(&self) -> bool {
        !self.host.is_empty() && !self.from.is_empty() && !self.to.is_empty()
    }

    /// The mailbox OV Refund would log in to is a Hotmail / Outlook.com / Live / MSN address (or
    /// Microsoft's consumer mail server), which no longer accepts app-password sign-in.
    /// Fingerprint of the mail server and user name an app password was entered for (SHA-256,
    /// hex; host and user name trimmed and lower-case). Stored next to the password, so a
    /// password is never sent to another server or account than the one it was typed for.
    pub fn password_fingerprint(&self) -> String {
        use sha2::Digest;
        let s = format!("{}\n{}", self.host.trim().to_lowercase(), self.username.trim().to_lowercase());
        hex::encode(sha2::Sha256::digest(s.as_bytes()))
    }

    pub fn unsupported_provider(&self) -> bool {
        let host = self.host.to_lowercase();
        unsupported_mail_address(&self.username)
            || unsupported_mail_address(&self.from)
            || (self.username.is_empty() && self.from.is_empty() && unsupported_mail_address(&self.to))
            || host == "smtp-mail.outlook.com"
            || host == "smtp.live.com"
    }
}

/// Refusal when e-mail is set up with such an address.
pub const UNSUPPORTED_PROVIDER_MSG: &str = "Hotmail, Outlook.com en Live laten apps zoals OV Refund geen mail meer versturen. Gebruik Windows-meldingen of een ander adres, zoals Gmail. / Hotmail, Outlook.com and Live no longer let apps like OV Refund send mail. Use Windows notifications or another address, such as Gmail.";

/// The stored app password belongs to another mail server or user name (changed in the settings,
/// or settings from elsewhere): nothing is sent or read until the password is entered again.
pub const SMTP_REENTER_MSG: &str = "Je e-mailinstellingen zijn gewijzigd. Vul het app-wachtwoord opnieuw in bij ‘E-mail voor meldingen’. / Your e-mail settings changed. Enter the app password again under ‘E-mail for notifications’.";

/// hotmail / outlook / live / msn at .com, .nl or .be (also subdomains such as "hotmail.co" are not matched).
pub fn unsupported_mail_address(addr: &str) -> bool {
    let Some((_, domain)) = addr.trim().rsplit_once('@') else { return false };
    let d = domain.trim().trim_end_matches('.').to_lowercase();
    ["hotmail", "outlook", "live", "msn"].iter().any(|n| ["com", "nl", "be"].iter().any(|tld| d == format!("{n}.{tld}")))
}

/// Language of everything the user receives (e-mails, notifications). Follows the app's language setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Nl,
    En,
}

impl Lang {
    /// "en" -> English; anything else (including unset) -> Dutch.
    pub fn from_code(code: &str) -> Lang {
        if code.trim().eq_ignore_ascii_case("en") {
            Lang::En
        } else {
            Lang::Nl
        }
    }
    pub fn t<'a>(self, nl: &'a str, en: &'a str) -> &'a str {
        match self {
            Lang::Nl => nl,
            Lang::En => en,
        }
    }
    /// "1 claim" / "3 claims", "1 reis" / "2 reizen", ...
    pub fn count(self, n: usize, nl: (&str, &str), en: (&str, &str)) -> String {
        let (one, many) = match self {
            Lang::Nl => nl,
            Lang::En => en,
        };
        format!("{n} {}", if n == 1 { one } else { many })
    }
}

/// Public website (footer links in the mails).
pub const WEBSITE_URL: &str = "https://ov-refund.github.io/OV-Refund/";
/// The logo, sent inside the mail as an inline image (never loaded from the internet).
const LOGO_PNG: &[u8] = include_bytes!("../../../site/assets/logo-mark.png");
const LOGO_CID: &str = "ovr-logo";

fn n_claims(lang: Lang, n: usize) -> String {
    lang.count(n, ("claim", "claims"), ("claim", "claims"))
}
fn n_trips(lang: Lang, n: usize) -> String {
    lang.count(n, ("reis", "reizen"), ("trip", "trips"))
}

/// "2 claims verlopen binnen 14 dagen" / "1 claim expires within 14 days".
fn expiring_text(lang: Lang, n: usize) -> String {
    format!(
        "{} {}",
        n_claims(lang, n),
        match (lang, n == 1) {
            (Lang::Nl, true) => "verloopt binnen 14 dagen",
            (Lang::Nl, false) => "verlopen binnen 14 dagen",
            (Lang::En, true) => "expires within 14 days",
            (Lang::En, false) => "expire within 14 days",
        }
    )
}

/// "ingediend", or "automatisch ingediend" for a run in automatic mode.
fn submitted_word(s: &RunSummary, lang: Lang) -> &'static str {
    if s.mode == "automatic" {
        lang.t("automatisch ingediend", "submitted automatically")
    } else {
        lang.t("ingediend", "submitted")
    }
}

/// Short subject for a run that could not log in to NS (None when logged in).
fn auth_subject(auth_state: &str, lang: Lang) -> Option<&'static str> {
    Some(match auth_state {
        "login_required" => lang.t("opnieuw inloggen bij NS nodig", "log in to NS again"),
        "autologin_failed" => lang.t("automatisch inloggen bij NS lukte niet", "automatic login to NS failed"),
        "account_mismatch" => lang.t("ander NS-account ingelogd", "a different NS account is logged in"),
        s if s == format!("blocked: {}", crate::pipeline::BLOCKED_VERIFICATION) => {
            lang.t("NS vraagt om een extra controle", "NS asks for an extra check")
        }
        s if s.starts_with("blocked") => lang.t("NS hield de controle tegen", "NS stopped the check"),
        _ => return None,
    })
}

/// Subject line of the summary mail (also the title of the Windows notification). Priority:
/// submitted, claims about to expire, login problems, claimable trips, trips to review, problems.
pub fn summary_subject(s: &RunSummary, lang: Lang) -> String {
    if s.submitted > 0 {
        format!("OV Refund: {} {}", n_claims(lang, s.submitted), submitted_word(s, lang))
    } else if s.expiring_soon > 0 {
        format!("OV Refund: {}", expiring_text(lang, s.expiring_soon))
    } else if let Some(a) = auth_subject(&s.auth_state, lang) {
        format!("OV Refund: {a}")
    } else if s.candidates > 0 {
        format!("OV Refund: {} {}", n_trips(lang, s.candidates), lang.t("te claimen", "to claim"))
    } else if s.needs_review > 0 {
        format!("OV Refund: {} {}", n_trips(lang, s.needs_review), lang.t("om te beoordelen", "to review"))
    } else if !s.errors.is_empty() {
        format!("OV Refund: {}", lang.t("actie nodig", "action needed"))
    } else {
        format!("OV Refund: {}", lang.t("controle afgerond", "check complete"))
    }
}

/// Plain-text summary (the HTML mail's text alternative).
pub fn render_summary(s: &RunSummary, lines: &[String], lang: Lang) -> (String, String) {
    let t = |nl: &'static str, en: &'static str| -> &'static str { lang.t(nl, en) };
    let session = match s.auth_state.as_str() {
        "ok" => t("in orde", "OK"),
        "login_required" => t("opnieuw inloggen nodig", "needs a new login"),
        "autologin_failed" => t("automatisch inloggen lukte niet", "automatic login failed"),
        "account_mismatch" => t("ander NS-account ingelogd", "different NS account logged in"),
        b if b.starts_with("blocked") => t("geblokkeerd door NS", "blocked by NS"),
        _ => t("onbekend", "unknown"),
    };
    let rows: [(&str, String); 11] = [
        (t("Reizen gecontroleerd", "Trips checked"), s.journeys_seen.to_string()),
        (t("Nieuwe reizen", "New trips"), s.journeys_new.to_string()),
        (t("Te claimen", "Claimable"), s.candidates.to_string()),
        (t("Te beoordelen", "To review"), s.needs_review.to_string()),
        (t("Ingediend", "Submitted"), s.submitted.to_string()),
        (t("Niet verzonden", "Not sent"), s.failed.to_string()),
        (t("Niet automatisch verstuurd", "Not sent automatically"), s.refused.to_string()),
        (t("Onzeker", "Uncertain"), s.indeterminate.to_string()),
        (t("Wacht op treintijden", "Awaiting timetable"), s.pending_evidence.to_string()),
        (t("Verloopt binnen 14 dagen", "Expires within 14 days"), s.expiring_soon.to_string()),
        (t("Mijn NS-sessie", "Mijn NS session"), session.to_string()),
    ];
    let mut body = format!("OV Refund — {} #{}\n\n", t("controle", "check"), s.run_id);
    for (k, v) in rows {
        body.push_str(&format!("{k}: {v}\n"));
    }
    if !lines.is_empty() {
        body.push_str(&format!("\n{}:\n", t("Wat er gebeurde", "What happened")));
        for l in lines {
            body.push_str(&format!("- {}\n", crate::redact::redact(&event_line(l, lang))));
        }
    }
    if !s.errors.is_empty() {
        body.push_str(&format!("\n{}:\n", t("Aandachtspunten", "Needs attention")));
        for e in &s.errors {
            body.push_str(&format!("- {}\n", crate::redact::redact(&event_line(e, lang))));
        }
    }
    body.push_str(&format!(
        "\n{}\n{}\n",
        t(
            "Open OV Refund voor alle details. Deze e-mail bevat nooit kaartnummers, bankgegevens of wachtwoorden.",
            "Open OV Refund for all details. This e-mail never contains card numbers, bank details or passwords."
        ),
        WEBSITE_URL
    ));
    (summary_subject(s, lang), body)
}

/// Is a run worth a notification? (Something to claim, review, a result, a deadline, a login
/// problem or another problem.) Scheduled runs stay silent otherwise.
pub fn summary_is_relevant(s: &RunSummary) -> bool {
    s.submitted + s.needs_review + s.candidates + s.failed + s.refused + s.indeterminate + s.approved_since_last + s.expiring_soon > 0
        || (s.auth_state != "ok" && s.auth_state != "unknown")
        || !s.errors.is_empty()
}

/// Text of the Windows notification after a run. Parts that are zero are left out.
pub fn toast_text(s: &RunSummary, lang: Lang) -> (String, String) {
    let t = |nl: &'static str, en: &'static str| -> &'static str { lang.t(nl, en) };
    if let Some(m) = crate::pipeline::auth_message(&s.auth_state) {
        let title = auth_subject(&s.auth_state, lang).map(|a| format!("OV Refund: {a}")).unwrap_or_else(|| summary_subject(s, lang));
        return (title, crate::pipeline::pick_lang(m, lang == Lang::Nl).to_string());
    }
    let mut parts = vec![];
    if s.journeys_seen > 0 {
        parts.push(format!("{} {}", n_trips(lang, s.journeys_seen), t("gecontroleerd", "checked")));
    }
    if s.candidates > 0 {
        parts.push(format!("{} {}", s.candidates, t("te claimen", "claimable")));
    }
    if s.needs_review > 0 {
        parts.push(format!("{} {}", s.needs_review, t("te beoordelen", "to review")));
    }
    if s.submitted > 0 {
        parts.push(format!("{} {}", s.submitted, submitted_word(s, lang)));
    }
    if s.indeterminate > 0 {
        parts.push(t("Uitkomst van indienen onzeker. Kijk in Mijn teruggaven.", "Submission outcome uncertain. Check Mijn teruggaven.").into());
    }
    if s.failed > 0 || s.refused > 0 {
        parts.push(t("Claims wachten op je aandacht. Open OV Refund.", "Claims need your attention. Open OV Refund.").into());
    }
    if s.expiring_soon > 0 {
        parts.push(expiring_text(lang, s.expiring_soon));
    }
    if !s.errors.is_empty() {
        parts.push(lang.count(s.errors.len(), ("aandachtspunt", "aandachtspunten"), ("issue", "issues")));
    }
    if parts.is_empty() {
        parts.push(t("Controle afgerond, niets nieuws.", "Check complete, nothing new.").to_string());
    }
    (summary_subject(s, lang), parts.join(" · "))
}

/// Subject and plain text of the welcome / test mail.
pub fn welcome_text(lang: Lang) -> (String, String) {
    (
        lang.t("OV Refund: e-mail is ingesteld", "OV Refund: e-mail is set up").to_string(),
        format!(
            "{}\n\n{}\n",
            lang.t(
                "Dit bericht bevestigt dat OV Refund e-mail kan versturen.",
                "This message confirms that OV Refund can send e-mail.",
            ),
            WEBSITE_URL
        ),
    )
}

/// Money overview for the report (cents), from the local ledger.
#[derive(Debug, Clone, Default)]
pub struct ReportTotals {
    pub received_cents: i64,
    pub received_count: i64,
    pub pending_cents: i64,
    pub pending_count: i64,
    pub claimable_count: i64,
    /// Paid out via another claim service (net, as recorded by the user).
    pub external_cents: i64,
    pub external_count: i64,
    /// Every claim worth showing (newest first), grouped in the mail by status.
    pub claims: Vec<ReportClaim>,
}

/// One line in the mail's claim overview.
#[derive(Debug, Clone)]
pub struct ReportClaim {
    pub date: String,
    pub time: String,
    pub route: String,
    pub delay_min: Option<i64>,
    pub band: Option<String>,
    pub status: String,
    pub reference: Option<String>,
    pub expected_cents: Option<i64>,
    pub paid_cents: Option<i64>,
    pub expires: Option<String>,
}

/// Build the money overview and claim list from the local ledger.
pub fn report_from_db(db: &crate::db::Db) -> Result<ReportTotals> {
    use chrono::{DateTime, Utc};
    use chrono_tz::Europe::Amsterdam;
    let t = db.totals()?;
    let g = |k: &str, f: &str| t[k][f].as_i64().unwrap_or(0);
    let now = Utc::now();
    let mut claims = vec![];
    for r in db.journey_rows(1000)? {
        let Some(status) = r.claim_status.clone() else { continue };
        let expired = r.expires_at.as_deref().and_then(|e| DateTime::parse_from_rfc3339(e).ok()).map(|e| e < now).unwrap_or(false);
        let open = matches!(status.as_str(), "READY" | "NEEDS_REVIEW" | "FAILED");
        // Open items need an unexpired claim period and a real delay indication.
        let relevant_delay = if r.journey.is_arriva() {
            r.reasons.first().is_some_and(|reason| {
                reason.starts_with(crate::eligibility::ARRIVA_ELIGIBLE_PREFIX) && reason.contains("of the fare; claim until")
            })
        } else {
            r.delay_min.or(r.proxy_min).unwrap_or(0) >= 25
        };
        if status == "DISMISSED" || status == "CANDIDATE" || (open && (expired || !relevant_delay)) {
            continue;
        }
        let ci = r.journey.check_in.with_timezone(&Amsterdam);
        claims.push(ReportClaim {
            date: ci.format("%d-%m-%Y").to_string(),
            time: ci.format("%H:%M").to_string(),
            route: format!("{} → {}", r.journey.origin.name, r.journey.destination.as_ref().map(|d| d.name.as_str()).unwrap_or("?")),
            delay_min: r.delay_min,
            band: r.delay_band.clone(),
            status,
            reference: r.ns_reference.clone(),
            expected_cents: r.compensation_cents,
            paid_cents: r.paid_cents,
            expires: if open {
                r.expires_at
                    .as_deref()
                    .and_then(|e| DateTime::parse_from_rfc3339(e).ok())
                    .map(|e| e.with_timezone(&Amsterdam).format("%d-%m-%Y").to_string())
            } else {
                None
            },
        });
    }
    Ok(ReportTotals {
        received_cents: g("paid", "cents"),
        received_count: g("paid", "count"),
        pending_cents: g("pending", "cents"),
        pending_count: g("pending", "count"),
        claimable_count: claims.iter().filter(|c| matches!(c.status.as_str(), "READY" | "NEEDS_REVIEW" | "FAILED")).count() as i64,
        external_cents: g("external", "cents"),
        external_count: g("external", "count"),
        claims,
    })
}

/// (group key, Dutch title, English title, statuses, pill background, pill text colour)
type Group = (&'static str, &'static str, &'static str, &'static [&'static str], &'static str, &'static str);
const GROUPS: [Group; 9] = [
    ("open", "Te claimen", "Claimable", &["READY"], "#FFF4DC", "#9A5B00"),
    ("review", "Beoordelen", "Review", &["NEEDS_REVIEW"], "#FFF4DC", "#9A5B00"),
    ("failed", "Niet verzonden", "Not sent", &["FAILED"], "#FDECEA", "#B42318"),
    ("uncertain", "Onzeker", "Uncertain", &["INDETERMINATE"], "#FDECEA", "#B42318"),
    ("approved", "Toegekend", "Approved", &["APPROVED"], "#EEF3FB", "#00307A"),
    ("submitted", "Geclaimd", "Claimed", &["SUBMITTING", "SUBMITTED"], "#EEF3FB", "#00307A"),
    ("paid", "Uitbetaald", "Paid", &["PAID"], "#E7F4EC", "#16703A"),
    ("external", "Elders geclaimd", "Claimed elsewhere", &["EXTERNALLY_CLAIMED"], "#F1F0EC", "#4A4D52"),
    ("rejected", "Afgewezen", "Rejected", &["REJECTED"], "#FDECEA", "#B42318"),
];

fn status_label(s: &str, lang: Lang) -> &'static str {
    let (nl, en) = match s {
        "READY" => ("Te claimen", "Claimable"),
        "NEEDS_REVIEW" => ("Beoordelen", "Review"),
        "FAILED" => ("Niet verzonden", "Not sent"),
        "SUBMITTING" => ("Wordt ingediend…", "Submitting…"),
        "SUBMITTED" => ("Ingediend", "Submitted"),
        "INDETERMINATE" => ("Onzeker", "Uncertain"),
        "APPROVED" => ("Toegekend", "Approved"),
        "PAID" => ("Uitbetaald", "Paid"),
        "EXTERNALLY_CLAIMED" => ("Elders geclaimd", "Claimed elsewhere"),
        "REJECTED" => ("Afgewezen", "Rejected"),
        _ => ("—", "—"),
    };
    lang.t(nl, en)
}

fn claims_table(claims: &[ReportClaim], lang: Lang) -> String {
    let t = |nl: &'static str, en: &'static str| -> &'static str { lang.t(nl, en) };
    const INK: &str = "#1C1D1F";
    const INK2: &str = "#6A6E75";
    const RULE: &str = "#E5E2DA";
    let mut out = String::new();
    for (_key, title_nl, title_en, states, bg, fg) in GROUPS {
        let title = t(title_nl, title_en);
        let rows: Vec<&ReportClaim> = claims.iter().filter(|c| states.contains(&c.status.as_str())).collect();
        if rows.is_empty() {
            continue;
        }
        let sum: i64 = rows.iter().filter_map(|c| c.paid_cents.or(c.expected_cents)).sum();
        let mut body = String::new();
        for c in &rows {
            let delay = match (c.delay_min, c.band.as_deref()) {
                (Some(d), _) if d > 0 => format!("≈ {d} min {}", t("vertraging", "delay")),
                (_, Some("60_plus")) => format!("60+ min {}", t("vertraging", "delay")),
                (_, Some("30_59")) => format!("30–59 min {}", t("vertraging", "delay")),
                _ => t("vertraging onbekend", "delay unknown").to_string(),
            };
            let mut meta = vec![format!("{} · {delay}", c.time)];
            if let Some(r) = &c.reference {
                meta.push(format!("ref. {}", esc(r)));
            }
            if let Some(e) = &c.expires {
                meta.push(format!("{} {e}", t("claimen vóór", "claim before")));
            }
            let amount = match (c.paid_cents, c.expected_cents) {
                (Some(p), _) => format!(
                    r#"<div style="font-size:15px;font-weight:700;color:{INK}">{}</div><div style="font-size:11.5px;color:{INK2}">{}</div>"#,
                    euro(p, lang),
                    if c.status == "APPROVED" {
                        t("toegekend", "approved")
                    } else if c.status == "EXTERNALLY_CLAIMED" {
                        t("bedrag", "amount")
                    } else {
                        t("ontvangen", "received")
                    }
                ),
                (None, Some(x)) => format!(
                    r#"<div style="font-size:15px;font-weight:700;color:{INK}">≈ {}</div><div style="font-size:11.5px;color:{INK2}">{}</div>"#,
                    euro(x, lang),
                    t("verwacht", "expected")
                ),
                _ => format!(r#"<div style="font-size:13px;color:{INK2}">{}</div>"#, t("bedrag onbekend", "amount unknown")),
            };
            body.push_str(&format!(
                r#"<tr><td style="padding:10px 0;border-top:1px solid {RULE}" valign="top"><div style="font-size:14px;font-weight:700;color:{INK}">{date} <span style="font-weight:400;color:{INK}">· {route}</span></div><div style="font-size:12px;color:{INK2};margin-top:2px">{meta}</div><div style="margin-top:5px"><span style="display:inline-block;background:{bg};color:{fg};font-size:11px;font-weight:700;border-radius:999px;padding:2px 8px">{st}</span></div></td><td align="right" valign="top" style="padding:10px 0 10px 12px;border-top:1px solid {RULE};white-space:nowrap">{amount}</td></tr>"#,
                date = c.date,
                route = esc(&c.route),
                meta = meta.join(" · "),
                st = status_label(&c.status, lang),
            ));
        }
        out.push_str(&format!(
            r#"<tr><td style="padding:16px 28px 2px"><table role="presentation" width="100%" cellspacing="0" cellpadding="0"><tr><td style="padding-bottom:6px"><span style="display:inline-block;width:8px;height:8px;border-radius:2px;background:{fg};margin-right:7px"></span><span style="font-size:13px;font-weight:700;letter-spacing:.06em;text-transform:uppercase;color:{fg}">{title}</span> <span style="font-size:12px;color:{INK2}">({n})</span></td><td align="right" style="padding-bottom:6px;font-size:12px;color:{INK2}">{total}</td></tr>{body}</table></td></tr>"#,
            n = rows.len(),
            total = if sum > 0 { format!("{} {}", t("totaal", "total"), euro(sum, lang)) } else { String::new() },
        ));
    }
    if out.is_empty() {
        return out;
    }
    format!(
        r#"<tr><td style="padding:18px 28px 0"><div style="font-size:16px;font-weight:800;color:#00307A">{}</div><div style="font-size:12px;color:{INK2}">{}</div></td></tr>{out}"#,
        t("Alle claims", "All claims"),
        t(
            "Per status, nieuwste eerst. Bedragen: wat NS heeft betaald, of een schatting van de vergoeding volgens de regels van NS.",
            "By status, newest first. Amounts: what NS has paid, or an estimate of the refund under the NS rules."
        )
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// User-facing wording for the run's event lines and errors (internally English; unknown lines are shown as they are).
fn event_line(l: &str, lang: Lang) -> String {
    let t = |nl: &str, en: &str| lang.t(nl, en).to_string();
    let nl = lang == Lang::Nl;
    // "claim #12: <Nederlands / English>": keep the prefix, pick the half.
    if let Some((head, rest)) = l.split_once(": ").filter(|(h, r)| h.starts_with("claim #") && r.contains(" / ")) {
        return format!("{head}: {}", crate::pipeline::pick_lang(rest, nl));
    }
    // "Nederlands / English" run errors (login, NS blocks, no card, locks, ...): the half for the
    // mail's language.
    if l.contains(" / ") {
        return crate::pipeline::pick_lang(l, nl).to_string();
    }
    // Older runs stored this English text.
    if l.starts_with("NS login expired") {
        return crate::pipeline::pick_lang(crate::pipeline::LOGIN_NEEDED_MSG, nl).to_string();
    }
    if l.starts_with("no travel-history response") || l.contains("timed out") {
        return crate::pipeline::pick_lang(crate::pipeline::NS_NO_RESPONSE_MSG, nl).to_string();
    }
    if let Some(code) = l.strip_prefix("travel history returned HTTP ") {
        return format!(
            "{} (HTTP {code}). {}",
            t("Mijn NS gaf een fout bij het ophalen van je reishistorie", "Mijn NS returned an error for your travel history"),
            t("OV Refund probeert het bij de volgende controle opnieuw.", "OV Refund will try again at the next check.")
        );
    }
    if l.starts_with("empty travel-history body") {
        return t(
            "Mijn NS stuurde een lege reishistorie. OV Refund probeert het bij de volgende controle opnieuw.",
            "Mijn NS sent an empty travel history. OV Refund will try again at the next check.",
        );
    }
    if l.starts_with("archive download failed") {
        return t(
            "De dienstregelingsgegevens konden niet worden gedownload. OV Refund probeert het bij de volgende controle opnieuw.",
            "The timetable data could not be downloaded. OV Refund will try again at the next check.",
        );
    }
    if l.starts_with("automatic submission is no longer enabled") {
        return t(
            "Automatisch indienen staat niet meer aan. OV Refund is gestopt met indienen",
            "Automatic submission is no longer on. OV Refund stopped submitting",
        );
    }
    if let Some(x) = l.strip_prefix("submitted: ") {
        return match x.rsplit_once(" on ") {
            Some((route, date)) => format!("{}: {route} {} {date}", t("Ingediend", "Submitted"), t("op", "on")),
            None => format!("{}: {x}", t("Ingediend", "Submitted")),
        };
    }
    if l.starts_with("NS login expired") {
        return t(
            "Je Mijn NS-sessie is verlopen. Open OV Refund en log opnieuw in.",
            "Your Mijn NS session has expired. Open OV Refund and log in again.",
        );
    }
    if let Some(x) = l.strip_prefix("NS website refused the request (") {
        let code = x.split(')').next().unwrap_or("");
        return format!(
            "{} ({code}); {}",
            t("De NS-website weigerde het verzoek", "The NS website refused the request"),
            t("OV Refund is gestopt. Probeer het later opnieuw", "OV Refund stopped. Try again later")
        );
    }
    if let Some(x) = l.strip_prefix("status lookup failed: ") {
        return format!("{}: {x}", t("Status opvragen bij NS mislukt", "Could not look up the status at NS"));
    }
    if l.starts_with("NS claim list unavailable") {
        return t(
            "Het claimoverzicht van NS was niet beschikbaar. Automatisch indienen is deze keer overgeslagen",
            "The NS claim list was unavailable. Automatic submission was skipped this time",
        );
    }
    if l.contains("automatic submission paused until reconciled") {
        let n: usize = l.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
        return format!(
            "{}; {}",
            lang.count(
                n,
                ("claim met onzekere uitkomst", "claims met onzekere uitkomst"),
                ("claim with an uncertain outcome", "claims with an uncertain outcome")
            ),
            t("automatisch indienen staat stil tot dit is opgehelderd", "automatic submission is paused until this is resolved")
        );
    }
    let (head, rest) = l.split_once(": ").unwrap_or(("", l));
    let msg = if rest == "submission confirmed by NS claim list" {
        t("NS heeft de claim bevestigd", "NS confirmed the claim")
    } else if let Some(x) = rest.strip_prefix("NS status ") {
        format!("{}: {x}", t("status bij NS", "status at NS"))
    } else if rest.starts_with("already listed at NS") {
        t("staat al bij NS, dus niet opnieuw ingediend", "already listed at NS, so not submitted again")
    } else if rest.starts_with("NS shows a claim around this date") {
        t(
            "NS toont rond deze datum al een claim, dus niet automatisch ingediend. Kijk in Mijn teruggaven of dit dezelfde reis is",
            "NS already lists a claim around this date, so it was not submitted automatically. Check Mijn teruggaven to see whether it is the same trip",
        )
    } else if rest.starts_with("previous submission interrupted") {
        t(
            "de vorige indiening werd onderbroken en staat op Onzeker. OV Refund kijkt dit na",
            "the previous submission was interrupted and is set to Uncertain. OV Refund will check it",
        )
    } else {
        return l.to_string();
    };
    if head.is_empty() {
        msg
    } else {
        format!("{head}: {msg}")
    }
}

/// An amount in the language's own format: "€ 6,35" (Dutch), "€6.35" (English).
pub fn euro(c: i64, lang: Lang) -> String {
    let sign = if c < 0 { "-" } else { "" };
    let (whole, cents) = (c.unsigned_abs() / 100, c.unsigned_abs() % 100);
    match lang {
        Lang::Nl => format!("{sign}€ {whole},{cents:02}"),
        Lang::En => format!("{sign}€{whole}.{cents:02}"),
    }
}

/// The summary as an HTML mail in the website's style (inline styles and tables only, so it renders in
/// Gmail, Outlook and phone mail apps; the only image is the logo, which travels inside the mail).
pub fn render_html(s: &RunSummary, lines: &[String], totals: Option<&ReportTotals>, demo: bool, lang: Lang) -> String {
    render_inner(s, lines, totals, demo, false, lang)
}

/// Welcome / test mail after setting up e-mail.
pub fn render_welcome_html(demo: bool, lang: Lang) -> String {
    render_inner(&RunSummary::default(), &[], None, demo, true, lang)
}

fn render_inner(s: &RunSummary, lines: &[String], totals: Option<&ReportTotals>, demo: bool, welcome: bool, lang: Lang) -> String {
    let t = |nl: &'static str, en: &'static str| -> &'static str { lang.t(nl, en) };
    // website colours (site/assets/site.css, light theme)
    const YELLOW: &str = "#FFC917";
    const BLUE: &str = "#00307A";
    const INK: &str = "#16181B";
    const INK2: &str = "#474B51";
    const INK3: &str = "#6F737A";
    const RULE: &str = "#E3E0D8";
    const BAND: &str = "#F6F5F1";
    const FONT: &str = "'Segoe UI',-apple-system,BlinkMacSystemFont,Roboto,Helvetica,Arial,sans-serif";
    let (headline, tone) = if welcome {
        (
            t(
                "Dit bericht bevestigt dat OV Refund e-mail kan versturen.",
                "This message confirms that OV Refund can send e-mail.",
            )
            .to_string(),
            ("#E7F4EC", "#16703A"),
        )
    } else if s.submitted > 0 {
        (format!("{} {}", n_claims(lang, s.submitted), t("ingediend bij NS", "submitted to NS")), ("#E7F4EC", "#16703A"))
    } else if s.expiring_soon > 0 {
        (expiring_text(lang, s.expiring_soon), ("#FFF4DC", "#9A5B00"))
    } else if let Some(m) = crate::pipeline::auth_message(&s.auth_state) {
        (crate::pipeline::pick_lang(m, lang == Lang::Nl).to_string(), ("#FDECEA", "#B42318"))
    } else if !s.errors.is_empty() || s.failed > 0 || s.indeterminate > 0 {
        (t("Actie nodig: bekijk de details hieronder", "Action needed: see the details below").to_string(), ("#FFF4DC", "#9A5B00"))
    } else if s.needs_review > 0 {
        (format!("{} {}", n_trips(lang, s.needs_review), t("om zelf te beoordelen", "for you to review")), ("#FFF4DC", "#9A5B00"))
    } else {
        (t("Controle afgerond, alles is bijgewerkt", "Check complete, everything is up to date").to_string(), ("#EEF3FB", BLUE))
    };
    let date = chrono::Local::now().format("%d-%m-%Y %H:%M").to_string();
    let eyebrow = |text: &str| {
        format!(
            r#"<div style="font-size:12px;font-weight:700;letter-spacing:.08em;text-transform:uppercase;color:{INK3};margin-bottom:6px">{text}</div>"#
        )
    };
    let tile = |label: &str, value: String, accent: bool| {
        format!(
            r#"<td width="25%" style="padding:5px"><table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="border:1px solid {RULE};border-radius:10px;background:#FFFFFF{}"><tr><td style="padding:12px 12px 10px"><div style="font-size:22px;font-weight:800;color:{INK};line-height:1.1">{value}</div><div style="font-size:12px;color:{INK3};margin-top:4px">{label}</div></td></tr></table></td>"#,
            if accent { format!(";border-top:3px solid {YELLOW}") } else { String::new() }
        )
    };
    let tiles = [
        tile(t("Nieuwe reizen", "New trips"), s.journeys_new.to_string(), false),
        tile(t("Te claimen", "Claimable"), s.candidates.to_string(), s.candidates > 0),
        tile(t("Ingediend", "Submitted"), s.submitted.to_string(), s.submitted > 0),
        tile(t("Te beoordelen", "To review"), s.needs_review.to_string(), s.needs_review > 0),
    ]
    .concat();
    let money = totals
        .map(|r| {
            let row = |label: &str, v: String, sub: String| {
                format!(
                    r#"<tr><td style="padding:10px 0;border-top:1px solid {RULE};font-size:14px;color:{INK}">{label}<div style="font-size:12px;color:{INK3}">{sub}</div></td><td align="right" style="padding:10px 0;border-top:1px solid {RULE};font-size:16px;font-weight:700;color:{INK};white-space:nowrap">{v}</td></tr>"#
                )
            };
            format!(
                r#"<tr><td style="padding:14px 28px 4px">{}<table role="presentation" width="100%" cellspacing="0" cellpadding="0">{}{}{}</table></td></tr>"#,
                eyebrow(t("Overzicht", "Overview")),
                row(
                    t("Totaal ontvangen", "Total received"),
                    euro(r.received_cents + r.external_cents, lang),
                    if r.external_count > 0 {
                        format!(
                            "{} via OV Refund ({}), {} {} ({})",
                            r.received_count,
                            euro(r.received_cents, lang),
                            r.external_count,
                            t("via een andere dienst", "via another service"),
                            euro(r.external_cents, lang)
                        )
                    } else {
                        n_claims(lang, r.received_count as usize)
                    },
                ),
                row(
                    t("In behandeling bij NS", "Pending at NS"),
                    format!("≈ {}", euro(r.pending_cents, lang)),
                    format!("{}, {}", n_claims(lang, r.pending_count as usize), t("verwacht bedrag", "expected amount")),
                ),
                row(
                    t("Nog te claimen (reizen)", "Still to claim (trips)"),
                    r.claimable_count.to_string(),
                    t("vertraagde reizen die je nog kunt claimen", "delayed trips you can still claim").to_string(),
                ),
            )
        })
        .unwrap_or_default();
    let list = |title: &str, items: &[String], color: &str| {
        if items.is_empty() {
            return String::new();
        }
        let li: String = items
            .iter()
            .map(|l| {
                format!(
                    r#"<tr><td width="14" valign="top" style="padding:8px 0;color:{color};font-size:13px">●</td><td style="padding:7px 0 7px 6px;font-size:14px;color:{INK};border-bottom:1px solid {RULE}">{}</td></tr>"#,
                    esc(&crate::redact::redact(&event_line(l, lang)))
                )
            })
            .collect();
        format!(
            r#"<tr><td style="padding:16px 28px 4px">{}<table role="presentation" width="100%" cellspacing="0" cellpadding="0">{li}</table></td></tr>"#,
            eyebrow(title)
        )
    };
    let details = list(t("Wat er gebeurde", "What happened"), lines, BLUE);
    let problems = list(t("Aandachtspunten", "Needs attention"), &s.errors, "#B42318");
    let demo_bar = if demo {
        format!(
            r#"<tr><td style="background:{INK};color:{YELLOW};font-size:12px;font-weight:700;letter-spacing:.08em;text-align:center;padding:7px">{}</td></tr>"#,
            t("DEMO · gesimuleerde gegevens, er is niets naar NS gestuurd", "DEMO · simulated data, nothing was sent to NS")
        )
    } else {
        String::new()
    };
    let link = |path: &str, label: &str| {
        format!(r#"<a href="{WEBSITE_URL}{path}" style="color:{BLUE};text-decoration:none;font-weight:600">{label}</a>"#)
    };
    let links = [
        link("", t("Website", "Website")),
        link("instructions.html", t("Instructies", "Instructions")),
        link("versions.html", t("Versies", "Versions")),
        link("disclaimer.html", t("Disclaimer", "Disclaimer")),
    ]
    .join(r#" <span style="color:#C9C5BA">&nbsp;·&nbsp;</span> "#);
    let closing = if welcome {
        t(
            "Dit is een testbericht. Je krijgt dit overzicht voortaan na elke automatische controle waarin iets gebeurt. Open OV Refund om claims te beoordelen of in te dienen.",
            "This is a test message. From now on you'll get this overview after every automatic check in which something happens. Open OV Refund to review or submit claims.",
        )
        .to_string()
    } else {
        format!(
            "{} {}{}. {}",
            n_trips(lang, s.journeys_seen),
            t("gecontroleerd", "checked"),
            if s.pending_evidence > 0 {
                format!(", {} {}", s.pending_evidence, t("nog wachten op treintijden", "still awaiting timetable"))
            } else {
                String::new()
            },
            t("Open OV Refund voor alle details en om claims te beoordelen.", "Open OV Refund for all details and to review claims.")
        )
    };
    let tiles_row = if welcome {
        String::new()
    } else {
        format!(
            r#"<tr><td style="padding:6px 23px 6px"><table role="presentation" width="100%" cellspacing="0" cellpadding="0"><tr>{tiles}</tr></table></td></tr>"#
        )
    };
    format!(
        r#"<!doctype html><html lang="{html_lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light"><meta name="supported-color-schemes" content="light"><title>OV Refund</title></head>
<body style="margin:0;padding:0;background:{BAND};font-family:{FONT};color:{INK}">
<div style="display:none;max-height:0;overflow:hidden;opacity:0">{headline}</div>
<table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="background:{BAND}"><tr><td align="center" style="padding:28px 12px">
<table role="presentation" width="600" cellspacing="0" cellpadding="0" style="max-width:600px;width:100%;background:#FFFFFF;border-radius:14px;overflow:hidden;border:1px solid {RULE}">
{demo_bar}
<tr><td style="padding:18px 28px;border-bottom:3px solid {YELLOW}">
  <table role="presentation" width="100%" cellspacing="0" cellpadding="0"><tr>
    <td width="44" valign="middle"><img src="cid:{LOGO_CID}" width="44" height="44" alt="OV Refund" style="display:block;border:0;border-radius:9px"></td>
    <td valign="middle" style="padding-left:12px;font-size:16px;font-weight:800;color:{INK}">OV Refund</td>
    <td valign="middle" align="right" style="font-size:12px;color:{INK3};white-space:nowrap">{subtitle}</td>
  </tr></table>
</td></tr>
<tr><td style="padding:24px 28px 4px"><div style="font-size:24px;font-weight:800;color:{INK};letter-spacing:-.01em">{title}</div></td></tr>
<tr><td style="padding:10px 28px 8px"><div style="background:{bg};color:{fg};border-radius:10px;padding:12px 14px;font-size:15px;font-weight:700">{headline}</div></td></tr>
{tiles_row}
{money}
{claims}
{details}
{problems}
<tr><td style="padding:22px 28px 26px"><div style="font-size:14px;line-height:1.5;color:{INK2}">{closing}</div></td></tr>
<tr><td style="background:{BAND};border-top:1px solid {RULE};padding:20px 28px 22px">
  <table role="presentation" width="100%" cellspacing="0" cellpadding="0"><tr>
    <td width="28" valign="top"><img src="cid:{LOGO_CID}" width="28" height="28" alt="" style="display:block;border:0;border-radius:6px"></td>
    <td valign="top" style="padding-left:10px">
      <div style="font-size:13px;font-weight:700;color:{INK}">OV Refund</div>
      <div style="font-size:12.5px;color:{INK3};margin-top:2px">{tagline}</div>
      <div style="font-size:12.5px;margin-top:10px">{links}</div>
      <div style="font-size:11.5px;line-height:1.55;color:{INK3};margin-top:12px">{footer}</div>
    </td>
  </tr></table>
</td></tr>
</table>
</td></tr></table></body></html>"#,
        html_lang = t("nl", "en"),
        bg = tone.0,
        fg = tone.1,
        claims = totals.map(|r| claims_table(&r.claims, lang)).unwrap_or_default(),
        title = if welcome { t("Welkom bij OV Refund", "Welcome to OV Refund") } else { t("Claimoverzicht", "Claim overview") },
        subtitle = if welcome { format!("{} · {date}", t("Testbericht", "Test message")) } else { format!("{} #{} · {date}", t("Controle", "Check"), s.run_id) },
        tagline = t("Geld terug bij vertraging, zonder gedoe.", "Delay refunds without the hassle."),
        footer = t(
            "Deze e-mail is door OV Refund op je eigen computer gemaakt en via je eigen mailbox verstuurd; hij bevat nooit kaartnummers, bankgegevens of wachtwoorden. Vertragingen en bedragen zijn schattingen; alleen NS of Arriva beslist over elke claim. OV Refund is niet verbonden aan NS of Arriva. Geen e-mails meer? Zet in OV Refund Meldingen → E-mailoverzicht uit.",
            "This e-mail was made by OV Refund on your own computer and sent through your own mailbox; it never contains card numbers, bank details or passwords. Delays and amounts are estimates; only NS or Arriva decides on each claim. OV Refund is not affiliated with NS or Arriva. No more e-mails? In OV Refund, turn off Notifications → E-mail summary.",
        ),
    )
}

/// Send a plain-text mail (optionally with an HTML alternative).
pub fn send(cfg: &SmtpConfig, password: Option<&str>, subject: &str, body: &str) -> Result<()> {
    send_rich(cfg, password, subject, body, None)
}

pub fn send_rich(cfg: &SmtpConfig, password: Option<&str>, subject: &str, body: &str, html: Option<&str>) -> Result<()> {
    let b = Message::builder()
        .from(cfg.from.parse().context("invalid from address")?)
        .to(cfg.to.parse().context("invalid to address")?)
        .subject(subject);
    let msg = match html {
        Some(h) => {
            // the logo travels inside the mail (cid:), so mail apps never fetch anything from the internet
            let html = SinglePart::html(h.to_string());
            let alt = MultiPart::alternative().singlepart(SinglePart::plain(body.to_string()));
            let alt = if h.contains("cid:ovr-logo") {
                let logo = Attachment::new_inline(LOGO_CID.to_string()).body(LOGO_PNG.to_vec(), "image/png".parse()?);
                alt.multipart(MultiPart::related().singlepart(html).singlepart(logo))
            } else {
                alt.singlepart(html)
            };
            b.multipart(alt)?
        }
        None => b.header(lettre::message::header::ContentType::TEXT_PLAIN).body(body.to_string())?,
    };
    let builder = match cfg.security.as_str() {
        "tls" => SmtpTransport::relay(&cfg.host)?,
        _ => SmtpTransport::starttls_relay(&cfg.host)?,
    };
    let mut builder = builder.port(cfg.port).timeout(Some(std::time::Duration::from_secs(30)));
    if let Some(pw) = password {
        builder = builder.credentials(Credentials::new(cfg.username.clone(), pw.to_string()));
    }
    builder.build().send(&msg).map_err(|e| anyhow::anyhow!(explain_smtp_error(&cfg.host, &e.to_string())))?;
    Ok(())
}

/// Turn SMTP failures into something a user can act on (no secrets are included).
pub fn explain_smtp_error(host: &str, raw: &str) -> String {
    let r = raw.to_lowercase();
    // "Nederlands / English": the app shows the half for its language (friendlyError in src/i18n.ts).
    let (nl, en) = if r.contains("535") || r.contains("username and password not accepted") || r.contains("authentication") {
        if host.contains("gmail") {
            (
                "Gmail weigerde de inlog. Gebruik een app-wachtwoord (16 letters, via myaccount.google.com/apppasswords), niet je gewone Gmail-wachtwoord. Daarvoor moet tweestapsverificatie aanstaan.",
                "Gmail refused the sign-in. Use an app password (16 letters, via myaccount.google.com/apppasswords), not your normal Gmail password. This needs 2-Step Verification turned on.",
            )
        } else if host.contains("outlook") || host.contains("office365") {
            (
                "Microsoft weigerde de inlog. Outlook.com en Microsoft 365 staan inloggen met een wachtwoord in mailprogramma's vaak niet meer toe. Gebruik een ander e-mailadres (bijvoorbeeld Gmail met een app-wachtwoord).",
                "Microsoft refused the sign-in. Outlook.com and Microsoft 365 often no longer allow password sign-in from mail programs. Use a different e-mail address (for example Gmail with an app password).",
            )
        } else {
            (
                "De mailserver weigerde de gebruikersnaam of het wachtwoord. Controleer je app-wachtwoord en vul het opnieuw in.",
                "The mail server refused the username or password. Check your app password and enter it again.",
            )
        }
    } else if r.contains("timed out") || r.contains("connection refused") || r.contains("dns") || r.contains("resolve") {
        (
            "OV Refund kan de mailserver niet bereiken. Controleer je internetverbinding, en onder Geavanceerd de server en poort.",
            "OV Refund cannot reach the mail server. Check your internet connection, and the server and port under Advanced.",
        )
    } else if r.contains("certificate") || r.contains("tls") {
        (
            "Beveiligde verbinding mislukt. Probeer onder Geavanceerd poort 465 met TLS of poort 587 met STARTTLS.",
            "Secure connection failed. Under Advanced, try port 465 with TLS or port 587 with STARTTLS.",
        )
    } else {
        (
            "Versturen mislukt. Controleer je e-mailinstellingen en probeer het opnieuw.",
            "Sending failed. Check your e-mail settings and try again.",
        )
    };
    // no " / " inside the server text, so the language split stays intact
    let server = crate::redact::redact(raw).chars().take(160).collect::<String>().replace(" / ", "/");
    format!("{nl} (server: {server}) / {en} (server: {server})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_notification_reports_an_unsent_or_uncertain_claim() {
        let uncertain = RunSummary { indeterminate: 1, auth_state: "ok".into(), ..Default::default() };
        assert!(toast_text(&uncertain, Lang::Nl).1.contains("Kijk in Mijn teruggaven"));
        assert!(toast_text(&uncertain, Lang::En).1.contains("Check Mijn teruggaven"));
        let failed = RunSummary { failed: 1, auth_state: "ok".into(), ..Default::default() };
        assert!(toast_text(&failed, Lang::Nl).1.contains("Open OV Refund"));
        assert!(toast_text(&failed, Lang::En).1.contains("Open OV Refund"));
    }

    #[test]
    fn microsoft_consumer_mail_is_unsupported() {
        for a in ["test-jan@hotmail.com", "test-Jan@Outlook.NL", "test-x@live.be", "test-y@msn.com", "test-z@hotmail.nl"] {
            assert!(unsupported_mail_address(a), "{a}");
        }
        for a in ["test-jan@gmail.com", "test-x@outlook.company.nl", "test-x@mylive.com", "hotmail.com", ""] {
            assert!(!unsupported_mail_address(a), "{a}");
        }
        let cfg = SmtpConfig {
            host: "smtp.gmail.com".into(),
            username: "test-a@gmail.com".into(),
            from: "test-a@gmail.com".into(),
            to: "test-a@hotmail.com".into(),
            ..Default::default()
        };
        assert!(!cfg.unsupported_provider(), "receiving at Hotmail via Gmail works");
        let cfg = SmtpConfig { host: "smtp-mail.outlook.com".into(), ..Default::default() };
        assert!(cfg.unsupported_provider());
    }

    #[test]
    fn gmail_auth_error_is_explained() {
        let m = explain_smtp_error("smtp.gmail.com", "permanent error (535): 5.7.8 Username and Password not accepted");
        assert!(m.contains("app-wachtwoord"));
    }

    #[test]
    fn amounts_follow_the_language() {
        assert_eq!(euro(635, Lang::Nl), "€ 6,35");
        assert_eq!(euro(635, Lang::En), "€6.35");
        assert_eq!(euro(5, Lang::Nl), "€ 0,05");
        assert_eq!(euro(123456, Lang::En), "€1234.56");
        assert_eq!(euro(-50, Lang::Nl), "-€ 0,50");
    }

    #[test]
    fn summary_has_no_secrets() {
        let s = RunSummary {
            run_id: 3,
            submitted: 1,
            errors: vec!["cookie=abc123 failed".into()],
            auth_state: "ok".into(),
            ..Default::default()
        };
        let (subj, body) = render_summary(&s, &["NL91ABNA0417164300 paid".into()], Lang::Nl);
        assert!(subj.contains("1 claim ingediend"), "{subj}");
        assert!(!body.contains("abc123"));
        assert!(!body.contains("NL91ABNA"));
        let html = render_html(
            &s,
            &["NL91ABNA0417164300 <b>paid</b>".into()],
            Some(&ReportTotals {
                received_cents: 658,
                claims: vec![ReportClaim {
                    date: "23-10-2026".into(),
                    time: "17:05".into(),
                    route: "A <x> → B".into(),
                    delay_min: Some(34),
                    band: Some("30_59".into()),
                    status: "PAID".into(),
                    reference: None,
                    expected_cents: Some(417),
                    paid_cents: Some(417),
                    expires: None,
                }],
                ..Default::default()
            }),
            true,
            Lang::Nl,
        );
        assert!(!html.contains("NL91ABNA") && !html.contains("abc123"));
        assert!(!html.contains("<b>paid"), "lines are escaped");
        assert!(html.contains("€ 6,58") && html.contains("DEMO"));
        assert!(html.contains("Uitbetaald") && html.contains("A &lt;x&gt; → B"));
    }

    #[test]
    fn english_mail_has_no_dutch_and_dutch_has_no_english() {
        let s = RunSummary {
            run_id: 7,
            submitted: 2,
            needs_review: 1,
            journeys_seen: 1,
            errors: vec!["NS login expired — open OV Refund and log in again".into()],
            auth_state: "ok".into(),
            ..Default::default()
        };
        let lines = vec![
            "submitted: Utrecht Centraal → Amsterdam Centraal on 03-11-2026".to_string(),
            "03-11-2026: already listed at NS — not submitted again".to_string(),
            "2 claim(s) with uncertain outcome — automatic submission paused until reconciled".to_string(),
        ];
        let claim = |status: &str| ReportClaim {
            date: "23-10-2026".into(),
            time: "17:05".into(),
            route: "A → B".into(),
            delay_min: Some(34),
            band: Some("30_59".into()),
            status: status.into(),
            reference: Some("R1".into()),
            expected_cents: Some(417),
            paid_cents: None,
            expires: Some("01-01-2027".into()),
        };
        let totals = ReportTotals {
            pending_count: 1,
            claims: ["READY", "SUBMITTED", "PAID", "EXTERNALLY_CLAIMED", "REJECTED"].iter().map(|s| claim(s)).collect(),
            ..Default::default()
        };
        let (subj, body) = render_summary(&s, &lines, Lang::En);
        let html = render_html(&s, &lines, Some(&totals), false, Lang::En);
        let welcome = render_welcome_html(false, Lang::En);
        assert!(!welcome.contains("23-10-2026") && !welcome.contains("All claims"));
        let (tt, tb) = toast_text(&s, Lang::En);
        let (ws, wb) = welcome_text(Lang::En);
        assert_eq!(subj, "OV Refund: 2 claims submitted");
        assert!(tb.contains("1 trip checked"), "{tb}");
        assert!(html.contains("2 claims submitted to NS") && html.contains("All claims") && html.contains(r#"lang="en""#));
        assert!(html.contains("cid:ovr-logo") && html.contains(WEBSITE_URL), "logo inline + website link");
        assert!(!html.contains("http://") && !html.contains("<img src=\"http"), "no remote images");
        assert!(body.contains("OV Refund needs to log in to NS again"), "{body}");
        assert!(html.contains("2 claims with an uncertain outcome; automatic submission is paused"));
        for text in [&subj, &body, &html, &welcome, &tt, &tb, &ws, &wb] {
            for nl in [
                "ingediend",
                "reizen",
                "Te claimen",
                "Uitbetaald",
                "Afgewezen",
                "vertraging",
                "Overzicht",
                "gecontroleerd",
                "claimen vóór",
                "Testbericht",
                "Deze e-mail",
                "Instructies",
                "(s)",
            ] {
                assert!(!text.contains(nl), "Dutch/awkward text {nl:?} in English output");
            }
        }
        let html_nl = render_html(&s, &lines, Some(&totals), false, Lang::Nl);
        let (_, body_nl) = render_summary(&s, &lines, Lang::Nl);
        let (_, tb_nl) = toast_text(&s, Lang::Nl);
        assert!(tb_nl.contains("1 reis gecontroleerd"), "{tb_nl}");
        for text in [&html_nl, &body_nl, &tb_nl] {
            for en in ["submitted", "already listed", "uncertain outcome", "login expired", "(s)", "reis/reizen", "Paid out", "All claims"]
            {
                assert!(!text.contains(en), "English/awkward text {en:?} in Dutch output");
            }
        }
    }

    #[test]
    fn subjects_follow_the_priority_and_plurals() {
        let base = RunSummary { auth_state: "ok".into(), ..Default::default() };
        let s = |f: &dyn Fn(&mut RunSummary)| {
            let mut x = base.clone();
            f(&mut x);
            (summary_subject(&x, Lang::Nl), summary_subject(&x, Lang::En))
        };
        assert_eq!(s(&|x| x.candidates = 1), ("OV Refund: 1 reis te claimen".into(), "OV Refund: 1 trip to claim".into()));
        assert_eq!(s(&|x| x.candidates = 3), ("OV Refund: 3 reizen te claimen".into(), "OV Refund: 3 trips to claim".into()));
        // claimable before review
        assert_eq!(
            s(&|x| {
                x.candidates = 2;
                x.needs_review = 5
            })
            .1,
            "OV Refund: 2 trips to claim"
        );
        // expiring right after submitted
        assert_eq!(
            s(&|x| {
                x.expiring_soon = 2;
                x.candidates = 2
            }),
            ("OV Refund: 2 claims verlopen binnen 14 dagen".into(), "OV Refund: 2 claims expire within 14 days".into())
        );
        assert_eq!(s(&|x| x.expiring_soon = 1).1, "OV Refund: 1 claim expires within 14 days");
        assert_eq!(
            s(&|x| {
                x.expiring_soon = 1;
                x.submitted = 1
            })
            .1,
            "OV Refund: 1 claim submitted"
        );
        assert_eq!(s(&|x| x.auth_state = "blocked: verificatiecode".into()).1, "OV Refund: NS asks for an extra check");
        assert_eq!(s(&|x| x.auth_state = "blocked: bot challenge".into()).0, "OV Refund: NS hield de controle tegen");
    }

    #[test]
    fn toasts_explain_login_problems_and_skip_zero_parts() {
        let mut s = RunSummary { auth_state: "blocked: verificatiecode".into(), ..Default::default() };
        assert_eq!(toast_text(&s, Lang::Nl).1, "NS vraagt om een extra controle. Open OV Refund en log zelf in.");
        s.auth_state = "blocked: bot/access challenge shown by NS".into();
        assert!(toast_text(&s, Lang::En).1.starts_with("NS stopped the check with an extra security check"));
        s.auth_state = "autologin_failed".into();
        assert!(toast_text(&s, Lang::En).1.contains("Did you change your NS password?"));
        s.auth_state = "login_required".into();
        let (title, body) = toast_text(&s, Lang::Nl);
        assert!(body.starts_with("OV Refund moet opnieuw inloggen bij NS") && !body.contains("verlopen"), "{title} {body}");
        s.auth_state = "account_mismatch".into();
        assert!(toast_text(&s, Lang::En).1.contains("a different NS account than before"));
        let ok = RunSummary { auth_state: "ok".into(), journeys_seen: 4, candidates: 1, ..Default::default() };
        let (_, b) = toast_text(&ok, Lang::En);
        assert_eq!(b, "4 trips checked · 1 claimable");
        assert!(!summary_is_relevant(&RunSummary { auth_state: "ok".into(), journeys_seen: 9, ..Default::default() }));
        assert!(summary_is_relevant(&RunSummary { auth_state: "ok".into(), expiring_soon: 1, ..Default::default() }));
        assert!(summary_is_relevant(&RunSummary { auth_state: "login_required".into(), ..Default::default() }));
        let auto = RunSummary { auth_state: "ok".into(), mode: "automatic".into(), submitted: 2, ..Default::default() };
        assert_eq!(summary_subject(&auto, Lang::Nl), "OV Refund: 2 claims automatisch ingediend");
        assert!(toast_text(&auto, Lang::En).1.contains("2 submitted automatically"));
        let manual = RunSummary { mode: "manual".into(), ..auto };
        assert_eq!(summary_subject(&manual, Lang::En), "OV Refund: 2 claims submitted");
    }

    #[test]
    fn event_lines_are_translated() {
        for (line, nl, en) in [
            (
                "automatic submission is no longer enabled — stopped",
                "Automatisch indienen staat niet meer aan",
                "Automatic submission is no longer on",
            ),
            ("no travel-history response from Mijn NS", "Mijn NS reageerde niet", "Mijn NS did not respond"),
            ("travel history returned HTTP 503", "HTTP 503", "Mijn NS returned an error"),
            (
                "claim #4: Er loopt al een controle. / A check is already running.",
                "claim #4: Er loopt al een controle.",
                "claim #4: A check is already running.",
            ),
            (crate::pipeline::NS_NO_RESPONSE_MSG, "Mijn NS reageerde niet", "Mijn NS did not respond"),
        ] {
            assert!(event_line(line, Lang::Nl).contains(nl), "{line}");
            assert!(event_line(line, Lang::En).contains(en), "{line}");
            assert!(!event_line(line, Lang::En).contains(" / "), "{line}");
        }
    }

    #[test]
    fn no_card_error_is_shown_in_the_mail_language() {
        let s = RunSummary { errors: vec![crate::pipeline::NO_CARD_ERROR.to_string()], auth_state: "ok".into(), ..Default::default() };
        let (_, en) = render_summary(&s, &[], Lang::En);
        let (_, nl) = render_summary(&s, &[], Lang::Nl);
        assert!(en.contains("No personal OV-chipkaart is linked") && !en.contains("Koppel je kaart"), "{en}");
        assert!(nl.contains("Er is geen persoonlijke OV-chipkaart") && !nl.contains("No personal"), "{nl}");
        let html = render_html(&s, &[], None, false, Lang::En);
        assert!(html.contains("No personal OV-chipkaart") && !html.contains("Koppel je kaart"));
    }
}
