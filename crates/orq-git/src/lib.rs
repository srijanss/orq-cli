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
    /// True when `root` is a linked worktree rather than the main checkout.
    pub is_linked_worktree: bool,
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
    let git_dir = rev_parse(path, "--git-dir")?;

    Ok(Project {
        name: project_name(&git_common_dir),
        root,
        is_linked_worktree: git_dir != git_common_dir,
        git_common_dir,
    })
}

/// The owning project's name, taken from the common directory so that every
/// worktree of one repository reports the same name: `<repo>/.git` -> `repo`,
/// a bare `repo.git` -> `repo`.
fn project_name(git_common_dir: &Path) -> String {
    let named = if git_common_dir.file_name().is_some_and(|name| name == ".git") {
        git_common_dir.parent().and_then(Path::file_name)
    } else {
        git_common_dir.file_stem()
    };
    named
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
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
