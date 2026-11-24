//! Bridge to the official Mijn NS website running in an app-owned, isolated
//! WebView2 (system runtime, no bundled browser).
//!
//! * The window is hidden for scans/submissions and shown only for login.
//! * We use WebView2's DevTools-protocol API (`CallDevToolsProtocolMethod`)
//!   to evaluate script, dispatch real mouse input and *passively* read JSON
//!   responses the official page itself requested. We never extract or replay
//!   NS tokens/subscription keys and never call NS endpoints ourselves.
//! * The NS window has no Tauri capability, so ns.nl JavaScript cannot invoke
//!   any Rust command.
use anyhow::{anyhow, bail, Context, Result};
use crossbeam_channel::bounded;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const NS_LABEL: &str = "ns";

/// Browser arguments for every OV Refund webview (identical for all windows sharing the profile):
/// keep wry's defaults and stop WebView2 from downloading optional components (DRM module,
/// component updates) that the app never uses, which otherwise add ~50 MB to the profile.
pub const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI --disable-component-update --disk-cache-size=8388608 --media-cache-size=1048576 --disable-gpu-shader-disk-cache";

/// Analytics / marketing / chat scripts on Mijn NS that OV Refund never needs. Blocking them keeps
/// the profile small, each check lighter, and avoids sending the session's behaviour to trackers.
/// NS's own app, login and bot protection (Akamai `/akam/`) and cookie consent are NOT blocked.
pub const BLOCKED_URLS: &[&str] = &[
    "*://gcrd.ns.nl/*",
    "*://spdc.ns.nl/*",
    "*googletagmanager.com/*",
    "*google-analytics.com/*",
    "*doubleclick.net/*",
    "*qualtrics.com/*",
    "*/apps/cognigy-chatbot/*",
    "*cognigy.ai/*",
    "*api.avo.app/*",
];
pub const MIJNNS_URL: &str = "https://www.ns.nl/mijnns#/reishistorie";

/// Response URLs whose bodies we keep (all official Mijn NS gateway reads/writes).
/// Diagnostic mode: capture every Mijn NS gateway response (used by `ov-refund probe`).
pub static CAPTURE_ALL_GATEWAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Extra API diagnostics are metadata-only and scoped to the claims-list wait.
static CLAIMS_DIAGNOSTICS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct ClaimsDiagnostics(bool);

impl ClaimsDiagnostics {
    pub fn start() -> Self {
        Self(CLAIMS_DIAGNOSTICS.swap(true, std::sync::atomic::Ordering::Relaxed))
    }
}

impl Drop for ClaimsDiagnostics {
    fn drop(&mut self) {
        CLAIMS_DIAGNOSTICS.store(self.0, std::sync::atomic::Ordering::Relaxed);
    }
}

fn diagnostic_api(url: &str, resource_type: &str) -> bool {
    matches!(resource_type, "Fetch" | "XHR")
        && tauri::Url::parse(url).ok().is_some_and(|u| {
            u.scheme() == "https"
                && u.host_str().is_some_and(|h| h.ends_with(".ns.nl"))
                && !u.path().contains("authoriz")
                && !u.path().contains("token")
        })
}

fn interesting(url: &str) -> bool {
    if !url.starts_with("https://gateway.apiportal.ns.nl/") {
        return false;
    }
    if CAPTURE_ALL_GATEWAY.load(std::sync::atomic::Ordering::Relaxed) {
        return !url.contains("authoriz") && !url.contains("token");
    }
    url.contains("/mijnns-transaction-api/")
        || url.contains("/mijnns-claim-api/")
        || url.contains("/mijnns-card-coupling-api/")
        || url.contains("/mijnns-invoice")
        || url.contains("/mijnns-payment")
}

#[derive(Debug, Clone)]
pub struct Captured {
    pub url: String,
    pub method: String,
    pub status: i64,
    pub body: Option<String>,
    pub seq: u64,
}

#[derive(Default)]
struct NetState {
    pending: HashMap<String, (String, String, i64, u64)>, // requestId -> (url, method, status, request seq)
    methods: HashMap<String, (String, u64)>,              // requestId -> (method, seq assigned when the request STARTED)
    captured: Vec<Captured>,
    seq: u64,
    claim_writes: Vec<u64>,
}

/// No mouse press was dispatched; a retryable result still needs the network-write guard.
#[derive(Debug)]
pub struct ClickNotDispatched;
impl std::fmt::Display for ClickNotDispatched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Het klikdoel is verplaatst, bedekt of ontbreekt. / The click target moved, is covered or is missing.")
    }
}
impl std::error::Error for ClickNotDispatched {}

#[derive(Debug)]
pub struct ClickRefused(pub anyhow::Error);

impl std::fmt::Display for ClickRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for ClickRefused {}

#[derive(Clone)]
pub struct NsBridge {
    pub window: WebviewWindow,
    net: Arc<Mutex<NetState>>,
}

// The NS window shares the app's default WebView2 profile (%LOCALAPPDATA%\nl.ovrefund.app\EBWebView).
// The local UI (tauri.localhost) and ns.nl are isolated by origin; only "main" has IPC capabilities.

