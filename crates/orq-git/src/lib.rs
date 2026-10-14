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
    /// `path` is not inside a Git repository (or does not exist).
    NotARepository { path: PathBuf },
    /// `git` could not be run, or failed for a reason other than the path not
    /// being a repository.
    GitUnavailable { path: PathBuf, message: String },
}

/// Resolve the repository containing `path`. Read-only: only `git rev-parse`
/// is invoked, with a fixed argv.
pub fn resolve_project(path: &Path) -> Result<Project, ResolveError> {
    // Only a path that is genuinely absent (or not a directory) is "not a
    // repository"; permission and I/O failures are operational errors.
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(ResolveError::NotARepository {
                path: path.to_path_buf(),
            })
        }
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Err(ResolveError::NotARepository {
                path: path.to_path_buf(),
            })
        }
        Err(err) => {
            return Err(ResolveError::GitUnavailable {
                path: path.to_path_buf(),
                message: err.to_string(),
            })
        }
    }
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
/// a bare `repo.git` -> `repo`. Any other bare directory keeps its full name.
fn project_name(git_common_dir: &Path) -> String {
    let is_dot_git = git_common_dir.file_name().is_some_and(|name| name == ".git");
    let basename = if is_dot_git {
        git_common_dir.parent().and_then(Path::file_name)
    } else {
        git_common_dir.file_name()
    };
    let name = basename
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if is_dot_git {
        return name;
    }
    match name.strip_suffix(".git") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => name,
    }
}

/// Environment variables that make Git use a repository other than the one
/// found from the working directory.
const REPOSITORY_SELECTION_VARS: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Run `git rev-parse --path-format=absolute <flag>` and return its single
/// output path byte-for-byte (paths may hold newlines or non-UTF-8 bytes, so
/// only the one trailing newline git adds is removed).
fn rev_parse(path: &Path, flag: &str) -> Result<PathBuf, ResolveError> {
    let unavailable = |message: String| ResolveError::GitUnavailable {
        path: path.to_path_buf(),
        message,
    };
    let mut command = Command::new("git");
    // Discovery must follow `path`, not repository-selection variables
    // inherited from the caller (e.g. when launched from a Git hook).
    for var in REPOSITORY_SELECTION_VARS {
        command.env_remove(var);
    }
    let output = command
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", flag])
        // Stable English diagnostics, so "not a git repository" is recognisable.
        .env("LC_ALL", "C")
        .output()
        .map_err(|err| unavailable(err.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.to_lowercase().contains("not a git repository") {
            return Err(ResolveError::NotARepository {
                path: path.to_path_buf(),
            });
        }
        return Err(unavailable(stderr.trim().to_string()));
    }

    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(unavailable(format!("git rev-parse {flag} produced no output")));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}
