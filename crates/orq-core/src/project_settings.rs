//! A project's own settings, kept in orq.db on this machine and keyed by the path of the project's
//! main checkout: the agents a run uses when it names none, and what a run does when it finishes.
//! A role left unset falls back to `.tdd-config.json`, `[roles]` and the machine defaults.

use std::io::{Error, ErrorKind, Result};

/// What a finished run does with its branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delivery {
    /// Push, open the PR/MR and wait for CI.
    #[default]
    Review,
    /// Push the branch and stop.
    Push,
    /// Fast-forward the base branch locally; nothing is pushed.
    MergeLocal,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Delivery::Review => "review",
            Delivery::Push => "push",
            Delivery::MergeLocal => "merge_local",
        }
    }

    pub fn parse(text: &str) -> Option<Delivery> {
        match text {
            "review" => Some(Delivery::Review),
            "push" => Some(Delivery::Push),
            "merge_local" => Some(Delivery::MergeLocal),
            _ => None,
        }
    }
}

/// The agent a project uses for one role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAgent {
    pub harness: String,
    pub model: String,
    /// The run's thinking level; unset leaves the harness's own default.
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectSettings {
    pub implementer: Option<ProjectAgent>,
    pub reviewer: Option<ProjectAgent>,
    pub delivery: Delivery,
}

/// A project's settings with the revision a later save must name.
/// Revision 0 means the project has no record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProjectSettings {
    pub settings: ProjectSettings,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved { revision: u64 },
    /// The project was saved since the revision the save was based on; nothing was written.
    Conflict,
}

/// Every value is a single non-empty line with no surrounding blanks.
pub(crate) fn check(settings: &ProjectSettings) -> Result<()> {
    for (role, agent) in [("implementer", &settings.implementer), ("reviewer", &settings.reviewer)] {
        let Some(agent) = agent else { continue };
        check_value(role, "harness", &agent.harness)?;
        check_value(role, "model", &agent.model)?;
        if let Some(effort) = &agent.effort {
            check_value(role, "effort", effort)?;
        }
    }
    Ok(())
}

fn check_value(role: &str, field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{role} {field} must be a single non-blank line without surrounding spaces, got {value:?}")));
    }
    Ok(())
}