impl NsBridge {
    /// Create (or reuse) the hidden NS window. Must be called from a non-main thread
    /// once the Tauri event loop runs, or from setup.
    pub fn open(app: &AppHandle, visible: bool) -> Result<Self> {
        let window = match app.get_webview_window(NS_LABEL) {
            Some(w) => w,
            None => WebviewWindowBuilder::new(app, NS_LABEL, WebviewUrl::External("about:blank".parse()?))
                .title(login_title())
                // The login window shows no address bar: only NS's own sites may load in it, so a
                // password can never be typed into a look-alike page.
                .on_navigation(|url| {
                    let ok = url.scheme() == "about" || (url.scheme() == "https" && url.host_str().map(is_ns_host).unwrap_or(false));
                    if !ok {
                        log::warn!("blocked navigation in NS window to {}", url.host_str().unwrap_or("?"));
                    }
                    ok
                })
                .inner_size(1000.0, 800.0)
                .visible(visible)
                .additional_browser_args(BROWSER_ARGS)
                .build()
                .context("create NS webview")?,
        };
        if visible {
            // The app's language may have changed since the window was made.
            let _ = window.set_title(login_title());
            let _ = window.show();
            let _ = window.set_focus();
        }
        static NET: std::sync::OnceLock<Arc<Mutex<NetState>>> = std::sync::OnceLock::new();
        static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let b = Self { window, net: NET.get_or_init(|| Arc::new(Mutex::new(NetState::default()))).clone() };
        if !INSTALLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            if let Err(e) = b.install_listeners() {
                INSTALLED.store(false, std::sync::atomic::Ordering::SeqCst);
                return Err(e);
            }
        }
        b.cdp("Network.enable", json!({}), Duration::from_secs(20))?;
        b.cdp("Network.setBlockedURLs", json!({ "urls": BLOCKED_URLS }), Duration::from_secs(10))?;
        Ok(b)
    }

    pub fn show(&self) {
        let _ = self.window.show();
        let _ = self.window.set_focus();
    }
    pub fn hide(&self) {
        let _ = self.window.hide();
    }

    /// Call a DevTools-protocol method on the NS webview and wait for the JSON result.
    pub fn cdp(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let (tx, rx) = bounded::<Result<String>>(1);
        let m = method.to_string();
        let p = params.to_string();
        self.window
            .with_webview(move |pw| {
                #[cfg(windows)]
                unsafe {
                    use webview2_com::{CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR};
                    let tx2 = tx.clone();
                    let r = (|| -> windows::core::Result<()> {
                        let core = pw.controller().CoreWebView2()?;
                        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |hr, json| {
                            let _ = tx2.send(hr.map(|_| json).map_err(|e| anyhow!("CDP error: {e}")));
                            Ok(())
                        }));
                        let mw = CoTaskMemPWSTR::from(m.as_str());
                        let pw_ = CoTaskMemPWSTR::from(p.as_str());
                        core.CallDevToolsProtocolMethod(*mw.as_ref().as_pcwstr(), *pw_.as_ref().as_pcwstr(), &handler)
                    })();
                    if let Err(e) = r {
                        let _ = tx.send(Err(anyhow!("CDP call failed: {e}")));
                    }
                }
            })
            .map_err(|e| anyhow!("with_webview: {e}"))?;
        let s = rx.recv_timeout(timeout).map_err(|_| anyhow!("CDP {method} timed out"))??;
        Ok(serde_json::from_str(&s).unwrap_or(Value::Null))
    }

    fn install_listeners(&self) -> Result<()> {
        let net = self.net.clone();
        let (tx, rx) = bounded::<Result<()>>(1);
        self.window
            .with_webview(move |pw| {
                #[cfg(windows)]
                unsafe {
                    let r = install_listeners_on(pw, net);
                    let _ = tx.send(r.map_err(|e| anyhow!("listener install: {e}")));
                }
            })
            .map_err(|e| anyhow!("with_webview: {e}"))?;
        rx.recv_timeout(Duration::from_secs(20)).map_err(|_| anyhow!("listener install timed out"))?
    }

    /// Evaluate JS in the page; returns the JSON value. Never use for secrets.
    pub fn eval(&self, expr: &str) -> Result<Value> {
        let r = self.cdp(
            "Runtime.evaluate",
            json!({"expression": expr, "awaitPromise": true, "returnByValue": true, "userGesture": true}),
            Duration::from_secs(60),
        )?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("page script error: {}", ex.get("text").and_then(|t| t.as_str()).unwrap_or("?"));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Real (trusted) mouse click at the centre of the element returned by `element_expr`.
    pub fn click(&self, element_expr: &str) -> Result<()> {
        self.click_after(element_expr, &|| Ok(()), &|| Ok(()))
    }

    pub fn click_after(
        &self, element_expr: &str, after_hover: &dyn Fn() -> Result<()>, before_press: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let pos = self.eval(&format!(
            "(() => {{ const e = {element_expr}; if (!e) return null; e.scrollIntoView({{block:'center'}}); const b = e.getBoundingClientRect(); let hit = document.elementFromPoint(b.x+b.width/2,b.y+b.height/2); while (hit?.shadowRoot) {{ const inner = hit.shadowRoot.elementFromPoint(b.x+b.width/2,b.y+b.height/2); if (!inner || inner === hit) break; hit = inner; }} if (!hit || !(() => {{ for (let n = hit; n; n = n.assignedSlot || n.parentNode || n.host) if (n === e) return true; return false; }})()) return null; return {{x: b.x + b.width/2, y: b.y + b.height/2, w: b.width}} }})()"
        )).map_err(|error| error.context(ClickNotDispatched))?;
        let (x, y) = match (pos.get("x").and_then(Value::as_f64), pos.get("y").and_then(Value::as_f64)) {
            (Some(x), Some(y)) if pos.get("w").and_then(Value::as_f64).unwrap_or(0.0) > 0.0 => (x, y),
            _ => bail!(ClickNotDispatched),
        };
        self.cdp("Input.dispatchMouseEvent", json!({"type":"mouseMoved", "x":x, "y":y}), Duration::from_secs(10))
            .map_err(|error| error.context(ClickNotDispatched))?;
        after_hover().map_err(ClickRefused)?;
        // Re-measure after scrolling, hovering and the page reads. Only local checks follow.
        let hit = self.eval(&format!("(() => {{ const e = {element_expr}; if (!e) return false; const b = e.getBoundingClientRect(); let hit = document.elementFromPoint({x},{y}); while (hit?.shadowRoot) {{ const inner = hit.shadowRoot.elementFromPoint({x},{y}); if (!inner || inner === hit) break; hit = inner; }} return Math.abs(b.x+b.width/2-{x}) < 1 && Math.abs(b.y+b.height/2-{y}) < 1 && !!hit && (() => {{ for (let n = hit; n; n = n.assignedSlot || n.parentNode || n.host) if (n === e) return true; return false; }})(); }})()")).map_err(|error| error.context(ClickNotDispatched))?;
        anyhow::ensure!(hit == true, ClickNotDispatched);
        before_press().map_err(ClickRefused)?;
        for t in ["mousePressed", "mouseReleased"] {
            self.cdp(
                "Input.dispatchMouseEvent",
                json!({"type": t, "x": x, "y": y, "button": "left", "clickCount": 1}),
                Duration::from_secs(10),
            )?;
        }
        Ok(())
    }

    pub fn navigate(&self, url: &str) -> Result<()> {
        self.cdp("Page.navigate", json!({ "url": url }), Duration::from_secs(30))?;
        Ok(())
    }

    pub fn reload(&self) -> Result<()> {
        self.cdp("Page.reload", json!({ "ignoreCache": true }), Duration::from_secs(30))?;
        Ok(())
    }

    pub fn wait_until(&self, what: &str, timeout: Duration, mut f: impl FnMut(&Self) -> Result<bool>) -> Result<()> {
        let start = Instant::now();
        loop {
            if f(self).unwrap_or(false) {
                return Ok(());
            }
            if start.elapsed() > timeout {
                bail!("timed out waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    pub fn mark(&self) -> u64 {
        self.net.lock().unwrap().seq
    }

    pub fn claim_write_since(&self, seq: u64) -> bool {
        self.claim_write_count_since(seq) > 0
    }

    pub fn claim_write_count_since(&self, seq: u64) -> usize {
        self.net.lock().unwrap().claim_writes.iter().filter(|s| **s > seq).count()
    }

    pub fn captured_since(&self, seq: u64) -> Vec<Captured> {
        self.net.lock().unwrap().captured.iter().filter(|c| c.seq > seq).cloned().collect()
    }

    pub fn page_state(&self) -> Result<PageState> {
        let v = self.eval(
            "({host: location.host, path: location.pathname, hash: location.hash, \
              loggedIn: /Uitloggen|Log out|Logout/.test(document.body ? document.body.innerText : ''), \
              loginPrompt: /Inloggen|Log in|Login/i.test(document.body ? document.body.innerText : ''), \
              textLen: document.body ? document.body.innerText.trim().length : 0, \
              challenge: !!document.querySelector('iframe[src*=captcha], #challenge-form, [id*=captcha]') || /Access Denied|Pardon Our Interruption|Request unsuccessful/i.test(document.title + ' ' + (document.body ? document.body.innerText.slice(0,400) : ''))})",
        )?;
        Ok(PageState {
            host: v["host"].as_str().unwrap_or("").to_string(),
            logged_in: v["loggedIn"].as_bool().unwrap_or(false),
            challenge: v["challenge"].as_bool().unwrap_or(false),
            login_prompt: v["loginPrompt"].as_bool().unwrap_or(false),
            text_len: v["textLen"].as_u64().unwrap_or(0),
        })
    }

    /// Mijn NS answered one of its own account APIs (travel history or card couplings) with 200
    /// since `mark`: logged in, whatever language or labels the page uses.
    pub fn api_logged_in_since(&self, mark: u64) -> bool {
        self.captured_since(mark)
            .iter()
            .any(|c| c.status == 200 && (c.url.contains("/mijnns-transaction-api/") || c.url.contains("/mijnns-card-coupling-api/")))
    }

    /// Logged in on www.ns.nl: the "Uitloggen" label, or an account API answered (language-independent).
    pub fn logged_in_now(&self, mark: u64) -> bool {
        self.page_state().map(|s| s.host == "www.ns.nl" && (s.logged_in || self.api_logged_in_since(mark))).unwrap_or(false)
    }

    /// NS's cookie dialog: choose the "necessary only" / reject option when there is one. Never
    /// clicks "accept all"; does nothing when the dialog offers no such option. Best effort.
    /// Returns true when it clicked.
    pub fn dismiss_cookie_banner(&self) -> bool {
        self.eval(COOKIE_JS).ok().and_then(|v| v.as_str().map(|s| s == "rejected")).unwrap_or(false)
    }

    /// Open Mijn NS and determine whether the stored session is still valid.
    pub fn ensure_session(&self, timeout: Duration) -> Result<SessionStatus> {
        // The caller holds the ledger lock. Merely constructing a bridge before a run acquires
        // its lock must never restore cookies during a concurrent logout or erase.
        static RESTORED: std::sync::Once = std::sync::Once::new();
        RESTORED.call_once(|| match crate::session_store::restore(self) {
            Ok(Some(_)) => log::info!("restored saved NS session"),
            Ok(None) => log::info!("no saved NS session"),
            Err(e) => log::warn!("could not restore NS session: {e:#}"),
        });
        let mark = self.mark();
        self.navigate(MIJNNS_URL)?;
        let start = Instant::now();
        let mut last: Option<PageState> = None;
        let mut banner_done = false;
        while start.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(700));
            if !banner_done && start.elapsed() < Duration::from_secs(15) {
                banner_done = self.dismiss_cookie_banner();
            }
            let Ok(s) = self.page_state() else { continue };
            if s.challenge {
                return Ok(SessionStatus::Blocked("bot/access challenge shown by NS".into()));
            }
            if s.host == "www.ns.nl" && (s.logged_in || self.api_logged_in_since(mark)) {
                return Ok(SessionStatus::LoggedIn);
            }
            if is_ns_login_host(&s.host) {
                // give the SSO redirect chain a moment, then try the saved (autofilled) login once
                if start.elapsed() > Duration::from_secs(6) {
                    return self.try_auto_login();
                }
            }
            last = Some(s);
        }
        Ok(session_after_timeout(last.as_ref()))
    }
}

/// No login page, no "Uitloggen" and no account API after the whole wait: a Mijn NS page with
/// content but without any login prompt means OV Refund no longer recognises the page (labels
/// changed): blocked, not "log in again" (logging in would not help).
pub fn session_after_timeout(last: Option<&PageState>) -> SessionStatus {
    match last {
        Some(s) if s.host == "www.ns.nl" && !s.logged_in && !s.login_prompt && s.text_len > 300 => {
            SessionStatus::Blocked(ovrefund_core::pipeline::BLOCKED_SITE_CHANGED.into())
        }
        _ => SessionStatus::LoginRequired,
    }
}

/// Title of the NS login window in the app's language.
pub fn login_title() -> &'static str {
    let en = crate::runner::open_db().map(|db| crate::runner::setting(&db, "ui_lang", "nl") == "en").unwrap_or(false);
    if en {
        "Log in to Mijn NS"
    } else {
        "Inloggen bij Mijn NS"
    }
}

