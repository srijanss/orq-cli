//! Removes secrets from text before Orq stores or shows it. Agent output,
//! repository comments and commit messages are untrusted and may contain
//! credentials; nothing Orq writes to a report, log or export may carry them.

use std::sync::OnceLock;

use regex::Regex;

/// What a removed secret is replaced with.
pub const REDACTED: &str = "[REDACTED]";

struct Rule {
    pattern: Regex,
    replacement: &'static str,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let rule = |pattern: &str, replacement: &'static str| Rule { pattern: Regex::new(pattern).expect("redaction pattern"), replacement };
        vec![
            // Whole private-key blocks, including one cut off before its END line.
            rule(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)", "[REDACTED]"),
            // Well-known token formats.
            rule(
                r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|glpat-[A-Za-z0-9_\-]{16,}|AKIA[0-9A-Z]{16}|xox[abprs]-[A-Za-z0-9\-]{10,}|sk-[A-Za-z0-9_\-]{20,}|eyJ[A-Za-z0-9_\-]{5,}\.[A-Za-z0-9_\-]{5,}\.[A-Za-z0-9_\-]{5,})",
                "[REDACTED]",
            ),
            // Credentials embedded in a URL: scheme://user:password@host.
            rule(r"\b([A-Za-z][A-Za-z0-9+.\-]*://)[^\s/@:]+:[^\s/@]+@", "${1}[REDACTED]@"),
            // Authorization header values.
            rule(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=\-]{8,}", "${1}[REDACTED]"),
            // name = value, name: value for secret-looking names; the name stays.
            rule(
                r#"(?i)\b([A-Za-z0-9_\-]*(?:password|passwd|secret|token|api[_-]?key|credential|private[_-]?key)[A-Za-z0-9_\-]*)(["']?\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s"',;]+)"#,
                "${1}${2}[REDACTED]",
            ),
        ]
    })
}

/// Redacts stored bytes. Text that is valid UTF-8 is redacted as text; bytes
/// that are not are converted lossily first, so a secret can never hide
/// behind an invalid byte.
pub fn redact_bytes(bytes: &[u8]) -> Vec<u8> {
    redact(&String::from_utf8_lossy(bytes)).into_bytes()
}

/// Returns `text` with every recognised secret replaced by [`REDACTED`].
/// Redacting already-redacted text changes nothing.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for rule in rules() {
        out = rule.pattern.replace_all(&out, rule.replacement).into_owned();
    }
    out
}
