//! Per-user data locations (%LOCALAPPDATA%\OVRefund). Never inside the install dir.
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    // Unit tests must never read or migrate the real user database: use a throw-away folder.
    #[cfg(test)]
    {
        return std::env::temp_dir().join(format!("ovrefund-test-{}", std::process::id()));
    }
    // Override only in developer builds (demo/tests). Release builds always use %LOCALAPPDATA%.
    #[allow(unreachable_code)]
    if cfg!(feature = "devtools") {
        if let Ok(p) = std::env::var("OVREFUND_DATA_DIR") {
            return PathBuf::from(p);
        }
    }
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("OVRefund")
}
/// A developer instance with its own OVREFUND_DATA_DIR (devtools builds only, not the demo, which has
/// its own namespace): a short suffix derived from that folder. Its Credential Manager service,
/// scheduled task and export folder use it, so such an instance never reads, writes or removes the
/// installed app's Windows state. Unit tests always get their own suffix.
pub fn isolated_suffix() -> Option<String> {
    if cfg!(test) {
        return Some(isolated_suffix_for(&data_dir().to_string_lossy()));
    }
    if !cfg!(feature = "devtools") || crate::demo::is_demo() {
        return None;
    }
    std::env::var("OVREFUND_DATA_DIR").ok().filter(|p| !p.trim().is_empty()).map(|p| isolated_suffix_for(&p))
}

/// "-dev-<8 hex>" from the folder (case-insensitive, trailing separators ignored).
pub fn isolated_suffix_for(dir: &str) -> String {
    format!("-dev-{}", ovrefund_core::short_hash(&dir.trim().trim_end_matches(['\\', '/']).to_lowercase()))
}

pub fn db_path() -> PathBuf {
    data_dir().join("ovrefund.sqlite")
}
pub fn log_path() -> PathBuf {
    data_dir().join("ovrefund.log")
}

/// Where exports and backups go, and whether Documents was avoided because OneDrive syncs it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ExportLocation {
    pub dir: PathBuf,
    /// Documents is synced by OneDrive (a backup would be copied to the cloud): exports go to
    /// %LOCALAPPDATA%\OVRefund\exports instead.
    pub onedrive_avoided: bool,
}

/// Avoid cloud folders: OneDrive prefixes, Dropbox, Google Drive and My Drive path segments,
/// plus paths under the configured OneDrive environment folders.
pub fn is_onedrive_path(p: &std::path::Path, onedrive_roots: &[PathBuf]) -> bool {
    let lower = |s: &str| s.to_lowercase();
    if p.components().any(|c| {
        let part = lower(&c.as_os_str().to_string_lossy());
        part.starts_with("onedrive") || ["dropbox", "google drive", "my drive"].contains(&part.as_str())
    }) {
        return true;
    }
    let ps = lower(&p.to_string_lossy());
    onedrive_roots.iter().filter(|r| !r.as_os_str().is_empty()).any(|r| {
        let rs = lower(r.to_string_lossy().trim_end_matches(['\\', '/']));
        ps == rs || ps.starts_with(&format!("{rs}\\")) || ps.starts_with(&format!("{rs}/"))
    })
}

fn onedrive_roots() -> Vec<PathBuf> {
    ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect()
}

/// Exports folder: Documents\OV Refund (Documents as Windows reports it, i.e. the known-folder
/// location, which follows a OneDrive redirect), or %LOCALAPPDATA%\OVRefund\exports when
/// Documents is synced by OneDrive or unknown. The demo uses its own temporary folder.
pub fn export_location() -> ExportLocation {
    if cfg!(test) || crate::demo::is_demo() || isolated_suffix().is_some() {
        return ExportLocation { dir: data_dir().join("exports"), onedrive_avoided: false };
    }
    match dirs::document_dir() {
        Some(d) if !is_onedrive_path(&d, &onedrive_roots()) => ExportLocation { dir: d.join("OV Refund"), onedrive_avoided: false },
        Some(_) => ExportLocation { dir: data_dir().join("exports"), onedrive_avoided: true },
        None => ExportLocation { dir: data_dir().join("exports"), onedrive_avoided: false },
    }
}

/// Is this a file OV Refund itself wrote to an exports folder (backups and claim CSVs)?
pub fn is_own_export(name: &str) -> bool {
    let n = name.to_lowercase();
    (n.starts_with("ovrefund-") && n.ends_with(".ovrbackup")) || (n.starts_with("ovrefund-claims-") && n.ends_with(".csv"))
}

/// "Forget everything": delete the exports OV Refund made (only its own backup and CSV files)
/// in both possible export folders, and the folder itself when it is empty afterwards. Other
/// files the user put there are never touched. Returns the number of files removed.
pub fn remove_own_exports() -> usize {
    remove_own_exports_checked().unwrap_or_default()
}

pub fn remove_own_exports_checked() -> anyhow::Result<usize> {
    let mut dirs = vec![data_dir().join("exports")];
    // The demo only ever touches its own temporary folder, never the real exports.
    if !cfg!(test) && !crate::demo::is_demo() && isolated_suffix().is_none() {
        if let Some(d) = dirs::document_dir() {
            dirs.push(d.join("OV Refund"));
        }
    }
    let mut n = 0;
    let mut failed = false;
    for d in dirs {
        let (count, errors) = remove_matching_in(&d, is_own_export);
        n += count;
        failed |= errors;
    }
    anyhow::ensure!(!failed, "Exportbestanden niet volledig gewist / Export files not fully erased");
    Ok(n)
}

/// Remove our diagnostics independently of a full data-folder wipe, which can stop early
/// on a locked database or log. Preserve unrelated files in the diagnostics folder.
pub fn remove_own_diagnostics() -> usize {
    remove_own_diagnostics_checked().unwrap_or_default()
}