/// Finds NS's cookie/consent dialog (also inside shadow roots) and clicks its "necessary only" /
/// reject button. Only a real dialog counts: `role=dialog` / `aria-modal=true` that is about
/// cookies, or a known consent-manager container (OneTrust, Cookiebot, Usercentrics, ...); never
/// `html`/`body` or a page section that merely mentions cookies. Never clicks "accept all".
/// Returns "none" (no dialog), "no-reject" (only accept options: left alone) or "rejected".
const COOKIE_JS: &str = r#"(() => {
  const all = [];
  const walk = (r) => { for (const el of r.querySelectorAll('*')) { all.push(el); if (el.shadowRoot) walk(el.shadowRoot); } };
  walk(document);
  const vis = (el) => !!(el.offsetWidth || el.offsetHeight || (el.getClientRects && el.getClientRects().length));
  const label = (el) => [el.id || '', typeof el.className === 'string' ? el.className : '', el.getAttribute && (el.getAttribute('aria-label') || ''), el.getAttribute && (el.getAttribute('aria-labelledby') || '')].join(' ');
  const known = '#onetrust-banner-sdk, #onetrust-consent-sdk, #CybotCookiebotDialog, #usercentrics-root, #cookie-consent, .cookie-consent, .cookie-consent-banner, .cc-window, [data-testid*=cookie-consent], [id*=cookie-consent], [class*=cookie-consent]';
  const dialog = (el) => el.getAttribute && (el.getAttribute('role') === 'dialog' || el.getAttribute('role') === 'alertdialog' || el.getAttribute('aria-modal') === 'true');
  const aboutCookies = (el) => /cookie|consent/i.test(label(el)) || /cookie/i.test(((el.innerText || el.textContent || '') + '').slice(0, 2000));
  const banners = all.filter((el) => el.tagName !== 'HTML' && el.tagName !== 'BODY' && vis(el) && ((el.matches && el.matches(known)) || (dialog(el) && aboutCookies(el))));
  if (!banners.length) return 'none';
  const inside = [];
  const collect = (r) => { for (const el of r.querySelectorAll('*')) { inside.push(el); if (el.shadowRoot) collect(el.shadowRoot); } };
  for (const b of banners) { inside.push(b); collect(b); if (b.shadowRoot) collect(b.shadowRoot); }
  const txt = (el) => (el.innerText || el.textContent || '').trim().toLowerCase().replace(/\s+/g, ' ');
  const isButton = (el) => ['BUTTON', 'A', 'NES-BUTTON'].includes(el.tagName) || (el.getAttribute && el.getAttribute('role') === 'button');
  const acceptAll = /alle cookies|accepteer alle|alles accepteren|accept all|allow all|akkoord/;
  const reject = /alleen (functionele|noodzakelijke|nodige|essenti)|noodzakelijke cookies|functionele cookies|weiger|niet accepteren|only (necessary|essential|functional)|necessary only|reject|decline/;
  const btn = inside.find((el) => isButton(el) && vis(el) && reject.test(txt(el)) && !acceptAll.test(txt(el)) && txt(el).length < 80);
  if (!btn) return 'no-reject';
  btn.click();
  return 'rejected';
})()"#;

