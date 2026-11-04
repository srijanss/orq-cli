//! The optional model summary of a review report. It is off unless the config
//! enables it and names a model; it is labelled as generated, written only from
//! the report's own evidence, and any failure leaves the report as it was.

use serde_json::{json, Value};

use crate::review_report::render_markdown;

/// Longest generated summary kept, in characters.
pub const MAX_SUMMARY_CHARS: usize = 1500;
/// Most evidence handed to the model, in characters.
const MAX_INPUT_CHARS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryConfig {
    pub model: String,
}

impl SummaryConfig {
    /// Reads the `[review_summary]` table of a config file. `None` means the
    /// summary is off, which is the default; turning it on needs a model.
    pub fn parse(text: &str) -> Result<Option<SummaryConfig>, String> {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|_: toml_edit::TomlError| "the config file is not valid TOML".to_string())?;
        let Some(item) = doc.get("review_summary") else { return Ok(None) };
        let table = item.as_table_like().ok_or("[review_summary] must be a table")?;
        let mut enabled = false;
        let mut model = None;
        for (key, value) in table.iter() {
            match key {
                "enabled" => enabled = value.as_bool().ok_or("review_summary.enabled must be true or false")?,
                "model" => model = Some(value.as_str().ok_or("review_summary.model must be text")?.to_string()),
                _ => return Err("review_summary has an unknown setting".to_string()),
            }
        }
        if !enabled {
            return Ok(None);
        }
        let model = model.ok_or("review_summary.enabled needs review_summary.model")?;
        if !is_plain_model(&model) {
            return Err("review_summary.model must be a plain model name".to_string());
        }
        Ok(Some(SummaryConfig { model }))
    }
}

/// A model name that cannot be mistaken for a flag or split into arguments.
pub fn is_plain_model(model: &str) -> bool {
    !model.is_empty()
        && !model.starts_with('-')
        && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '+'))
}

/// The evidence a summary is written from: the deterministic report, without
/// any earlier generated summary.
pub fn summary_input(report: &Value) -> String {
    let mut plain = report.clone();
    if let Some(object) = plain.as_object_mut() {
        object.remove("generated_summary");
    }
    render_markdown(&plain).chars().take(MAX_INPUT_CHARS).collect()
}

/// Asks `run` for a summary of the report and records it, labelled generated.
/// Returns whether a summary was added; on a failed or empty answer the report
/// is untouched.
pub fn apply_model_summary(report: &mut Value, model: &str, run: &dyn Fn(&str) -> Result<String, String>) -> bool {
    let Ok(answer) = run(&summary_input(report)) else { return false };
    let words: Vec<&str> = answer.split_whitespace().collect();
    let line: String = words.join(" ").chars().filter(|c| !c.is_control()).take(MAX_SUMMARY_CHARS).collect();
    let line = line.trim().to_string();
    if line.is_empty() {
        return false;
    }
    report["generated_summary"] = json!({"generated": true, "model": model, "text": line});
    true
}
