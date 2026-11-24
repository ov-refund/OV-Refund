//! Secrets in Windows Credential Manager (DPAPI-protected, per user).
//! The NS session itself lives in the WebView2 profile (Chromium encrypts cookies with DPAPI).
use anyhow::Result;
use keyring::Entry;

fn service() -> String {
    if crate::demo::is_demo() {
        format!("OVRefund-demo{}", crate::demo::instance_suffix())
    } else if shared_read_only() {
        "OVRefund".to_string()
    } else {
        service_for(crate::paths::isolated_suffix().as_deref())
    }
}

/// "OVRefund" for the installed app; "OVRefund-dev-<hash>" for a developer instance with its own data
/// folder (see [`crate::paths::isolated_suffix`]).
fn service_for(isolated: Option<&str>) -> String {
    format!("OVRefund{}", isolated.unwrap_or_default())
}

/// Developer builds only: OVREFUND_SHARED_SECRETS=1 lets a developer instance with its own data folder
/// (the maintainer's daily dry run) READ the installed app's saved NS login, bank details and card
/// numbers. Writing or deleting them is then refused, so such a run never changes the real entries.
pub fn shared_read_only() -> bool {
    shared_read_only_for(
        cfg!(feature = "devtools") && !cfg!(test),
        crate::demo::is_demo(),
        crate::paths::isolated_suffix().is_some(),
        std::env::var("OVREFUND_SHARED_SECRETS").as_deref() == Ok("1"),
    )
}

fn shared_read_only_for(devtools: bool, demo: bool, isolated: bool, env: bool) -> bool {
    devtools && !demo && isolated && env
}

fn ensure_writable() -> Result<()> {
    anyhow::ensure!(
        !shared_read_only(),
        "Opgeslagen gegevens zijn in deze testrun alleen-lezen. / Saved details are read-only in this test run."
    );
    Ok(())
}
pub const BANK: &str = "ns-bank-details";
pub const SMTP_PASSWORD: &str = "smtp-password";
/// NS login for unattended runs (user decision 2026-11-12; see SECURITY.md).
pub const NS_LOGIN: &str = "ns-login";

pub struct NsLogin {
    pub email: String,
    pub password: String,
}

pub fn ns_login() -> Option<NsLogin> {
    let raw = get(NS_LOGIN).ok().flatten()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(NsLogin { email: v["email"].as_str()?.to_string(), password: v["password"].as_str()?.to_string() })
}

pub fn set_ns_login(email: &str, password: &str) -> Result<()> {
    anyhow::ensure!(
        email.contains('@') && !password.is_empty(),
        "Vul je e-mailadres en wachtwoord voor Mijn NS in. / Enter your Mijn NS e-mail address and password."
    );
    anyhow::ensure!(
        email.len() <= 254 && password.chars().count() <= 200,
        "E-mailadres of wachtwoord is te lang. / E-mail address or password is too long."
    );
    set(NS_LOGIN, &serde_json::json!({ "email": email.trim(), "password": password }).to_string())
}

fn entry(key: &str) -> Result<Entry> {
    Ok(Entry::new(&service(), key)?)
}

pub fn set(key: &str, value: &str) -> Result<()> {
    ensure_writable()?;
    entry(key)?.set_password(value)?;
    Ok(())
}

