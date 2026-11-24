//! One resolver for the elements OV Refund uses on NS's website (claim form, travel-history filter).
//!
//! Every logical element has an ordered list of strategies: (a) the id/selector observed live,
//! (b) its visible label or question text (Dutch, exactly as NS shows it), (c) its ARIA role and
//! accessible name. Every strategy searches through open shadow roots. A strategy only counts
//! when it finds exactly ONE element; several matches never pick "the first". When two strategies
//! each find one element but not the same one (e.g. NS swapped ids), nothing is used at all.
//!
//! The page script ([`RESOLVER_JS`]) only reports what each strategy finds; the decision is made
//! in Rust ([`choose`]) so it can be unit-tested. The resulting element expression re-runs just
//! the chosen strategy and again returns an element only when it is unique.
//!
//! A fallback match never relaxes a safety check: the callers still verify prefilled values,
//! read back what was typed, require the declaration to be ticked, run the pre-submit form check
//! and require the button to be the form's own submit button.
use crate::ns_bridge::NsBridge;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// What kind of control an element must be (every strategy filters on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Radio,
    Checkbox,
    Text,
    Select,
    Button,
    Any,
}

impl Kind {
    fn js(self) -> &'static str {
        match self {
            Kind::Radio => "radio",
            Kind::Checkbox => "checkbox",
            Kind::Text => "text",
            Kind::Select => "select",
            Kind::Button => "button",
            Kind::Any => "any",
        }
    }
}

/// One way to find an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// CSS selector (as observed live), optionally restricted to elements whose text contains `text`.
    Css { sel: &'static str, text: Option<&'static str> },
    /// Visible label text (form fields) or own text (buttons, links). For a radio button `scope`
    /// is the question: its nearest group (fieldset / radiogroup) must contain that text.
    Label { text: &'static str, scope: Option<&'static str> },
    /// A `<select>` that offers an option with this text.
    Option { text: &'static str },
    /// ARIA role plus accessible name (aria-label, aria-labelledby, label, own text).
    Aria { role: &'static str, name: &'static str },
}

impl Strategy {
    pub fn name(&self) -> &'static str {
        match self {
            Strategy::Css { .. } => "id/selector",
            Strategy::Label { .. } => "label text",
            Strategy::Option { .. } => "option text",
            Strategy::Aria { .. } => "ARIA role/name",
        }
    }

    fn json(&self) -> Value {
        match *self {
            Strategy::Css { sel, text } => json!({"t": "css", "sel": sel, "text": text}),
            Strategy::Label { text, scope } => json!({"t": "label", "text": text, "scope": scope}),
            Strategy::Option { text } => json!({"t": "option", "text": text}),
            Strategy::Aria { role, name } => json!({"t": "aria", "role": role, "name": name}),
        }
    }
}

/// A logical NS page element and its ordered strategies (the first one is the primary).
#[derive(Debug)]
pub struct ElSpec {
    pub key: &'static str,
    pub kind: Kind,
    pub strategies: &'static [Strategy],
}

impl ElSpec {
    /// The spec as a JSON literal for the page script (static texts only, no personal data).
    pub fn json(&self) -> String {
        json!({"key": self.key, "kind": self.kind.js(), "strategies": self.strategies.iter().map(Strategy::json).collect::<Vec<_>>()})
            .to_string()
    }
}

// Texts exactly as on NS's claim form ("Geld terug bij vertraging", observed November 2026).
pub const Q_ARRIVAL: &str = "Heb je je gewenste NS-eindstation bereikt?";
pub const A_ARRIVAL_YES: &str = "Ja, ik heb mijn NS-eindstation bereikt";
pub const A_ARRIVAL_NO: &str = "Nee, ik heb mijn NS-eindstation niet bereikt";
pub const L_DEPARTURE: &str = "Vertrektijd volgens dienstregeling";
pub const Q_DELAY: &str = "Vertraging";
pub const A_DELAY_30: &str = "30 t/m 59 min";
pub const A_DELAY_60: &str = "60 minuten of meer";
pub const L_NAME: &str = "Naam rekeninghouder";
pub const L_IBAN: &str = "Bankrekeningnummer (IBAN)";
pub const L_DECLARATION: &str = "Ik heb alles naar waarheid ingevuld.";
pub const L_SUBMIT: &str = "Verzenden";
pub const L_SHOW: &str = "Tonen";
pub const L_CLAIM_LINK: &str = "Geld terug bij vertraging";
pub const PERIOD_YEAR: &str = "Afgelopen jaar";
pub const PERIOD_MAX: &str = "Afgelopen 1,5 jaar";

