//! Secret redaction for diagnostic text. Anything that may reach a log or a
//! report passes through `redact_secrets` first.

const MARKER: &str = "[REDACTED]";

const SENSITIVE_KEY_PARTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
    "credential",
    "oauth",
    "cookie",
    "private_key",
    "session",
];

const TOKEN_PREFIXES: &[&str] = &[
    "sk-", "sk_", "ghp_", "gho_", "ghu_", "ghs_", "github_pat_", "xoxb-", "xoxp-", "glpat-", "ya29.",
];

const EDGE: &[char] = &['"', '\'', ',', ';', '(', ')', '[', ']', '{', '}'];

fn is_sensitive_key(key: &str) -> bool {
    let key = key.trim_matches(|c: char| EDGE.contains(&c) || c == '-' || c == ':').to_ascii_lowercase();
    !key.is_empty() && SENSITIVE_KEY_PARTS.iter().any(|part| key.contains(part))
}

/// `NAME`, `NAME_2`, `HOME`: an environment-variable style name.
fn is_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn looks_like_token(word: &str) -> bool {
    let word = word.trim_matches(EDGE);
    if word.is_empty() || word.starts_with('/') || word.starts_with('.') || word.contains("://") {
        return false;
    }
    if TOKEN_PREFIXES.iter().any(|p| word.starts_with(p)) && word.len() >= 10 {
        return true;
    }
    if word.starts_with("eyJ") && word.len() >= 20 {
        return true;
    }
    if word.starts_with("AKIA") && word.len() == 20 {
        return true;
    }
    word.len() >= 32
        && word.chars().all(|c| c.is_ascii_alphanumeric() || "_-+/=.".contains(c))
        && word.chars().any(|c| c.is_ascii_digit())
        && word.chars().any(|c| c.is_ascii_alphabetic())
}

/// Byte index just past the quote that closes the one opening at `open`,
/// honouring backslash escapes; `None` when the quote never closes.
fn closing_quote(line: &str, open: usize) -> Option<usize> {
    let quote = line[open..].chars().next()?;
    let mut escaped = false;
    for (i, c) in line[open + 1..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(open + 1 + i);
        }
    }
    None
}

/// Whether the quoted span starting at `open` is the value of a credential:
/// `key: "v"`, `key="v"`, `"key":"v"` or `--key "v"`.
fn quoted_value_is_secret(before: &str) -> bool {
    let trimmed = before.trim_end();
    let (key_end, has_separator) = match trimmed.strip_suffix(['=', ':']) {
        Some(rest) => (rest.trim_end(), true),
        None => (trimmed, false),
    };
    let start = key_end
        .rfind(|c: char| c.is_whitespace() || ",{[(".contains(c))
        .map_or(0, |i| i + 1);
    let key = &key_end[start..];
    if !has_separator && !key.starts_with('-') {
        return false;
    }
    is_sensitive_key(key)
}

/// Replaces quoted values that belong to a credential key, whole, so JSON
/// objects without spaces and multi-word quoted secrets cannot slip through
/// the word-based pass.
fn redact_quoted_values(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        let c = line[i..].chars().next().unwrap_or(' ');
        let opens = (c == '"' || c == '\'')
            && line[..i].chars().next_back().is_none_or(|p| !p.is_alphanumeric());
        if opens {
            if let Some(close) = closing_quote(line, i) {
                if quoted_value_is_secret(&line[..i]) {
                    out.push(c);
                    out.push_str(MARKER);
                    out.push(c);
                } else {
                    out.push_str(&line[i..=close]);
                }
                i = close + 1;
                continue;
            }
        }
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn redact_line(line: &str) -> String {
    let line = redact_quoted_values(line);
    let line = line.as_str();
    let mut out = String::with_capacity(line.len());
    // The next word is a secret value (after `Bearer`, `password:` or `--api-key`).
    let mut redact_next = false;
    let mut rest = line;
    while !rest.is_empty() {
        let blank = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..blank]);
        rest = &rest[blank..];
        if rest.is_empty() {
            break;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..end];
        rest = &rest[end..];

        let lowered = word.trim_matches(|c: char| EDGE.contains(&c) || c == ':').to_ascii_lowercase();
        if redact_next {
            redact_next = false;
            // `Authorization: Bearer x`: the scheme word is not the secret.
            if matches!(lowered.as_str(), "bearer" | "basic" | "token") {
                redact_next = true;
                out.push_str(word);
            } else {
                out.push_str(MARKER);
            }
            continue;
        }
        if let Some(eq) = word.find('=') {
            let (key, value) = (&word[..eq], &word[eq + 1..]);
            let bare = key.trim_matches(|c: char| EDGE.contains(&c) || c == '-');
            if !value.is_empty() && (is_sensitive_key(key) || is_env_name(bare)) {
                out.push_str(key);
                out.push('=');
                out.push_str(MARKER);
                continue;
            }
        }
        if matches!(lowered.as_str(), "bearer" | "basic") {
            redact_next = true;
            out.push_str(word);
            continue;
        }
        if is_sensitive_key(word) {
            // `password: x`, `"secret": "x"` or `--api-key x`; a bare
            // `session` in prose is left for the value check below.
            let key_like = word.ends_with(':') || word.starts_with('-') || word.starts_with('"');
            if key_like {
                redact_next = true;
            }
            out.push_str(word);
            continue;
        }
        if looks_like_token(word) {
            out.push_str(MARKER);
            continue;
        }
        out.push_str(word);
    }
    out
}

