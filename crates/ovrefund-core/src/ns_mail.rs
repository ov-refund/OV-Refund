//! Opt-in reading of NS "geld terug bij vertraging" e-mails from the user's own mailbox, to track
//! decisions and paid amounts for every claim (also claims filed by other services).
//!
//! Only messages that look like NS delay-refund mail are fetched (`BODY.PEEK`, so they stay
//! unread). Parsing is local; nothing leaves the PC. Formats observed (user-provided, 2026):
//! subjects "aanvraag voor geld terug bij vertraging", "je aanvraag met <nummer> is goedgekeurd.",
//! "je aanvraag (<nummer>) is helaas afgewezen"; bodies (based on the structure of NS mails):
//! - received: "... Het nummer van je aanvraag is <nummer>. We gaan aan de slag met je aanvraag ..."
//! - paid:     "Relatienummer: .. | Ons kenmerk: .. / Je aanvraag met nummer <nummer> ... is afgehandeld.
//!   Toelichting op het uit te betalen bedrag: ... Het bedrag EUR x,yy staat binnen twee weken op ..."
//! - rejected: "Relatienummer: .. | Ons kenmerk: .. / We hebben je aanvraag <nummer> ... bekeken. ...
//!   We kunnen je daarom geen geld teruggeven."
//!
//! NS's "Ons kenmerk" and "Relatienummer" are NOT the application number.
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailKind {
    Submitted,
    Approved,
    Rejected,
    Paid,
}

impl MailKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MailKind::Submitted => "submitted",
            MailKind::Approved => "approved",
            MailKind::Rejected => "rejected",
            MailKind::Paid => "paid",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [MailKind::Submitted, MailKind::Approved, MailKind::Rejected, MailKind::Paid].into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NsMail {
    /// Stable key (hash of Message-ID or date+subject) to process each mail once.
    pub key: String,
    pub kind: MailKind,
    pub reference: Option<String>,
    pub amount_cents: Option<i64>,
    pub travel_date: Option<NaiveDate>,
    pub mail_date: DateTime<Utc>,
    /// Station names found in the body (for matching), lower-case.
    pub stations: Vec<String>,
}

// The application number follows "aanvraag" ("je aanvraag met nummer N", "je aanvraag N",
// "het nummer van je aanvraag is N") or stands in parentheses. Never "Ons kenmerk"/"Relatienummer".
static REF: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)(?:aanvraag|aanvraagnummer|referentie|referentienummer|dossiernummer)(?:\s+(?:met\s+)?(?:nummer\s+|nr\.?\s*|is\s+)?|\s*:\s*)(\d{8,12})\b|\((\d{8,12})\)",
    )
    .unwrap()
});
static AMOUNT: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?:€|EUR|eur)\s?(\d{1,4})[,.](\d{2})").unwrap());
static DATE_NUM: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\d{1,2})[-/](\d{1,2})[-/](20\d{2})\b").unwrap());
static DATE_TXT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(\d{1,2})\s+(januari|februari|maart|april|mei|juni|juli|augustus|september|oktober|november|december)\s+(20\d{2})\b")
        .unwrap()
});

/// Does this subject look like an NS delay-refund mail?
pub fn is_ns_refund_subject(subject: &str) -> bool {
    let s = subject.to_lowercase();
    (s.contains("geld terug") && s.contains("vertraging"))
        || (s.contains("je aanvraag") && (s.contains("goedgekeurd") || s.contains("afgewezen") || s.contains("toegekend")))
}

/// Subject or body: a delay-refund mail? (Decision mails may have a generic "aanvraag" subject;
/// then the body has to be about a delay refund.)
fn is_refund_mail(subject: &str, body_lc: &str) -> bool {
    is_ns_refund_subject(subject)
        || (subject.to_lowercase().contains("aanvraag") && body_lc.contains("vertraging") && body_lc.contains("aanvraag"))
}

/// The decision, from NS's own sentences (body first: subjects are short and vary).
fn mail_kind(subject_lc: &str, b: &str) -> MailKind {
    // Only NS's own decision sentences. Not "kunnen we je aanvraag niet (behandelen)": NS also
    // writes that when it needs more information, which is no rejection.
    const REJECTED: [&str; 4] = ["geen geld teruggeven", "geen geld terug geven", "is helaas afgewezen", "hebben we afgewezen"];
    const PAID: [&str; 5] =
        ["uit te betalen bedrag", "staat binnen twee weken op", "hebben we overgemaakt", "is overgemaakt", "is uitbetaald"];
    if REJECTED.iter().any(|p| b.contains(p)) {
        MailKind::Rejected
    } else if PAID.iter().any(|p| b.contains(p)) {
        MailKind::Paid
    } else if subject_lc.contains("afgewezen") {
        MailKind::Rejected
    } else if subject_lc.contains("goedgekeurd") || subject_lc.contains("toegekend") {
        MailKind::Approved
    } else if subject_lc.contains("uitbetaald") || subject_lc.contains("overgemaakt") {
        MailKind::Paid
    } else {
        MailKind::Submitted
    }
}

fn month_nl(m: &str) -> Option<u32> {
    Some(match m.to_lowercase().as_str() {
        "januari" => 1,
        "februari" => 2,
        "maart" => 3,
        "april" => 4,
        "mei" => 5,
        "juni" => 6,
        "juli" => 7,
        "augustus" => 8,
        "september" => 9,
        "oktober" => 10,
        "november" => 11,
        "december" => 12,
        _ => return None,
    })
}