pub static ARRIVAL_YES: ElSpec = ElSpec {
    key: "arrival_yes",
    kind: Kind::Radio,
    strategies: &[
        Strategy::Css { sel: "#arrival0", text: None },
        Strategy::Label { text: A_ARRIVAL_YES, scope: Some(Q_ARRIVAL) },
        Strategy::Aria { role: "radio", name: A_ARRIVAL_YES },
    ],
};
pub static ARRIVAL_NO: ElSpec = ElSpec {
    key: "arrival_no",
    kind: Kind::Radio,
    strategies: &[
        Strategy::Css { sel: "#arrival1", text: None },
        Strategy::Label { text: A_ARRIVAL_NO, scope: Some(Q_ARRIVAL) },
        Strategy::Aria { role: "radio", name: A_ARRIVAL_NO },
    ],
};
pub static DEPARTURE: ElSpec = ElSpec {
    key: "departure",
    kind: Kind::Text,
    strategies: &[
        Strategy::Css { sel: "#departure", text: None },
        Strategy::Label { text: L_DEPARTURE, scope: None },
        Strategy::Aria { role: "textbox", name: L_DEPARTURE },
    ],
};
pub static DELAY_30: ElSpec = ElSpec {
    key: "delay_30_59",
    kind: Kind::Radio,
    strategies: &[
        Strategy::Css { sel: "#delay0", text: None },
        Strategy::Label { text: A_DELAY_30, scope: Some(Q_DELAY) },
        Strategy::Aria { role: "radio", name: A_DELAY_30 },
    ],
};
pub static DELAY_60: ElSpec = ElSpec {
    key: "delay_60_plus",
    kind: Kind::Radio,
    strategies: &[
        Strategy::Css { sel: "#delay1", text: None },
        Strategy::Label { text: A_DELAY_60, scope: Some(Q_DELAY) },
        Strategy::Aria { role: "radio", name: A_DELAY_60 },
    ],
};
/// Prefilled by NS and verified against the trip; only the observed id is trusted (no label
/// fallback: the visible texts of these fields have not been recorded).
pub static CHECK_IN: ElSpec = ElSpec { key: "check_in", kind: Kind::Select, strategies: &[Strategy::Css { sel: "#checkIn", text: None }] };
pub static CHECK_OUT: ElSpec =
    ElSpec { key: "check_out", kind: Kind::Select, strategies: &[Strategy::Css { sel: "#checkOut", text: None }] };
pub static NAME: ElSpec = ElSpec {
    key: "account_holder",
    kind: Kind::Text,
    strategies: &[
        Strategy::Css { sel: "#name", text: None },
        Strategy::Label { text: L_NAME, scope: None },
        Strategy::Aria { role: "textbox", name: L_NAME },
    ],
};
pub static IBAN: ElSpec = ElSpec {
    key: "iban",
    kind: Kind::Text,
    strategies: &[
        Strategy::Css { sel: "#iban", text: None },
        Strategy::Label { text: L_IBAN, scope: None },
        Strategy::Aria { role: "textbox", name: L_IBAN },
    ],
};
pub static DECLARATION: ElSpec = ElSpec {
    key: "declaration",
    kind: Kind::Checkbox,
    strategies: &[
        Strategy::Css { sel: "#declaration", text: None },
        Strategy::Label { text: L_DECLARATION, scope: None },
        Strategy::Aria { role: "checkbox", name: L_DECLARATION },
    ],
};
pub static SUBMIT: ElSpec = ElSpec {
    key: "submit",
    kind: Kind::Button,
    strategies: &[
        Strategy::Css { sel: "button[type=submit]", text: None },
        Strategy::Label { text: L_SUBMIT, scope: None },
        Strategy::Aria { role: "button", name: L_SUBMIT },
    ],
};
pub static PERIOD_FILTER: ElSpec = ElSpec {
    key: "history_period",
    kind: Kind::Select,
    strategies: &[Strategy::Css { sel: "#filter-period-control", text: None }, Strategy::Option { text: PERIOD_MAX }],
};
pub static SHOW_BUTTON: ElSpec = ElSpec {
    key: "history_show",
    kind: Kind::Button,
    strategies: &[
        Strategy::Css { sel: "mns-transaction-filter nes-button", text: Some(L_SHOW) },
        Strategy::Label { text: L_SHOW, scope: None },
        Strategy::Aria { role: "button", name: L_SHOW },
    ],
};
/// Searched only inside the opened trip row's container (see `open_claim_form`).
pub static CLAIM_LINK: ElSpec = ElSpec {
    key: "claim_link",
    kind: Kind::Any,
    strategies: &[Strategy::Css { sel: "[data-test=gtbv-claim-link]", text: None }, Strategy::Label { text: L_CLAIM_LINK, scope: None }],
};