/// NS's own login hosts, matched exactly (never a prefix: `login.ns.nl.example` must not match).
pub const NS_LOGIN_HOSTS: [&str; 2] = ["login.ns.nl", "loginapi.ns.nl"];

pub fn is_ns_login_host(host: &str) -> bool {
    NS_LOGIN_HOSTS.contains(&host)
}

/// NS's own sites (ns.nl and its subdomains), exact suffix match.
pub fn is_ns_host(host: &str) -> bool {
    host == "ns.nl" || host.ends_with(".ns.nl")
}

static AUTO_LOGIN_TRIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Allow one automatic login attempt for the next run (called at the start of each run).
pub fn reset_auto_login_attempt() {
    AUTO_LOGIN_TRIED.store(false, std::sync::atomic::Ordering::SeqCst);
}

impl NsBridge {
    /// One attempt per process: type the NS login saved in Windows Credential Manager into NS's
    /// own login form (trusted input) and press "Inloggen". Never retries; stops on any challenge.
    fn try_auto_login(&self) -> Result<SessionStatus> {
        let r = self.try_auto_login_inner();
        if let Ok(st) = &r {
            record_auto_login_outcome(st);
        }
        r
    }

    fn try_auto_login_inner(&self) -> Result<SessionStatus> {
        let enabled = crate::runner::open_db().map(|db| crate::runner::setting(&db, "auto_login", "1") == "1").unwrap_or(false);
        if !enabled {
            retry_erase_disabled_login();
        }
        let Some(cred) = crate::secrets::ns_login().filter(|_| enabled) else {
            return Ok(SessionStatus::LoginRequired);
        };
        if AUTO_LOGIN_TRIED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Ok(SessionStatus::LoginRequired);
        }
        self.wait_until("login form", Duration::from_secs(15), |b| {
            Ok(b.eval("!!document.querySelector('input[name=email]') && !!document.querySelector('input[name=password]')")?
                .as_bool()
                .unwrap_or(false))
        })?;
        for (sel, val) in [("input[name=email]", cred.email.as_str()), ("input[name=password]", cred.password.as_str())] {
            // Re-check right before typing: only ever into NS's own login page over HTTPS.
            let url = self.window.url()?;
            if url.scheme() != "https" || !url.host_str().map(is_ns_login_host).unwrap_or(false) {
                anyhow::bail!("automatic login stopped: unexpected page {}", url.host_str().unwrap_or("?"));
            }
            // clear (the page may remember the e-mail), focus with a real click, then type
            self.eval(&format!(
                "(() => {{ const i = document.querySelector('{sel}'); const s = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; s.call(i, ''); i.dispatchEvent(new Event('input', {{bubbles: true}})); return true }})()"
            ))?;
            self.click(&format!("document.querySelector('{sel}')"))?;
            // Type only if the focused element is exactly that field in NS's own top-level page.
            let focused_ok = self
                .eval(&format!("window.top === window && location.protocol === 'https:' && ['login.ns.nl','loginapi.ns.nl'].includes(location.hostname) && document.activeElement === document.querySelector('{sel}')"))?
                .as_bool()
                .unwrap_or(false);
            if !focused_ok {
                anyhow::bail!("automatic login stopped: the login field did not have focus");
            }
            self.cdp("Input.insertText", json!({ "text": val }), Duration::from_secs(10))?;
        }
        std::thread::sleep(Duration::from_millis(300));
        // Messages already on the form before the click (e.g. a cookie or info notice) are never
        // taken as NS refusing the login.
        let before = login_error_signature(&self.eval(LOGIN_PAGE_JS).unwrap_or(Value::Null));
        let mark = self.mark();
        self.click("document.querySelector('form input[type=submit], form button[type=submit]')")?;
        let start = Instant::now();
        let mut refusal = RefusalWatch::default();
        let mut extra_fields = false;
        while start.elapsed() < Duration::from_secs(45) {
            std::thread::sleep(Duration::from_millis(700));
            let Ok(s) = self.page_state() else { continue };
            if s.challenge {
                return Ok(SessionStatus::Blocked("NS showed a security check during automatic login".into()));
            }
            if s.host == "www.ns.nl" && (s.logged_in || self.api_logged_in_since(mark)) {
                log::info!("auto-login: success");
                return Ok(SessionStatus::LoggedIn);
            }
            if is_ns_login_host(&s.host) {
                let probe = self.eval(LOGIN_PAGE_JS).unwrap_or(Value::Null);
                match login_page_verdict(&probe) {
                    LoginPage::VerificationCode => {
                        log::warn!("auto-login: NS asks for a verification code / extra check");
                        return Ok(SessionStatus::Blocked(ovrefund_core::pipeline::BLOCKED_VERIFICATION.into()));
                    }
                    // A new error on the form that is still there (unchanged) 3 s later, after
                    // the page settled: NS refused the login (not a transient message).
                    LoginPage::Refused => {
                        let sig = login_error_signature(&probe);
                        if refusal.observe(&before, &sig, start.elapsed()) {
                            log::warn!("auto-login: NS refused the stored login");
                            return Ok(SessionStatus::AutoLoginFailed);
                        }
                    }
                    LoginPage::OtherFields => extra_fields = true,
                    LoginPage::Busy => refusal = RefusalWatch::default(),
                }
            }
        }
        if extra_fields {
            // Still on NS's login site with fields OV Refund does not know: an extra step.
            return Ok(SessionStatus::Blocked(ovrefund_core::pipeline::BLOCKED_VERIFICATION.into()));
        }
        log::warn!("auto-login: did not complete");
        Ok(SessionStatus::LoginRequired)
    }

    /// Clear all profile browsing data, including history, cache, cookies and saved logins.
    pub fn clear_saved_logins(&self) -> Result<()> {
        let (tx, rx) = bounded::<Result<()>>(1);
        self.window
            .with_webview(move |pw| {
                #[cfg(windows)]
                unsafe {
                    use webview2_com::Microsoft::Web::WebView2::Win32::*;
                    let tx2 = tx.clone();
                    let r = (|| -> windows::core::Result<()> {
                        let core = pw.controller().CoreWebView2()?;
                        let wv13: ICoreWebView2_13 = windows::core::Interface::cast(&core)?;
                        let profile: ICoreWebView2Profile2 = windows::core::Interface::cast(&wv13.Profile()?)?;
                        let h = webview2_com::ClearBrowsingDataCompletedHandler::create(Box::new(move |hr| {
                            let _ = tx2.send(hr.map_err(|e| anyhow!("clear: {e}")));
                            Ok(())
                        }));
                        profile.ClearBrowsingData(COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_PROFILE, &h)
                    })();
                    if let Err(e) = r {
                        let _ = tx.send(Err(anyhow!("clear saved logins: {e}")));
                    }
                }
            })
            .map_err(|e| anyhow!("with_webview: {e}"))?;
        rx.recv_timeout(Duration::from_secs(20)).map_err(|_| anyhow!("clearing saved logins timed out"))?
    }
}