pub fn get(key: &str) -> Result<Option<String>> {
    match entry(key)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Credential Manager key of a full OV-chipkaart number: per linked card (a short hash of NS's
/// coupling id) and its last four digits, so two cards ending in the same digits never share one.
pub fn card_key(coupling: &str, last4: &str) -> String {
    format!("ov-card-{last4}-{}", ovrefund_core::short_hash(coupling))
}

/// Key of older versions: the last four digits only.
pub fn legacy_card_key(last4: &str) -> String {
    format!("ov-card-{last4}")
}

/// Stores a full card number (8-24 digits) under its linked card.
pub fn store_card_number(coupling: &str, number: &str) -> Result<()> {
    anyhow::ensure!(crate::ns_adapters::card_number_ok(number), "Ongeldig kaartnummer. / Invalid card number.");
    set(&card_key(coupling, &number[number.len() - 4..]), number)
}

/// The full number (digits) of the card ending in `last4` of linked card `coupling`. An entry of an
/// older version (last four digits only) is used only when the ledger knows no other linked card
/// ending in the same digits.
pub fn card_number(db: &ovrefund_core::db::Db, coupling: Option<&str>, last4: &str) -> Option<String> {
    let valid = |n: String| {
        let d: String = n.chars().filter(char::is_ascii_digit).collect();
        (crate::ns_adapters::card_number_ok(&d) && d.ends_with(last4)).then_some(d)
    };
    if let Some(n) = coupling.and_then(|c| get(&card_key(c, last4)).ok().flatten()).and_then(valid) {
        return Some(n);
    }
    let known = db.couplings_for_last4(last4).ok()?;
    let unambiguous = known.len() <= 1 && coupling.is_none_or(|c| known.iter().all(|k| k == c));
    if !unambiguous {
        return None;
    }
    get(&legacy_card_key(last4)).ok().flatten().and_then(valid)
}

pub fn delete(key: &str) -> Result<()> {
    ensure_writable()?;
    match entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn bank_details() -> Option<crate::ns_adapters::BankDetails> {
    let raw = get(BANK).ok().flatten()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(crate::ns_adapters::BankDetails { name: v["name"].as_str()?.to_string(), iban: v["iban"].as_str()?.to_string() })
}

pub fn set_bank_details(name: &str, iban: &str) -> Result<()> {
    let iban: String = iban.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
    anyhow::ensure!(iban_valid(&iban), "Dit IBAN klopt niet. Controleer het nummer. / This IBAN is not valid. Check the number.");
    // A typographic apostrophe (as phones and Word type it) is stored as the plain one.
    let name = holder_name_plain(name);
    let name = name.trim();
    anyhow::ensure!(valid_holder_name(name), "Vul de naam van de rekeninghouder in. / Enter the account holder's name.");
    anyhow::ensure!(name.trim().chars().count() <= 70, "De naam is te lang (max. 70 tekens). / The name is too long (max. 70 characters).");
    set(BANK, &serde_json::json!({ "name": name.trim(), "iban": iban }).to_string())
}

/// Expected IBAN length for common SEPA countries (ISO 13616 registry).
fn iban_length(country: &str) -> Option<usize> {
    Some(match country {
        "NL" => 18,
        "BE" => 16,
        "DE" => 22,
        "FR" | "IT" | "MC" => 27,
        "ES" | "SE" => 24,
        "LU" => 20,
        "AT" => 20,
        "IE" | "GB" => 22,
        "PT" => 25,
        "DK" | "FI" => 18,
        "PL" => 28,
        _ => return None,
    })
}

/// Dutch bank from the 4-letter bank code in an NL IBAN (positions 5-8).
pub fn nl_bank_name(iban: &str) -> Option<&'static str> {
    if !iban.starts_with("NL") || iban.len() < 8 {
        return None;
    }
    Some(match &iban[4..8] {
        "ABNA" => "ABN AMRO",
        "INGB" => "ING",
        "RABO" => "Rabobank",
        "SNSB" => "SNS",
        "ASNB" => "ASN Bank",
        "RBRB" => "RegioBank",
        "TRIO" => "Triodos Bank",
        "KNAB" => "Knab",
        "BUNQ" => "bunq",
        "REVO" => "Revolut",
        "FVLB" => "Van Lanschot",
        "NNBA" => "Nationale-Nederlanden",
        "HAND" => "Svenska Handelsbanken",
        "ADYB" => "Adyen",
        _ => return None,
    })
}

/// ISO 13616: alphanumeric, country-specific length (where known) and the mod-97 checksum.
pub fn iban_valid(iban: &str) -> bool {
    if iban.len() < 15 || iban.len() > 34 || !iban.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let bytes = iban.as_bytes();
    if !bytes[..2].iter().all(u8::is_ascii_alphabetic) || !bytes[2..4].iter().all(u8::is_ascii_digit) {
        return false;
    }
    if iban.starts_with("NL") && (!bytes[4..8].iter().all(u8::is_ascii_alphabetic) || !bytes[8..].iter().all(u8::is_ascii_digit)) {
        return false;
    }
    if let Some(n) = iban_length(&iban[..2]) {
        if iban.len() != n {
            return false;
        }
    }
    let rearranged = format!("{}{}", &iban[4..], &iban[..4]);
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let v = if c.is_ascii_digit() { c as u32 - '0' as u32 } else { c.to_ascii_uppercase() as u32 - 'A' as u32 + 10 };
        rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
    }
    rem == 1
}

/// Name + bank + last 4 IBAN characters only, for display.
pub fn bank_hint() -> Option<String> {
    bank_details().map(|b| {
        let bank = nl_bank_name(&b.iban).map(|n| format!(" ({n})")).unwrap_or_default();
        format!("{}{} · …{}", b.name, bank, &b.iban[b.iban.len().saturating_sub(4)..])
    })
}

/// Is this Credential Manager target name one of ours? keyring stores generic credentials with
/// target name "<key>.<service>": service "OVRefund" (normal) or "OVRefund-demo[-<instance>]"
/// (demo). The normal app's wipe covers both; a demo wipe only its own demo namespace (it must
/// never touch the real credentials).
fn owned_target(target: &str, demo: bool, demo_service: &str) -> bool {
    // The demo and a developer instance with its own data folder: only their own namespace.
    if demo {
        return target.ends_with(&format!(".{demo_service}"));
    }
    target.ends_with(".OVRefund") || target.rsplit_once('.').map(|(_, svc)| svc.starts_with("OVRefund-demo")).unwrap_or(false)
}

/// Delete EVERY Credential Manager entry of OV Refund (normal and demo namespaces; in the demo
/// only its own), found by enumeration rather than by known keys, so nothing is orphaned (e.g.
/// card entries after a history wipe). Returns the number of entries removed.
#[cfg(windows)]
pub fn delete_all() -> Result<usize> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Security::Credentials::{
        CredDeleteW, CredEnumerateW, CredFree, CREDENTIALW, CRED_ENUMERATE_FLAGS, CRED_TYPE_GENERIC,
    };
    ensure_writable()?;
    let demo = crate::demo::is_demo() || crate::paths::isolated_suffix().is_some();
    let svc = service();
    let mut count = 0u32;
    let mut list: *mut *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: Win32 API with valid out-pointers; the returned buffer is freed with CredFree.
    // No filter (CredEnumerate only supports "prefix*" filters): all of this user's credentials
    // are listed and ours are picked by name below.
    unsafe {
        if let Err(err) = CredEnumerateW(PCWSTR::null(), Some(CRED_ENUMERATE_FLAGS(0)), &mut count, &mut list) {
            if err.code() == windows::core::HRESULT::from_win32(1168) {
                // ERROR_NOT_FOUND: empty vault
                return Ok(0);
            }
            anyhow::bail!(
                "Opgeslagen wachtwoorden wissen is mislukt. Probeer het opnieuw. / Deleting saved passwords failed. Please try again."
            );
        }
        let names: Vec<Vec<u16>> = (0..count as usize)
            .filter_map(|i| {
                let c = *list.add(i);
                if c.is_null() || (*c).Type != CRED_TYPE_GENERIC {
                    return None;
                }
                let p: PWSTR = (*c).TargetName;
                let name = p.to_string().ok()?;
                owned_target(&name, demo, &svc).then(|| p.as_wide().iter().copied().chain(Some(0)).collect())
            })
            .collect();
        CredFree(list as *const _);
        let mut removed = 0;
        let mut failed = false;
        for name in names {
            match CredDeleteW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None) {
                Ok(()) => removed += 1,
                Err(err) if err.code() == windows::core::HRESULT::from_win32(1168) => {}
                Err(_) => failed = true,
            }
        }
        anyhow::ensure!(!failed, "Niet alle opgeslagen wachtwoorden zijn gewist. Probeer het opnieuw. / Some saved passwords could not be deleted. Please try again.");
        Ok(removed)
    }
}