fn date_from(c: &regex::Captures) -> Option<NaiveDate> {
    if let (Some(d), Some(m), Some(y)) = (c.name("nd"), c.name("nm"), c.name("ny")) {
        return NaiveDate::from_ymd_opt(y.as_str().parse().ok()?, m.as_str().parse().ok()?, d.as_str().parse().ok()?);
    }
    let (d, m, y) = (c.name("td")?, c.name("tm")?, c.name("ty")?);
    NaiveDate::from_ymd_opt(y.as_str().parse().ok()?, month_nl(m.as_str())?, d.as_str().parse().ok()?)
}

/// The travel date: the date after "reisdatum" / "op reisdatum" first, then a date after "op"
/// ("vertraging op 17 oktober 2026"), and only then any other date in the body (a mail can
/// also mention the date it was written, a deadline, ...).
fn travel_date(body: &str) -> Option<NaiveDate> {
    const DATE: &str = r"(?:(?P<nd>\d{1,2})[-/](?P<nm>\d{1,2})[-/](?P<ny>20\d{2})|(?P<td>\d{1,2})\s+(?P<tm>januari|februari|maart|april|mei|juni|juli|augustus|september|oktober|november|december)\s+(?P<ty>20\d{2}))\b";
    static REISDATUM: Lazy<Regex> = Lazy::new(|| Regex::new(&format!(r"(?i)\breisdatum\s*:?\s*{DATE}")).unwrap());
    static OP: Lazy<Regex> = Lazy::new(|| Regex::new(&format!(r"(?i)\bop\s+{DATE}")).unwrap());
    REISDATUM
        .captures(body)
        .and_then(|c| date_from(&c))
        .or_else(|| OP.captures(body).and_then(|c| date_from(&c)))
        .or_else(|| {
            DATE_NUM.captures(body).and_then(|c| NaiveDate::from_ymd_opt(c[3].parse().ok()?, c[2].parse().ok()?, c[1].parse().ok()?))
        })
        .or_else(|| DATE_TXT.captures(body).and_then(|c| NaiveDate::from_ymd_opt(c[3].parse().ok()?, month_nl(&c[2])?, c[1].parse().ok()?)))
}

/// Parse one mail. `stations_known` are station names to look for in the body (for matching).
pub fn parse(key: &str, subject: &str, body: &str, mail_date: DateTime<Utc>, stations_known: &[String]) -> Option<NsMail> {
    let b = body.to_lowercase();
    if !is_refund_mail(subject, &b) {
        return None;
    }
    let kind = mail_kind(&subject.to_lowercase(), &b);
    let text = format!("{subject}\n{body}");
    let reference = REF.captures(&text).and_then(|c| c.get(1).or(c.get(2))).map(|m| m.as_str().to_string());
    let amount_cents = if kind == MailKind::Rejected {
        None
    } else {
        AMOUNT.captures(body).and_then(|c| Some(c[1].parse::<i64>().ok()? * 100 + c[2].parse::<i64>().ok()?))
    };
    let travel_date = travel_date(body);
    let stations = stations_known.iter().filter(|n| n.len() > 3 && b.contains(&n.to_lowercase())).map(|n| n.to_lowercase()).collect();
    Some(NsMail { key: key.to_string(), kind, reference, amount_cents, travel_date, mail_date, stations })
}

/// IMAP server for a known SMTP host (same account, same app password).
pub fn imap_for_smtp(smtp_host: &str) -> Option<(&'static str, u16)> {
    Some(match smtp_host {
        "smtp.gmail.com" => ("imap.gmail.com", 993),
        "smtp-mail.outlook.com" | "smtp.office365.com" => ("outlook.office365.com", 993),
        "smtp.mail.me.com" => ("imap.mail.me.com", 993),
        "smtp.mail.yahoo.com" => ("imap.mail.yahoo.com", 993),
        "smtp.ziggo.nl" => ("imap.ziggo.nl", 993),
        "smtp.kpnmail.nl" => ("imap.kpnmail.nl", 993),
        "smtp.xs4all.nl" => ("imap.xs4all.nl", 993),
        "smtp.fastmail.com" => ("imap.fastmail.com", 993),
        _ => return None,
    })
}

/// Sender address domain must be exactly ns.nl or a subdomain of it (not "evilns.nl").
pub fn is_ns_sender(from_header: &str) -> bool {
    let addr = from_header.rsplit('<').next().unwrap_or(from_header).trim_end_matches('>').trim();
    let domain = addr.rsplit('@').next().unwrap_or("").trim().to_lowercase();
    domain == "ns.nl" || domain.ends_with(".ns.nl")
}

/// Domain part of a From header ("NS <noreply@ns.nl>" -> "ns.nl"), for diagnostics only: lower
/// case, only `[a-z0-9.-]`, at most 100 characters; never the local part of the address.
pub fn sender_domain(from_header: &str) -> String {
    let addr = from_header.rsplit('<').next().unwrap_or(from_header).trim_end_matches('>').trim();
    let d: String = match addr.rsplit_once('@') {
        Some((_, d)) => d.trim().to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-').take(100).collect(),
        None => String::new(),
    };
    if d.is_empty() {
        "(unknown)".into()
    } else {
        d
    }
}

