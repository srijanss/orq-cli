use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum SpecFileError {
    NotFound,
    /// The path resolves (after symlinks) to somewhere outside the repository.
    OutsideRepository,
    NotAFile,
    NotMarkdown,
    Unreadable,
}

impl std::fmt::Display for SpecFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SpecFileError::NotFound => "spec file not found",
            SpecFileError::OutsideRepository => "spec file is outside the repository",
            SpecFileError::NotAFile => "spec path is not a file",
            SpecFileError::NotMarkdown => "spec file must be a Markdown file (.md)",
            SpecFileError::Unreadable => "spec file could not be read",
        })
    }
}

impl std::error::Error for SpecFileError {}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// Resolves a user-chosen spec path against the repository root. The result is
/// canonical and guaranteed to be a Markdown file inside the repository after
/// symlinks and `..` are resolved; anything else is refused.
pub fn resolve_spec_file(repo_root: &Path, input: &str) -> Result<PathBuf, SpecFileError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SpecFileError::NotFound);
    }
    let root = repo_root.canonicalize().map_err(|_| SpecFileError::Unreadable)?;
    let resolved = root.join(input).canonicalize().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => SpecFileError::NotFound,
        _ => SpecFileError::Unreadable,
    })?;
    if !resolved.starts_with(&root) {
        return Err(SpecFileError::OutsideRepository);
    }
    let metadata = std::fs::metadata(&resolved).map_err(|_| SpecFileError::Unreadable)?;
    if !metadata.is_file() {
        return Err(SpecFileError::NotAFile);
    }
    if !is_markdown(&resolved) {
        return Err(SpecFileError::NotMarkdown);
    }
    Ok(resolved)
}

/// Markdown files at the repository root that may be offered as a spec,
/// `SPEC.md` first when present and the rest sorted by name. Files whose
/// symlinks leave the repository are not offered.
pub fn spec_file_candidates(repo_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(repo_root) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            resolve_spec_file(repo_root, &name).ok()
        })
        .collect();
    found.sort_by(|a, b| {
        let key = |p: &PathBuf| (p.file_name().is_none_or(|n| n != "SPEC.md"), p.file_name().map(|n| n.to_owned()));
        key(a).cmp(&key(b))
    });
    found
}