#[cfg(not(windows))]
pub fn delete_all() -> Result<usize> {
    Ok(0)
}

#[cfg(test)]
mod owned_tests {
    use super::owned_target;
    #[test]
    fn normal_wipe_covers_demo_namespaces_demo_wipe_only_its_own() {
        assert!(owned_target("ns-bank-details.OVRefund", false, ""));
        assert!(owned_target("smtp-password.OVRefund-demo", false, ""));
        assert!(owned_target("ov-card-1234.OVRefund-demo-a1", false, ""));
        assert!(owned_target(&format!("{}.OVRefund", super::card_key("coupling-a", "1234")), false, ""));
        assert_ne!(super::card_key("coupling-a", "1234"), super::card_key("coupling-b", "1234"), "same last four digits, other card");
        assert!(!super::card_key("coupling-a", "1234").contains("coupling"), "the coupling id itself is not in the name");
        assert!(!owned_target("something.OVRefundX", false, ""));
        assert!(!owned_target("OVRefund", false, ""));
        assert!(!owned_target("git:https://github.com", false, ""));
        assert!(owned_target("smtp-password.OVRefund-demo-a1", true, "OVRefund-demo-a1"));
        assert!(!owned_target("smtp-password.OVRefund", true, "OVRefund-demo-a1"), "demo never touches real credentials");
        assert!(!owned_target("smtp-password.OVRefund-demo", true, "OVRefund-demo-a1"));
        // A developer instance with its own data folder wipes only its own namespace.
        assert!(owned_target("ns-login.OVRefund-dev-0123abcd", true, "OVRefund-dev-0123abcd"));
        assert!(!owned_target("ns-login.OVRefund", true, "OVRefund-dev-0123abcd"));
    }

