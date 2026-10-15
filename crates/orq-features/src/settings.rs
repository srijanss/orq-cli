use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use toml_edit::{value, DocumentMut, Item, Table};

use crate::write_ledger_atomic;

/// Where the user-level settings live: `$XDG_CONFIG_HOME/orq/config.toml`,
/// else `$HOME/.config/orq/config.toml`. As the XDG spec requires, an empty
/// or relative `XDG_CONFIG_HOME` is ignored, and a relative `HOME` is no home.
pub fn settings_path(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let base = match absolute(xdg_config_home) {
        Some(xdg) => xdg.to_path_buf(),
        None => absolute(home)?.join(".config"),
    };
    Some(base.join("orq").join("config.toml"))
}

fn absolute(value: Option<&OsStr>) -> Option<&Path> {
    value.map(Path::new).filter(|path| path.is_absolute())
}

#[derive(Debug)]
pub enum SettingsError {
    /// The file exists but is not valid TOML; it is never overwritten.
    Malformed { path: PathBuf, message: String },
    /// The file changed (or appeared) since it was loaded; nothing was written.
    Conflict { path: PathBuf },
    Io { path: PathBuf, source: std::io::Error },
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Malformed { path, message } => {
                write!(f, "{} is not valid TOML and was left untouched: {message}", path.display())
            }
            SettingsError::Conflict { path } => {
                write!(f, "{} changed while it was being edited; nothing was written, try again", path.display())
            }
            SettingsError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SettingsError {}

/// A loaded settings file. Edits keep unknown keys, tables and comments; the
/// file is created only by the first successful `save`.
#[derive(Debug)]
pub struct Settings {
    path: PathBuf,
    doc: DocumentMut,
    /// The bytes the document was parsed from; `None` when the file was absent.
    loaded: Option<String>,
}

impl Settings {
    pub fn load(path: &Path) -> Result<Settings, SettingsError> {
        let io = |source| SettingsError::Io { path: path.to_path_buf(), source };
        let loaded = match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io(error)),
        };
        let doc = match &loaded {
            Some(text) => text
                .parse::<DocumentMut>()
                .map_err(|error| SettingsError::Malformed { path: path.to_path_buf(), message: error.to_string() })?,
            None => DocumentMut::new(),
        };
        Ok(Settings { path: path.to_path_buf(), doc, loaded })
    }

    pub(crate) fn document(&self) -> &DocumentMut {
        &self.doc
    }

    pub(crate) fn document_mut(&mut self) -> &mut DocumentMut {
        &mut self.doc
    }

    /// A string value at `[section] key`; `None` when absent or not a string.
    pub fn get(&self, section: &str, key: &str) -> Option<String> {
        self.doc.get(section)?.as_table()?.get(key)?.as_str().map(str::to_string)
    }

    pub fn set(&mut self, section: &str, key: &str, text: &str) {
        let item = self.doc.entry(section).or_insert_with(|| Item::Table(Table::new()));
        if let Some(table) = item.as_table_mut() {
            table.insert(key, value(text));
        }
    }

    /// Writes the document atomically, creating the directory on first save.
    /// Refuses when the file differs from what was loaded.
    pub fn save(&mut self) -> Result<(), SettingsError> {
        let io = |source| SettingsError::Io { path: self.path.clone(), source };
        // Serialise the compare and the write across processes by locking the
        // directory itself (no lock file); released when `_guard` drops.
        // A bare relative path has an empty parent: that is the current directory.
        let parent = self.path.parent().map(|p| if p.as_os_str().is_empty() { Path::new(".") } else { p });
        if let Some(parent) = parent {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let _guard = match parent {
            Some(parent) => {
                let dir = std::fs::File::open(parent).map_err(io)?;
                dir.lock().map_err(io)?;
                Some(dir)
            }
            None => None,
        };
        let current = match std::fs::read_to_string(&self.path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io(error)),
        };
        if current != self.loaded {
            return Err(SettingsError::Conflict { path: self.path.clone() });
        }
        let text = self.doc.to_string();
        write_ledger_atomic(&self.path, &text).map_err(io)?;
        self.loaded = Some(text);
        Ok(())
    }
}