/// Raw mail (subject, plain-text body, date, key) fetched from the mailbox.
pub struct RawMail {
    pub key: String,
    pub subject: String,
    pub body: String,
    pub date: DateTime<Utc>,
}

/// NS-looking refund mails that were NOT used, per sender domain (counts only, for diagnostics).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SkippedCounts {
    /// Sender domain is not ns.nl (or a subdomain).
    pub not_ns_sender: std::collections::BTreeMap<String, u32>,
    /// Sender says ns.nl, but the mail provider did not confirm DKIM/DMARC for ns.nl.
    pub not_authenticated: std::collections::BTreeMap<String, u32>,
}

impl SkippedCounts {
    pub fn total(&self) -> u32 {
        self.not_ns_sender.values().sum::<u32>() + self.not_authenticated.values().sum::<u32>()
    }
}

/// Result of [`fetch`]: the usable NS mails plus counts of NS-looking mails that were skipped.
pub struct FetchResult {
    pub mails: Vec<RawMail>,
    pub skipped: SkippedCounts,
}

/// Which mails are used: (looks like an NS refund mail?, sender ok?, authenticated?) -> decision.
/// Pure, so the rules can be tested without a mailbox.
pub fn classify(subject: &str, body: &str, from: &str, auth_results: &[String], imap_host: &str, skipped: &mut SkippedCounts) -> bool {
    if !is_refund_mail(subject, &body.to_lowercase()) {
        return false; // not NS-looking at all: not counted
    }
    if !is_ns_sender(from) {
        *skipped.not_ns_sender.entry(sender_domain(from)).or_default() += 1;
        return false;
    }
    if !authenticated_ns(auth_results, imap_host) {
        *skipped.not_authenticated.entry(sender_domain(from)).or_default() += 1;
        return false;
    }
    true
}

/// The authserv-id (first token, RFC 8601) of an `Authentication-Results` header, lowercased.
/// `None` when the header has no id (it starts with a result such as `spf=pass`).
pub fn authserv_id(header: &str) -> Option<String> {
    let first = header.split(';').next()?.split_whitespace().next()?.to_lowercase();
    let host_like = !first.is_empty()
        && first.contains('.')
        && first.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !first.starts_with('.')
        && !first.ends_with('.');
    host_like.then_some(first)
}

/// `id` is a host strictly below `suffix` (which starts with a dot), e.g. `x.prod.outlook.com`.
fn below(id: &str, suffix: &str) -> bool {
    id.len() > suffix.len() && id.ends_with(suffix)
}

/// Does a single topmost authserv-id belong to the mailbox provider behind `imap_host`?
/// Gmail: exactly `mx.google.com`; Microsoft: `*.outlook.com` (incl. `*.prod.outlook.com`);
/// Yahoo: `*.yahoo.com` (incl. `*.mail.yahoo.com`); Fastmail: `*.messagingengine.com`.
/// iCloud uses two headers, see [`provider_auth_headers`]. Providers whose ids are not
/// documented (Ziggo, KPN, XS4ALL, unknown hosts) never match.
pub fn provider_authserv_ok(imap_host: &str, authserv_id: &str) -> bool {
    let id = authserv_id.trim().trim_end_matches(';').to_lowercase();
    match imap_host {
        "imap.gmail.com" => id == "mx.google.com",
        "outlook.office365.com" => below(&id, ".outlook.com"),
        "imap.mail.yahoo.com" => below(&id, ".yahoo.com"),
        "imap.fastmail.com" => below(&id, ".messagingengine.com"),
        _ => false,
    }
}

/// The provider's own TOP block of `Authentication-Results` headers (header order, topmost
/// first). A receiving provider adds its headers above everything already in the message, so a
/// header written by the sender always sits below this block and is never read.
/// - Gmail, Microsoft, Yahoo, Fastmail: only the topmost header, if its id is the provider's.
/// - iCloud: the first two headers, ids exactly `bimi.icloud.com` and then another
///   `*.icloud.com` (the DMARC result is in the second).
///
/// A header without id, a repeated id or a different id ends the block: nothing is trusted.
pub fn provider_auth_headers<'a>(auth_results: &'a [String], imap_host: &str) -> Vec<&'a String> {
    let ids: Vec<Option<String>> = auth_results.iter().take(2).map(|h| authserv_id(h)).collect();
    if imap_host == "imap.mail.me.com" {
        return match (ids.first(), ids.get(1)) {
            (Some(Some(a)), Some(Some(b))) if a == "bimi.icloud.com" && b != a && below(b, ".icloud.com") => {
                auth_results.iter().take(2).collect()
            }
            _ => vec![],
        };
    }
    match ids.first() {
        Some(Some(id)) if provider_authserv_ok(imap_host, id) => auth_results.iter().take(1).collect(),
        _ => vec![],
    }
}