pub fn remove_own_diagnostics_checked() -> anyhow::Result<usize> {
    let (count, failed) = remove_diagnostics_in(&data_dir().join("diagnostics"));
    anyhow::ensure!(!failed, "Diagnosebestanden niet volledig gewist / Diagnostic files not fully erased");
    Ok(count)
}

fn remove_diagnostics_in(dir: &std::path::Path) -> (usize, bool) {
    remove_matching_in(dir, |name| {
        let name = name.to_lowercase();
        name.starts_with("ovrefund-diagnostics-") && name.ends_with(".json")
    })
}

/// Try every matching file even when another file is locked. Never remove unrelated files.
fn remove_matching_in(dir: &std::path::Path, matches: impl Fn(&str) -> bool) -> (usize, bool) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return (0, e.kind() != std::io::ErrorKind::NotFound),
    };
    let (mut removed, mut failed) = (0, false);
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        if !matches(&entry.file_name().to_string_lossy()) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {
                if std::fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                } else {
                    failed = true;
                }
            }
            Err(_) => failed = true,
            _ => {}
        }
    }
    // Unrelated files keep the directory in place.
    if let Err(e) = std::fs::remove_dir(dir) {
        if e.kind() != std::io::ErrorKind::DirectoryNotEmpty && e.kind() != std::io::ErrorKind::NotFound {
            failed = true;
        }
    }
    (removed, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_separate_data_dir_gets_its_own_windows_names() {
        let a = isolated_suffix_for(r"C:\Temp\canary\run\data");
        assert!(a.starts_with("-dev-") && a.len() == 13, "{a}");
        assert_eq!(a, isolated_suffix_for(r"c:\temp\CANARY\run\data\"), "case and trailing separator do not matter");
        assert_ne!(a, isolated_suffix_for(r"C:\Temp\other"));
        assert!(!a.to_lowercase().contains("canary"), "the folder name itself is not in the suffix");
        // Unit tests never use the real namespace.
        assert!(isolated_suffix().is_some());
        assert!(export_location().dir.starts_with(data_dir()));
    }

    #[test]
    fn diagnostics_are_removed_independently_and_selectively() {
        let dir = std::env::temp_dir().join(format!(
            "ovrefund-diag-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let diagnostics = dir.join("diagnostics");
        std::fs::create_dir_all(&diagnostics).unwrap();
        let own = diagnostics.join("ovrefund-diagnostics-20261113.json");
        let unrelated = diagnostics.join("notes.json");
        std::fs::write(&own, "{}").unwrap();
        std::fs::write(&unrelated, "{}").unwrap();
        // A locked file elsewhere in the data folder cannot prevent diagnostics cleanup.
        let locked = std::fs::File::create(dir.join("ovrefund.log")).unwrap();
        assert_eq!(remove_diagnostics_in(&diagnostics), (1, false));
        assert!(!own.exists());
        assert!(unrelated.exists());
        assert_eq!(remove_diagnostics_in(&diagnostics), (0, false));
        drop(locked);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(remove_diagnostics_in(&diagnostics), (0, false));
    }

    #[cfg(windows)]
    #[test]
    fn locked_private_export_does_not_stop_other_removals() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("ovrefund-cleanup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let locked = dir.join("ovrefund-backup-locked.ovrbackup");
        let other = dir.join("ovrefund-backup-other.ovrbackup");
        let handle = std::fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(0).open(&locked).unwrap();
        std::fs::write(&other, "fixture").unwrap();
        assert_eq!(remove_matching_in(&dir, is_own_export), (1, true));
        assert!(locked.exists() && !other.exists());
        drop(handle);
        assert_eq!(remove_matching_in(&dir, is_own_export), (1, false));
        assert!(!dir.exists());
    }

    #[test]
    fn tests_keep_exports_inside_the_test_data_folder() {
        assert_eq!(export_location().dir, data_dir().join("exports"));
        assert!(data_dir().starts_with(std::env::temp_dir()));
    }

    #[test]
    fn onedrive_documents_are_detected() {
        for cloud in ["Dropbox", "Google Drive", "My Drive"] {
            let path = PathBuf::from(r"C:\Users\test").join(cloud).join("Documents");
            assert!(is_onedrive_path(&path, &[]), "{cloud}");
            assert!(!is_onedrive_path(&PathBuf::from(format!("{cloud}Other")).join("Documents"), &[]));
        }
        let roots = vec![PathBuf::from(r"C:\Users\x\OneDrive")];
        assert!(is_onedrive_path(std::path::Path::new(r"C:\Users\x\OneDrive\Documenten"), &roots));
        assert!(is_onedrive_path(std::path::Path::new(r"C:\Users\x\OneDrive - Bedrijf\Documents"), &[]));
        assert!(is_onedrive_path(std::path::Path::new(r"D:\Sync\Docs"), &[PathBuf::from(r"D:\Sync")]));
        assert!(!is_onedrive_path(std::path::Path::new(r"C:\Users\x\Documents"), &roots));
        assert!(!is_onedrive_path(std::path::Path::new(r"D:\SyncOther\Docs"), &[PathBuf::from(r"D:\Sync")]));
    }

    #[test]
    fn only_our_own_exports_are_removed() {
        assert!(is_own_export("ovrefund-backup-20261113-1000.ovrbackup"));
        assert!(is_own_export("ovrefund-before-restore-20261113-1000.ovrbackup"));
        assert!(is_own_export("ovrefund-claims-20261113-1000-2.csv"));
        assert!(!is_own_export("my-notes.csv"));
        assert!(!is_own_export("ovrefund-claims.xlsx"));
        assert!(!is_own_export("photo.ovrbackup"));
    }
}
