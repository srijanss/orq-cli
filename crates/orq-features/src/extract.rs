use serde_json::{Map, Value};

use crate::FeatureDraft;

/// A feature the agent proposed, and the ledger-owned fields it tried to set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub draft: FeatureDraft,
    /// Keys Orq refuses to take from an agent (`status`, `recordedAt`,
    /// `cyclesCompleted`), in that fixed order.
    pub ignored_fields: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExtractError {
    /// No JSON array could be found in the output.
    NoJson,
    /// JSON was found but it is not an array of feature objects.
    Shape { index: Option<usize>, reason: String },
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::NoJson => f.write_str("the agent output contained no JSON array"),
            ExtractError::Shape { index: Some(i), reason } => {
                write!(f, "candidate {} is invalid: {reason}", i + 1)
            }
            ExtractError::Shape { index: None, reason } => write!(f, "agent output is invalid: {reason}"),
        }
    }
}

impl std::error::Error for ExtractError {}

const LEDGER_OWNED: [&str; 3] = ["status", "recordedAt", "cyclesCompleted"];

/// Parses agent output as untrusted data. Nothing in it is executed or
/// resolved; strings are carried verbatim as inert text.
pub fn extract_candidates(output: &str) -> Result<Vec<Candidate>, ExtractError> {
    let array = find_array(output)?;
    array.iter().enumerate().map(|(i, value)| candidate(i, value)).collect()
}

fn find_array(output: &str) -> Result<Vec<Value>, ExtractError> {
    let text = output.trim();
    if text.is_empty() {
        return Err(ExtractError::NoJson);
    }
    let mut sources: Vec<&str> = vec![text];
    // Fenced blocks: the content between a ``` line and the next ```.
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        let body_start = after.find('\n').map_or(0, |n| n + 1);
        let Some(end) = after[body_start..].find("```") else { break };
        sources.push(after[body_start..body_start + end].trim());
        rest = &after[body_start + end + 3..];
    }
    if let (Some(open), Some(close)) = (text.find('['), text.rfind(']')) {
        if open < close {
            sources.push(&text[open..=close]);
        }
    }
    let mut saw_other = false;
    for source in sources {
        match serde_json::from_str::<Value>(source) {
            Ok(Value::Array(items)) => return Ok(items),
            Ok(_) => saw_other = true,
            Err(_) => {}
        }
    }
    if saw_other {
        return Err(ExtractError::Shape {
            index: None,
            reason: "expected a JSON array of feature objects".into(),
        });
    }
    Err(ExtractError::NoJson)
}

fn shape(index: usize, reason: &str) -> ExtractError {
    ExtractError::Shape { index: Some(index), reason: reason.into() }
}

fn string(index: usize, map: &Map<String, Value>, key: &str) -> Result<Option<String>, ExtractError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(shape(index, &format!("{key} must be a string"))),
    }
}

fn strings(index: usize, map: &Map<String, Value>, key: &str) -> Result<Option<Vec<String>>, ExtractError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => Ok(s.clone()),
                _ => Err(shape(index, &format!("{key} must be an array of strings"))),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(shape(index, &format!("{key} must be an array of strings"))),
    }
}

fn candidate(index: usize, value: &Value) -> Result<Candidate, ExtractError> {
    let Value::Object(map) = value else {
        return Err(shape(index, "expected an object"));
    };
    let name = match map.get("featureName") {
        Some(Value::String(name)) if !name.trim().is_empty() => name.clone(),
        Some(Value::String(_)) => return Err(shape(index, "featureName is empty")),
        Some(_) => return Err(shape(index, "featureName must be a string")),
        None => return Err(shape(index, "featureName is missing")),
    };
    if name.chars().any(char::is_control) {
        return Err(shape(index, "featureName contains control characters"));
    }
    let parallelizable = match map.get("parallelizable") {
        None => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(shape(index, "parallelizable must be a boolean")),
    };
    let draft = FeatureDraft {
        feature_name: name,
        description: string(index, map, "description")?,
        depends_on: strings(index, map, "dependsOn")?,
        parallelizable,
        test_file: string(index, map, "testFile")?,
        target_files: strings(index, map, "targetFiles")?,
    };
    let ignored_fields = LEDGER_OWNED
        .iter()
        .filter(|key| map.contains_key(**key))
        .map(|key| key.to_string())
        .collect::<Vec<_>>();
    Ok(Candidate { draft, ignored_fields })
}
