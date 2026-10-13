use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
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
    let root = rev_parse(path, "--show-toplevel")?;
    let git_common_dir = rev_parse(path, "--git-common-dir")?;
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    Ok(Project {
        name,
        root,
        git_common_dir,
    })
}

/// Run `git rev-parse --path-format=absolute <flag>` and return its single
/// output path byte-for-byte (paths may hold newlines or non-UTF-8 bytes, so
/// only the one trailing newline git adds is removed).
fn rev_parse(path: &Path, flag: &str) -> Result<PathBuf, ResolveError> {
    let fail = |message: String| ResolveError::Git {
        path: path.to_path_buf(),
        message,
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", flag])
        .output()
        .map_err(|err| fail(err.to_string()))?;
    if !output.status.success() {
        return Err(fail(String::from_utf8_lossy(&output.stderr).trim().to_string()));
    }

    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(fail(format!("git rev-parse {flag} produced no output")));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}
