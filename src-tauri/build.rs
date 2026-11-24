fn main() {
    // Bug-report relay key (Web3Forms access key; public by design, it can only deliver to the
    // project mailbox). Never stored in the repository: env OVREFUND_BUG_FORM_KEY, else
    // %USERPROFILE%\.ovrefund-release\bug-form-key.txt, else empty (relay off).
    println!("cargo:rerun-if-env-changed=OVREFUND_BUG_FORM_KEY");
    let file = std::env::var_os("USERPROFILE").map(|h| std::path::PathBuf::from(h).join(".ovrefund-release").join("bug-form-key.txt"));
    // (a missing path would make cargo rerun this script on every build)
    if let Some(f) = &file {
        if f.exists() {
            println!("cargo:rerun-if-changed={}", f.display());
        } else if let Some(dir) = f.parent().filter(|d| d.exists()) {
            println!("cargo:rerun-if-changed={}", dir.display());
        }
    }
    let key = std::env::var("OVREFUND_BUG_FORM_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| file.and_then(|f| std::fs::read_to_string(f).ok()))
        .map(|k| k.trim().to_string())
        .unwrap_or_default();
    // Only the characters of a Web3Forms key (a UUID); anything else is ignored.
    let key = if key.len() <= 64 && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') { key } else { String::new() };
    println!("cargo:rustc-env=OVREFUND_BUG_FORM_KEY={key}");
    tauri_build::build()
}
