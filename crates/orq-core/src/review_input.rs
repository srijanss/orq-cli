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