/// Every field OV Refund fills or checks on the claim form (the pre-submit check treats any
/// other visible required field as unknown).
pub static FORM_FIELDS: [&ElSpec; 10] =
    [&CHECK_IN, &CHECK_OUT, &ARRIVAL_YES, &ARRIVAL_NO, &DEPARTURE, &DELAY_30, &DELAY_60, &NAME, &IBAN, &DECLARATION];

/// What one strategy found: the number of matches and, for exactly one match, the identity of
/// that element followed by its (composed) ancestors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub count: u64,
    pub chain: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The first strategy with a unique match, consistent with every other unique match.
    Found(usize),
    NotFound,
    /// Strategies matched, but none of them uniquely.
    Ambiguous,
    /// Two strategies each found one element, but different ones: never guess.
    Conflict,
}

/// Same control: one element is the other or contains it (e.g. a component and its inner input).
fn same_control(a: &[u64], b: &[u64]) -> bool {
    match (a.first(), b.first()) {
        (Some(x), Some(y)) => a.contains(y) || b.contains(x),
        _ => false,
    }
}

/// The ordering and uniqueness rule (pure; the page script only reports [`Hit`]s).
pub fn choose(hits: &[Hit]) -> Choice {
    let unique: Vec<(usize, &[u64])> =
        hits.iter().enumerate().filter(|(_, h)| h.count == 1 && !h.chain.is_empty()).map(|(i, h)| (i, h.chain.as_slice())).collect();
    match unique.first() {
        Some(&(i, _))
            if unique.iter().enumerate().all(|(j, (_, control))| unique[j + 1..].iter().all(|(_, other)| same_control(control, other))) =>
        {
            Choice::Found(i)
        }
        Some(_) => Choice::Conflict,
        None if hits.iter().any(|h| h.count > 1) => Choice::Ambiguous,
        None => Choice::NotFound,
    }
}