    #[test]
    fn a_separate_data_dir_never_uses_the_real_credentials() {
        assert_eq!(super::service_for(None), "OVRefund");
        let s = super::service_for(Some(&crate::paths::isolated_suffix_for("C:/x/data")));
        assert!(s.starts_with("OVRefund-dev-") && s != "OVRefund");
        // Unit tests: own namespace, never the real one.
        assert!(super::service().starts_with("OVRefund-dev-"));
        // Shared read-only use only in a developer build, outside the demo, with its own folder and the env var.
        assert!(super::shared_read_only_for(true, false, true, true));
        assert!(!super::shared_read_only_for(false, false, true, true), "never in a release build");
        assert!(!super::shared_read_only_for(true, true, true, true), "never in the demo");
        assert!(!super::shared_read_only_for(true, false, false, true), "only with its own data folder");
        assert!(!super::shared_read_only_for(true, false, true, false), "only when asked for");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn iban_check() {
        assert!(super::iban_valid("NL91ABNA0417164300"));
        assert!(!super::iban_valid("NL91ABNA0417164301"));
        assert!(!super::iban_valid("NL91ABNA041716430"), "wrong NL length");
        assert!(super::iban_valid("BE68539007547034"));
        // Mod-97 alone would accept these malformed, fictional account strings.
        for malformed in ["NL062CN00000000000", "NL13FAKE000000000X", "0011FAKE0000000000"] {
            assert!(!super::iban_valid(malformed));
        }
        assert_eq!(super::nl_bank_name("NL91ABNA0417164300"), Some("ABN AMRO"));
    }
}

fn holder_name_plain(name: &str) -> String {
    name.replace(['\u{2019}', '\u{2018}'], "'")
}

fn valid_holder_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= 70
        && name.chars().any(char::is_alphabetic)
        && name.chars().all(|c| c.is_alphabetic() || matches!(c, ' ' | '.' | '\'' | '-'))
}

#[cfg(test)]
mod holder_tests {
    #[test]
    fn holder_alphabet_and_length() {
        for name in ["É. de Vries", "O'Neil-Sánchez"] {
            assert!(super::valid_holder_name(name));
        }
        for name in ["", "123", "Ada@bank", "Ada\nBank", "Ada 💶"] {
            assert!(!super::valid_holder_name(name));
        }
        assert!(!super::valid_holder_name(&"A".repeat(71)));
        assert_eq!(super::holder_name_plain("O\u{2019}Neil"), "O'Neil");
        assert!(super::valid_holder_name(&super::holder_name_plain("O\u{2019}Neil")));
    }
}
