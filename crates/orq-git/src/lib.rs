use std::path::{Path, PathBuf};
use std::process::Command;

/// A Git repository resolved from some path inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub root: PathBuf,
    pub git_common_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// `git` ran but did not produce a repository for `path`.
    Git { path: PathBuf, message: String },
}

/// Resolve the repository containing `path`. Read-only: only `git rev-parse`
/// is invoked, with a fixed argv.
pub fn resolve_project(path: &Path) -> Result<Project, ResolveError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ])
        .output()
        .map_err(|err| ResolveError::Git {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
    if !output.status.success() {
        return Err(ResolveError::Git {
            path: path.to_path_buf(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let (Some(root), Some(common)) = (lines.next(), lines.next()) else {
        return Err(ResolveError::Git {
            path: path.to_path_buf(),
            message: format!("unexpected git output: {stdout:?}"),
        });
    };
    let root = PathBuf::from(root);
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    Ok(Project {
        name,
        root,
        git_common_dir: PathBuf::from(common),
    })
}