/// Replaces credential-looking text with `[REDACTED]`, keeping line structure.
pub fn redact_secrets(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for (i, line) in input.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&redact_line(line));
    }
    out
}

/// Redacts an argument vector, carrying context across elements so the value
/// that follows a credential flag (`--api-key VALUE`) is hidden even when it
/// looks innocuous on its own.
fn redact_argv(argv: &[String]) -> Vec<String> {
    let mut hide_next = false;
    argv.iter()
        .map(|arg| {
            let is_flag = arg.starts_with('-');
            let out = if hide_next && !is_flag {
                MARKER.to_string()
            } else {
                redact_secrets(arg)
            };
            hide_next = is_flag && !arg.contains('=') && is_sensitive_key(arg);
            out
        })
        .collect()
}

// ---- generation failure report ------------------------------------------

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Longest stretch of each captured stream written to a report.
const REPORT_STREAM_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct GenerationReport {
    pub harness: String,
    pub model: String,
    pub spec: String,
    pub focus: String,
    pub program: String,
    /// Already has the long prompt elided.
    pub argv: Vec<String>,
    pub cwd: String,
    pub exit: String,
    pub duration: Duration,
    pub stderr_tail: String,
    pub stdout_head: String,
    pub error: String,
}

/// `$XDG_STATE_HOME/orq/logs`, else `~/.local/state/orq/logs`.
pub fn generation_log_dir(xdg_state: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match xdg_state.filter(|p| p.is_absolute()) {
        Some(dir) => dir,
        None => home?.join(".local").join("state"),
    };
    Some(base.join("orq").join("logs"))
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn utc_stamp(since_epoch: Duration) -> String {
    let secs = since_epoch.as_secs() as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn utc_readable(since_epoch: Duration) -> String {
    let s = utc_stamp(since_epoch);
    format!("{}-{}-{} {}:{}:{} UTC", &s[0..4], &s[4..6], &s[6..8], &s[9..11], &s[11..13], &s[13..15])
}

fn clip_head(text: &str) -> String {
    let mut end = text.len().min(REPORT_STREAM_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn clip_tail(text: &str) -> String {
    let mut start = text.len().saturating_sub(REPORT_STREAM_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

/// Writes `generation-<timestamp>.log` (owner-only, never overwriting) with
/// every free-text field redacted, and returns its path.
pub fn write_generation_report(dir: &Path, report: &GenerationReport) -> std::io::Result<PathBuf> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let argv = redact_argv(&report.argv).join(" ");
    let body = format!(
        "Orq generation report\n\
time: {time}\n\
harness: {harness}\n\
model: {model}\n\
spec: {spec}\n\
focus: {focus}\n\
program: {program}\n\
argv: {argv}\n\
working directory: {cwd}\n\
exit: {exit}\n\
duration: {secs:.1}s\n\
error: {error}\n\
\n\
--- stderr (last {limit} bytes at most) ---\n{stderr}\n\
\n\
--- stdout (first {limit} bytes at most) ---\n{stdout}\n",
        time = utc_readable(now),
        harness = redact_secrets(&report.harness),
        model = redact_secrets(&report.model),
        spec = redact_secrets(&report.spec),
        focus = redact_secrets(&report.focus),
        program = redact_secrets(&report.program),
        cwd = redact_secrets(&report.cwd),
        exit = redact_secrets(&report.exit),
        secs = report.duration.as_secs_f64(),
        error = redact_secrets(&report.error),
        limit = REPORT_STREAM_BYTES,
        stderr = redact_secrets(&clip_tail(&report.stderr_tail)),
        stdout = redact_secrets(&clip_head(&report.stdout_head)),
    );
    let stamp = utc_stamp(now);
    for attempt in 0..100u32 {
        let name = if attempt == 0 {
            format!("generation-{stamp}.log")
        } else {
            format!("generation-{stamp}-{attempt}.log")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(mut file) => {
                file.write_all(body.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "could not pick a unique report name"))
}