#[derive(Debug, Clone, Default)]
pub struct PageState {
    pub host: String,
    pub logged_in: bool,
    pub challenge: bool,
    /// The page offers a login ("Inloggen" / "Log in").
    pub login_prompt: bool,
    /// Length of the visible text (0 = nothing loaded).
    pub text_len: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    LoggedIn,
    LoginRequired,
    /// `Blocked("verificatiecode")`: NS asks for a code / extra check.
    Blocked(String),
    /// NS's login form came back with an error for the stored login.
    AutoLoginFailed,
}

/// What NS's login site shows after OV Refund pressed "Inloggen".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoginPage {
    /// A one-time code / verification step.
    VerificationCode,
    /// The e-mail and password form is back, with an error message.
    Refused,
    /// Visible input fields that are neither the e-mail nor the password field.
    OtherFields,
    /// Still loading / nothing conclusive.
    Busy,
}

/// Facts about the login page (kept small; no field values are read).
const LOGIN_PAGE_JS: &str = r#"(() => {
  const vis = (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length);
  const inputs = [...document.querySelectorAll('input')].filter((i) => vis(i) && !['hidden', 'submit', 'button', 'checkbox', 'radio'].includes(i.type));
  const text = (document.body ? document.body.innerText : '').toLowerCase();
  const otp = inputs.some((i) => i.autocomplete === 'one-time-code' || /code|otp|token|pin|verif/i.test((i.name || '') + ' ' + (i.id || '')) || (i.inputMode === 'numeric' && i.maxLength > 0 && i.maxLength <= 8));
  const errors = [...document.querySelectorAll('[role=alert], .error, .alert, .notification, [aria-invalid=true]')].filter(vis).map((e) => (e.innerText || '').trim()).filter(Boolean);
  return {
    otp,
    codeText: /verificatiecode|eenmalige code|bevestigingscode|beveiligingscode|verification code|one-time code|security code/.test(text),
    hasEmail: !!document.querySelector('input[name=email]'),
    hasPassword: !!document.querySelector('input[name=password]'),
    errors: errors.length,
    errorText: errors.join(' | ').slice(0, 300),
    invalid: !!document.querySelector('input[aria-invalid=true]'),
    other: inputs.filter((i) => i.name !== 'email' && i.name !== 'password').length,
  };
})()"#;

