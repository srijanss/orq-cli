use toml_edit::{value, Item, Table};

use crate::Settings;

/// The two roles Settings can name a harness and profile for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Implementer,
    Reviewer,
}

impl AgentRole {
    fn table(self) -> &'static str {
        match self {
            AgentRole::Implementer => "implementer",
            AgentRole::Reviewer => "reviewer",
        }
    }
}

/// A harness (`claude`, `pi`, ...) and the profile name run on it. Names
/// only: Settings never holds credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    pub harness: String,
    pub profile: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// A required value is missing or empty.
    Blank { role: AgentRole, field: &'static str },
    /// A value is not a single-line string, or the entry is the wrong shape.
    Invalid { role: AgentRole, field: &'static str },
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (what, role, field) = match self {
            ProfileError::Blank { role, field } => ("is empty", role, field),
            ProfileError::Invalid { role, field } => ("is not a single-line string", role, field),
        };
        write!(f, "{} {field} {what}", role.table())
    }
}

impl std::error::Error for ProfileError {}

fn clean(role: AgentRole, field: &'static str, text: &str) -> Result<String, ProfileError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ProfileError::Blank { role, field });
    }
    if text.chars().any(char::is_control) {
        return Err(ProfileError::Invalid { role, field });
    }
    Ok(text.to_string())
}

impl Settings {
    /// The role's configured harness and profile; `None` when the role has no
    /// entry at all. A half-filled or wrongly typed entry is an error, never
    /// a silent default.
    pub fn agent(&self, role: AgentRole) -> Result<Option<AgentProfile>, ProfileError> {
        let Some(item) = self.document().get(role.table()) else {
            return Ok(None);
        };
        let table = item.as_table().ok_or(ProfileError::Invalid { role, field: "harness" })?;
        let read = |field: &'static str| -> Result<String, ProfileError> {
            match table.get(field) {
                None => Err(ProfileError::Blank { role, field }),
                Some(item) => {
                    let text = item.as_str().ok_or(ProfileError::Invalid { role, field })?;
                    clean(role, field, text)
                }
            }
        };
        Ok(Some(AgentProfile { harness: read("harness")?, profile: read("profile")? }))
    }

    /// Feature generation runs on the implementer's harness and profile.
    pub fn generation_agent(&self) -> Result<Option<AgentProfile>, ProfileError> {
        self.agent(AgentRole::Implementer)
    }

    /// Sets a role's harness and profile (trimmed). Nothing changes when
    /// either value is rejected.
    pub fn set_agent(&mut self, role: AgentRole, harness: &str, profile: &str) -> Result<(), ProfileError> {
        let harness = clean(role, "harness", harness)?;
        let profile = clean(role, "profile", profile)?;
        let item = self.document_mut().entry(role.table()).or_insert_with(|| Item::Table(Table::new()));
        let table = item.as_table_mut().ok_or(ProfileError::Invalid { role, field: "harness" })?;
        table.insert("harness", value(harness));
        table.insert("profile", value(profile));
        Ok(())
    }
}
