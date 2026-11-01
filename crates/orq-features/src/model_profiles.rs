//! Named model profiles and per-role defaults: `[models.<name>]` and
//! `[roles]` in the user-level `config.toml`, overridden by an optional
//! per-repo `.orq.toml`. Read-only: Orq never rewrites either file here.
//!
//! The MVP 1.1 `[implementer]` / `[reviewer]` tables in `config.toml` are a
//! separate mechanism and are ignored by this reader.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, TableLike};

use crate::AgentRole;

/// A named harness + model (+ optional thinking level and provider).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProfile {
    /// The key after `models.`; a label for pickers and `--implementer` /
    /// `--reviewer`, not the model ID the harness receives.
    pub name: String,
    pub harness: String,
    pub model: String,
    pub thinking: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldProblem {
    Missing,
    Blank,
    NotASingleLineString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProfilesError {
    /// The file exists but could not be read (a missing file is not an error).
    Io { path: PathBuf, message: String },
    Malformed { path: PathBuf, message: String },
    /// A `[models]` entry (or `models` itself) is not a table.
    NotAProfile { path: PathBuf, profile: String },
    InvalidField {
        path: PathBuf,
        profile: String,
        field: &'static str,
        problem: FieldProblem,
    },
    /// A profile name that `[roles]` (which trims its values) could never
    /// reference: empty, padded with whitespace, or containing a control character.
    InvalidProfileName { path: PathBuf, profile: String },
    /// `roles` is not a table.
    InvalidRolesSection { path: PathBuf },
    /// A `[roles]` value is not a non-empty single-line string.
    InvalidRole { path: PathBuf, role: AgentRole },
    /// A role names a profile no file defines.
    UnknownRoleProfile { role: AgentRole, profile: String },
}

impl std::fmt::Display for ModelProfilesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelProfilesError::Io { path, message } => write!(f, "{}: {message}", path.display()),
            ModelProfilesError::Malformed { path, message } => {
                write!(f, "{} is not valid TOML and was left untouched: {message}", path.display())
            }
            ModelProfilesError::NotAProfile { path, profile } => {
                write!(f, "{}: models entry {profile:?} is not a table", path.display())
            }
            ModelProfilesError::InvalidField { path, profile, field, problem } => {
                let what = match problem {
                    FieldProblem::Missing => "is missing",
                    FieldProblem::Blank => "is empty",
                    FieldProblem::NotASingleLineString => "is not a single-line string",
                };
                write!(f, "{}: models.{profile} {field} {what}", path.display())
            }
            ModelProfilesError::InvalidProfileName { path, profile } => {
                write!(f, "{}: models name {profile:?} is empty, padded or has control characters", path.display())
            }
            ModelProfilesError::InvalidRolesSection { path } => {
                write!(f, "{}: roles is not a table", path.display())
            }
            ModelProfilesError::InvalidRole { path, role } => {
                write!(f, "{}: roles.{} is not a profile name", path.display(), role_key(*role))
            }
            ModelProfilesError::UnknownRoleProfile { role, profile } => {
                write!(f, "roles.{} names unknown profile {profile:?}", role_key(*role))
            }
        }
    }
}

impl std::error::Error for ModelProfilesError {}

fn role_key(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Implementer => "implementer",
        AgentRole::Reviewer => "reviewer",
    }
}

/// The merged profiles and role defaults, profiles in first-seen order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelProfiles {
    profiles: Vec<ModelProfile>,
    implementer: Option<String>,
    reviewer: Option<String>,
}

impl ModelProfiles {
    pub fn profiles(&self) -> &[ModelProfile] {
        &self.profiles
    }

    pub fn get(&self, name: &str) -> Option<&ModelProfile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// The profile name `[roles]` assigns to the role, if any.
    pub fn role_profile_name(&self, role: AgentRole) -> Option<&str> {
        match role {
            AgentRole::Implementer => self.implementer.as_deref(),
            AgentRole::Reviewer => self.reviewer.as_deref(),
        }
    }

    /// The profile `[roles]` assigns to the role, if any.
    pub fn role(&self, role: AgentRole) -> Option<&ModelProfile> {
        self.get(self.role_profile_name(role)?)
    }
}

/// One file's contribution before merging.
struct Layer {
    profiles: Vec<ModelProfile>,
    implementer: Option<String>,
    reviewer: Option<String>,
}

