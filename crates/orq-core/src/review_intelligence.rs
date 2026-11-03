//! Evidence-based reading of a review: which changed files deserve attention
//! and why. Every judgement names the path evidence behind it; a file Orq has
//! no rule for stays unclassified rather than being guessed at.

use crate::review_lifecycle::plain_line;
use crate::review_report::ReportFile;

/// Most files classified in one report; the riskiest are kept.
pub const MAX_CLASSIFIED_FILES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Security,
    Schema,
    Dependencies,
    Build,
    Code,
    Tests,
    Docs,
    Unclassified,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Security => "security",
            Category::Schema => "schema",
            Category::Dependencies => "dependencies",
            Category::Build => "build",
            Category::Code => "code",
            Category::Tests => "tests",
            Category::Docs => "docs",
            Category::Unclassified => "unclassified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    High,
    Medium,
    Low,
    Unknown,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskLevel::High => "high",
            RiskLevel::Medium => "medium",
            RiskLevel::Low => "low",
            RiskLevel::Unknown => "unknown",
        }
    }

    /// Attention order: unknown outranks low, since nobody has looked at it.
    fn rank(self) -> u8 {
        match self {
            RiskLevel::High => 3,
            RiskLevel::Medium => 2,
            RiskLevel::Unknown => 1,
            RiskLevel::Low => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRisk {
    pub path: String,
    pub category: Category,
    pub level: RiskLevel,
    /// The part of the path that decided it.
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub files: Vec<FileRisk>,
    /// Files left out because the diff was larger than the bound.
    pub omitted: usize,
}

const SECURITY: &[&str] = &[
    "auth", "authn", "authz", "authentication", "authorization", "security", "login", "logout", "password", "passwords", "credential", "credentials", "secret", "secrets", "permission", "permissions", "crypto", "token", "tokens", "oauth",
];
const SCHEMA: &[&str] = &["migration", "migrations", "migrate", "schema"];
const SCHEMA_EXTENSIONS: &[&str] = &["sql", "prisma"];
const TESTS: &[&str] = &["test", "tests", "spec", "specs"];
const DOCS: &[&str] = &["readme", "changelog", "docs", "doc"];
const DOC_EXTENSIONS: &[&str] = &["md", "rst", "txt", "adoc"];
const MANIFESTS: &[&str] = &[
    "cargo.toml", "cargo.lock", "package.json", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "requirements.txt", "go.mod", "go.sum", "gemfile", "gemfile.lock", "pyproject.toml", "poetry.lock", "pom.xml",
];
const BUILD_FILES: &[&str] = &["dockerfile", "makefile", "jenkinsfile", ".gitlab-ci.yml"];
const BUILD_DIRS: &[&str] = &[".github", ".circleci"];
const CODE_EXTENSIONS: &[&str] = &["rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "rb", "c", "h", "cpp", "cs", "kt", "swift", "php", "sh"];

fn words(part: &str) -> impl Iterator<Item = String> + '_ {
    part.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase)
}

fn classify_path(path: &str) -> (Category, RiskLevel, String) {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let name = parts.last().copied().unwrap_or("").to_lowercase();
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
    let component_hit = |list: &[&str]| parts.iter().flat_map(|p| words(p)).find(|w| list.contains(&w.as_str()));

    if let Some(word) = component_hit(TESTS) {
        return (Category::Tests, RiskLevel::Low, format!("path component '{word}'"));
    }
    if let Some(word) = component_hit(DOCS) {
        return (Category::Docs, RiskLevel::Low, format!("path component '{word}'"));
    }
    if MANIFESTS.contains(&name.as_str()) {
        return (Category::Dependencies, RiskLevel::Medium, format!("file name '{name}'"));
    }
    if DOC_EXTENSIONS.contains(&extension.as_str()) {
        return (Category::Docs, RiskLevel::Low, format!("file extension '.{extension}'"));
    }
    if let Some(word) = component_hit(SECURITY) {
        return (Category::Security, RiskLevel::High, format!("path component '{word}'"));
    }
    if let Some(word) = component_hit(SCHEMA) {
        return (Category::Schema, RiskLevel::High, format!("path component '{word}'"));
    }
    if SCHEMA_EXTENSIONS.contains(&extension.as_str()) {
        return (Category::Schema, RiskLevel::High, format!("file extension '.{extension}'"));
    }
    if BUILD_FILES.contains(&name.as_str()) || name.starts_with("docker-compose") {
        return (Category::Build, RiskLevel::Medium, format!("file name '{name}'"));
    }
    if let Some(dir) = parts.iter().find(|p| BUILD_DIRS.contains(&p.to_lowercase().as_str())) {
        return (Category::Build, RiskLevel::Medium, format!("path component '{dir}'"));
    }
    if CODE_EXTENSIONS.contains(&extension.as_str()) {
        return (Category::Code, RiskLevel::Medium, format!("file extension '.{extension}'"));
    }
    (Category::Unclassified, RiskLevel::Unknown, "no rule for this path".to_string())
}

/// Classify changed files from their paths alone. A renamed file takes the
/// riskier of its new and old path. The result is ordered riskiest first and
/// holds at most [`MAX_CLASSIFIED_FILES`] files.
pub fn classify_files(files: &[ReportFile]) -> Classification {
    let mut classified: Vec<FileRisk> = files
        .iter()
        .map(|file| {
            let path = plain_line(&file.path);
            let new = classify_path(&path);
            let best = match &file.old_path {
                Some(old) => {
                    let old = classify_path(&plain_line(old));
                    if old.1.rank() > new.1.rank() { old } else { new }
                }
                None => new,
            };
            FileRisk { path, category: best.0, level: best.1, evidence: best.2 }
        })
        .collect();
    classified.sort_by_key(|f| std::cmp::Reverse(f.level.rank()));
    let omitted = classified.len().saturating_sub(MAX_CLASSIFIED_FILES);
    classified.truncate(MAX_CLASSIFIED_FILES);
    Classification { files: classified, omitted }
}
