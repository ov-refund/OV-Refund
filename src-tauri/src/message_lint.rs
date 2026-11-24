//! Test: every user-facing error message is bilingual ("Nederlands / English"; the GUI and the
//! e-mails show one half). Scans the Rust sources of the app and the core for `anyhow!(...)`,
//! `bail!(...)` and `ensure!(cond, ...)` with a string literal. Internal / developer messages
//! (never shown as such: wrapped in a bilingual message, logged, or CLI-only) are allow-listed
//! below with the reason. Messages passed as constants or expressions are not checked here; the
//! constants have their own tests.

/// (file name, start of the message, why it may stay English)
const ALLOWED: &[(&str, &str, &str)] = &[
    // GUI contract errors: only reachable with a wrong call from our own frontend
    ("commands.rs", "unknown status", "frontend contract"),
    ("commands.rs", "invalid mode", "frontend contract"),
    ("commands.rs", "unknown field", "frontend contract"),
    ("commands.rs", "unknown flag or value", "frontend contract"),
    // command line and developer diagnostics (English console)
    ("main.rs", "journey not assessed", "devtools form-test"),
    ("main.rs", "no scheduled departure for this journey", "devtools form-test"),
    ("main.rs", "journey lacks check-out", "devtools form-test"),
    ("main.rs", "no screenshot data", "devtools form-test"),
    ("main.rs", "no recent trip to test with", "devtools form-test"),
    ("main.rs", "skipped (--no-screenshot)", "devtools form-test"),
    ("main.rs", "the claim form route does not work", "devtools form-test"),
    ("main.rs", "this build has no relay key", "devtools relay-report"),
    ("main.rs", "relay answered", "devtools relay-report"),
    ("main.rs", "route must look like", "devtools probe"),
    ("main.rs", "usage: record-external", "CLI usage"),
    ("main.rs", "usage: arriva-eval", "devtools arriva-eval usage"),
    ("main.rs", "no trip to capture", "devtools history-shot"),
    ("main.rs", "usage: delay-shot", "devtools delay-shot"),
    ("main.rs", "no train match for this journey yet", "devtools delay-shot"),
    ("main.rs", "internal error (panic)", "logged; run fails"),
    // form / page steps: wrapped by ns_adapters::form_error or ::bilingual with this as detail
    ("ns_adapters.rs", "empty travel-history body", "wrapped by bilingual()"),
    ("ns_adapters.rs", "travel history did not load", "wrapped by form_error()"),
    ("ns_adapters.rs", "trip {} {} not found", "wrapped by form_error()"),
    ("ns_adapters.rs", "the form that opened is not for this trip", "wrapped by form_error()"),
    ("ns_adapters.rs", "{}", "formats a bilingual constant"),
    ("ns_adapters.rs", "invalid scheduled departure", "wrapped by form_error()"),
    ("ns_adapters.rs", "could not set scheduled departure", "wrapped by form_error()"),
    ("ns_adapters.rs", "delay radio not selected", "wrapped by form_error()"),
    ("ns_adapters.rs", "departure field lost its value", "wrapped by form_error()"),
    ("ns_adapters.rs", "destination answer not selected", "wrapped by form_error()"),
    ("ns_adapters.rs", "refund link in the trip details not found", "wrapped by form_error()"),
    ("ns_adapters.rs", "{e}: {}", "names the missing form element; wrapped by form_error()"),
    ("ns_adapters.rs", "period filter not ready", "logged / wrapped by form_error()"),
    ("ns_adapters.rs", "period option not found or not unique", "logged / wrapped by form_error()"),
    ("ns_adapters.rs", "period option could not be selected", "logged / wrapped by form_error()"),
    ("ns_adapters.rs", "not tried", "logged / wrapped by form_error()"),
    ("ns_adapters.rs", "Tonen is disabled", "logged / wrapped by form_error()"),
    ("ns_elements.rs", "NS page element", "logged; detail inside form_error() or a bilingual NotSent reason"),
    // browser plumbing: detail inside bilingual()/form_error() messages
    ("ns_bridge.rs", "CDP ", "browser plumbing"),
    ("ns_bridge.rs", "with_webview", "browser plumbing"),
    ("ns_bridge.rs", "listener install", "browser plumbing"),
    ("ns_bridge.rs", "page script error", "browser plumbing"),
    ("ns_bridge.rs", "element not found or not visible", "browser plumbing"),
    ("ns_bridge.rs", "timed out waiting for", "browser plumbing"),
    ("ns_bridge.rs", "automatic login stopped", "detail of the bilingual fetch error"),
    ("ns_bridge.rs", "clear", "logout cleanup, logged"),
    ("updater.rs", "updates are not configured", "wrapped: 'Bijwerken mislukt. / Update failed. (...)'"),
    ("updater.rs", "invalid update information", "wrapped: 'Zoeken naar updates is mislukt (...). / Checking for updates failed (...).'"),
    ("archive.rs", "invalid month", "internal"),
    ("db.rs", "backup file already exists", "export names are unique"),
    ("db.rs", "corrupt status", "internal"),
    ("db.rs", "claim {claim_id} is {}", "internal guard; the GUI checks the status first"),
    ("db.rs", "another claim for this journey", "internal duplicate guard"),
    ("fixtures.rs", "simulated notification failure", "test fixture"),
    ("ns_mail.rs", "IMAP login failed", "wrapped by runner::sync_ns_mail"),
    ("ns_mail.rs", "open {folder}", "wrapped by runner::sync_ns_mail"),
];

