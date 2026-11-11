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

/// The version of config.toml's text, where an absent file is the empty text: a hex sha256 keyed with a
/// secret this process drew at random, so a reader can tell the file changed but cannot test guesses
/// about the fields the settings page hides. A restart draws a new key, so a page opened before it is
/// asked to reload, which only costs that one change.
pub fn version_of(text: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(version_key());
    hash.update(text.as_bytes());
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

fn version_key() -> &'static [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        use std::io::Read;
        let mut key = [0u8; 32];
        if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut key)).is_err() {
            // The standard library's hasher seeds are random per process too.
            use std::hash::{BuildHasher, Hasher};
            for (i, chunk) in key.chunks_mut(8).enumerate() {
                let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
                hasher.write_usize(i);
                chunk.copy_from_slice(&hasher.finish().to_le_bytes());
            }
        }
        key
    })
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