/// What the login form shows as errors (texts + invalid flag), to compare before/after a click.
pub fn login_error_signature(v: &Value) -> String {
    format!("{}|{}", v["errorText"].as_str().unwrap_or(""), v["invalid"].as_bool().unwrap_or(false))
}

/// Counts a refused automatic login only for an error that was not on the form before the click,
/// seen at least 2 s after the click (the page has settled) and still the same at least 3 s later.
#[derive(Debug, Default)]
pub struct RefusalWatch {
    first: Option<(String, Duration)>,
}

impl RefusalWatch {
    /// `before`: error signature before the click; `now`: current one; `elapsed`: since the click.
    /// True when the refusal is confirmed.
    pub fn observe(&mut self, before: &str, now: &str, elapsed: Duration) -> bool {
        if now == before || elapsed < Duration::from_secs(2) {
            self.first = None;
            return false;
        }
        match &self.first {
            Some((sig, at)) if sig == now => elapsed.saturating_sub(*at) >= Duration::from_secs(3),
            _ => {
                self.first = Some((now.to_string(), elapsed));
                false
            }
        }
    }
}

pub fn login_page_verdict(v: &Value) -> LoginPage {
    let b = |k: &str| v[k].as_bool().unwrap_or(false);
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    if b("otp") || (b("codeText") && !b("hasPassword")) {
        LoginPage::VerificationCode
    } else if b("hasEmail") && b("hasPassword") && (n("errors") > 0 || b("invalid")) {
        LoginPage::Refused
    } else if !b("hasPassword") && n("other") > 0 {
        LoginPage::OtherFields
    } else {
        LoginPage::Busy
    }
}

/// Consecutive failed automatic logins. After [`MAX_AUTO_LOGIN_FAILURES`] automatic login is
/// switched off (the password stays stored) so NS never locks the account; a success or a newly
/// entered password resets it.
pub const AUTO_LOGIN_FAILURES: &str = "autologin_failures";
pub const AUTO_LOGIN_DISABLED: &str = "autologin_disabled";
pub const MAX_AUTO_LOGIN_FAILURES: i64 = 2;

/// Automatic login was switched off after failures but the stored login could not be erased
/// then: try again.
fn retry_erase_disabled_login() {
    let Ok(db) = crate::runner::open_db() else { return };
    if crate::runner::setting(&db, AUTO_LOGIN_DISABLED, "0") == "1"
        && crate::runner::setting(&db, "auto_login", "1") == "0"
        && crate::secrets::ns_login().is_some()
        && crate::secrets::delete(crate::secrets::NS_LOGIN).is_err()
    {
        log::warn!("could not erase saved NS login; will retry next run");
    }
}

