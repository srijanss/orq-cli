//! Settings changes made from the web: each one is written only when the version of config.toml it was
//! read at is still the file's, so two people editing at once never silently overwrite each other.

use crate::storage::StorageRoots;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Why a settings change was not written.
#[derive(Debug)]
pub enum SettingsWriteError {
    /// The file changed after the version presented was read; nothing was written.
    Conflict,
    /// The file could not be read, parsed or written.
    Failed(String),
}

impl SettingsWriteError {
    pub fn status(&self) -> u16 {
        match self {
            SettingsWriteError::Conflict => 409,
            SettingsWriteError::Failed(_) => 500,
        }
    }
}

impl std::fmt::Display for SettingsWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsWriteError::Conflict => f.write_str("Settings changed since you opened them"),
            SettingsWriteError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SettingsWriteError {}

/// The version of config.toml's text: a hex sha256, where an absent file is the empty text.
pub fn version_of(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The text of config.toml, where an absent file is empty.
pub fn read_config(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        other => other,
    }
}

/// Applies `edit` to config.toml and writes it, only while the file is still at `version`.
/// Comments and layout outside the edited keys are kept.
pub fn write_if_version(roots: &StorageRoots, version: &str, edit: impl FnOnce(&mut toml_edit::DocumentMut)) -> Result<(), SettingsWriteError> {
    let failed = |why: &str| SettingsWriteError::Failed(why.to_string());
    // Held until the write lands, so no other writer slips in between the check and the rename.
    let dir = std::fs::File::open(&roots.config).map_err(|_| failed("the settings could not be written"))?;
    dir.lock().map_err(|_| failed("the settings could not be written"))?;
    let text = read_config(&roots.config_file()).map_err(|_| failed("the settings could not be read"))?;
    if version_of(&text) != version {
        return Err(SettingsWriteError::Conflict);
    }
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|_| failed("the settings could not be read"))?;
    edit(&mut doc);
    crate::storage::write_atomic(&roots.config, Path::new("config.toml"), doc.to_string().as_bytes()).map_err(|_| failed("the settings could not be written"))
}
