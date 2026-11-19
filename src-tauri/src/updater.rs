//! In-app updates (Tauri updater). The app downloads a small `latest.json` from UPDATE_URL; if it
//! lists a newer version, the user can install it with one click. Every installer is signed with
//! the maintainer's private key and verified against the public key in tauri.conf.json before it
//! runs, so a compromised or spoofed host cannot push its own code.
//!
//! Privacy: an update check is a plain HTTPS GET of a static file (the host sees an IP address and
//! "OV Refund/<version>"); nothing about the user is sent. Checks can be turned off.
use anyhow::Result;
use serde_json::{json, Value};
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// Where `latest.json` is published. Empty = update checks are disabled in this build.
pub const UPDATE_URL: &str = "https://github.com/ov-refund/OV-Refund/releases/latest/download/latest.json";

/// Developer builds only: `OVREFUND_UPDATE_URL` points the updater at a local test server
/// (used to test the whole update flow). Release builds always use UPDATE_URL.
fn update_url() -> String {
    if cfg!(feature = "devtools") {
        if let Ok(u) = std::env::var("OVREFUND_UPDATE_URL") {
            if !u.is_empty() {
                return u;
            }
        }
    }
    UPDATE_URL.to_string()
}

pub fn configured() -> bool {
    !update_url().is_empty() && !crate::demo::is_demo()
}

fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater> {
    Ok(app
        .updater_builder()
        .endpoints(vec![update_url().parse()?])?
        .timeout(std::time::Duration::from_secs(30))
        .header("User-Agent", format!("OV Refund/{}", env!("CARGO_PKG_VERSION")))?
        .build()?)
}

/// Missing releases are normal before the first public release. Keep actual transport and
/// installation failures distinct; never hide signature or download errors.
fn no_public_release(error: &tauri_plugin_updater::Error) -> bool {
    use tauri_plugin_updater::Error;
    match error {
        Error::ReleaseNotFound | Error::Serialization(_) | Error::Semver(_) => true,
        Error::Reqwest(e) => e.status().map(|s| s.as_u16() == 404).unwrap_or(false) || e.is_decode(),
        _ => false,
    }
}

fn release_result<T>(result: tauri_plugin_updater::Result<Option<T>>) -> Result<Option<T>> {
    match result {
        Err(e) if no_public_release(&e) => {
            static LOGGED: std::sync::Once = std::sync::Once::new();
            LOGGED.call_once(|| log::info!("no public update release available"));
            Ok(None)
        }
        other => Ok(other?),
    }
}

/// `{configured, current, available, version?, notes?, date?}`
pub async fn check(app: &AppHandle) -> Result<Value> {
    let current = env!("CARGO_PKG_VERSION");
    if !configured() {
        return Ok(json!({ "configured": false, "current": current, "available": false }));
    }
    let u = release_result(updater(app)?.check().await)?;
    Ok(match u {
        Some(u) => json!({
            "configured": true, "current": current, "available": true,
            "version": u.version, "notes": u.body, "date": u.date.map(|d| d.to_string()),
        }),
        None => json!({ "configured": true, "current": current, "available": false }),
    })
}

/// Download, verify the signature and start the installer (passive mode). On Windows the
/// installer closes this app and starts the new version; data, login and schedule are kept.
pub async fn install(app: &AppHandle) -> Result<()> {
    anyhow::ensure!(configured(), "updates are not configured in this build");
    let Some(u) = release_result(updater(app)?.check().await)? else {
        anyhow::bail!("Je hebt al de nieuwste versie. / You already have the latest version.");
    };
    log::info!("installing update {} -> {}", env!("CARGO_PKG_VERSION"), u.version);
    u.download_and_install(|_, _| {}, || {}).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_invalid_release_are_silent_no_update() {
        use tauri_plugin_updater::Error;
        assert!(release_result::<()>(Err(Error::ReleaseNotFound)).unwrap().is_none());
        let invalid = serde_json::from_str::<Value>("not JSON").unwrap_err();
        assert!(release_result::<()>(Err(Error::Serialization(invalid))).unwrap().is_none());
        let invalid_schema = serde_json::from_str::<tauri_plugin_updater::RemoteRelease>("{}").unwrap_err();
        assert!(release_result::<()>(Err(Error::Serialization(invalid_schema))).unwrap().is_none());
        assert!(release_result::<()>(Err(Error::Network("offline".into()))).is_err());
        assert_eq!(release_result(Ok(Some(42))).unwrap(), Some(42));
        assert!(release_result::<()>(Ok(None)).unwrap().is_none());
    }
}