fn record_auto_login_outcome(st: &SessionStatus) {
    let Ok(db) = crate::runner::open_db() else { return };
    match st {
        SessionStatus::LoggedIn => {
            let _ = db.conn.execute("DELETE FROM settings WHERE key IN (?1, ?2)", [AUTO_LOGIN_FAILURES, AUTO_LOGIN_DISABLED]);
        }
        SessionStatus::AutoLoginFailed => {
            let n = crate::runner::setting(&db, AUTO_LOGIN_FAILURES, "0").parse::<i64>().unwrap_or(0) + 1;
            let _ = db.set_setting(AUTO_LOGIN_FAILURES, &n.to_string());
            if n >= MAX_AUTO_LOGIN_FAILURES {
                log::warn!("auto-login failed {n} times in a row; switched off");
                // Switch off first, whatever happens to the stored login.
                let _ = db.set_setting("auto_login", "0");
                let _ = db.set_setting(AUTO_LOGIN_DISABLED, "1");
                if crate::secrets::delete(crate::secrets::NS_LOGIN).is_err() {
                    log::warn!("could not erase saved NS login; will retry next run");
                }
            }
        }
        _ => {}
    }
}

#[cfg(windows)]
unsafe fn install_listeners_on(pw: tauri::webview::PlatformWebview, net: Arc<Mutex<NetState>>) -> windows::core::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::{CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR, DevToolsProtocolEventReceivedEventHandler};
    let core = pw.controller().CoreWebView2()?;

    // Browser-engine password manager (user decision 2026-11-12): WebView2 saves the NS login
    // DPAPI-encrypted in its profile and autofills it on login.ns.nl. OV Refund never reads it.
    if let Ok(s4) = core.Settings().and_then(|s| windows::core::Interface::cast::<ICoreWebView2Settings4>(&s)) {
        // The engine's own password manager did not capture NS's XHR login (observed); keep it off.
        let _ = s4.SetIsPasswordAutosaveEnabled(false);
        let _ = s4.SetIsGeneralAutofillEnabled(false);
    }

    // Popups (e.g. "opens in new tab" links) stay inside our hidden webview.
    let handler = webview2_com::NewWindowRequestedEventHandler::create(Box::new(|_wv, args| {
        if let Some(a) = args {
            a.SetHandled(true)?;
        }
        Ok(())
    }));
    let mut tok = Default::default();
    core.add_NewWindowRequested(&handler, &mut tok)?;

    for ev in ["Network.requestWillBeSent", "Network.responseReceived", "Network.loadingFinished", "Network.loadingFailed"] {
        let receiver = core.GetDevToolsProtocolEventReceiver(*CoTaskMemPWSTR::from(ev).as_ref().as_pcwstr())?;
        let net = net.clone();
        let ev_name = ev.to_string();
        let h = DevToolsProtocolEventReceivedEventHandler::create(Box::new(
            move |sender: Option<ICoreWebView2>, args: Option<ICoreWebView2DevToolsProtocolEventReceivedEventArgs>| {
                let Some(args) = args else { return Ok(()) };
                let mut p = windows::core::PWSTR::null();
                args.ParameterObjectAsJson(&mut p)?;
                let s = CoTaskMemPWSTR::from(p).to_string();
                let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
                let rid = v["requestId"].as_str().unwrap_or("").to_string();
                match ev_name.as_str() {
                    "Network.requestWillBeSent" => {
                        let url = v.pointer("/request/url").and_then(Value::as_str).unwrap_or("");
                        if interesting(url)
                            || (CLAIMS_DIAGNOSTICS.load(std::sync::atomic::Ordering::Relaxed)
                                && diagnostic_api(url, v["type"].as_str().unwrap_or("")))
                        {
                            let method = v.pointer("/request/method").and_then(Value::as_str).unwrap_or_default().to_string();
                            let mut n = net.lock().unwrap();
                            n.seq += 1;
                            let seq = n.seq;
                            if crate::ns_adapters::is_claim_submit_write(&method, url) {
                                n.claim_writes.push(seq);
                            }
                            n.methods.insert(rid, (method, seq));
                        }
                    }
                    "Network.responseReceived" => {
                        let url = v.pointer("/response/url").and_then(Value::as_str).unwrap_or("").to_string();
                        let metadata_only = !interesting(&url)
                            && CLAIMS_DIAGNOSTICS.load(std::sync::atomic::Ordering::Relaxed)
                            && diagnostic_api(&url, v["type"].as_str().unwrap_or(""));
                        if interesting(&url) || metadata_only {
                            let status = v.pointer("/response/status").and_then(Value::as_i64).unwrap_or(0);
                            let mut n = net.lock().unwrap();
                            let (method, seq) = match n.methods.remove(&rid) {
                                Some(m) => m,
                                None => {
                                    n.seq += 1;
                                    (String::new(), n.seq)
                                }
                            };
                            if metadata_only {
                                n.captured.push(Captured { url: crate::ns_adapters::anon_path(&url), method, status, body: None, seq });
                                if n.captured.len() > 200 {
                                    n.captured.drain(0..100);
                                }
                            } else {
                                n.pending.insert(rid, (url, method, status, seq));
                            }
                        }
                    }
                    "Network.loadingFinished" => {
                        let mut n = net.lock().unwrap();
                        n.methods.remove(&rid);
                        let entry = n.pending.remove(&rid);
                        drop(n);
                        if let (Some((url, method, status, seq)), Some(wv)) = (entry, sender) {
                            if method == "OPTIONS" {
                                return Ok(());
                            }
                            let net2 = net.clone();
                            let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |hr, json| {
                                let body = hr.ok().and_then(|_| serde_json::from_str::<Value>(&json).ok()).and_then(|b| {
                                    let raw = b["body"].as_str()?.to_string();
                                    if b["base64Encoded"].as_bool().unwrap_or(false) {
                                        None
                                    } else {
                                        Some(raw)
                                    }
                                });
                                let mut n = net2.lock().unwrap();
                                n.captured.push(Captured { url, method, status, body, seq });
                                if n.captured.len() > 200 {
                                    n.captured.drain(0..100);
                                }
                                Ok(())
                            }));
                            let params = json!({ "requestId": rid }).to_string();
                            let m = CoTaskMemPWSTR::from("Network.getResponseBody");
                            let pp = CoTaskMemPWSTR::from(params.as_str());
                            wv.CallDevToolsProtocolMethod(*m.as_ref().as_pcwstr(), *pp.as_ref().as_pcwstr(), &done)?;
                        }
                    }
                    "Network.loadingFailed" => {
                        let mut n = net.lock().unwrap();
                        n.methods.remove(&rid);
                        n.pending.remove(&rid);
                    }
                    _ => {}
                }
                Ok(())
            },
        ));
        let mut token = Default::default();
        receiver.add_DevToolsProtocolEventReceived(&h, &mut token)?;
    }
    Ok(())
}