/// Did the receiving mail provider verify the message as really coming from ns.nl? Only the
/// provider's own topmost `Authentication-Results` headers count ([`provider_auth_headers`]):
/// dmarc=pass header.from=ns.nl, or dkim=pass header.d=…ns.nl. `auth_results` in header order.
pub fn authenticated_ns(auth_results: &[String], imap_host: &str) -> bool {
    let auth_results = provider_auth_headers(auth_results, imap_host);
    let ns_domain = |d: &str| {
        let d = d.trim().trim_matches(|c: char| c == ';' || c == '"' || c == '@').to_lowercase();
        d == "ns.nl" || d.ends_with(".ns.nl")
    };
    auth_results.iter().any(|h| {
        h.to_lowercase().split(';').any(|clause| {
            let words: Vec<&str> = clause.split_whitespace().collect();
            let has = |k: &str| words.contains(&k);
            let val = |k: &str| words.iter().find_map(|w| w.strip_prefix(k));
            (has("dmarc=pass") && val("header.from=").map(ns_domain).unwrap_or(false))
                || (has("dkim=pass") && (val("header.d=").or_else(|| val("header.i=")).map(ns_domain).unwrap_or(false)))
        })
    })
}

/// Which folders to read, from the server's folder list (name, LIST attributes as text):
/// Gmail's "All Mail" (`\All`, holds the inbox and archived mail) when there is one, otherwise
/// INBOX; plus the spam folder (`\Junk`, or a folder named like spam/junk) when there is one.
pub fn pick_folders(list: &[(String, Vec<String>)]) -> Vec<String> {
    let has = |attrs: &[String], a: &str| attrs.iter().any(|x| x.trim_start_matches('\\').eq_ignore_ascii_case(a));
    let selectable = |attrs: &[String]| !has(attrs, "NoSelect") && !has(attrs, "NonExistent");
    let mut out = vec![];
    match list.iter().find(|(_, a)| has(a, "All") && selectable(a)) {
        Some((n, _)) => out.push(n.clone()),
        None => out.push("INBOX".to_string()),
    }
    let junk = list.iter().find(|(_, a)| has(a, "Junk") && selectable(a)).or_else(|| {
        list.iter().find(|(n, a)| {
            let last = n.rsplit(['/', '.']).next().unwrap_or(n).to_lowercase();
            selectable(a) && ["spam", "junk", "ongewenste e-mail", "ongewenst", "junk e-mail", "bulk mail"].contains(&last.as_str())
        })
    });
    if let Some((n, _)) = junk {
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
    out
}

/// Fetch NS refund mails since `since` (read-only; messages stay unread).
pub fn fetch(host: &str, port: u16, username: &str, password: &str, since: NaiveDate) -> Result<FetchResult> {
    let client = imap::ClientBuilder::new(host, port).connect().with_context(|| format!("IMAP connect {host}:{port}"))?;
    let mut session = client.login(username, password).map_err(|e| anyhow::anyhow!("IMAP login failed: {}", e.0))?;
    let folders = match session.list(None, Some("*")) {
        Ok(names) => pick_folders(
            &names
                .iter()
                .map(|n| (n.name().to_string(), n.attributes().iter().map(|a| format!("{a:?}")).collect::<Vec<_>>()))
                .collect::<Vec<_>>(),
        ),
        Err(_) => vec!["INBOX".to_string()],
    };
    let since_s = since.format("%d-%b-%Y").to_string();
    let mut out = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut skipped = SkippedCounts::default();
    for (fi, folder) in folders.iter().enumerate() {
        // INBOX (or All Mail) must open and be read; an extra folder (spam) that fails is skipped.
        if let Err(e) = session.examine(folder) {
            if fi == 0 {
                return Err(anyhow::anyhow!("open {folder}: {e}"));
            }
            log::warn!("optional mail folder skipped (open failed)");
            continue;
        }
        if let Err(e) = fetch_folder(&mut session, host, &since_s, &mut out, &mut seen, &mut skipped) {
            if fi == 0 {
                return Err(e);
            }
            log::warn!("optional mail folder skipped: {}", crate::redact::redact(&format!("{e:#}")));
        }
    }
    let _ = session.logout();
    Ok(FetchResult { mails: out, skipped })
}

fn fetch_folder<T: std::io::Read + std::io::Write>(
    session: &mut imap::Session<T>,
    imap_host: &str,
    since_s: &str,
    out: &mut Vec<RawMail>,
    seen: &mut std::collections::HashSet<String>,
    skipped: &mut SkippedCounts,
) -> Result<()> {
    // Narrow server-side: recent mail whose subject mentions the refund, or decisions on "je aanvraag".
    let mut ids: Vec<u32> = vec![];
    for q in [format!("SINCE {since_s} SUBJECT \"geld terug\""), format!("SINCE {since_s} SUBJECT \"aanvraag\"")] {
        if let Ok(found) = session.search(&q) {
            ids.extend(found);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    for chunk in ids.chunks(50) {
        let set = chunk.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let msgs = session.fetch(&set, "BODY.PEEK[]")?;
        for m in msgs.iter() {
            let Some(raw) = m.body() else { continue };
            let Ok(parsed) = mailparse::parse_mail(raw) else { continue };
            use mailparse::MailHeaderMap;
            let subject = parsed.headers.get_first_value("Subject").unwrap_or_default();
            let from = parsed.headers.get_first_value("From").unwrap_or_default().to_lowercase();
            // Only NS's own mail. The From header alone can be forged: require the mail
            // provider's verdict that DKIM/DMARC passed for ns.nl (fail closed when the provider
            // adds no such header). Only the provider's own topmost headers, recognised by their
            // authserv-id: headers further down may have been written by the sender. This holds
            // for every folder, so mail in the spam folder is used only when authenticated too.
            let auth: Vec<String> = parsed.headers.get_all_values("Authentication-Results");
            let body = body_text(&parsed);
            let msgid0 = parsed.headers.get_first_value("Message-ID");
            // The same mail in two folders (e.g. All Mail and a label) is used and counted once.
            if let Some(id) = &msgid0 {
                if seen.contains(id) {
                    continue;
                }
            }
            if !classify(&subject, &body, &from, &auth, imap_host, skipped) {
                continue;
            }
            let date = parsed
                .headers
                .get_first_value("Date")
                .and_then(|d| mailparse::dateparse(&d).ok())
                .and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0))
                .unwrap_or_else(Utc::now);
            let msgid = msgid0.unwrap_or_else(|| format!("{date}{subject}"));
            if !seen.insert(msgid.clone()) {
                continue;
            }
            out.push(RawMail { key: hex::encode(sha2::Sha256::digest_str(&msgid)), subject, body, date });
        }
    }
    Ok(())
}

trait DigestStr {
    fn digest_str(s: &str) -> Vec<u8>;
}
impl DigestStr for sha2::Sha256 {
    fn digest_str(s: &str) -> Vec<u8> {
        use sha2::Digest;
        sha2::Sha256::digest(s.as_bytes()).to_vec()
    }
}

/// Prefer text/plain; fall back to HTML with tags stripped.
fn body_text(m: &mailparse::ParsedMail) -> String {
    fn walk(m: &mailparse::ParsedMail, plain: &mut Option<String>, html: &mut Option<String>) {
        let ct = m.ctype.mimetype.to_lowercase();
        if m.subparts.is_empty() {
            if ct == "text/plain" && plain.is_none() {
                *plain = m.get_body().ok();
            } else if ct == "text/html" && html.is_none() {
                *html = m.get_body().ok();
            }
        }
        for p in &m.subparts {
            walk(p, plain, html);
        }
    }
    let (mut plain, mut html) = (None, None);
    walk(m, &mut plain, &mut html);
    plain.or_else(|| html.map(|h| strip_html(&h))).unwrap_or_default()
}

fn strip_html(h: &str) -> String {
    static TAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<style.*?</style>|<script.*?</script>|<[^>]+>").unwrap());
    TAG.replace_all(h, " ").replace("&nbsp;", " ").replace("&euro;", "€").replace("&amp;", "&")
}

#[cfg(test)]
mod folder_tests {
    use super::pick_folders;

    fn l(v: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        v.iter().map(|(n, a)| (n.to_string(), a.iter().map(|x| x.to_string()).collect())).collect()
    }

    #[test]
    fn gmail_all_mail_and_spam() {
        let gmail = l(&[
            ("INBOX", &[]),
            ("[Gmail]", &["NoSelect"]),
            ("[Gmail]/All Mail", &["All", "HasNoChildren"]),
            ("[Gmail]/Spam", &["Junk"]),
            ("[Gmail]/Sent Mail", &["Sent"]),
        ]);
        assert_eq!(pick_folders(&gmail), vec!["[Gmail]/All Mail", "[Gmail]/Spam"]);
    }

    #[test]
    fn inbox_plus_named_junk_folder() {
        assert_eq!(pick_folders(&l(&[("INBOX", &[]), ("INBOX.Spam", &[])])), vec!["INBOX", "INBOX.Spam"]);
        assert_eq!(pick_folders(&l(&[("INBOX", &[]), ("Ongewenste e-mail", &[])])), vec!["INBOX", "Ongewenste e-mail"]);
        assert_eq!(pick_folders(&l(&[("INBOX", &[]), ("Archive", &["Archive"])])), vec!["INBOX"]);
        assert_eq!(pick_folders(&[]), vec!["INBOX"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d() -> DateTime<Utc> {
        "2026-06-17T10:00:00Z".parse().unwrap()
    }

    #[test]
    fn approved_with_reference_and_amount() {
        let m = parse(
            "k1",
            "je aanvraag met 600000001 is goedgekeurd.",
            "Beste reiziger, je reis van Haarlem naar Amersfoort Centraal op 12-03-2026 had vertraging. We maken € 3,10 over op je rekening.",
            d(),
            &["Haarlem".into(), "Amersfoort Centraal".into(), "Alkmaar".into()],
        )
        .unwrap();
        assert_eq!(m.kind, MailKind::Approved);
        assert_eq!(m.reference.as_deref(), Some("600000001"));
        assert_eq!(m.amount_cents, Some(310));
        assert_eq!(m.travel_date, NaiveDate::from_ymd_opt(2026, 3, 12));
        assert_eq!(m.stations, vec!["haarlem".to_string(), "amersfoort centraal".to_string()]);
    }

    #[test]
    fn rejected_in_parentheses_has_no_amount() {
        let m = parse("k2", "je aanvraag (600000002) is helaas afgewezen", "Je reis op 9 september 2026 ... € 4,00", d(), &[]).unwrap();
        assert_eq!(m.kind, MailKind::Rejected);
        assert_eq!(m.reference.as_deref(), Some("600000002"));
        assert_eq!(m.amount_cents, None);
        assert_eq!(m.travel_date, NaiveDate::from_ymd_opt(2026, 9, 9));
    }

    #[test]
    fn submitted_and_unrelated_mail() {
        assert_eq!(parse("k3", "aanvraag voor geld terug bij vertraging", "", d(), &[]).unwrap().kind, MailKind::Submitted);
        assert!(parse("k4", "Je NS-factuur van oktober", "", d(), &[]).is_none());
    }

    #[test]
    fn sender_domain_is_exact() {
        assert!(is_ns_sender("NS Klantenservice <noreply@ns.nl>"));
        assert!(is_ns_sender("x@mail.ns.nl"));
        assert!(!is_ns_sender("NS <noreply@evilns.nl>"));
        assert!(!is_ns_sender("noreply@ns.nl.attacker.com"));
    }

    #[test]
    fn provider_must_have_authenticated_ns() {
        let ok = vec![
            "mx.google.com; dkim=pass header.i=@mail.ns.nl header.s=s1; spf=pass; dmarc=pass (p=REJECT) header.from=ns.nl".to_string()
        ];
        let g = "imap.gmail.com";
        assert!(authenticated_ns(&ok, g));
        let dkim_only = vec!["mx.google.com; dkim=pass header.d=ns.nl".to_string()];
        assert!(authenticated_ns(&dkim_only, g));
        let forged = vec!["mx.google.com; dkim=pass header.i=@attacker.example; dmarc=fail header.from=ns.nl".to_string()];
        assert!(!authenticated_ns(&forged, g));
        assert!(!authenticated_ns(&[], g), "no verdict -> not trusted");
        assert!(!authenticated_ns(&["mx.google.com; dkim=pass header.d=ns.nl.evil.example".to_string()], g));
    }

    #[test]
    fn only_the_providers_own_topmost_headers_count() {
        let pass = "dkim=pass header.d=ns.nl; dmarc=pass header.from=ns.nl";
        let g = "imap.gmail.com";
        // sender-written header (no provider header on top): forged
        assert!(!authenticated_ns(&[format!("mx.example; {pass}")], g));
        assert!(!authenticated_ns(&[format!("spf=pass smtp.mailfrom=ns.nl; {pass}")], g), "no authserv-id");
        // provider header on top says fail; a forged pass below is ignored
        let h = vec!["mx.google.com; dmarc=fail header.from=ns.nl".to_string(), format!("mx.google.com; {pass}")];
        assert!(!authenticated_ns(&h, g), "repeated id below is not the provider's");
        let h = vec!["mx.google.com; dmarc=fail header.from=ns.nl".to_string(), format!("evil.example; {pass}")];
        assert!(!authenticated_ns(&h, g));
        // another provider's id is not accepted for Gmail
        assert!(!authenticated_ns(&[format!("mx.yahoo.com; {pass}")], g));
        // Outlook, Yahoo, iCloud (two own headers), Fastmail
        assert!(authenticated_ns(&[format!("am6pr01.prod.outlook.com; {pass}")], "outlook.office365.com"));
        assert!(authenticated_ns(&[format!("db8pr02mb1234.eurprd02.prod.outlook.com; {pass}")], "outlook.office365.com"));
        assert!(authenticated_ns(&[format!("spf.protection.outlook.com; {pass}")], "outlook.office365.com"));
        assert!(
            !authenticated_ns(&[format!("spf=pass smtp.mailfrom=ns.nl; {pass}")], "outlook.office365.com"),
            "Microsoft header without id"
        );
        assert!(authenticated_ns(&[format!("atlas-production.v2.mail.yahoo.com; {pass}")], "imap.mail.yahoo.com"));
        assert!(authenticated_ns(&[format!("atlas123.free.mail.ne1.yahoo.com; {pass}")], "imap.mail.yahoo.com"));
        let icloud = vec!["bimi.icloud.com; bimi=skipped".to_string(), "dmarc.icloud.com; dmarc=pass header.from=ns.nl".to_string()];
        assert!(authenticated_ns(&icloud, "imap.mail.me.com"), "iCloud: bimi header on top, DMARC in the second");
        assert!(authenticated_ns(&[format!("mx6.messagingengine.com; {pass}")], "imap.fastmail.com"));
        assert!(authenticated_ns(&[format!("mx3.messagingengine.com 1; {pass}")], "imap.fastmail.com"), "version after the id");
        // providers without documented authserv-ids: never authenticated
        for host in ["imap.ziggo.nl", "imap.kpnmail.nl", "imap.xs4all.nl", "imap.example.org"] {
            assert!(!authenticated_ns(&[format!("mx.{host}; {pass}")], host), "{host}");
        }
        assert!(!provider_authserv_ok("imap.gmail.com", "evilmx.google.com"));
        assert!(!provider_authserv_ok("imap.gmail.com", "x.mx.google.com"));
        assert!(!provider_authserv_ok("imap.mail.yahoo.com", "yahoo.com.evil.example"));
        assert!(!provider_authserv_ok("imap.mail.yahoo.com", "evilyahoo.com"));
        assert!(!provider_authserv_ok("outlook.office365.com", "outlook.com"));
        assert!(!provider_authserv_ok("outlook.office365.com", "evil-outlook.com"));
        assert!(!provider_authserv_ok("imap.fastmail.com", "messagingengine.com.evil.example"));
        assert!(!provider_authserv_ok("imap.mail.me.com", "dmarc.icloud.com"), "iCloud needs both headers");
    }

    #[test]
    fn body_boilerplate_afgewezen_does_not_reject() {
        let m = parse("k", "aanvraag voor geld terug bij vertraging", "Als je aanvraag wordt afgewezen, laten we het weten.", d(), &[])
            .unwrap();
        assert_eq!(m.kind, MailKind::Submitted);
    }

    #[test]
    fn imap_hosts() {
        assert_eq!(imap_for_smtp("smtp.gmail.com"), Some(("imap.gmail.com", 993)));
        assert_eq!(imap_for_smtp("smtp.example.org"), None);
    }

    // Fictional samples based on the structure of NS mails.
    const PAID: &str = "Logo Nederlandse Spoorwegen Link Mijn NS Zakelijk\nRelatienummer: 11112222 | Ons kenmerk: 9998887776\nBeste J. de Vries,\nJe aanvraag met nummer 600000123 voor een vergoeding vanwege vertraging op 17 oktober 2026 is afgehandeld.\nToelichting op het uit te betalen bedrag:\n Restitutie 30-59 minuten vertraging Haarlem – Alkmaar op 17 oktober 2026 van: € 4,17\nHet bedrag € 4,17 staat binnen twee weken op het door jou opgegeven rekeningnummer NL** **** **** ***1 23.";
    const REJECTED: &str = "Relatienummer: 11113333 | Ons kenmerk: 9998880001\nBeste J. de Vries,\nWe hebben je aanvraag 600000456 voor geld terug bij vertraging op reisdatum 6 augustus 2026 bekeken.\nOp de door jou aangegeven reisdatum was je niet in- of uitgecheckt. We kunnen je daarom geen geld teruggeven.\nGeef meer informatie door";
    const RECEIVED: &str = "Beste J. de Vries,\nIn je aanvraag geef je aan dat je op 6 augustus 2026 om 09:42 uur een vertraging van 30-59 minuten hebt gehad op het traject tussen Delft en Gouda. Het nummer van je aanvraag is 600000456.\nWe gaan aan de slag met je aanvraag\nWe laten je meestal binnen een week weten of je aanvraag is goedgekeurd.";

    #[test]
    fn sample_paid_mail_uses_application_number_not_kenmerk() {
        let st = ["Haarlem".to_string(), "Alkmaar".to_string()];
        for subject in ["Je aanvraag met 600000123 is goedgekeurd.", "Je aanvraag is afgehandeld"] {
            let m = parse("p", subject, PAID, d(), &st).unwrap();
            assert_eq!(m.kind, MailKind::Paid, "{subject}");
            assert_eq!(m.reference.as_deref(), Some("600000123"), "never 'Ons kenmerk' or 'Relatienummer'");
            assert_eq!(m.amount_cents, Some(417));
            assert_eq!(m.travel_date, NaiveDate::from_ymd_opt(2026, 10, 17));
            assert_eq!(m.stations.len(), 2);
        }
    }

    #[test]
    fn sample_rejection_mail() {
        for subject in ["Je aanvraag (600000456) is helaas afgewezen", "Over je aanvraag voor geld terug bij vertraging"] {
            let m = parse("r", subject, REJECTED, d(), &[]).unwrap();
            assert_eq!(m.kind, MailKind::Rejected, "{subject}");
            assert_eq!(m.reference.as_deref(), Some("600000456"));
            assert_eq!(m.amount_cents, None);
            assert_eq!(m.travel_date, NaiveDate::from_ymd_opt(2026, 8, 6));
        }
    }

    #[test]
    fn sample_received_mail_is_not_a_decision() {
        let st = ["Delft".to_string(), "Gouda".to_string()];
        let m = parse("s", "Aanvraag voor geld terug bij vertraging", RECEIVED, d(), &st).unwrap();
        assert_eq!(m.kind, MailKind::Submitted, "'...of je aanvraag is goedgekeurd' is not an approval");
        assert_eq!(m.reference.as_deref(), Some("600000456"));
        assert_eq!(m.travel_date, NaiveDate::from_ymd_opt(2026, 8, 6));
        assert_eq!(m.stations, vec!["delft".to_string(), "gouda".to_string()]);
    }

    #[test]
    fn more_information_needed_is_not_a_rejection() {
        let body = "We hebben je aanvraag 600000789 voor geld terug bij vertraging op 3 oktober 2026 bekeken. We hebben meer informatie nodig. Zonder deze informatie kunnen we je aanvraag niet behandelen. Geef meer informatie door.";
        let m = parse("i", "Over je aanvraag voor geld terug bij vertraging", body, d(), &[]).unwrap();
        assert_ne!(m.kind, MailKind::Rejected);
        assert_eq!(m.kind, MailKind::Submitted);
        assert_eq!(m.reference.as_deref(), Some("600000789"));
        // the real rejection sentences still reject
        for p in ["geen geld teruggeven", "geen geld terug geven", "is helaas afgewezen", "hebben we afgewezen"] {
            let m = parse("r", "Over je aanvraag voor geld terug bij vertraging", &format!("Je aanvraag: {p}."), d(), &[]).unwrap();
            assert_eq!(m.kind, MailKind::Rejected, "{p}");
        }
    }

    #[test]
    fn travel_date_prefers_the_date_after_op_or_reisdatum() {
        // a numeric date elsewhere (mail date, deadline) must not win over the travel date
        let body = "Datum: 02-11-2026\nJe reis op 17 oktober 2026 had vertraging. Reageer voor 30-11-2026.";
        assert_eq!(travel_date(body), NaiveDate::from_ymd_opt(2026, 10, 17));
        let body = "Verzonden 01-11-2026. Je aanvraag voor geld terug bij vertraging op reisdatum 06-08-2026 is bekeken.";
        assert_eq!(travel_date(body), NaiveDate::from_ymd_opt(2026, 8, 6));
        let body = "Geschreven op 5 november 2026. Reisdatum: 12 september 2026.";
        assert_eq!(travel_date(body), NaiveDate::from_ymd_opt(2026, 9, 12), "'reisdatum' beats 'op'");
        // no prefix: the first date, as before
        assert_eq!(travel_date("Vertraging 12-03-2026."), NaiveDate::from_ymd_opt(2026, 3, 12));
        assert_eq!(travel_date("geen datum"), None);
    }

    #[test]
    fn ns_klantenservice_sender_and_skip_counts() {
        assert!(is_ns_sender("NS Klantenservice <noreply.ns@ns.nl>"));
        assert_eq!(sender_domain("NS Klantenservice <noreply.ns@ns.nl>"), "ns.nl");
        assert_eq!(sender_domain("x"), "(unknown)");
        let auth_ok = vec!["mx.google.com; dmarc=pass header.from=ns.nl".to_string()];
        let mut sk = SkippedCounts::default();
        let subject = "Aanvraag voor geld terug bij vertraging";
        let g = "imap.gmail.com";
        assert!(classify(subject, "", "NS <noreply@ns.nl>", &auth_ok, g, &mut sk));
        assert!(!classify(subject, "", "NS <noreply@ns-mail.example>", &auth_ok, g, &mut sk));
        assert!(!classify(subject, "", "NS <noreply@ns.nl>", &[], g, &mut sk));
        assert!(!classify("Je NS-factuur", "", "NS <noreply@other.example>", &auth_ok, g, &mut sk), "not NS-looking");
        assert_eq!(sk.not_ns_sender.get("ns-mail.example"), Some(&1));
        assert_eq!(sk.not_authenticated.get("ns.nl"), Some(&1));
        assert_eq!(sk.total(), 2, "unrelated mail is not counted");
        let json = serde_json::to_string(&sk).unwrap();
        assert!(!json.contains("noreply"), "domains only: {json}");
    }

    #[test]
    fn other_ns_aanvraag_mail_is_ignored() {
        assert!(parse("o", "Je aanvraag voor een OV-chipkaart", "Je aanvraag nummer 600000999 is ontvangen.", d(), &[]).is_none());
    }
}

#[cfg(test)]
mod topmost_regression_tests {
    use super::*;
    #[test]
    fn attacker_pass_below_provider_failure_is_ignored_for_every_provider() {
        for (host, id) in [
            ("imap.gmail.com", "mx.google.com"),
            ("outlook.office365.com", "am6pr01.prod.outlook.com"),
            ("imap.mail.yahoo.com", "atlas-production.v2.mail.yahoo.com"),
            ("imap.fastmail.com", "mx6.messagingengine.com"),
        ] {
            let pass = format!("{id}; dmarc=pass header.from=ns.nl");
            assert!(authenticated_ns(std::slice::from_ref(&pass), host), "{host}");
            // attacker header directly below the provider's own header: never read
            assert!(!authenticated_ns(&[format!("{id}; dmarc=fail header.from=ns.nl"), pass.clone()], host), "{host}");
            assert!(!authenticated_ns(&[format!("{id}; dmarc=none"), "evil.example; dmarc=pass header.from=ns.nl".into()], host), "{host}");
            // provider header missing: the attacker's header on top is not the provider's id
            assert!(!authenticated_ns(&["evil.example; dmarc=pass header.from=ns.nl".into(), pass.clone()], host), "{host}");
            assert!(!provider_authserv_ok(host, &format!("{id}.attacker.example")));
        }
        let ic = "imap.mail.me.com";
        let bimi = || "bimi.icloud.com; bimi=skipped".to_string();
        let dmarc = |r: &str| format!("dmarc.icloud.com; dmarc={r} header.from=ns.nl");
        assert!(authenticated_ns(&[bimi(), dmarc("pass")], ic));
        // iCloud's DMARC says fail; an attacker pass directly below is ignored
        assert!(!authenticated_ns(&[bimi(), dmarc("fail"), dmarc("pass")], ic));
        assert!(!authenticated_ns(&[bimi(), dmarc("fail"), "evil.example; dmarc=pass header.from=ns.nl".into()], ic));
        // wrong order, repeated id, header without id, single header: not trusted
        assert!(!authenticated_ns(&[dmarc("pass"), bimi()], ic));
        assert!(!authenticated_ns(&[bimi(), "bimi.icloud.com; dmarc=pass header.from=ns.nl".into()], ic));
        assert!(!authenticated_ns(&[bimi(), "dmarc=pass header.from=ns.nl".into()], ic));
        assert!(!authenticated_ns(&[dmarc("pass")], ic));
        assert!(!authenticated_ns(&[bimi(), "dmarc.icloud.com.evil.example; dmarc=pass header.from=ns.nl".into()], ic));
    }
}
