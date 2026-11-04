//! Source branch and worktree directory for a run, following the Bash
//! orchestrator: branch `<prefix><feature>`, worktree
//! `.claude/worktrees/<branch>` under the main checkout.

use std::path::PathBuf;

/// Directory, relative to the main checkout, that holds run worktrees.
pub const WORKTREES_DIR: &str = ".claude/worktrees";

/// Bash orchestrator's default branch prefix.
pub const DEFAULT_BRANCH_PREFIX: &str = "fix--";

/// Highest numbered suffix tried before giving up.
const MAX_SUFFIX: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunNaming {
    pub branch: String,
    /// Relative to the main checkout's root.
    pub worktree_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamingError {
    /// Not a slug (letters, digits, `.`, `_`, `-`, starting alphanumeric) or
    /// not usable as a ref name component.
    InvalidFeatureName(String),
    /// Would yield an unsafe ref name or escape the worktrees directory.
    InvalidPrefix(String),
    /// Every candidate up to the suffix limit is taken; carries the base name.
    NoFreeName(String),
}

impl std::fmt::Display for NamingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NamingError::InvalidFeatureName(name) => write!(
                f,
                "feature name must be a slug (letters, digits, . _ -): {name:?}"
            ),
            NamingError::InvalidPrefix(prefix) => write!(f, "invalid branch prefix: {prefix:?}"),
            NamingError::NoFreeName(base) => {
                write!(f, "no free branch or worktree name for {base:?} (tried up to -{MAX_SUFFIX})")
            }
        }
    }
}

impl std::error::Error for NamingError {}

fn is_slug_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// A ref path component git and the filesystem both accept.
fn valid_component(component: &str) -> bool {
    !component.is_empty()
        && !component.starts_with('.')
        && !component.ends_with(".lock")
        && !component.contains("..")
        && component.chars().all(is_slug_char)
}

fn valid_feature_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && !name.ends_with('.')
        && valid_component(name)
}

/// Whether `prefix` can start a run branch name.
pub fn is_valid_prefix(prefix: &str) -> bool {
    valid_prefix(prefix)
}

fn valid_prefix(prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if prefix.starts_with('-') {
        return false;
    }
    // A trailing `/` leaves an empty last component: the feature fills it.
    let body = prefix.strip_suffix('/').unwrap_or(prefix);
    body.split('/').all(valid_component)
}

/// The first free branch/worktree name for `feature`: `<prefix><feature>`,
/// then `-2`, `-3`, ... `taken` reports whether a candidate's branch or
/// worktree path already exists.
pub fn run_naming(
    prefix: &str,
    feature: &str,
    taken: impl Fn(&RunNaming) -> bool,
) -> Result<RunNaming, NamingError> {
    if !valid_feature_name(feature) {
        return Err(NamingError::InvalidFeatureName(feature.to_string()));
    }
    if !valid_prefix(prefix) {
        return Err(NamingError::InvalidPrefix(prefix.to_string()));
    }
    let base = format!("{prefix}{feature}");
    // Git refuses `HEAD` as a branch name; catch it before any branch or
    // worktree is touched, whichever of prefix and feature spells it.
    if base == "HEAD" {
        return Err(NamingError::InvalidFeatureName(feature.to_string()));
    }
    let candidate = |branch: String| RunNaming {
        worktree_dir: PathBuf::from(WORKTREES_DIR).join(&branch),
        branch,
    };
    std::iter::once(base.clone())
        .chain((2..=MAX_SUFFIX).map(|n| format!("{base}-{n}")))
        .map(candidate)
        .find(|naming| !taken(naming))
        .ok_or(NamingError::NoFreeName(base))
}
