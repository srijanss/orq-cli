use std::path::{Path, PathBuf};

/// Why a launch working directory could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchCwdError {
    /// The chosen source supplied an empty path.
    Empty { source: CwdSource },
    /// The chosen source named a directory that does not exist.
    NotFound { source: CwdSource, path: PathBuf },
}

/// Where a launch working directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdSource {
    Explicit,
    HerdrActivePaneCwd,
    ProcessCwd,
}

/// Resolve the directory `orq` should treat as the launch context.
///
/// Precedence: an explicit `--cwd`, then `HERDR_ACTIVE_PANE_CWD`, then the
/// process working directory.
pub fn resolve_launch_cwd(
    explicit: Option<&Path>,
    herdr_active_pane_cwd: Option<&str>,
    process_cwd: &Path,
) -> Result<PathBuf, LaunchCwdError> {
    // The first source that is present is authoritative: if it is unusable
    // that is an error, never a silent fallback to another directory.
    if let Some(path) = explicit {
        return validate(path, CwdSource::Explicit);
    }
    if let Some(path) = herdr_active_pane_cwd {
        return validate(Path::new(path), CwdSource::HerdrActivePaneCwd);
    }
    validate(process_cwd, CwdSource::ProcessCwd)
}

fn validate(path: &Path, source: CwdSource) -> Result<PathBuf, LaunchCwdError> {
    if path.as_os_str().is_empty() {
        return Err(LaunchCwdError::Empty { source });
    }
    if !path.is_dir() {
        return Err(LaunchCwdError::NotFound {
            source,
            path: path.to_path_buf(),
        });
    }
    Ok(path.to_path_buf())
}
