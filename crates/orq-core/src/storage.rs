//! Where Orq keeps its own data: XDG roots outside any project tree, created
//! owner-only. Nothing here touches project or MCP-owned files.

use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

/// Orq's four roots, each already ending in `orq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRoots {
    pub data: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl StorageRoots {
    /// Resolves the roots through `env`, so callers (and tests) choose the
    /// environment. An XDG value that is empty or relative is ignored, as the
    /// XDG specification requires; without an absolute `HOME` there is no
    /// fallback and resolution fails rather than writing into the working directory.
    pub fn resolve(env: &dyn Fn(&str) -> Option<String>) -> std::io::Result<Self> {
        let home = env("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
        let pick = |var: &str, default: &[&str]| -> std::io::Result<PathBuf> {
            if let Some(base) = env(var).map(PathBuf::from).filter(|base| base.is_absolute()) {
                return Ok(base.join("orq"));
            }
            let home = home.as_ref().ok_or_else(|| Error::new(ErrorKind::NotFound, format!("{var} is unset and HOME is not an absolute path")))?;
            Ok(default.iter().fold(home.clone(), |path, part| path.join(part)).join("orq"))
        };
        Ok(StorageRoots {
            data: pick("XDG_DATA_HOME", &[".local", "share"])?,
            config: pick("XDG_CONFIG_HOME", &[".config"])?,
            state: pick("XDG_STATE_HOME", &[".local", "state"])?,
            cache: pick("XDG_CACHE_HOME", &[".cache"])?,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("orq.db")
    }

    pub fn runs(&self) -> PathBuf {
        self.data.join("runs")
    }

    pub fn logs(&self) -> PathBuf {
        self.state.join("logs")
    }

    /// Creates every directory owner-only (`0700`); safe to call repeatedly.
    pub fn create_all(&self) -> std::io::Result<()> {
        for dir in [&self.data, &self.config, &self.state, &self.cache, &self.runs(), &self.logs()] {
            create_private_dir(dir)?;
        }
        Ok(())
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}
