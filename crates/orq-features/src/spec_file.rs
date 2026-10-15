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

const CHOICE_MAX_DEPTH: usize = 4;
const CHOICE_MAX_FILES: usize = 500;
/// Directory entries examined in one listing; bounds the scan of a huge tree.
const CHOICE_MAX_VISITS: usize = 10_000;

/// Markdown files anywhere in the repository (a few levels deep) that may be
/// offered as a spec, as paths relative to `repo_root`: `SPEC.md` first, the
/// rest sorted. Hidden directories, `target` and `node_modules` are skipped,
/// directory symlinks are never followed and files that resolve outside the
/// repository are not offered. The walk is in name order and bounded (at
/// most `CHOICE_MAX_VISITS` entries per directory and overall), so a very
/// large tree yields a partial listing rather than a long stall.
pub fn spec_file_choices(repo_root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    if resolve_spec_file(repo_root, "SPEC.md").is_ok() {
        found.push("SPEC.md".to_string());
    }
    let mut visits = 0;
    collect_choices(repo_root, repo_root, 0, &mut visits, &mut found);
    found.sort_by(|a, b| ((a != "SPEC.md"), a).cmp(&((b != "SPEC.md"), b)));
    found
}

fn collect_choices(root: &Path, dir: &Path, depth: usize, visits: &mut usize, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // At most the visit budget is read from one directory, so a directory
    // of millions of entries is neither listed nor sorted in full.
    let mut entries: Vec<_> = entries.filter_map(Result::ok).take(CHOICE_MAX_VISITS).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if found.len() >= CHOICE_MAX_FILES || *visits >= CHOICE_MAX_VISITS {
            return;
        }
        *visits += 1;
        let Ok(kind) = entry.file_type() else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        let path = entry.path();
        if kind.is_dir() {
            let skipped = name.starts_with('.') || name == "target" || name == "node_modules";
            if !skipped && depth + 1 < CHOICE_MAX_DEPTH {
                collect_choices(root, &path, depth + 1, visits, found);
            }
        } else if is_markdown_name(&name) {
            if let Ok(relative) = path.strip_prefix(root) {
                if let Some(text) = relative.to_str() {
                    if text != "SPEC.md" && resolve_spec_file(root, text).is_ok() {
                        found.push(text.to_string());
                    }
                }
            }
        }
    }
}

fn is_markdown_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}
