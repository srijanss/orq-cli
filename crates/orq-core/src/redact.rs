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
                r#"(?i)\b([A-Za-z0-9_\-]*(?:password|passwd|secret|token|api[_-]?key|credential|private[_-]?key)[A-Za-z0-9_\-]*)(["']?\s*[:=]\s*)(\\"[^"]*?\\"|\\'[^']*?\\'|"[^"]*"|'[^']*'|[^\s"',;]+)"#,
                "${1}${2}[REDACTED]",
            ),
        ]
    })
}

/// Redacts a stored artifact. A JSON document, or a log of JSON lines, is
/// redacted value by value so the result stays parseable and secrets hidden
/// behind JSON escapes are found; JSON that holds no secret is returned
/// byte for byte. Anything else is redacted as text, where bytes that are
/// not UTF-8 are converted lossily first so a secret cannot hide behind them.
pub fn redact_bytes(bytes: &[u8]) -> Vec<u8> {
    if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        return if redact_value(&mut value) { serde_json::to_vec(&value).unwrap_or_else(|_| redact_text_bytes(bytes)) } else { bytes.to_vec() };
    }
    if let Some(out) = redact_json_lines(bytes) {
        return out;
    }
    redact_text_bytes(bytes)
}

fn redact_text_bytes(bytes: &[u8]) -> Vec<u8> {
    redact(&String::from_utf8_lossy(bytes)).into_bytes()
}

/// Handles a log where every non-empty line is JSON; a log that mixes in
/// plain lines is redacted line by line, still keeping its line structure.
fn redact_json_lines(bytes: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut out = Vec::with_capacity(bytes.len());
    let mut any_json = false;
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        match serde_json::from_str::<serde_json::Value>(body) {
            Ok(mut value) if value.is_object() || value.is_array() => {
                any_json = true;
                if redact_value(&mut value) {
                    out.extend(serde_json::to_vec(&value).ok()?);
                } else {
                    out.extend(body.as_bytes());
                }
            }
            _ => out.extend(redact(body).into_bytes()),
        }
        out.extend(newline.as_bytes());
    }
    any_json.then_some(out)
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["password", "passwd", "secret", "token", "apikey", "api_key", "api-key", "credential", "private_key", "private-key", "privatekey"].iter().any(|word| key.contains(word))
}

/// Redacts every string in `value`, and whole string values under secret-named
/// keys. Returns whether anything changed.
fn redact_value(value: &mut serde_json::Value) -> bool {
    use serde_json::Value;
    match value {
        Value::String(text) => {
            let redacted = redact(text);
            let changed = redacted != *text;
            *text = redacted;
            changed
        }
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| redact_value(item) | changed),
        Value::Object(map) => map.iter_mut().fold(false, |changed, (key, item)| {
            if is_secret_key(key) && matches!(item, Value::String(text) if text != REDACTED) {
                *item = Value::String(REDACTED.to_string());
                true
            } else {
                redact_value(item) | changed
            }
        }),
        _ => false,
    }
}

/// Returns `text` with every recognised secret replaced by [`REDACTED`].
/// Redacting already-redacted text changes nothing.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for rule in rules() {
        // A rule that matches nothing returns the borrowed input; only a real
        // replacement allocates a new buffer.
        if let std::borrow::Cow::Owned(replaced) = rule.pattern.replace_all(&out, rule.replacement) {
            out = replaced;
        }
    }
    out
}
