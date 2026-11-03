//! The structured review input Orq imports from an exported file. It never
//! guesses: a missing or unreadable file is an error, and a round count the
//! file does not state stays unknown.

use std::io::ErrorKind;
use std::path::Path;

const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInput {
    /// Rounds the file lists; `None` when it lists none (not "three").
    pub rounds_observed: Option<usize>,
    pub outcome: Option<String>,
    /// The commit the review describes; `None` when the file names none.
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    Missing,
    Inaccessible(String),
    Malformed(String),
    /// Evidence that describes a different commit than the one reported.
    Stale(String),
}

pub fn load_review_input(path: &Path) -> Result<ReviewInput, InputError> {
    let text = std::fs::read_to_string(path).map_err(|err| match err.kind() {
        ErrorKind::NotFound => InputError::Missing,
        _ => InputError::Inaccessible(err.to_string()),
    })?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|err| InputError::Malformed(err.to_string()))?;
    match value.get("schema_version").and_then(serde_json::Value::as_u64) {
        Some(SCHEMA_VERSION) => {}
        other => return Err(InputError::Malformed(format!("unsupported schema_version {other:?}"))),
    }
    let rounds_observed = match value.get("rounds") {
        None => None,
        Some(rounds) => Some(
            rounds.as_array().ok_or_else(|| InputError::Malformed("rounds is not a list".to_string()))?.len(),
        ),
    };
    let outcome = match value.get("outcome") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(InputError::Malformed("outcome is not text".to_string())),
    };
    let head_sha = optional_text(&value, "head_sha")?;
    Ok(ReviewInput { rounds_observed, outcome, head_sha })
}

/// An optional text field: absent or null is `None`; any other type is malformed.
pub(crate) fn optional_text(value: &serde_json::Value, key: &str) -> Result<Option<String>, InputError> {
    match value.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(InputError::Malformed(format!("{key} is not text"))),
    }
}

const MAX_ROUNDS: usize = 10;
const MAX_FINDINGS_PER_ROUND: usize = 200;
const MAX_FIELD_CHARS: usize = 500;
const MAX_SUMMARY_CHARS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    High,
    Medium,
    Low,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingStatus {
    Open,
    Fixed,
    Rejected,
    Deferred,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Confirmed,
    Plausible,
}

/// One finding as the review skill recorded it. Words it does not know stay `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub file: Option<String>,
    pub line: Option<u64>,
    pub category: String,
    pub claim: String,
    pub status: FindingStatus,
    pub verdict: Option<Verdict>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round {
    /// 1-based position in the file.
    pub number: usize,
    pub summary: Option<String>,
    pub findings: Vec<Finding>,
}

/// One line of text, control characters replaced and length bounded.
fn bounded(text: &str, limit: usize) -> String {
    crate::review_lifecycle::plain_line(text).chars().take(limit).collect()
}

fn bounded_field(value: &serde_json::Value, key: &str, limit: usize) -> Result<Option<String>, InputError> {
    Ok(optional_text(value, key)?.map(|text| bounded(&text, limit)))
}

fn parse_finding(value: &serde_json::Value) -> Result<Finding, InputError> {
    if !value.is_object() {
        return Err(InputError::Malformed("a finding is not an object".to_string()));
    }
    let id = bounded_field(value, "id", MAX_FIELD_CHARS)?.filter(|id| !id.trim().is_empty()).ok_or_else(|| InputError::Malformed("a finding has no id".to_string()))?;
    let word = |key: &str| bounded_field(value, key, MAX_FIELD_CHARS).map(|w| w.map(|w| w.to_lowercase()));
    let severity = match word("severity")?.as_deref() {
        Some("high") => Severity::High,
        Some("medium") => Severity::Medium,
        Some("low") => Severity::Low,
        _ => Severity::Unknown,
    };
    let status = match word("status")?.as_deref() {
        Some("open") => FindingStatus::Open,
        Some("fixed") => FindingStatus::Fixed,
        Some("rejected") => FindingStatus::Rejected,
        Some("deferred") => FindingStatus::Deferred,
        _ => FindingStatus::Unknown,
    };
    let verdict = match word("verdict")?.as_deref() {
        Some("confirmed") => Some(Verdict::Confirmed),
        Some("plausible") => Some(Verdict::Plausible),
        _ => None,
    };
    let line = match value.get("line") {
        None | Some(serde_json::Value::Null) => None,
        Some(line) => Some(line.as_u64().ok_or_else(|| InputError::Malformed("line is not a number".to_string()))?),
    };
    Ok(Finding {
        id,
        severity,
        file: bounded_field(value, "file", MAX_FIELD_CHARS)?,
        line,
        category: bounded_field(value, "category", MAX_FIELD_CHARS)?.unwrap_or_default(),
        claim: bounded_field(value, "claim", MAX_FIELD_CHARS)?.unwrap_or_default(),
        status,
        verdict,
    })
}

/// The rounds and findings of an exported review. A file without `rounds` has none.
pub fn load_review_findings(path: &Path) -> Result<Vec<Round>, InputError> {
    let text = std::fs::read_to_string(path).map_err(|err| match err.kind() {
        ErrorKind::NotFound => InputError::Missing,
        _ => InputError::Inaccessible(err.to_string()),
    })?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|err| InputError::Malformed(err.to_string()))?;
    match value.get("schema_version").and_then(serde_json::Value::as_u64) {
        Some(SCHEMA_VERSION) => {}
        other => return Err(InputError::Malformed(format!("unsupported schema_version {other:?}"))),
    }
    let Some(rounds) = value.get("rounds") else { return Ok(Vec::new()) };
    let rounds = rounds.as_array().ok_or_else(|| InputError::Malformed("rounds is not a list".to_string()))?;
    if rounds.len() > MAX_ROUNDS {
        return Err(InputError::Malformed(format!("more than {MAX_ROUNDS} rounds")));
    }
    rounds
        .iter()
        .enumerate()
        .map(|(index, round)| {
            if !round.is_object() {
                return Err(InputError::Malformed("a round is not an object".to_string()));
            }
            let findings = match round.get("findings") {
                None | Some(serde_json::Value::Null) => Vec::new(),
                Some(list) => {
                    let list = list.as_array().ok_or_else(|| InputError::Malformed("findings is not a list".to_string()))?;
                    if list.len() > MAX_FINDINGS_PER_ROUND {
                        return Err(InputError::Malformed(format!("more than {MAX_FINDINGS_PER_ROUND} findings in a round")));
                    }
                    list.iter().map(parse_finding).collect::<Result<_, _>>()?
                }
            };
            Ok(Round { number: index + 1, summary: bounded_field(round, "summary", MAX_SUMMARY_CHARS)?, findings })
        })
        .collect()
}
