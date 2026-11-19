//! Persist the Mijn NS browser session between runs.
//!
//! WebView2 drops session cookies (NS's `SESSION`, `INGRESSCOOKIE`, … on login.ns.nl)
//! when the process exits, and NS's long-lived `authToken` alone does not log the user
//! back in (observed 2026-11-12). Each scheduled run is a new process, so we snapshot the
//! ns.nl cookies after a confirmed login / successful run and restore them before the next
//! run — the same thing a browser's "continue where you left off" does.
//!
//! Storage: `%LOCALAPPDATA%\OVRefund\ns-session.bin`, encrypted with Windows DPAPI
//! (CryptProtectData, current-user scope). Values are never logged. `clear()` deletes it.
use crate::ns_bridge::NsBridge;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

fn path() -> std::path::PathBuf {
    crate::paths::data_dir().join("ns-session.bin")
}

fn is_ns_domain(d: &str) -> bool {
    let d = d.trim_start_matches('.');
    d == "ns.nl" || d.ends_with(".ns.nl")
}

/// Snapshot all ns.nl cookies (incl. session cookies) into the DPAPI-protected store.
pub fn save(b: &NsBridge) -> Result<usize> {
    let r = b.cdp("Network.getAllCookies", json!({}), Duration::from_secs(15))?;
    let cookies: Vec<Value> = r["cookies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| is_ns_domain(c["domain"].as_str().unwrap_or("")))
        .collect();
    let n = cookies.len();
    let blob = serde_json::to_vec(&json!({ "saved_at": chrono::Utc::now().to_rfc3339(), "cookies": cookies }))?;
    let enc = dpapi::protect(&blob).context("DPAPI encrypt")?;
    std::fs::create_dir_all(crate::paths::data_dir())?;
    let tmp = path().with_extension("tmp");
    std::fs::write(&tmp, enc)?;
    std::fs::rename(&tmp, path())?;
    Ok(n)
}

/// Restore the snapshot into the (fresh) webview before navigating to Mijn NS.
pub fn restore(b: &NsBridge) -> Result<Option<String>> {
    let Ok(enc) = std::fs::read(path()) else { return Ok(None) };
    let raw = dpapi::unprotect(&enc).context("DPAPI decrypt")?;
    let v: Value = serde_json::from_slice(&raw)?;
    let now = chrono::Utc::now().timestamp() as f64;
    let params: Vec<Value> = v["cookies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c["session"].as_bool().unwrap_or(false) || c["expires"].as_f64().map(|e| e > now).unwrap_or(true))
        .map(|c| {
            let domain = c["domain"].as_str().unwrap_or("");
            let path = c["path"].as_str().unwrap_or("/");
            let mut p = json!({
                "name": c["name"], "value": c["value"], "path": path,
                "secure": c["secure"], "httpOnly": c["httpOnly"],
            });
            if domain.starts_with('.') {
                p["domain"] = json!(domain); // domain cookie
            } else {
                p["url"] = json!(format!("https://{domain}{path}")); // host-only cookie: set via URL, no domain
            }
            if let Some(s) = c["sameSite"].as_str() {
                p["sameSite"] = json!(s);
            }
            if !c["session"].as_bool().unwrap_or(false) {
                p["expires"] = c["expires"].clone();
            }
            p
        })
        .collect();
    b.cdp("Network.setCookies", json!({ "cookies": params }), Duration::from_secs(15))?;
    Ok(v["saved_at"].as_str().map(str::to_string))
}

pub fn clear() -> Result<()> {
    match std::fs::remove_file(path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn exists() -> bool {
    path().exists()
}

#[cfg(windows)]
mod dpapi {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

    fn run(data: &[u8], protect: bool) -> windows::core::Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            if protect {
                CryptProtectData(&input, windows::core::w!("OV Refund NS session"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)?;
            } else {
                CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)?;
            }
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
            Ok(v)
        }
    }
    pub fn protect(d: &[u8]) -> windows::core::Result<Vec<u8>> {
        run(d, true)
    }
    pub fn unprotect(d: &[u8]) -> windows::core::Result<Vec<u8>> {
        run(d, false)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dpapi_roundtrip() {
        let enc = super::dpapi::protect(b"secret cookie jar").unwrap();
        assert!(!enc.windows(6).any(|w| w == b"secret"));
        assert_eq!(super::dpapi::unprotect(&enc).unwrap(), b"secret cookie jar");
    }
    #[test]
    fn domain_filter() {
        assert!(super::is_ns_domain(".ns.nl"));
        assert!(super::is_ns_domain("login.ns.nl"));
        assert!(!super::is_ns_domain("evilns.nl"));
    }
}