/// Loads `user_config` (`config.toml`) then lays `repo_config` (`.orq.toml`)
/// over it: a profile of the same name is replaced whole, new ones are
/// appended, and each `[roles]` key overrides on its own. Absent files
/// contribute nothing.
pub fn load_model_profiles(user_config: &Path, repo_config: &Path) -> Result<ModelProfiles, ModelProfilesError> {
    let mut merged = ModelProfiles::default();
    for path in [user_config, repo_config] {
        let Some(layer) = read_layer(path)? else { continue };
        for profile in layer.profiles {
            match merged.profiles.iter_mut().find(|p| p.name == profile.name) {
                Some(existing) => *existing = profile,
                None => merged.profiles.push(profile),
            }
        }
        if layer.implementer.is_some() {
            merged.implementer = layer.implementer;
        }
        if layer.reviewer.is_some() {
            merged.reviewer = layer.reviewer;
        }
    }
    for role in [AgentRole::Implementer, AgentRole::Reviewer] {
        if let Some(name) = merged.role_profile_name(role) {
            if merged.get(name).is_none() {
                return Err(ModelProfilesError::UnknownRoleProfile { role, profile: name.to_string() });
            }
        }
    }
    Ok(merged)
}

fn read_layer(path: &Path) -> Result<Option<Layer>, ModelProfilesError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ModelProfilesError::Io { path: path.to_path_buf(), message: error.to_string() })
        }
    };
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|error| ModelProfilesError::Malformed { path: path.to_path_buf(), message: error.to_string() })?;
    let not_a_profile = |profile: &str| ModelProfilesError::NotAProfile {
        path: path.to_path_buf(),
        profile: profile.to_string(),
    };

    let mut profiles = Vec::new();
    if let Some(models) = doc.get("models") {
        let models = models.as_table_like().ok_or_else(|| not_a_profile("models"))?;
        for (name, item) in models.iter() {
            // Role values are trimmed on read, so only a trimmed name is referenceable.
            if name.is_empty() || name.trim() != name || name.chars().any(char::is_control) {
                return Err(ModelProfilesError::InvalidProfileName {
                    path: path.to_path_buf(),
                    profile: name.to_string(),
                });
            }
            let table = item.as_table_like().ok_or_else(|| not_a_profile(name))?;
            profiles.push(read_profile(path, name, table)?);
        }
    }

    let (mut implementer, mut reviewer) = (None, None);
    if let Some(roles) = doc.get("roles") {
        let roles = roles
            .as_table_like()
            .ok_or_else(|| ModelProfilesError::InvalidRolesSection { path: path.to_path_buf() })?;
        implementer = read_role(path, roles, AgentRole::Implementer)?;
        reviewer = read_role(path, roles, AgentRole::Reviewer)?;
    }
    Ok(Some(Layer { profiles, implementer, reviewer }))
}

fn read_role(path: &Path, roles: &dyn TableLike, role: AgentRole) -> Result<Option<String>, ModelProfilesError> {
    let Some(item) = roles.get(role_key(role)) else { return Ok(None) };
    clean(item)
        .ok()
        .flatten()
        .map(Some)
        .ok_or_else(|| ModelProfilesError::InvalidRole { path: path.to_path_buf(), role })
}

fn read_profile(path: &Path, name: &str, table: &dyn TableLike) -> Result<ModelProfile, ModelProfilesError> {
    let field = |field: &'static str, required: bool| -> Result<Option<String>, ModelProfilesError> {
        let problem = |problem| ModelProfilesError::InvalidField {
            path: path.to_path_buf(),
            profile: name.to_string(),
            field,
            problem,
        };
        match table.get(field) {
            None if required => Err(problem(FieldProblem::Missing)),
            None => Ok(None),
            Some(item) => match clean(item) {
                Ok(Some(text)) => Ok(Some(text)),
                Ok(None) => Err(problem(FieldProblem::Blank)),
                Err(()) => Err(problem(FieldProblem::NotASingleLineString)),
            },
        }
    };
    Ok(ModelProfile {
        name: name.to_string(),
        harness: field("harness", true)?.unwrap_or_default(),
        model: field("model", true)?.unwrap_or_default(),
        thinking: field("thinking", false)?,
        provider: field("provider", false)?,
    })
}

/// `Ok(Some(text))` for a trimmed non-empty single-line string, `Ok(None)` for
/// a blank one, `Err` for any other value.
fn clean(item: &Item) -> Result<Option<String>, ()> {
    let text = item.as_str().ok_or(())?;
    if text.chars().any(char::is_control) {
        return Err(());
    }
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_string()))
}
