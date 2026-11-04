//! Which models a run uses. The implementer comes from the `--implementer`
//! flag, then `[roles].implementer`, then a built-in; the reviewer from the
//! `--reviewer` flag, then `.tdd-config.json` `reviewers.default`, then
//! `[roles].reviewer`, then a built-in. `reviewers.allowedModels` overrides
//! every source, and the implementer must run on the claude harness.

use crate::model_profiles::{ModelProfile, ModelProfiles};
use crate::profiles::AgentRole;
use crate::reviewer_config::TddReviewerConfig;

const BUILT_IN_IMPLEMENTER: (&str, &str) = ("claude", "opus");
const BUILT_IN_REVIEWER: (&str, &str) = ("pi", "openai/gpt-6-astra");

/// Profile names picked on the command line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunSelection<'a> {
    pub implementer: Option<&'a str>,
    pub reviewer: Option<&'a str>,
}

/// One role's resolved harness, model and thinking level, for the run record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub harness: String,
    pub model: String,
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModels {
    pub implementer: ResolvedModel,
    pub reviewer: ResolvedModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunModelsError {
    UnknownProfile { role: &'static str, name: String },
    /// The implementer's profile names a harness no adapter exists for.
    ImplementerHarnessUnknown { harness: String, model: String },
    ReviewerNotAllowed { model: String, allowed: Vec<String> },
}

impl std::fmt::Display for RunModelsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunModelsError::UnknownProfile { role, name } => {
                write!(f, "the {role} profile {name:?} is not defined in the model profiles")
            }
            RunModelsError::ImplementerHarnessUnknown { harness, model } => write!(
                f,
                "the implementer model {model} uses the unknown harness {harness:?}; use claude or pi"
            ),
            RunModelsError::ReviewerNotAllowed { model, allowed } => write!(
                f,
                "reviewer model {model} is not in reviewers.allowedModels ({})",
                allowed.join(", ")
            ),
        }
    }
}

impl std::error::Error for RunModelsError {}

fn from_profile(profile: &ModelProfile) -> ResolvedModel {
    ResolvedModel {
        harness: profile.harness.clone(),
        model: profile.model.clone(),
        thinking: profile.thinking.clone(),
    }
}

fn named(profiles: &ModelProfiles, role: &'static str, name: &str) -> Result<ResolvedModel, RunModelsError> {
    profiles
        .get(name)
        .map(from_profile)
        .ok_or_else(|| RunModelsError::UnknownProfile { role, name: name.to_string() })
}

fn built_in((harness, model): (&str, &str)) -> ResolvedModel {
    ResolvedModel { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

/// Resolve both roles for a run, or say which policy refused them.
pub fn resolve_run_models(
    profiles: &ModelProfiles,
    reviewer_config: &TddReviewerConfig,
    selection: &RunSelection<'_>,
) -> Result<ResolvedModels, RunModelsError> {
    let implementer = match selection.implementer {
        Some(name) => named(profiles, "implementer", name)?,
        None => match profiles.role_profile_name(AgentRole::Implementer) {
            Some(name) => named(profiles, "implementer", name)?,
            None => built_in(BUILT_IN_IMPLEMENTER),
        },
    };
    if orq_agents::adapter_for(&implementer.harness).is_err() {
        return Err(RunModelsError::ImplementerHarnessUnknown {
            harness: implementer.harness,
            model: implementer.model,
        });
    }

    let reviewer = if let Some(name) = selection.reviewer {
        named(profiles, "reviewer", name)?
    } else if let Some(default) = &reviewer_config.default_reviewer {
        ResolvedModel { harness: default.harness.clone(), model: default.model.clone(), thinking: None }
    } else if let Some(name) = profiles.role_profile_name(AgentRole::Reviewer) {
        named(profiles, "reviewer", name)?
    } else {
        built_in(BUILT_IN_REVIEWER)
    };
    if let Some(allowed) = &reviewer_config.allowed_models {
        if !allowed.contains(&reviewer.model) {
            return Err(RunModelsError::ReviewerNotAllowed { model: reviewer.model, allowed: allowed.clone() });
        }
    }

    Ok(ResolvedModels { implementer, reviewer })
}
