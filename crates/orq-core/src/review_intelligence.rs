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

use crate::review_input::{Finding, FindingStatus, Round, Severity, Verdict};

/// How firmly the recorded evidence backs a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// A confirmed verdict, or a fix that was made for it.
    Confirmed,
    /// Raised, but not confirmed by the evidence recorded.
    Potential,
    /// Rejected after review.
    Dismissed,
}

impl Standing {
    pub fn as_str(self) -> &'static str {
        match self {
            Standing::Confirmed => "confirmed",
            Standing::Potential => "potential",
            Standing::Dismissed => "dismissed",
        }
    }
}

/// Whether the file a finding points at is part of the reported change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    Changed,
    /// The file is not in the changed set: the finding may describe older code.
    Outdated,
    NoFile,
}

impl Reference {
    pub fn as_str(self) -> &'static str {
        match self {
            Reference::Changed => "changed",
            Reference::Outdated => "outdated",
            Reference::NoFile => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedFinding {
    pub round: usize,
    pub id: String,
    pub severity: Severity,
    pub category: String,
    pub claim: String,
    pub file: Option<String>,
    pub line: Option<u64>,
    pub standing: Standing,
    pub unresolved: bool,
    pub reference: Reference,
    pub evidence: String,
}

fn attribute_one(round: usize, finding: &Finding, changed: &[&str]) -> AttributedFinding {
    let standing = if finding.status == FindingStatus::Rejected {
        Standing::Dismissed
    } else if finding.verdict == Some(Verdict::Confirmed) || finding.status == FindingStatus::Fixed {
        Standing::Confirmed
    } else {
        Standing::Potential
    };
    let unresolved = !matches!(finding.status, FindingStatus::Fixed | FindingStatus::Rejected);
    let reference = match finding.file.as_deref() {
        None => Reference::NoFile,
        Some(file) if changed.contains(&file.strip_prefix("./").unwrap_or(file)) => Reference::Changed,
        Some(_) => Reference::Outdated,
    };
    let verdict = match finding.verdict {
        Some(Verdict::Confirmed) => "verdict confirmed",
        Some(Verdict::Plausible) => "verdict plausible",
        None => "no verdict recorded",
    };
    let status = match finding.status {
        FindingStatus::Open => "open",
        FindingStatus::Fixed => "fixed",
        FindingStatus::Rejected => "rejected",
        FindingStatus::Deferred => "deferred",
        FindingStatus::Unknown => "status unknown",
    };
    AttributedFinding {
        round,
        id: finding.id.clone(),
        severity: finding.severity,
        category: finding.category.clone(),
        claim: finding.claim.clone(),
        file: finding.file.clone(),
        line: finding.line,
        standing,
        unresolved,
        reference,
        evidence: format!("round {round}: {verdict}; {status}"),
    }
}

/// Attribute every finding to the evidence recorded for it. A finding that
/// recurs across rounds is reported once, at its latest state.
pub fn attribute(rounds: &[Round], files: &[ReportFile]) -> Vec<AttributedFinding> {
    let changed: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let mut out: Vec<AttributedFinding> = Vec::new();
    for round in rounds {
        for finding in &round.findings {
            let item = attribute_one(round.number, finding, &changed);
            match out.iter_mut().find(|existing| existing.id == item.id) {
                Some(existing) => *existing = item,
                None => out.push(item),
            }
        }
    }
    out
}

fn percent_encode(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A link to a file at one commit, or `None` unless every part can be trusted:
/// an https base, a full commit id, and a path that exists in that tree.
pub fn file_link(base_url: &str, sha: &str, path: &str, line: Option<u64>, exists: &dyn Fn(&str) -> bool) -> Option<String> {
    let base = base_url.strip_prefix("https://")?.trim_end_matches('/');
    if base.is_empty() || base.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let plain = !path.is_empty() && !path.starts_with('/') && !path.chars().any(char::is_control) && path.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
    if !plain || !exists(path) {
        return None;
    }
    let anchor = line.map(|l| format!("#L{l}")).unwrap_or_default();
    Some(format!("https://{base}/blob/{sha}/{}{anchor}", percent_encode(path)))
}

/// A link for one changed file, or the reason there is none.
fn provider_link(source: Option<&LinkSource>, current: bool, changed: &[&str], path: &str, line: Option<u64>) -> (Option<String>, Option<String>) {
    let Some(source) = source else { return (None, Some("no provider review is associated".to_string())) };
    if !current {
        return (None, Some("the report is not current".to_string()));
    }
    if !changed.contains(&path) {
        return (None, Some("the file is not changed in this review".to_string()));
    }
    match file_link(source.base_url, source.sha, path, line, source.exists) {
        Some(link) if source.provider == "gitlab" => (Some(link.replacen("/blob/", "/-/blob/", 1)), None),
        Some(link) => (Some(link), None),
        None => (None, Some("the file cannot be linked at the reviewed commit".to_string())),
    }
}

/// The repository page behind a review URL, or `None` when the provider is
/// unknown or the URL is not an https review page of that provider.
pub fn repository_base(provider: &str, review_url: &str) -> Option<String> {
    let marker = match provider {
        "github" => "/pull/",
        "gitlab" => "/-/merge_requests/",
        _ => return None,
    };
    let rest = review_url.strip_prefix("https://")?;
    let (repository, number) = rest.split_once(marker)?;
    let digits = number.split(['/', '?', '#']).next().unwrap_or("");
    if repository.is_empty() || !repository.contains('/') || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("https://{}", repository.trim_end_matches('/')))
}

/// Where file links point: a repository page, the reviewed commit, and a check
/// that a path exists in that commit's tree.
pub struct LinkSource<'a> {
    /// `github` or `gitlab`; decides the shape of the link.
    pub provider: &'a str,
    pub base_url: &'a str,
    pub sha: &'a str,
    pub exists: &'a dyn Fn(&str) -> bool,
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::High => "high",
        Severity::Medium => "medium",
        Severity::Low => "low",
        Severity::Unknown => "unknown",
    }
}