/// (file name, line, message) of every literal error message without " / ".
fn scan(name: &str, src: &str) -> Vec<(String, usize, String)> {
    let b = src.as_bytes();
    let mut out = vec![];
    for mac in ["anyhow!(", "bail!(", "ensure!("] {
        let mut from = 0;
        while let Some(pos) = src[from..].find(mac) {
            let start = from + pos;
            from = start + mac.len();
            // only the macro itself (not e.g. "my_bail!(")
            if start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
                continue;
            }
            let mut i = from;
            if mac == "ensure!(" {
                // skip the condition: up to the first top-level comma
                let mut depth = 0i32;
                while i < b.len() {
                    match b[i] {
                        b'(' | b'[' | b'{' => depth += 1,
                        b')' | b']' | b'}' if depth == 0 => break,
                        b')' | b']' | b'}' => depth -= 1,
                        b'"' => {
                            i += 1;
                            while i < b.len() && b[i] != b'"' {
                                if b[i] == b'\\' {
                                    i += 1;
                                }
                                i += 1;
                            }
                        }
                        b',' if depth == 0 => break,
                        _ => {}
                    }
                    i += 1;
                }
                if i >= b.len() || b[i] != b',' {
                    continue;
                }
                i += 1;
            }
            let rest = src[i..].trim_start();
            let Some(lit) = rest.strip_prefix('"') else { continue }; // constant or expression
            let mut end = 0;
            let lb = lit.as_bytes();
            while end < lb.len() && lb[end] != b'"' {
                if lb[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            let msg = &lit[..end.min(lit.len())];
            if !msg.contains(" / ") {
                out.push((name.to_string(), src[..start].matches('\n').count() + 1, msg.to_string()));
            }
        }
    }
    out
}

#[test]
fn user_facing_errors_are_bilingual() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut dirs = vec![root.join("src")];
    for e in std::fs::read_dir(root.join("..").join("crates")).unwrap().flatten() {
        dirs.push(e.path().join("src"));
    }
    let mut bad = vec![];
    let mut files = 0;
    for d in dirs {
        for f in std::fs::read_dir(&d).unwrap().flatten() {
            let p = f.path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if p.extension().map(|x| x != "rs").unwrap_or(true) || name == "message_lint.rs" {
                continue;
            }
            files += 1;
            let src = std::fs::read_to_string(&p).unwrap();
            for (file, line, msg) in scan(&name, &src) {
                if !ALLOWED.iter().any(|(f, start, _)| *f == file && msg.starts_with(start)) {
                    bad.push(format!("{file}:{line}: {msg:?}"));
                }
            }
        }
    }
    assert!(files > 20, "sources not found");
    assert!(bad.is_empty(), "error messages without \"Nederlands / English\":\n{}", bad.join("\n"));
}

#[test]
fn scanner_finds_literals() {
    let src = "fn a() { anyhow::bail!(\"only English\"); anyhow::ensure!(x(1, 2), \"Nee / No\"); my_bail!(\"x\"); ensure!(y, MSG); }";
    let found = scan("x.rs", src);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].2, "only English");
}