#[cfg(test)]
mod login_page_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claims_diagnostics_only_consider_ns_api_resources() {
        for host in ["gateway.apiportal.ns.nl", "api.ns.nl", "different.ns.nl", "www.ns.nl"] {
            assert!(diagnostic_api(&format!("https://{host}/api/claims?token=secret"), "Fetch"));
        }
        for url in ["https://ns.nl.evil.example/api", "https://evilns.nl/api", "http://api.ns.nl/api", "https://api.ns.nl/token"] {
            assert!(!diagnostic_api(url, "XHR"));
        }
        assert!(!diagnostic_api("https://www.ns.nl/mijnns", "Document"));
    }

    #[test]
    fn refused_login_needs_a_new_stable_error() {
        let s = |x: u64| Duration::from_millis(x);
        let mut w = RefusalWatch::default();
        // the same notice as before the click never counts
        assert!(!w.observe("cookie notice|false", "cookie notice|false", s(10_000)));
        // a new error right after the click: wait for the page to settle
        let mut w = RefusalWatch::default();
        assert!(!w.observe("|false", "Onjuist wachtwoord|true", s(1_000)));
        assert!(!w.observe("|false", "Onjuist wachtwoord|true", s(2_100)));
        assert!(!w.observe("|false", "Onjuist wachtwoord|true", s(4_000)), "not yet 3 s apart");
        assert!(w.observe("|false", "Onjuist wachtwoord|true", s(5_200)));
        // an error that changes (e.g. a transient "bezig" message) starts over
        let mut w = RefusalWatch::default();
        assert!(!w.observe("|false", "a|false", s(3_000)));
        assert!(!w.observe("|false", "b|false", s(6_500)));
        assert!(w.observe("|false", "b|false", s(9_600)));
        assert_eq!(login_error_signature(&json!({"errorText": "x", "invalid": true})), "x|true");
    }

    #[test]
    fn verification_code_and_refused_login_are_told_apart() {
        assert_eq!(login_page_verdict(&json!({"otp": true, "hasEmail": false})), LoginPage::VerificationCode);
        assert_eq!(login_page_verdict(&json!({"codeText": true, "hasPassword": false, "other": 1})), LoginPage::VerificationCode);
        assert_eq!(login_page_verdict(&json!({"hasEmail": true, "hasPassword": true, "errors": 1, "other": 0})), LoginPage::Refused);
        assert_eq!(login_page_verdict(&json!({"hasEmail": true, "hasPassword": true, "errors": 0})), LoginPage::Busy);
        assert_eq!(login_page_verdict(&json!({"hasPassword": false, "other": 2})), LoginPage::OtherFields);
        assert_eq!(login_page_verdict(&json!(null)), LoginPage::Busy);
    }

    #[test]
    fn unrecognised_mijn_ns_page_is_blocked_not_login() {
        let page = |logged_in, login_prompt, text_len| PageState {
            host: "www.ns.nl".into(),
            logged_in,
            login_prompt,
            text_len,
            ..Default::default()
        };
        assert_eq!(
            session_after_timeout(Some(&page(false, false, 2000))),
            SessionStatus::Blocked(ovrefund_core::pipeline::BLOCKED_SITE_CHANGED.into())
        );
        assert_eq!(session_after_timeout(Some(&page(false, true, 2000))), SessionStatus::LoginRequired);
        assert_eq!(session_after_timeout(Some(&page(false, false, 10))), SessionStatus::LoginRequired, "page did not load");
        assert_eq!(session_after_timeout(None), SessionStatus::LoginRequired);
    }
}

#[cfg(test)]
mod host_tests {
    #[test]
    fn login_host_is_exact() {
        assert!(super::is_ns_login_host("login.ns.nl"));
        assert!(!super::is_ns_login_host("login.ns.nl.evil.example"));
        assert!(!super::is_ns_login_host("xlogin.ns.nl"));
        assert!(super::is_ns_host("www.ns.nl") && super::is_ns_host("ns.nl"));
        assert!(!super::is_ns_host("evilns.nl") && !super::is_ns_host("ns.nl.evil.com"));
    }
}

#[cfg(test)]
mod write_mark_tests {
    use super::*;
    #[test]
    fn a_started_write_remains_visible_without_a_response_body() {
        let mut net = NetState { seq: 2, ..Default::default() };
        net.claim_writes.push(2);
        assert!(net.claim_writes.iter().any(|seq| *seq > 1));
        assert!(!net.claim_writes.iter().any(|seq| *seq > 2));
        assert!(net.captured.is_empty());
    }
}