/// Add risks, file risks and a per-round summary to a built report. The
/// rounds are summarized from what was recorded; nothing is rerun.
pub fn apply_intelligence(report: &mut serde_json::Value, files: &[ReportFile], rounds: &[Round], links: Option<&LinkSource>) {
    use serde_json::json;
    let classification = classify_files(files);
    let changed: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let current = report["status"] == "current";
    let risks: Vec<serde_json::Value> = attribute(rounds, files)
        .iter()
        .map(|a| {
            let (link, link_reason) = match a.file.as_deref() {
                Some(path) => provider_link(links, current, &changed, path.strip_prefix("./").unwrap_or(path), a.line),
                None => (None, Some("the finding names no file".to_string())),
            };
            json!({
                "id": a.id, "round": a.round, "severity": severity_name(a.severity), "category": a.category, "claim": a.claim,
                "file": a.file, "line": a.line, "standing": a.standing.as_str(), "unresolved": a.unresolved,
                "reference": a.reference.as_str(), "evidence": a.evidence, "link": link, "link_reason": link_reason,
            })
        })
        .collect();
    let summaries: Vec<serde_json::Value> = rounds
        .iter()
        .map(|round| {
            let each: Vec<AttributedFinding> = round.findings.iter().map(|f| attribute_one(round.number, f, &changed)).collect();
            let count = |standing: Standing| each.iter().filter(|a| a.standing == standing).count();
            json!({
                "round": round.number, "summary": round.summary, "findings": each.len(),
                "confirmed": count(Standing::Confirmed), "potential": count(Standing::Potential), "dismissed": count(Standing::Dismissed),
                "unresolved": each.iter().filter(|a| a.unresolved).count(),
            })
        })
        .collect();
    report["risks"] = json!(risks);
    report["rounds"] = json!(summaries);
    report["file_risks"] = json!({
        "files": classification.files.iter().map(|f| {
            let (link, link_reason) = provider_link(links, current, &changed, &f.path, None);
            json!({"path": f.path, "category": f.category.as_str(), "level": f.level.as_str(), "evidence": f.evidence, "link": link, "link_reason": link_reason})
        }).collect::<Vec<_>>(),
        "omitted": classification.omitted,
    });
    if classification.omitted > 0 {
        if let Some(gaps) = report["gaps"].as_array_mut() {
            gaps.push(json!(format!("{} files were not classified because the change has more than {MAX_CLASSIFIED_FILES} files", classification.omitted)));
        }
        report["status"] = json!("incomplete");
    }
}