/// Parse the page script's answer; anything malformed counts as "nothing found".
pub fn hits_from(v: &Value, strategies: usize) -> Vec<Hit> {
    let hits: Vec<Hit> = v["hits"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|h| Hit {
                    count: h["n"].as_u64().unwrap_or(0),
                    chain: h["chain"].as_array().map(|c| c.iter().filter_map(Value::as_u64).collect()).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    if hits.len() == strategies {
        hits
    } else {
        vec![]
    }
}

/// Text comparison used for labels and option texts: case, runs of whitespace (also non-breaking
/// spaces), a decimal point or comma between digits ("1.5" = "1,5") and trailing `*:.?` are
/// ignored. Mirrors `norm` in [`RESOLVER_JS`] (tested against it).
pub fn norm_text(s: &str) -> String {
    let collapsed = s.replace(['\u{a0}', '\u{202f}'], " ").split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let chars: Vec<char> = collapsed.chars().collect();
    let mut out = String::with_capacity(collapsed.len());
    for (i, &c) in chars.iter().enumerate() {
        let between_digits = i > 0 && i + 1 < chars.len() && chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit();
        out.push(if c == '.' && between_digits { ',' } else { c });
    }
    out.trim_end_matches(|c: char| c.is_whitespace() || "*:.?".contains(c)).to_string()
}

/// Index of the period option whose text is `wanted` (tolerant: [`norm_text`]); `None` when it is
/// missing or appears more than once.
pub fn period_option_index(options: &[String], wanted: &str) -> Option<usize> {
    let w = norm_text(wanted);
    let mut found = options.iter().enumerate().filter(|(_, o)| norm_text(o) == w).map(|(i, _)| i);
    match (found.next(), found.next()) {
        (Some(i), None) => Some(i),
        _ => None,
    }
}

/// Installs `window.__ovr` in the page (idempotent). Reports per strategy how many elements
/// match; `pick` returns the element of one strategy only when it is unique.
pub const RESOLVER_JS: &str = r#"(() => {
  if (window.__ovr && window.__ovr.v === 1) return true;
  const ids = new WeakMap(); let next = 1;
  const tok = (e) => { if (!ids.has(e)) ids.set(e, next++); return ids.get(e); };
  const norm = (s) => String(s == null ? '' : s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim().toLowerCase().replace(/(\d)\.(?=\d)/g, '$1,').replace(/[\s*:.?]+$/, '');
  const tag = (e) => String((e && e.tagName) || '').toLowerCase();
  const attr = (e, k) => (e && e.getAttribute ? e.getAttribute(k) : null);
  const up = (n) => { const p = n && n.parentNode; if (!p) return (n && n.host) || null; return p.nodeType === 11 ? (p.host || null) : p; };
  const within = (a, b) => { for (let n = b; n; n = up(n)) if (n === a) return true; return false; };
  const walk = (root, sel) => { const out = []; const go = (r) => { for (const e of r.querySelectorAll(sel)) out.push(e); for (const e of r.querySelectorAll('*')) if (e.shadowRoot) go(e.shadowRoot); }; if (root.shadowRoot) go(root.shadowRoot); go(root); return out; };
  const text = (n, d) => { d = d || 0; if (!n || d > 60) return ''; if (n.nodeType === 3) return n.textContent || ''; if (n.nodeType !== 1 && n.nodeType !== 9 && n.nodeType !== 11) return ''; const t = tag(n); if (t === 'script' || t === 'style' || t === 'template') return ''; if (t === 'slot' && n.assignedNodes) { const a = n.assignedNodes({ flatten: true }); if (a.length) return a.map((x) => text(x, d + 1)).join(' '); } const kids = n.shadowRoot ? n.shadowRoot.childNodes : n.childNodes; return [...kids].map((x) => text(x, d + 1)).join(' '); };
  const byId = (r, id) => { if (!r || !id) return null; if (r.getElementById) return r.getElementById(id); if (r.querySelectorAll) for (const e of r.querySelectorAll('*')) if (e.id === id) return e; return null; };
  const labels = (e) => { const out = []; try { for (const l of e.labels || []) out.push(text(l)); } catch (_) {} for (let n = e.parentNode; n && n.nodeType === 1; n = n.parentNode) if (tag(n) === 'label') { out.push(text(n)); break; } const r = e.getRootNode ? e.getRootNode() : null; if (e.id && r && r.querySelectorAll) for (const l of r.querySelectorAll('label')) if (attr(l, 'for') === e.id) out.push(text(l)); return out; };
  const named = (e) => { const out = []; const al = attr(e, 'aria-label'); if (al) out.push(al); const lb = attr(e, 'aria-labelledby'); if (lb) { const r = e.getRootNode ? e.getRootNode() : null; out.push(lb.split(/\s+/).map((id) => text(byId(r, id))).join(' ')); } return out; };
  const NOT_TEXT = ['radio', 'checkbox', 'hidden', 'submit', 'button', 'reset', 'image', 'file'];
  const role = (e) => { const r = attr(e, 'role'); if (r) return r.toLowerCase(); const t = tag(e), ty = String(e.type || '').toLowerCase(); if (t === 'input') return ty === 'radio' ? 'radio' : ty === 'checkbox' ? 'checkbox' : ['submit', 'button', 'reset', 'image'].includes(ty) ? 'button' : 'textbox'; if (t === 'button') return 'button'; if (t === 'select') return 'combobox'; if (t === 'textarea') return 'textbox'; if (t === 'a' && attr(e, 'href') !== null) return 'link'; return ''; };
  const kindOk = (e, k) => { const t = tag(e), ty = String(e.type || '').toLowerCase(), r = attr(e, 'role');
    if (k === 'radio') return (t === 'input' && ty === 'radio') || r === 'radio';
    if (k === 'checkbox') return (t === 'input' && ty === 'checkbox') || r === 'checkbox';
    if (k === 'text') return (t === 'input' && !NOT_TEXT.includes(ty)) || t === 'textarea';
    if (k === 'select') return t === 'select';
    if (k === 'button') return t === 'button' || (t === 'input' && (ty === 'submit' || ty === 'button')) || r === 'button' || /-button$/.test(t);
    return e.nodeType === 1; };
  const field = (k) => k !== 'button' && k !== 'any';
  const native = (e) => ['input', 'select', 'textarea'].includes(tag(e));
  const matchText = (have, want, prefix) => { const h = norm(have); return h === want || (prefix && h.startsWith(want + ' ')); };
  const clean = (list) => { const s = [...new Set(list)]; return s.filter((a) => !s.some((b) => b !== a && within(a, b))); };
  const grouped = (e, q) => { for (let n = up(e), i = 0; n && i < 25; n = up(n), i++) { const t = tag(n), r = attr(n, 'role'); if (t === 'fieldset' || r === 'radiogroup' || r === 'group' || /radio-?group|fieldset/.test(t)) return norm(text(n)).includes(q); } return false; };
  // NS's design system renders a button inside a component and keeps a hidden, zero-size native twin in
  // the light DOM for the form: of several matching buttons only the rendered ones count (several rendered = ambiguous).
  const rendered = (e) => { try { const b = e.getBoundingClientRect(); return b.width > 0 && b.height > 0 && (!e.checkVisibility || e.checkVisibility()); } catch (_) { return false; } };
  const strat = (spec, s, root) => { const c = strat0(spec, s, root); if (spec.kind !== 'button' || c.length < 2) return c; const shown = c.filter(rendered); return shown.length ? shown : c; };
  const strat0 = (spec, s, root) => {
    const k = spec.kind;
    if (s.t === 'css') { let c = walk(root, s.sel).filter((e) => kindOk(e, k)); if (s.text) { const w = norm(s.text); c = c.filter((e) => norm(text(e)).includes(w)); } return clean(c); }
    const pool = walk(root, '*').filter((e) => kindOk(e, k));
    if (s.t === 'label') { const w = norm(s.text), q = s.scope ? norm(s.scope) : ''; return clean(pool.filter((e) => { const names = labels(e); if (!field(k) || !native(e)) names.push(text(e)); return names.some((x) => matchText(x, w, field(k))) && (!q || grouped(e, q)); })); }
    if (s.t === 'option') { const w = norm(s.text); return clean(pool.filter((e) => [...(e.options || [])].some((o) => norm(o.text) === w))); }
    if (s.t === 'aria') { const w = norm(s.name); return clean(pool.filter((e) => { const names = [...named(e), ...labels(e)]; if (!field(k) || !native(e)) names.push(text(e)); return role(e) === s.role && names.some((x) => matchText(x, w, field(k))); })); }
    return [];
  };
  const chain = (e) => { const out = []; for (let n = e, i = 0; n && i < 8; n = up(n), i++) if (n.nodeType === 1) out.push(tok(n)); return out; };
  const cands = (spec, root) => ({ hits: root ? spec.strategies.map((s) => { let c = []; try { c = strat(spec, s, root); } catch (_) { c = []; } return { n: c.length, chain: c.length === 1 ? chain(c[0]) : [] }; }) : spec.strategies.map(() => ({ n: 0, chain: [] })) });
  const pick = (spec, i, root) => { if (!root || !spec.strategies[i]) return null; try { const c = strat(spec, spec.strategies[i], root); return c.length === 1 ? c[0] : null; } catch (_) { return null; } };
  const checked = (e) => !!e && (e.checked === true || attr(e, 'aria-checked') === 'true');
  const disabled = (e) => { for (let n = e, i = 0; n && i < 8; n = up(n), i++) if (n.nodeType === 1 && (n.disabled === true || (n.hasAttribute && n.hasAttribute('disabled')) || attr(n, 'aria-disabled') === 'true')) return true; return false; };
  const formOf = (e) => { if (e && e.form) return e.form; for (let n = e; n; n = up(n)) if (tag(n) === 'form') return n; return null; };
  const isSubmit = (b) => !!b && ((tag(b) === 'button' && String(b.type || 'submit').toLowerCase() === 'submit') || (tag(b) === 'input' && String(b.type || '').toLowerCase() === 'submit'));
  const submitOk = (b, decl) => { if (!isSubmit(b) || !decl || disabled(b)) return false; const f = formOf(decl); if (f) return formOf(b) === f; const all = walk(document, 'button[type=submit], input[type=submit]'); return all.length === 1 && all[0] === b; };
  window.__ovr = { v: 1, norm, cands, pick, checked, disabled, submitOk, text };
  return true;
})()"#;

/// `document`, or a scope element given as a JS expression (an exception there means "no scope").
fn root_js(root: Option<&str>) -> String {
    match root {
        Some(r) => format!("(() => {{ try {{ return ({r}) || null; }} catch (_) {{ return null; }} }})()"),
        None => "document".into(),
    }
}

/// JS expression for the element strategy `i` finds (null unless exactly one element matches,
/// or when the resolver is not installed in the page).
pub fn element_expr(spec: &ElSpec, i: usize, root: Option<&str>) -> String {
    format!("(window.__ovr ? window.__ovr.pick({}, {i}, {}) : null)", spec.json(), root_js(root))
}

/// The page script that reports every strategy's matches for `spec` (installs the resolver first).
pub fn candidates_js(spec: &ElSpec, root: Option<&str>) -> String {
    format!("({RESOLVER_JS}, window.__ovr.cands({}, {}))", spec.json(), root_js(root))
}

/// An element found on the page: `expr` is a JS expression evaluating to it.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub key: &'static str,
    /// The fallback strategy that matched; `None` when the primary (observed id/selector) did.
    pub via: Option<&'static str>,
    pub expr: String,
}

