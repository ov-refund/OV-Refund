//! Secret / personal-data redaction for logs, audit details, diagnostics and emails.
use once_cell::sync::Lazy;
use regex::Regex;

static RULES: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    vec![
        // Bearer tokens first, then header-style lines (whole value), then key=value pairs
        (Regex::new(r"(?i)bearer\s+[A-Za-z0-9._\-~+/=]+").unwrap(), "Bearer <redacted>"),
        (Regex::new(r"(?im)^(\s*(?:authorization|cookie|set-cookie|ocp-apim-subscription-key|x-csrf-token)\s*:\s*).*$").unwrap(), "$1<redacted>"),
        // Quoted JSON / key-value secrets can contain spaces and escaped quotes.
        (Regex::new(r#"(?i)(["']?(?:authorization|cookie|set-cookie|ocp-apim-subscription-key|x-csrf-token|csrftoken|access[_-]?token|refresh[_-]?token|authtoken|password|wachtwoord)["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')"#).unwrap(), "$1<redacted>"),
        (Regex::new(r"(?i)(authorization|cookie|set-cookie|ocp-apim-subscription-key|x-csrf-token|csrftoken|access[_-]?token|refresh[_-]?token|authtoken|password|wachtwoord)(\s*[:=]\s*)[^\s,;&]+").unwrap(), "$1$2<redacted>"),
        // JWT-like
        (Regex::new(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}").unwrap(), "<jwt>"),
        // IBAN
        (Regex::new(r"(?i)\b[A-Z]{2}\d{2}\s?(?:[A-Z0-9]{4}\s?){2,7}[A-Z0-9]{1,4}\b").unwrap(), "<iban>"),
        // UUIDs (NS coupling / transaction ids)
        (Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").unwrap(), "<id>"),
        // e-mail
        (Regex::new(r"[\w.+-]+@[\w-]+\.[\w.-]+").unwrap(), "<email>"),
        // card numbers / long digit runs (spaced or not)
        (Regex::new(r"\b\d{4}(?:[ .-]?\d{4}){2,4}\b").unwrap(), "<card>"),
        (Regex::new(r"\b\d(?:[\s().-]*\d){9,}\b").unwrap(), "<num>"),
    ]
});

pub fn redact(s: &str) -> String {
    let mut out = s.to_string();
    for (re, rep) in RULES.iter() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets() {
        let s = "Authorization: Bearer abc.def.ghi cookie=SESSION123 card 3528 0000 1111 2222 iban NL91ABNA0417164300 nl91abna0417164300 tx 1b4e28ba-2fa1-11d2-883f-0016d3cca427 mail a.b@example.org pw password=hunter2";
        let r = redact(s);
        for leak in ["abc.def", "SESSION123", "3528 0000", "NL91ABNA", "nl91abna", "1b4e28ba", "a.b@example", "hunter2"] {
            assert!(!r.contains(leak), "{leak} leaked in {r}");
        }
    }

    #[test]
    fn quoted_secrets_are_fully_redacted() {
        let s = r#"{"password": "four word app secret", "access_token":"abc123"} password='another long secret'"#;
        let r = redact(s);
        for leak in ["four word app secret", "abc123", "another", "long secret"] {
            assert!(!r.contains(leak), "{leak} leaked in {r}");
        }
    }

    #[test]
    fn formatted_phone_numbers_are_redacted() {
        for phone in ["06 12345678", "06.1234.5678", "+31 (0)6 1234 5678", "020-1234567"] {
            let cleaned = redact(&format!("tel: {phone}"));
            assert!(!cleaned.contains("1234"), "{cleaned}");
        }
    }

    #[test]
    fn keeps_normal_text() {
        assert_eq!(redact("Utrecht Centraal -> Amersfoort 30 t/m 59 min"), "Utrecht Centraal -> Amersfoort 30 t/m 59 min");
    }
}
