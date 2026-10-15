use std::path::{Path, PathBuf};

use crate::AgentProfile;

/// What `.tdd-config.json` says about reviewers. Read-only: Orq never writes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TddReviewerConfig {
    /// `reviewers.default`: `reviewer` is the harness, `model` the model.
    pub default_reviewer: Option<AgentProfile>,
    /// `reviewers.allowedModels`; `None` when the list is absent.
    pub allowed_models: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum ReviewerConfigError {
    Malformed { path: PathBuf, message: String },
    Io { path: PathBuf, source: std::io::Error },
}

impl std::fmt::Display for ReviewerConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReviewerConfigError::Malformed { path, message } => {
                write!(f, "{} is not usable: {message}", path.display())
            }
            ReviewerConfigError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for ReviewerConfigError {}

/// Reads the reviewer section of `.tdd-config.json`; a missing file says nothing.
pub fn load_reviewer_config(path: &Path) -> Result<TddReviewerConfig, ReviewerConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TddReviewerConfig::default());
        }
        Err(source) => return Err(ReviewerConfigError::Io { path: path.to_path_buf(), source }),
    };
    let malformed = |message: String| ReviewerConfigError::Malformed { path: path.to_path_buf(), message };
    let root: serde_json::Value = serde_json::from_str(&text).map_err(|e| malformed(e.to_string()))?;
    let Some(root) = root.as_object() else {
        return Err(malformed("the file must hold a JSON object".into()));
    };
    let Some(reviewers) = root.get("reviewers") else {
        return Ok(TddReviewerConfig::default());
    };
    let Some(reviewers) = reviewers.as_object() else {
        return Err(malformed("reviewers must be an object".into()));
    };
    // A name must be a non-blank single-line string.
    let name = |value: Option<&serde_json::Value>, what: &str| -> Result<String, ReviewerConfigError> {
        match value.and_then(serde_json::Value::as_str).map(str::trim) {
            Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => Ok(text.to_string()),
            _ => Err(malformed(format!("{what} must be a non-blank single-line string"))),
        }
    };
    let default_reviewer = match reviewers.get("default") {
        None => None,
        Some(serde_json::Value::Object(map)) => Some(AgentProfile {
            harness: name(map.get("reviewer"), "reviewers.default.reviewer")?,
            model: name(map.get("model"), "reviewers.default.model")?,
        }),
        Some(_) => return Err(malformed("reviewers.default must be an object".into())),
    };
    let allowed_models = match reviewers.get("allowedModels") {
        None => None,
        Some(serde_json::Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| name(Some(item), "reviewers.allowedModels entries"))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(_) => return Err(malformed("reviewers.allowedModels must be an array".into())),
    };
    Ok(TddReviewerConfig { default_reviewer, allowed_models })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveReviewer {
    Config(AgentProfile),
    Settings(AgentProfile),
    Unset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerResolution {
    pub effective: EffectiveReviewer,
    /// The config default replaces a different reviewer from the settings.
    pub overrides_settings: bool,
    /// Why the effective reviewer is not allowed, if it is not.
    pub invalid: Option<String>,
}

/// `.tdd-config.json` stays authoritative: its default wins over the settings.
pub fn resolve_reviewer(settings: Option<&AgentProfile>, config: &TddReviewerConfig) -> ReviewerResolution {
    let (effective, overrides_settings) = match (&config.default_reviewer, settings) {
        (Some(default), settings) => {
            (EffectiveReviewer::Config(default.clone()), settings.is_some_and(|s| s != default))
        }
        (None, Some(settings)) => (EffectiveReviewer::Settings(settings.clone()), false),
        (None, None) => (EffectiveReviewer::Unset, false),
    };
    let invalid = match (&effective, &config.allowed_models) {
        (EffectiveReviewer::Config(p) | EffectiveReviewer::Settings(p), Some(allowed))
            if !allowed.contains(&p.model) =>
        {
            Some(format!("model \"{}\" is not in .tdd-config.json reviewers.allowedModels", p.model))
        }
        _ => None,
    };
    ReviewerResolution { effective, overrides_settings, invalid }
}