/// Make sure the resolver helpers exist in the current page.
pub fn ensure_resolver(b: &NsBridge) -> Result<()> {
    b.eval(RESOLVER_JS)?;
    Ok(())
}

fn reason(choice: Choice) -> &'static str {
    match choice {
        Choice::Found(_) => "found",
        Choice::NotFound => "not found",
        Choice::Ambiguous => "ambiguous (several matches)",
        Choice::Conflict => "conflicting matches (its id and its label point to different fields)",
    }
}

/// Counts per strategy for the log: no page text, no values.
fn counts(spec: &ElSpec, hits: &[Hit]) -> String {
    spec.strategies.iter().zip(hits).map(|(s, h)| format!("{}={}", s.name(), h.count)).collect::<Vec<_>>().join(", ")
}

/// Resolve once (no waiting). Errors name the element and why; a match that is not the primary
/// strategy is logged so NS's page drift becomes visible.
pub fn resolve(b: &NsBridge, spec: &'static ElSpec, root: Option<&str>) -> Result<Resolved> {
    let hits = hits_from(&b.eval(&candidates_js(spec, root))?, spec.strategies.len());
    match choose(&hits) {
        Choice::Found(i) => {
            if i > 0 {
                log::info!(
                    "NS page element '{}' found via fallback '{}' (primary did not match uniquely; {})",
                    spec.key,
                    spec.strategies[i].name(),
                    counts(spec, &hits)
                );
            }
            Ok(Resolved { key: spec.key, via: (i > 0).then(|| spec.strategies[i].name()), expr: element_expr(spec, i, root) })
        }
        other => Err(anyhow!("NS page element '{}': {} ({})", spec.key, reason(other), counts(spec, &hits))),
    }
}

/// Resolve, retrying while the page renders. Ambiguous or conflicting matches are retried too
/// (the page may be mid-update) and fail after `timeout`.
pub fn resolve_wait(b: &NsBridge, spec: &'static ElSpec, root: Option<&str>, timeout: Duration) -> Result<Resolved> {
    let start = Instant::now();
    loop {
        let r = resolve(b, spec, root);
        if r.is_ok() || start.elapsed() >= timeout {
            if let Err(e) = &r {
                log::warn!("{e}");
            }
            return r;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(count: u64, chain: &[u64]) -> Hit {
        Hit { count, chain: chain.to_vec() }
    }

    #[test]
    fn first_unique_strategy_wins() {
        assert_eq!(choose(&[hit(1, &[7, 3]), hit(1, &[7, 3]), hit(1, &[7, 3])]), Choice::Found(0));
        assert_eq!(choose(&[hit(0, &[]), hit(1, &[9]), hit(0, &[])]), Choice::Found(1));
        assert_eq!(choose(&[hit(0, &[]), hit(0, &[]), hit(1, &[9])]), Choice::Found(2));
        // the primary is ambiguous: a unique label match is used
        assert_eq!(choose(&[hit(2, &[]), hit(1, &[9]), hit(0, &[])]), Choice::Found(1));
        // an ambiguous later strategy does not overrule a unique earlier one
        assert_eq!(choose(&[hit(1, &[9]), hit(3, &[]), hit(0, &[])]), Choice::Found(0));
    }

    #[test]
    fn ambiguity_and_absence_fail() {
        assert_eq!(choose(&[hit(2, &[]), hit(2, &[]), hit(0, &[])]), Choice::Ambiguous);
        assert_eq!(choose(&[hit(0, &[]), hit(0, &[]), hit(0, &[])]), Choice::NotFound);
        assert_eq!(choose(&[]), Choice::NotFound);
        // a "unique" hit without identity is not trusted
        assert_eq!(choose(&[hit(1, &[])]), Choice::NotFound);
    }

    #[test]
    fn strategies_pointing_at_different_fields_never_guess() {
        // e.g. NS swapped the ids of "Ja" and "Nee"
        assert_eq!(choose(&[hit(1, &[5, 2, 1]), hit(1, &[6, 2, 1]), hit(0, &[])]), Choice::Conflict);
        assert_eq!(choose(&[hit(1, &[5, 2, 1]), hit(1, &[5, 2, 1]), hit(1, &[6, 2, 1])]), Choice::Conflict);
        // Two sibling controls conflict even when the primary finds their common component.
        assert_eq!(choose(&[hit(1, &[4, 2]), hit(1, &[8, 4, 2]), hit(1, &[9, 4, 2])]), Choice::Conflict);
        // a component and the control inside it are the same field
        assert_eq!(choose(&[hit(1, &[4, 2]), hit(1, &[8, 4, 2]), hit(0, &[])]), Choice::Found(0));
        assert_eq!(choose(&[hit(1, &[8, 4, 2]), hit(1, &[4, 2]), hit(0, &[])]), Choice::Found(0));
    }

    #[test]
    fn malformed_page_answers_count_as_nothing_found() {
        let ok = json!({"hits": [{"n": 1, "chain": [3, 1]}, {"n": 0, "chain": []}]});
        assert_eq!(hits_from(&ok, 2), vec![hit(1, &[3, 1]), hit(0, &[])]);
        assert!(hits_from(&ok, 3).is_empty(), "wrong number of strategies");
        assert!(hits_from(&Value::Null, 2).is_empty());
        assert_eq!(choose(&hits_from(&json!({"hits": "x"}), 1)), Choice::NotFound);
    }

    #[test]
    fn period_options_are_matched_tolerantly_and_uniquely() {
        let opts: Vec<String> =
            ["Laatste 7 dagen", "Afgelopen maand", "Afgelopen  Jaar", "Afgelopen 1.5 jaar"].iter().map(|s| s.to_string()).collect();
        assert_eq!(period_option_index(&opts, PERIOD_YEAR), Some(2));
        assert_eq!(period_option_index(&opts, PERIOD_MAX), Some(3));
        assert_eq!(period_option_index(&opts, "afgelopen\u{a0}1,5 JAAR "), Some(3));
        assert_eq!(period_option_index(&opts, "Afgelopen 2 jaar"), None);
        let twice: Vec<String> = vec!["Afgelopen jaar".into(), "afgelopen jaar".into()];
        assert_eq!(period_option_index(&twice, PERIOD_YEAR), None, "a duplicate option is never guessed");
        // "Afgelopen jaar" is not a prefix match of "Afgelopen jaren"
        assert_eq!(period_option_index(&["Afgelopen jaren".to_string()], PERIOD_YEAR), None);
    }

    #[test]
    fn text_normalisation() {
        assert_eq!(norm_text("  Ik heb alles naar waarheid ingevuld. "), "ik heb alles naar waarheid ingevuld");
        assert_eq!(norm_text("Naam rekeninghouder *"), "naam rekeninghouder");
        assert_eq!(norm_text("Heb je je gewenste NS-eindstation bereikt?"), "heb je je gewenste ns-eindstation bereikt");
        assert_eq!(norm_text("Afgelopen 1.5 jaar"), norm_text("Afgelopen 1,5 jaar"));
        assert_eq!(norm_text("30 t/m 59 min"), "30 t/m 59 min");
        assert_eq!(norm_text("Station 1. Utrecht"), "station 1. utrecht", "a point not between digits stays");
    }

    #[test]
    fn every_element_starts_with_its_observed_selector() {
        let all: [&ElSpec; 14] = [
            &ARRIVAL_YES,
            &ARRIVAL_NO,
            &DEPARTURE,
            &DELAY_30,
            &DELAY_60,
            &CHECK_IN,
            &CHECK_OUT,
            &NAME,
            &IBAN,
            &DECLARATION,
            &SUBMIT,
            &PERIOD_FILTER,
            &SHOW_BUTTON,
            &CLAIM_LINK,
        ];
        let mut keys = std::collections::HashSet::new();
        for s in all {
            assert!(matches!(s.strategies.first(), Some(Strategy::Css { .. })), "{}", s.key);
            assert!(keys.insert(s.key), "duplicate key {}", s.key);
            let v: Value = serde_json::from_str(&s.json()).unwrap();
            assert_eq!(v["strategies"].as_array().unwrap().len(), s.strategies.len());
            // the spec handed to the page carries static texts only
            assert!(!s.json().contains('@'));
        }
        // the two answers of one question are told apart by their labels
        for (a, b) in [(&ARRIVAL_YES, &ARRIVAL_NO), (&DELAY_30, &DELAY_60)] {
            assert_ne!(a.strategies[1], b.strategies[1]);
            assert_ne!(
                norm_text(match a.strategies[1] {
                    Strategy::Label { text, .. } => text,
                    _ => unreachable!(),
                }),
                ""
            );
        }
        assert_eq!(FORM_FIELDS.len(), 10);
    }

    #[test]
    fn resolver_script_walks_shadow_roots_and_requires_uniqueness() {
        assert!(RESOLVER_JS.contains("shadowRoot") && RESOLVER_JS.contains("c.length === 1 ? c[0] : null"));
        // never reads or returns field values
        assert!(!RESOLVER_JS.contains(".value"));
        let e = element_expr(&IBAN, 1, None);
        assert!(
            e.starts_with("(window.__ovr ? window.__ovr.pick(")
                && e.contains("Bankrekeningnummer (IBAN)")
                && e.ends_with(", 1, document) : null)")
        );
        assert!(element_expr(&CLAIM_LINK, 0, Some("rows[3]")).contains("try { return (rows[3]) || null; }"));
    }

    /// Runs the real page script against a small fake DOM (with shadow roots) in Node.js when it
    /// is installed (CI has it); skipped otherwise. No network, no files.
    #[test]
    fn resolver_script_in_a_fake_dom() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let specs = json!({
            "arrival_yes": serde_json::from_str::<Value>(&ARRIVAL_YES.json()).unwrap(),
            "arrival_no": serde_json::from_str::<Value>(&ARRIVAL_NO.json()).unwrap(),
            "delay_30": serde_json::from_str::<Value>(&DELAY_30.json()).unwrap(),
            "delay_60": serde_json::from_str::<Value>(&DELAY_60.json()).unwrap(),
            "departure": serde_json::from_str::<Value>(&DEPARTURE.json()).unwrap(),
            "iban": serde_json::from_str::<Value>(&IBAN.json()).unwrap(),
            "name": serde_json::from_str::<Value>(&NAME.json()).unwrap(),
            "declaration": serde_json::from_str::<Value>(&DECLARATION.json()).unwrap(),
            "submit": serde_json::from_str::<Value>(&SUBMIT.json()).unwrap(),
            "check_in": serde_json::from_str::<Value>(&CHECK_IN.json()).unwrap(),
            "period": serde_json::from_str::<Value>(&PERIOD_FILTER.json()).unwrap(),
            "show": serde_json::from_str::<Value>(&SHOW_BUTTON.json()).unwrap(),
            "claim_link": serde_json::from_str::<Value>(&CLAIM_LINK.json()).unwrap(),
        });
        let program = format!(
            "{}\nconst SPECS = {specs};\nconst RESOLVER = {};\n{}",
            include_str!("ns_elements_fake_dom.js"),
            serde_json::to_string(RESOLVER_JS).unwrap(),
            "runScenarios();"
        );
        let child = Command::new("node").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let Ok(mut child) = child else {
            eprintln!("node not found: fake-DOM resolver test skipped");
            return;
        };
        child.stdin.take().unwrap().write_all(program.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "node failed: {}\n{stdout}", String::from_utf8_lossy(&out.stderr));
        let v: Value = serde_json::from_str(stdout.trim()).expect("scenario JSON");
        let choice = |scenario: &str, key: &str| {
            let n = v[scenario][key]["strategies"].as_u64().unwrap() as usize;
            choose(&hits_from(&v[scenario][key], n))
        };
        // Today's page: every element by its observed id.
        for key in
            ["arrival_yes", "arrival_no", "delay_30", "delay_60", "departure", "iban", "name", "declaration", "check_in", "period", "show"]
        {
            assert_eq!(choice("current", key), Choice::Found(0), "current/{key}");
        }
        // The header search button makes `button[type=submit]` ambiguous: "Verzenden" decides.
        assert_eq!(choice("current", "submit"), Choice::Found(1));
        assert_eq!(v["current"]["submit_ok"], true, "the form's own submit button");
        assert_eq!(v["current"]["header_submit_ok"], false, "a submit button of another form");
        assert_eq!(v["current"]["checked_after_click"], true);
        assert_eq!(v["current"]["claim_link_row1"], "link-1", "scoped to the opened row");
        // The form's button lost its type attribute: the selector now uniquely finds the header's
        // search button while "Verzenden" finds the form's button. Never guessed.
        assert_eq!(choice("untyped", "submit"), Choice::Conflict);
        // NS renamed every id: labels and question texts find the same fields.
        for key in
            ["arrival_yes", "arrival_no", "delay_30", "delay_60", "departure", "iban", "name", "declaration", "period", "show", "submit"]
        {
            assert_eq!(choice("renamed", key), Choice::Found(1), "renamed/{key}");
        }
        assert_eq!(v["renamed"]["picked_arrival_no"], "ans-no");
        assert_eq!(v["renamed"]["picked_delay_60"], "late-60");
        assert_eq!(v["renamed"]["submit_ok"], true, "a button without a type attribute submits its form");
        assert_eq!(choice("renamed", "check_in"), Choice::NotFound, "no fallback for prefilled fields");
        assert_eq!(v["renamed"]["claim_link_row1"], "row1-link", "link found by its text");
        // Only ARIA names left.
        for key in ["iban", "name", "declaration"] {
            assert_eq!(choice("aria", key), Choice::Found(2), "aria/{key}");
        }
        // Ids swapped between "Ja" and "Nee": never guessed.
        assert_eq!(choice("swapped", "arrival_yes"), Choice::Conflict);
        assert_eq!(choice("swapped", "arrival_no"), Choice::Conflict);
        // Two radios with the same answer text, ids gone: ambiguous.
        assert_eq!(choice("duplicate", "delay_30"), Choice::Ambiguous);
        // A hidden zero-size native twin of Verzenden (as on NS's page, 2026-11-20): only the rendered button counts.
        assert_eq!(choice("twin", "submit"), Choice::Found(0));
        assert_eq!(v["twin"]["picked_in_shadow"], true, "the component's rendered button, not the hidden twin");
        assert_eq!(v["twin"]["submit_ok"], true);
        // Two rendered Verzenden buttons: never guessed.
        assert_eq!(choice("twin_rendered", "submit"), Choice::Ambiguous);
        // An answer outside its question's group does not count.
        assert_eq!(choice("decoy", "arrival_yes"), Choice::Found(1));
        // Text normalisation is the same in the page and in Rust.
        for (input, js) in v["norm"].as_object().unwrap() {
            assert_eq!(js.as_str().unwrap(), norm_text(input), "{input}");
        }
    }
}
