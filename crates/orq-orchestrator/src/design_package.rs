//! A frontend run's design package: `designs/<feature>/` in the main checkout, checked without any
//! agent and frozen into the run's storage, so it needn't be committed and can't change under the
//! run. The package is project data: its files are read as bytes and text, never as instructions.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::run_settings::{DesignMode, DesignSettings};

/// The folder of the main checkout holding one package per feature.
const DESIGNS_DIR: &str = "designs";
/// Under the run's logs: the frozen package and its manifest.
const RUN_DESIGN_DIR: &str = "design";
const PACKAGE_DIR: &str = "package";
const MANIFEST_FILE: &str = "manifest.json";
/// Under the run's design folder: the feature and the scope its package was frozen for.
const SCOPE_FILE: &str = "scope.json";
/// The implementer's read-only copy, in the worktree.
const WORKTREE_COPY_DIR: &str = ".orq-design";

const DESIGN_DOC: &str = "DESIGN.md";
const DOCUMENTS: [&str; 3] = [DESIGN_DOC, "RESPONSIVE.md", "UX.md"];
const TOKENS: [&str; 2] = ["tokens.css", "tokens.json"];

/// How much a package may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageLimits {
    pub max_total_bytes: u64,
    pub max_files: usize,
}

impl Default for PackageLimits {
    fn default() -> Self {
        PackageLimits {
            max_total_bytes: 200 * 1024 * 1024,
            max_files: 2000,
        }
    }
}

/// Where one run's package comes from and goes.
#[derive(Debug, Clone, Copy)]
pub struct DesignPackageRun<'a> {
    /// The project's main checkout, holding `designs/<feature>/`.
    pub main_checkout: &'a Path,
    pub feature: &'a str,
    pub settings: &'a DesignSettings,
    /// The run's log folder; the frozen package goes in its `design/`.
    pub run_logs: &'a Path,
    /// The run's worktree; the implementer's read-only copy goes in its `.orq-design/`.
    pub worktree: &'a Path,
    pub limits: PackageLimits,
}

/// What became of the package when the run started or resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesignPackageOutcome {
    /// Checked and frozen: the run goes on with this manifest.
    Frozen(DesignManifest),
    /// The run pauses before any agent until the person adds these files.
    FilesNeeded(DesignFilesNeeded),
    /// A check failed: the run stops before any agent, with every problem listed.
    Refused(Vec<String>),
    /// The main checkout's package is not the one frozen for the run: the run pauses.
    Changed(String),
}

/// `manifest.json`: every frozen file, and what the package lacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignManifest {
    pub feature: String,
    /// One SHA-256 over every file's path and SHA-256.
    pub package_sha256: String,
    /// Sorted by path.
    pub files: Vec<ManifestFile>,
    /// References no viewport could be assigned to.
    pub unassigned: Vec<String>,
    pub missing: MissingPieces,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Relative to the package folder, `/`-separated.
    pub path: String,
    pub sha256: String,
    pub size: u64,
    /// `reference`, `asset`, `document`, `tokens` or `other`.
    pub kind: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub viewport: Option<AssignedViewport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignedViewport {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingPieces {
    /// Viewports of the settings with no reference.
    pub viewports: Vec<String>,
    /// Documents the package lacks; `tokens.css` stands for either tokens file.
    pub documents: Vec<String>,
    /// Assets `DESIGN.md` names that the package lacks, as `assets/…` paths.
    pub assets: Vec<String>,
}

/// A design files needed request: what the person has to make and where each goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignFilesNeeded {
    pub feature: String,
    pub files: Vec<NeededFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeededFile {
    /// What the file is, in a few words.
    pub what: String,
    /// Where it goes, from the main checkout: `designs/<feature>/references/desktop.png`.
    pub path: String,
}

impl DesignFilesNeeded {
    /// The request as the person reads it.
    pub fn message(&self) -> String {
        let mut text = format!(
            "Design files needed for {}. Orq never makes a design: make each file below \
             (in ChatGPT, for example), put it where shown in the main checkout, then resume the run.\n",
            self.feature
        );
        for file in &self.files {
            text.push_str(&format!("- {}: {}\n", file.what, file.path));
        }
        text
    }
}

/// A feature's part of a package shared by several features, from its ledger description's
/// `Design package: <name>`, `Design components: <ids>|all` and `Design components done: <ids>` lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignScope {
    /// The feature the package is frozen for.
    pub feature: String,
    /// The package folder's name: `designs/<package>/`.
    pub package: String,
    /// The ids of the components the feature builds; `None` for the whole page (`all`).
    pub components: Option<Vec<String>>,
    /// The ids of the components already done: checked for regressions only.
    pub done: Vec<String>,
}

const PACKAGE_LINE: &str = "Design package:";
const COMPONENTS_LINE: &str = "Design components:";
const DONE_LINE: &str = "Design components done:";

/// A plain word: ASCII letters, digits, `-` and `_`, not empty.
fn plain_word(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// The ids a components line lists, comma-separated, each a plain word.
fn component_ids(line: &str, value: &str) -> Result<Vec<String>, String> {
    let ids: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return Err(format!("the line {line:?} names no component"));
    }
    if let Some(id) = ids.iter().find(|id| !plain_word(id)) {
        return Err(format!(
            "the line {line:?} names {id:?}, which is not a component id (a plain word)"
        ));
    }
    Ok(ids)
}

/// The feature's design scope, read from its ledger `description`: `None` when it has none of the
/// design lines, so the package is `designs/<feature>/` and the whole page is reviewed. A line that
/// can't be read is refused, naming it.
pub fn design_scope(
    feature: &str,
    description: Option<&str>,
) -> Result<Option<DesignScope>, String> {
    let mut package: Option<String> = None;
    let mut components: Option<Option<Vec<String>>> = None;
    let mut done: Vec<String> = Vec::new();
    let mut found = false;
    for line in description.unwrap_or_default().lines().map(str::trim) {
        if let Some(value) = line.strip_prefix(PACKAGE_LINE).map(str::trim) {
            found = true;
            if !plain_word(value) {
                return Err(format!(
                    "the line {line:?} does not name a design package by a plain word (letters, digits, - and _)"
                ));
            }
            if package.as_deref().is_some_and(|named| named != value) {
                return Err(format!(
                    "the description names two design packages, {} and {value}",
                    package.unwrap_or_default()
                ));
            }
            package = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix(DONE_LINE).map(str::trim) {
            found = true;
            for id in component_ids(line, value)? {
                if !done.contains(&id) {
                    done.push(id);
                }
            }
        } else if let Some(value) = line.strip_prefix(COMPONENTS_LINE).map(str::trim) {
            found = true;
            let ids = if value == "all" {
                None
            } else {
                Some(component_ids(line, value)?)
            };
            if components.as_ref().is_some_and(|named| named != &ids) {
                return Err("the description has two different Design components lines".to_string());
            }
            components = Some(ids);
        }
    }
    Ok(found.then(|| DesignScope {
        feature: feature.to_string(),
        package: package.unwrap_or_else(|| feature.to_string()),
        components: components.flatten(),
        done,
    }))
}

/// The scope the run's package was frozen for, from the run's `design/scope.json`; `None` when it was
/// frozen for the feature's own whole page, or not yet.
pub fn frozen_design_scope(run_logs: &Path) -> Option<DesignScope> {
    let bytes = std::fs::read(run_logs.join(RUN_DESIGN_DIR).join(SCOPE_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The folder of a design group's members' own logs, in the run's logs.
const MEMBERS_DIR: &str = "members";

/// What of the run's design folder a design group member's design loop reads: the frozen package, its
/// manifest and what the preparer inferred, never another member's rounds or reports.
const MEMBER_COPIED: [&str; 5] = [
    MANIFEST_FILE,
    PACKAGE_DIR,
    "prepared",
    "prepared.json",
    "prepared.md",
];

/// The logs of design group member `member`'s design loop, `run_logs/members/<member>/`: its design
/// folder holds a copy of the package the run froze once, and its scope, the package's `components` it
/// builds with the components of the members before it (`done`) checked for regressions only. A copy
/// already made is kept, so a resume goes on with the member's own rounds.
pub fn member_design_logs(
    run_logs: &Path,
    member: &str,
    components: &[String],
    done: &[String],
) -> Result<std::path::PathBuf, String> {
    if !plain_word(member) {
        return Err(format!("{member:?} is not a feature name"));
    }
    let logs = run_logs.join(MEMBERS_DIR).join(member);
    let design = logs.join(RUN_DESIGN_DIR);
    let frozen = run_logs.join(RUN_DESIGN_DIR);
    let package = frozen_design_scope(run_logs)
        .map(|scope| scope.package)
        .ok_or_else(|| {
            "the run's design package was not frozen for a shared package".to_string()
        })?;
    let failed = |e: std::io::Error| {
        format!(
            "the design folder of {member} could not be written ({:?})",
            e.kind()
        )
    };
    std::fs::create_dir_all(&design).map_err(failed)?;
    for name in MEMBER_COPIED {
        let (from, to) = (frozen.join(name), design.join(name));
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        match std::fs::symlink_metadata(&from) {
            Ok(meta) if meta.is_dir() => copy_regular_tree(&from, &to).map_err(failed)?,
            Ok(meta) if meta.is_file() => {
                std::fs::copy(&from, &to).map_err(failed)?;
            }
            _ => {}
        }
    }
    let scope_file = design.join(SCOPE_FILE);
    if std::fs::symlink_metadata(&scope_file).is_err() {
        let scope = DesignScope {
            feature: member.to_string(),
            package,
            // A member naming no components builds the whole package.
            components: (!components.is_empty()).then(|| components.to_vec()),
            done: done.to_vec(),
        };
        let json = serde_json::to_vec_pretty(&scope)
            .map_err(|e| format!("the design scope of {member} could not be written ({e})"))?;
        std::fs::write(&scope_file, json).map_err(failed)?;
    }
    Ok(logs)
}

/// Copies the folders and regular files under `from` to `to`; a symlink is never followed or copied.
fn copy_regular_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_regular_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The components of a DESIGN.md: each `###` heading's leading word, with its section (the heading
/// and what follows it up to the next heading of level 3 or above), in the document's order.
pub fn design_components(design: &str) -> Vec<(String, String)> {
    let level = |line: &str| {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        (hashes > 0 && line[hashes..].starts_with([' ', '\t'])).then_some(hashes)
    };
    let mut components: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in design.lines() {
        match level(line) {
            Some(hashes) if hashes <= 3 => {
                components.extend(current.take());
                if hashes == 3 {
                    if let Some(id) = line[3..].split_whitespace().next() {
                        current = Some((id.to_string(), format!("{line}\n")));
                    }
                }
            }
            _ => {
                if let Some((_, section)) = current.as_mut() {
                    section.push_str(line);
                    section.push('\n');
                }
            }
        }
    }
    components.extend(current);
    components
}

/// The problem with `scope` for the package's DESIGN.md: an id it names that DESIGN.md has no `###`
/// component for, named with the ids DESIGN.md has.
fn scope_problem(scope: &DesignScope, design_text: Option<&str>) -> Option<String> {
    let has: Vec<String> = design_components(design_text.unwrap_or_default())
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let named = scope.components.iter().flatten().chain(&scope.done);
    let unknown: Vec<&String> = named.filter(|id| !has.contains(id)).collect();
    if unknown.is_empty() {
        return None;
    }
    let ids: Vec<&str> = unknown.iter().map(|id| id.as_str()).collect();
    let listed = if has.is_empty() {
        "none".to_string()
    } else {
        has.join(", ")
    };
    Some(format!(
        "{} names the component {}, which {DESIGN_DOC} of {DESIGNS_DIR}/{} has no ### section for (it has {listed})",
        scope.feature,
        ids.join(", "),
        scope.package
    ))
}

/// The package's folder name: the scope's package, or the run's feature.
fn package_name<'a>(run: &'a DesignPackageRun, scope: Option<&'a DesignScope>) -> &'a str {
    scope.map_or(run.feature, |scope| scope.package.as_str())
}

/// Checks the feature's package in the main checkout and freezes it for the run.
pub fn start_design_package(run: &DesignPackageRun) -> DesignPackageOutcome {
    start_design_package_scoped(run, None)
}

/// [`start_design_package`] for a feature with a design `scope`: the package is `designs/<package>/`,
/// every component id the scope names must be one of DESIGN.md's, and the frozen copy records the scope.
pub fn start_design_package_scoped(
    run: &DesignPackageRun,
    scope: Option<&DesignScope>,
) -> DesignPackageOutcome {
    let name = package_name(run, scope);
    let dir = match package_folder(run, name) {
        Ok(dir) => dir,
        Err(problems) => return DesignPackageOutcome::Refused(problems),
    };
    let scanned = if std::fs::symlink_metadata(&dir).is_err() {
        None
    } else {
        match scan(&dir, run) {
            Ok(scanned) => Some(scanned),
            Err(problems) => return DesignPackageOutcome::Refused(problems),
        }
    };
    let missing = match &scanned {
        Some(scanned) => scanned.manifest.missing.clone(),
        None => missing_pieces(&[], None, run.settings),
    };
    let has_reference = scanned
        .as_ref()
        .is_some_and(|s| s.manifest.files.iter().any(|file| file.kind == "reference"));
    let request = files_needed(run, name, &missing, has_reference);
    let Some(scanned) = scanned.filter(|_| has_reference && request.files.is_empty()) else {
        // The person is shown where the files go.
        for sub in ["references", "assets"] {
            if let Err(e) = std::fs::create_dir_all(dir.join(sub)) {
                return DesignPackageOutcome::Refused(vec![format!(
                    "{DESIGNS_DIR}/{name}/{sub} could not be created ({:?})",
                    e.kind()
                )]);
            }
        }
        return DesignPackageOutcome::FilesNeeded(request);
    };
    if let Some(problem) = scope.and_then(|scope| scope_problem(scope, scanned.design_text())) {
        return DesignPackageOutcome::Refused(vec![problem]);
    }
    match freeze(run, &scanned, scope) {
        Ok(()) => DesignPackageOutcome::Frozen(scanned.manifest),
        Err(problem) => DesignPackageOutcome::Refused(vec![problem]),
    }
}

/// Checks the package again and freezes it anew when a run its design review paused for files
/// resumes. The package's own checks do not know what the review asked for, so the run stays paused,
/// with nothing frozen anew, until each file of `requested` is in the main checkout as a regular file.
pub fn refreeze_design_package(
    run: &DesignPackageRun,
    requested: &DesignFilesNeeded,
) -> DesignPackageOutcome {
    refreeze_design_package_scoped(run, requested, None)
}

/// [`refreeze_design_package`] for a feature with a design `scope`; the scope the package was frozen
/// for before, when there is one, is kept.
pub fn refreeze_design_package_scoped(
    run: &DesignPackageRun,
    requested: &DesignFilesNeeded,
    scope: Option<&DesignScope>,
) -> DesignPackageOutcome {
    let frozen = frozen_design_scope(run.run_logs);
    let scope = frozen.as_ref().or(scope);
    let missing: Vec<NeededFile> = requested
        .files
        .iter()
        .filter(|file| {
            !std::fs::symlink_metadata(run.main_checkout.join(&file.path))
                .is_ok_and(|meta| meta.file_type().is_file())
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        return DesignPackageOutcome::FilesNeeded(DesignFilesNeeded {
            feature: requested.feature.clone(),
            files: missing,
        });
    }
    start_design_package_scoped(run, scope)
}

/// Checks the package again when a paused run resumes. A run that froze its package goes on only
/// while the main checkout's package is still the one frozen; a run still waiting for design files
/// checks the folder again as if it were starting.
pub fn resume_design_package(run: &DesignPackageRun) -> DesignPackageOutcome {
    resume_design_package_scoped(run, None)
}

/// [`resume_design_package`] for a feature with a design `scope`. A package frozen for the run keeps the
/// scope it was frozen for: `scope` is used only when the package is checked as if starting.
pub fn resume_design_package_scoped(
    run: &DesignPackageRun,
    scope: Option<&DesignScope>,
) -> DesignPackageOutcome {
    let saved = run.run_logs.join(RUN_DESIGN_DIR).join(MANIFEST_FILE);
    let text = match std::fs::read(&saved) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return start_design_package_scoped(run, scope)
        }
        Err(e) => {
            return DesignPackageOutcome::Refused(vec![format!(
                "the run's design manifest could not be read ({:?})",
                e.kind()
            )])
        }
    };
    let frozen: DesignManifest = match serde_json::from_slice(&text) {
        Ok(manifest) => manifest,
        Err(e) => {
            return DesignPackageOutcome::Refused(vec![format!(
                "the run's design manifest is not valid ({e})"
            )])
        }
    };
    let frozen_scope = frozen_design_scope(run.run_logs);
    let name = package_name(run, frozen_scope.as_ref());
    let current = package_folder(run, name).and_then(|dir| {
        if std::fs::symlink_metadata(&dir).is_err() {
            return Err(vec![format!("{DESIGNS_DIR}/{name} is gone")]);
        }
        scan(&dir, run)
    });
    let now = match current {
        Ok(scanned) if scanned.manifest.package_sha256 == frozen.package_sha256 => {
            // The same files as frozen: the worktree's copy is put back whole from them.
            return match copy_to_worktree(run, &scanned) {
                Ok(()) => DesignPackageOutcome::Frozen(frozen),
                Err(problem) => DesignPackageOutcome::Refused(vec![problem]),
            };
        }
        Ok(_) => "its files differ".to_string(),
        Err(problems) => problems.join("; "),
    };
    DesignPackageOutcome::Changed(format!(
        "the design package changed since this run started: {DESIGNS_DIR}/{name} is not the package \
         frozen for the run ({now}). Put the package back as it was, or start a new run to use the new one."
    ))
}

/// What the package lacks: viewports with no reference, documents, and assets `DESIGN.md` names.
fn missing_pieces(
    files: &[ManifestFile],
    design_text: Option<&str>,
    settings: &DesignSettings,
) -> MissingPieces {
    let has = |path: &str| files.iter().any(|file| file.path == path);
    let viewports = settings
        .viewports
        .keys()
        .filter(|name| {
            !files
                .iter()
                .any(|file| file.viewport.as_ref().is_some_and(|v| &v.name == *name))
        })
        .cloned()
        .collect();
    let mut documents: Vec<String> = DOCUMENTS
        .iter()
        .filter(|doc| !has(doc))
        .map(|doc| doc.to_string())
        .collect();
    if !TOKENS.iter().any(|tokens| has(tokens)) {
        documents.push(TOKENS[0].to_string());
    }
    let assets = design_text
        .map(named_assets)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| !name.split('/').any(|part| part == ".." || part == "."))
        .filter(|name| !has(name))
        .collect();
    MissingPieces {
        viewports,
        documents,
        assets,
    }
}

/// The request for what is missing: every piece in `prepared` mode; in `infer` mode the documents
/// are left to the design preparer, and once the package has a reference a viewport without one is
/// reviewed by the rules alone, so only a package with no reference at all is asked for one.
fn files_needed(
    run: &DesignPackageRun,
    name: &str,
    missing: &MissingPieces,
    has_reference: bool,
) -> DesignFilesNeeded {
    let at = |path: &str| format!("{DESIGNS_DIR}/{name}/{path}");
    let mut files = Vec::new();
    let viewports: &[String] = if run.settings.mode == DesignMode::Infer && has_reference {
        &[]
    } else {
        &missing.viewports
    };
    for name in viewports {
        let size = run.settings.viewports.get(name);
        files.push(NeededFile {
            what: format!(
                "the {name} reference{} (PNG, JPEG, WebP or PDF)",
                size.map_or(String::new(), |v| format!(", {}x{}", v.width, v.height))
            ),
            path: at(&format!("references/{name}.png")),
        });
    }
    if run.settings.mode == DesignMode::Prepared {
        for doc in &missing.documents {
            let what = if doc == TOKENS[0] {
                "the design tokens (tokens.css or tokens.json)".to_string()
            } else {
                doc.clone()
            };
            files.push(NeededFile {
                what,
                path: at(doc),
            });
        }
    }
    for asset in &missing.assets {
        files.push(NeededFile {
            what: format!("the asset {DESIGN_DOC} names"),
            path: at(asset),
        });
    }
    DesignFilesNeeded {
        feature: run.feature.to_string(),
        files,
    }
}

/// The package as read: its manifest and every file's bytes, frozen from exactly what was hashed.
struct Scanned {
    manifest: DesignManifest,
    contents: Vec<(String, Vec<u8>)>,
}

impl Scanned {
    /// The package's DESIGN.md, as text.
    fn design_text(&self) -> Option<&str> {
        self.contents
            .iter()
            .find(|(path, _)| path == DESIGN_DOC)
            .and_then(|(_, bytes)| std::str::from_utf8(bytes).ok())
    }
}

/// `designs/<name>` in the main checkout, refused when it or `designs` is a symlink.
fn package_folder(run: &DesignPackageRun, name: &str) -> Result<PathBuf, Vec<String>> {
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !plain {
        return Err(vec![format!(
            "{name:?} is not a usable design package name: designs/<feature> must stay inside the designs folder"
        )]);
    }
    let designs = run.main_checkout.join(DESIGNS_DIR);
    let dir = designs.join(name);
    for (path, shown) in [
        (&designs, DESIGNS_DIR.to_string()),
        (&dir, format!("{DESIGNS_DIR}/{name}")),
    ] {
        if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(vec![format!(
                "{shown} is a symlink: a design package must be a plain folder"
            )]);
        }
    }
    Ok(dir)
}

fn scan(dir: &Path, run: &DesignPackageRun) -> Result<Scanned, Vec<String>> {
    let mut paths = Vec::new();
    let mut problems = Vec::new();
    let limits = run.limits;
    walk(dir, "", &mut paths, &mut problems, limits.max_files);
    if paths.len() > limits.max_files {
        problems.push(format!(
            "the package has more than {} files: remove some, or split it",
            limits.max_files
        ));
        return Err(problems);
    }
    let over_size = |total: u64| {
        format!(
            "the package holds {total} bytes, over its limit of {} bytes",
            limits.max_total_bytes
        )
    };
    let listed: u64 = paths.iter().map(|(_, _, size)| size).sum();
    if listed > limits.max_total_bytes {
        problems.push(over_size(listed));
        return Err(problems);
    }
    paths.sort();
    let mut files = Vec::new();
    let mut contents = Vec::new();
    let mut unassigned = Vec::new();
    let mut warnings = Vec::new();
    let mut total = 0u64;
    for (relative, path, _) in paths {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                problems.push(format!("{relative} could not be read ({:?})", e.kind()));
                continue;
            }
        };
        // A file that grew since it was listed still counts at the size read.
        total += bytes.len() as u64;
        if total > limits.max_total_bytes {
            problems.push(over_size(total));
            return Err(problems);
        }
        let kind = kind_of(&relative);
        let size = match check_file(kind, &relative, &bytes) {
            Ok(size) => size,
            Err(problem) => {
                problems.push(format!("{relative} {problem}"));
                None
            }
        };
        let viewport = if kind == "reference" {
            assign_viewport(&relative, run.settings)
        } else {
            None
        };
        if kind == "reference" && viewport.is_none() {
            unassigned.push(relative.clone());
        }
        if let (Some(v), Some((width, _))) = (&viewport, size) {
            if !matches_viewport_width(width, v.width) {
                warnings.push(format!(
                    "{relative} is {width} pixels wide, not 1, 2 or 3 times its {} viewport's {} CSS pixels",
                    v.name, v.width
                ));
            }
        }
        files.push(ManifestFile {
            path: relative.clone(),
            sha256: hex(&Sha256::digest(&bytes)),
            size: bytes.len() as u64,
            kind: kind.to_string(),
            width: size.map(|(w, _)| w),
            height: size.map(|(_, h)| h),
            viewport,
        });
        contents.push((relative, bytes));
    }
    let design_text = contents
        .iter()
        .find(|(path, _)| path == DESIGN_DOC)
        .map(|(_, text)| String::from_utf8_lossy(text).into_owned());
    if let Some(text) = &design_text {
        for name in named_assets(text) {
            if name.split('/').any(|part| part == ".." || part == ".") {
                problems.push(format!(
                    "{DESIGN_DOC} names {name}: a path with .. or . can't be in the package"
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let missing = missing_pieces(&files, design_text.as_deref(), run.settings);
    let manifest = DesignManifest {
        feature: run.feature.to_string(),
        package_sha256: package_hash(&files),
        files,
        unassigned,
        missing,
        warnings,
    };
    Ok(Scanned { manifest, contents })
}

/// A capture of a viewport is its width at a device pixel ratio of 1, 2 or 3, within 1%; its
/// height is free, since a capture may hold the whole page.
fn matches_viewport_width(pixels: u32, css_width: u32) -> bool {
    (1..=3u64).any(|ratio| {
        let expected = css_width as u64 * ratio;
        (pixels as u64).abs_diff(expected) * 100 <= expected
    })
}

/// Every plain file under `dir`, as (`/`-separated relative path, full path, size); a symlink or
/// anything else that is no plain file or folder is a problem, never followed. Stops once it has
/// found more than `max_files`.
fn walk(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(String, PathBuf, u64)>,
    problems: &mut Vec<String>,
    max_files: usize,
) {
    let unlisted =
        |e: std::io::Error| format!("{} could not be listed ({:?})", shown(prefix), e.kind());
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return problems.push(unlisted(e)),
    };
    for entry in entries {
        if out.len() > max_files {
            return;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => return problems.push(unlisted(e)),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                problems.push(format!("{relative} could not be read ({:?})", e.kind()));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            problems.push(format!(
                "{relative} is a symlink: a design package holds only plain files"
            ));
        } else if meta.is_dir() {
            walk(&path, &relative, out, problems, max_files);
        } else if meta.is_file() {
            out.push((relative, path, meta.len()));
        } else {
            problems.push(format!("{relative} is not a plain file"));
        }
    }
}

/// The `assets/…` file paths a document's text names, read as plain text: each starts a word with
/// `assets/` and ends at a space, quote or bracket; only names with a file extension count.
fn named_assets(text: &str) -> Vec<String> {
    const START: &str = "assets/";
    let mut names = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(START) {
        let at = from + found;
        from = at + START.len();
        let starts_word = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')));
        if !starts_word {
            continue;
        }
        let rest = &text[at..];
        let end = rest
            .find(|c: char| c.is_whitespace() || "()[]{}<>\"'`,;|*".contains(c))
            .unwrap_or(rest.len());
        let name = rest[..end].trim_end_matches(['.', ':', '!', '?']);
        let file = name.rsplit('/').next().unwrap_or_default();
        let has_extension = file
            .rsplit_once('.')
            .is_some_and(|(base, ext)| !base.is_empty() && !ext.is_empty());
        if has_extension && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

fn shown(prefix: &str) -> &str {
    if prefix.is_empty() {
        "the package folder"
    } else {
        prefix
    }
}

fn kind_of(relative: &str) -> &'static str {
    if DOCUMENTS.contains(&relative) {
        "document"
    } else if TOKENS.contains(&relative) {
        "tokens"
    } else if relative.starts_with("references/") {
        "reference"
    } else if relative.starts_with("assets/") {
        "asset"
    } else {
        "other"
    }
}

fn extension(relative: &str) -> String {
    Path::new(relative)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn stem(relative: &str) -> String {
    Path::new(relative)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A reference named after a viewport of the settings is that viewport; one named
/// `<name>-<w>x<h>` is the viewport `name` at its own size.
fn assign_viewport(relative: &str, settings: &DesignSettings) -> Option<AssignedViewport> {
    let stem = stem(relative);
    if let Some(v) = settings.viewports.get(&stem) {
        return Some(AssignedViewport {
            name: stem.clone(),
            width: v.width,
            height: v.height,
        });
    }
    let (name, size) = stem.rsplit_once('-')?;
    let (width, height) = size.split_once('x')?;
    let side = |text: &str| {
        let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        digits
            .then(|| text.parse::<u32>().ok())
            .flatten()
            .filter(|n| *n > 0)
    };
    let plain_name = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    plain_name.then_some(AssignedViewport {
        name: name.to_string(),
        width: side(width)?,
        height: side(height)?,
    })
}

/// Checks a reference or asset is a file of an allowed type that decodes; the pixel size of an
/// image. The error completes a sentence after the file's path.
fn check_file(kind: &str, relative: &str, bytes: &[u8]) -> Result<Option<(u32, u32)>, String> {
    let extension = extension(relative);
    let image = |size: Option<(u32, u32)>, name: &str| {
        size.filter(|(w, h)| *w > 0 && *h > 0)
            .map(Some)
            .ok_or_else(|| format!("does not decode as a {name} image"))
    };
    let font = |ok: bool| {
        ok.then_some(None)
            .ok_or_else(|| format!("does not read as a {} font", extension.to_ascii_uppercase()))
    };
    match (kind, extension.as_str()) {
        ("reference" | "asset", "png") => image(png_size(bytes), "PNG"),
        ("reference" | "asset", "jpg" | "jpeg") => image(jpeg_size(bytes), "JPEG"),
        ("reference" | "asset", "webp") => image(webp_size(bytes), "WebP"),
        ("reference", "pdf") => {
            let tail = &bytes[bytes.len().saturating_sub(1024)..];
            let ends = tail.windows(5).any(|w| w == b"%%EOF");
            (bytes.starts_with(b"%PDF-") && ends)
                .then_some(None)
                .ok_or_else(|| "does not read as a PDF".to_string())
        }
        ("reference", _) => Err("is not a PNG, JPEG, WebP or PDF reference".to_string()),
        ("asset", "svg") => svg_problem(bytes).map_or(Ok(None), Err),
        ("asset", "woff") => font(bytes.starts_with(b"wOFF")),
        ("asset", "woff2") => font(bytes.starts_with(b"wOF2")),
        ("asset", "ttf" | "otf") => font(
            [&b"\x00\x01\x00\x00"[..], b"OTTO", b"true", b"ttcf"]
                .iter()
                .any(|magic| bytes.starts_with(magic)),
        ),
        ("asset", _) => {
            Err("is not an SVG, PNG, JPEG, WebP or font (WOFF, WOFF2, TTF, OTF) asset".to_string())
        }
        _ => Ok(None),
    }
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// Why an SVG is unsafe: a `<script>`, a `<foreignObject>`, an `on*` attribute, an entity
/// declaration or stylesheet link, or anything that loads a resource (`href`, `src`, CSS `url()`,
/// `@import`) that isn't a local fragment or a data image. Read as text, case-insensitively. The
/// markup is read as written, as an XML parser does; XML character references are decoded only
/// inside attribute values and `<style>` contents, where they stand for text.
fn svg_problem(bytes: &[u8]) -> Option<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Some("is not a UTF-8 SVG".to_string());
    };
    let text = text.to_ascii_lowercase();
    if text.contains("<!entity") {
        return Some("is refused: it declares an <!ENTITY>".to_string());
    }
    if text.contains("<?xml-stylesheet") {
        return Some("is refused: it links a stylesheet".to_string());
    }
    if !tags_named(&text, "script", false).is_empty() {
        return Some("is refused: it has a <script>".to_string());
    }
    if !tags_named(&text, "foreignobject", false).is_empty() {
        return Some("is refused: it has a <foreignObject>".to_string());
    }
    let decoded = |raw: &str| decode_references(raw).to_ascii_lowercase();
    // Where CSS may be: `<style>` contents and every attribute value (`style` and presentation
    // attributes such as `fill` alike).
    let mut css: Vec<String> = style_elements(&text).iter().map(|s| decoded(s)).collect();
    let mut styles = css.clone();
    for (name, value) in svg_attributes(&text) {
        let value = decoded(&value);
        if name.starts_with("on") {
            return Some(format!("is refused: it has the event attribute {name}"));
        }
        if (name == "href" || name.ends_with(":href") || name == "src") && !local_resource(&value) {
            return Some(format!(
                "is refused: its {name} is neither a local #fragment nor a data image"
            ));
        }
        if name == "style" {
            styles.push(value.clone());
        }
        css.push(value);
    }
    if css.iter().any(|css| css.contains("@import")) {
        return Some("is refused: its CSS has an @import".to_string());
    }
    if let Some(problem) = css.iter().find_map(|css| css_url_problem(css)) {
        return Some(problem);
    }
    if styles.iter().any(|style| style.contains('\\')) {
        return Some("is refused: its CSS has a backslash escape".to_string());
    }
    None
}

/// A resource an SVG may load: a fragment of itself, or a data image that isn't another SVG.
fn local_resource(value: &str) -> bool {
    let value = value.trim();
    value.starts_with('#')
        || (value.starts_with("data:image/") && !value.starts_with("data:image/svg"))
}

/// `text` with its XML character references (`&#117;`, `&#x75;`, `&lt;` and the like) decoded;
/// anything else is kept as written.
fn decode_references(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // Only a short way ahead, so a text of many bare `&`s stays linear.
        let decoded = rest[1..]
            .bytes()
            .take(11)
            .position(|b| b == b';')
            .and_then(|end| {
                let name = &rest[1..1 + end];
                let c = match name {
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "amp" => Some('&'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => {
                        let number = name.strip_prefix('#')?;
                        let code = match number.strip_prefix('x') {
                            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                            None => number.parse().ok()?,
                        };
                        char::from_u32(code)
                    }
                };
                c.map(|c| (c, end + 2))
            });
        match decoded {
            Some((c, length)) => {
                out.push(c);
                rest = &rest[length..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Why a CSS text's `url(...)`s are refused: the first that isn't a local fragment or a data
/// image, or one never closed. One forward pass: each target is skipped once read.
fn css_url_problem(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(at) = rest.find("url(") {
        rest = rest[at + 4..].trim_start();
        let (target, after) = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let end = rest[1..].find(quote).map(|end| end + 1);
                let closed = end
                    .and_then(|end| rest[end + 1..].trim_start().starts_with(')').then_some(end));
                match closed {
                    Some(end) => (&rest[1..end], end + 1),
                    None => return Some("is refused: it has an unterminated url(".to_string()),
                }
            }
            _ => match rest.find(')') {
                Some(end) => (&rest[..end], end + 1),
                None => return Some("is refused: it has an unterminated url(".to_string()),
            },
        };
        if !local_resource(target) {
            return Some(format!(
                "is refused: url({}) is neither a local #fragment nor a data image",
                target.trim()
            ));
        }
        rest = &rest[after..];
    }
    None
}

/// Every opening (or, with `closing`, closing) tag in `text` whose local name is `local`,
/// whatever its namespace prefix (`<style`, `<s:style`), as (where the tag starts, where its name
/// ends). A name ends at whitespace, `>`, `/` or the next `<`, so each character is read once.
fn tags_named(text: &str, local: &str, closing: bool) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    for (at, _) in text.match_indices('<') {
        let mut start = at + 1;
        if closing {
            if !text[start..].starts_with('/') {
                continue;
            }
            start += 1;
        }
        let length = text[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '>' | '/' | '<'))
            .unwrap_or(text.len() - start);
        let name = &text[start..start + length];
        if name.rsplit(':').next() == Some(local) {
            found.push((at, start + length));
        }
    }
    found
}

/// The spans of `text` (lowercased) that hold no markup: CDATA sections and comments, as
/// (start, end), in order. Read in one forward pass, so a `<!--` inside a CDATA section (or the
/// other way round) is text.
fn unmarked_sections(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find("<!") {
        let start = at + offset;
        let rest = &text[start..];
        let Some((opener, terminator)) = [("<![cdata[", "]]>"), ("<!--", "-->")]
            .into_iter()
            .find(|(opener, _)| rest.starts_with(opener))
        else {
            at = start + 2;
            continue;
        };
        let body = start + opener.len();
        let end = text[body..]
            .find(terminator)
            .map_or(text.len(), |end| body + end + terminator.len());
        found.push((start, end));
        at = end;
    }
    found
}

/// The contents of every `<style>` element in `text`, prefixed (`<s:style>`) or not. A closing
/// tag inside a CDATA section or a comment is text and doesn't end the element.
fn style_elements(text: &str) -> Vec<String> {
    let opening = tags_named(text, "style", false);
    let sections = unmarked_sections(text);
    let mut section = 0;
    let closing: Vec<(usize, usize)> = tags_named(text, "style", true)
        .into_iter()
        .filter(|&(at, _)| {
            while section < sections.len() && sections[section].1 <= at {
                section += 1;
            }
            sections.get(section).is_none_or(|&(start, _)| at < start)
        })
        .collect();
    let mut found = Vec::new();
    let mut from = 0;
    let mut next_close = 0;
    for (tag, name_end) in opening {
        if tag < from {
            continue;
        }
        while next_close < closing.len() && closing[next_close].0 < name_end {
            next_close += 1;
        }
        let end = closing.get(next_close).map_or(text.len(), |close| close.0);
        found.push(text[name_end..end].to_string());
        from = end;
    }
    found
}

/// Every attribute of every tag in `text`, as (name, value), quotes honoured.
fn svg_attributes(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        if chars[i..].starts_with(&['<', '!', '-', '-']) {
            // A comment holds no attributes. Its end is looked for in place, so each character is
            // read once however many comments there are.
            i = chars[i + 4..]
                .windows(3)
                .position(|w| w == ['-', '-', '>'])
                .map_or(chars.len(), |end| i + 4 + end + 3);
            continue;
        }
        i += 1;
        // The tag's name.
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '>' {
            i += 1;
        }
        loop {
            while i < chars.len() && (chars[i].is_whitespace() || chars[i] == '/') {
                i += 1;
            }
            if i >= chars.len() || chars[i] == '>' {
                break;
            }
            let start = i;
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !matches!(chars[i], '=' | '>' | '/')
            {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            let mut value = String::new();
            if i < chars.len() && chars[i] == '=' {
                i += 1;
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                if i < chars.len() && matches!(chars[i], '"' | '\'') {
                    let quote = chars[i];
                    i += 1;
                    while i < chars.len() && chars[i] != quote {
                        value.push(chars[i]);
                        i += 1;
                    }
                    i += 1;
                } else {
                    while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '>' {
                        value.push(chars[i]);
                        i += 1;
                    }
                }
            }
            if !name.is_empty() {
                found.push((name, value));
            }
        }
    }
    found
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The most filtered image data a PNG may inflate to (1 GiB): an 8K 16-bit RGBA image is a
/// quarter of it.
const MAX_PNG_DATA: u64 = 1 << 30;

/// A PNG's size, once its signature, every chunk's CRC, a leading IHDR and the closing IEND check
/// out and its IDAT data inflates to exactly the rows IHDR describes.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut at = 8;
    let mut header = None;
    let mut zlib = Vec::new();
    loop {
        let length = be32(bytes, at)? as usize;
        let body = bytes.get(at + 4..(at + 8).checked_add(length)?)?;
        let crc = be32(bytes, at + 8 + length)?;
        if crc32(body) != crc {
            return None;
        }
        let (kind, data) = body.split_at(4);
        match kind {
            b"IHDR" if at == 8 && data.len() == 13 => header = Some(data),
            _ if at == 8 => return None,
            b"IDAT" => zlib.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + length;
    }
    let header = header?;
    let size = (be32(header, 0)?, be32(header, 4)?);
    let rows = png_rows(size.0, size.1, header)?;
    inflate_png(&zlib, &rows).map(|()| size)
}

/// The rows of a PNG's filtered image data, pass by pass: (bytes in a row with its filter byte,
/// rows). None for a header PNG doesn't allow, or data over `MAX_PNG_DATA`.
fn png_rows(width: u32, height: u32, header: &[u8]) -> Option<Vec<(u64, u64)>> {
    let (depth, colour) = (header[8], header[9]);
    let channels: u64 = match (colour, depth) {
        (0, 1 | 2 | 4 | 8 | 16) | (3, 1 | 2 | 4 | 8) => 1,
        (2, 8 | 16) => 3,
        (4, 8 | 16) => 2,
        (6, 8 | 16) => 4,
        _ => return None,
    };
    if header[10] != 0 || header[11] != 0 || header[12] > 1 {
        return None;
    }
    // Adam7's passes: first column and row, then the step between them.
    const ADAM7: [(u64, u64, u64, u64); 7] = [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ];
    let passes: &[(u64, u64, u64, u64)] = if header[12] == 1 {
        &ADAM7
    } else {
        &[(0, 0, 1, 1)]
    };
    let bits = channels * depth as u64;
    let mut rows = Vec::new();
    let mut total = 0u64;
    for &(x, y, dx, dy) in passes {
        let columns = (width as u64).saturating_sub(x).div_ceil(dx);
        let lines = (height as u64).saturating_sub(y).div_ceil(dy);
        if columns == 0 || lines == 0 {
            continue;
        }
        let row = 1 + (columns * bits).div_ceil(8);
        total += row * lines;
        rows.push((row, lines));
    }
    (total <= MAX_PNG_DATA).then_some(rows)
}

/// Reads a deflate stream's bits, least significant first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    buffer: u64,
    count: u32,
}

impl Bits<'_> {
    fn take(&mut self, n: u32) -> Option<u32> {
        while self.count < n {
            self.buffer |= (*self.data.get(self.at)? as u64) << self.count;
            self.at += 1;
            self.count += 8;
        }
        let value = (self.buffer & ((1u64 << n) - 1)) as u32;
        self.buffer >>= n;
        self.count -= n;
        Some(value)
    }

    /// Drops the bits left of the current byte; fewer than 8 are ever held.
    fn align(&mut self) {
        self.buffer = 0;
        self.count = 0;
    }
}

/// A canonical Huffman code: how many codes of each length, and the symbols in code order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Huffman> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            counts[length as usize] += 1;
        }
        counts[0] = 0;
        let mut left = 1i32;
        for count in &counts[1..] {
            left = (left << 1) - *count as i32;
            if left < 0 {
                return None;
            }
        }
        let mut offsets = [0u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbols[offsets[length as usize] as usize] = symbol as u16;
                offsets[length as usize] += 1;
            }
        }
        Some(Huffman { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for count in &self.counts[1..] {
            code |= bits.take(1)? as i32;
            let count = *count as i32;
            if code - first < count {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const WINDOW: usize = 1 << 15;

/// The inflated bytes of a PNG, checked as they come against the rows they must make up; only
/// deflate's 32 KiB window is kept.
struct PngData<'a> {
    rows: &'a [(u64, u64)],
    pass: usize,
    row: u64,
    column: u64,
    window: Vec<u8>,
    total: u64,
    adler: (u32, u32),
}

impl PngData<'_> {
    fn push(&mut self, byte: u8) -> Option<()> {
        // Past the last row is more data than the image holds.
        let &(length, rows) = self.rows.get(self.pass)?;
        if self.column == 0 && byte > 4 {
            return None;
        }
        self.column += 1;
        if self.column == length {
            self.column = 0;
            self.row += 1;
            if self.row == rows {
                self.row = 0;
                self.pass += 1;
            }
        }
        self.window[self.total as usize % WINDOW] = byte;
        self.total += 1;
        let (a, b) = self.adler;
        let a = (a + byte as u32) % 65521;
        self.adler = (a, (b + a) % 65521);
        Some(())
    }

    fn copy(&mut self, distance: usize, length: usize) -> Option<()> {
        if distance as u64 > self.total || distance > WINDOW {
            return None;
        }
        for _ in 0..length {
            let byte = self.window[(self.total - distance as u64) as usize % WINDOW];
            self.push(byte)?;
        }
        Some(())
    }
}

/// Inflates a PNG's zlib stream, checking its bytes make up exactly `rows`, each starting with a
/// known filter, and that its Adler-32 matches.
fn inflate_png(zlib: &[u8], rows: &[(u64, u64)]) -> Option<()> {
    let (cmf, flg) = (*zlib.first()?, *zlib.get(1)?);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return None;
    }
    if flg & 0x20 != 0 {
        return None;
    }
    let mut bits = Bits {
        data: &zlib[2..],
        at: 0,
        buffer: 0,
        count: 0,
    };
    let mut out = PngData {
        rows,
        pass: 0,
        row: 0,
        column: 0,
        window: vec![0; WINDOW],
        total: 0,
        adler: (1, 0),
    };
    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            0 => {
                bits.align();
                let at = bits.at;
                let length = u16::from_le_bytes(bits.data.get(at..at + 2)?.try_into().ok()?);
                let check = u16::from_le_bytes(bits.data.get(at + 2..at + 4)?.try_into().ok()?);
                if length != !check {
                    return None;
                }
                for &byte in bits.data.get(at + 4..at + 4 + length as usize)? {
                    out.push(byte)?;
                }
                bits.at = at + 4 + length as usize;
            }
            1 => {
                let mut lengths = [8u8; 288];
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                let literals = Huffman::new(&lengths)?;
                let distances = Huffman::new(&[5; 30])?;
                inflate_block(&mut bits, &mut out, &literals, &distances)?;
            }
            2 => {
                let (literals, distances) = dynamic_codes(&mut bits)?;
                inflate_block(&mut bits, &mut out, &literals, &distances)?;
            }
            _ => return None,
        }
        if last == 1 {
            break;
        }
    }
    bits.align();
    let (a, b) = out.adler;
    (out.pass == rows.len() && be32(bits.data, bits.at)? == (b << 16 | a)).then_some(())
}

/// A dynamic block's literal/length and distance codes, read from its header.
fn dynamic_codes(bits: &mut Bits) -> Option<(Huffman, Huffman)> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let literals = bits.take(5)? as usize + 257;
    let distances = bits.take(5)? as usize + 1;
    let header = bits.take(4)? as usize + 4;
    if literals > 286 || distances > 30 {
        return None;
    }
    let mut code_lengths = [0u8; 19];
    for &i in &ORDER[..header] {
        code_lengths[i] = bits.take(3)? as u8;
    }
    let code = Huffman::new(&code_lengths)?;
    let mut lengths = vec![0u8; literals + distances];
    let mut i = 0;
    while i < lengths.len() {
        let symbol = code.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => (*lengths.get(i.checked_sub(1)?)?, 3 + bits.take(2)? as usize),
            17 => (0, 3 + bits.take(3)? as usize),
            18 => (0, 11 + bits.take(7)? as usize),
            _ => return None,
        };
        lengths.get_mut(i..i + repeat)?.fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        return None;
    }
    Some((
        Huffman::new(&lengths[..literals])?,
        Huffman::new(&lengths[literals..])?,
    ))
}

/// One compressed block's literals and copies, up to its end-of-block code.
fn inflate_block(
    bits: &mut Bits,
    out: &mut PngData,
    literals: &Huffman,
    distances: &Huffman,
) -> Option<()> {
    const LENGTH_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LENGTH_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DISTANCE_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DISTANCE_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    loop {
        let symbol = literals.decode(bits)? as usize;
        match symbol {
            0..=255 => out.push(symbol as u8)?,
            256 => return Some(()),
            _ => {
                let index = symbol - 257;
                let length = *LENGTH_BASE.get(index)? as usize
                    + bits.take(LENGTH_EXTRA[index] as u32)? as usize;
                let code = distances.decode(bits)? as usize;
                let distance = *DISTANCE_BASE.get(code)? as usize
                    + bits.take(DISTANCE_EXTRA[code] as u32)? as usize;
                out.copy(distance, length)?;
            }
        }
    }
}

/// A JPEG's size from its frame header, once its segments are whole, its quantization and
/// Huffman (or arithmetic) tables come before a well-formed scan, entropy-coded data follows the
/// scan header and the file ends with EOI.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xFF, 0xD8]) || !bytes.ends_with(&[0xFF, 0xD9]) || bytes.len() < 4 {
        return None;
    }
    let mut at = 2;
    let mut size = None;
    let (mut quantization, mut huffman, mut arithmetic) = (false, false, false);
    loop {
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            0xFF => {
                at += 1;
                continue;
            }
            0x01 | 0xD0..=0xD7 => {
                at += 2;
                continue;
            }
            // The image ends before any scan.
            0xD9 => return None,
            _ => {}
        }
        let length = be16(bytes, at + 2)? as usize;
        if length < 2 || at + 2 + length > bytes.len() - 2 {
            return None;
        }
        let segment = &bytes[at + 4..at + 2 + length];
        match marker {
            0xDB => quantization = true,
            0xC4 => huffman = true,
            0xC0..=0xCF if ![0xC8, 0xCC].contains(&marker) => {
                let components = *segment.get(5)? as usize;
                if components == 0 || length != 8 + 3 * components {
                    return None;
                }
                size = Some((be16(bytes, at + 7)? as u32, be16(bytes, at + 5)? as u32));
                arithmetic = marker >= 0xC9;
            }
            0xDA => {
                let components = *segment.first()? as usize;
                if !(1..=4).contains(&components) || length != 6 + 2 * components {
                    return None;
                }
                let entropy = &bytes[at + 2 + length..bytes.len() - 2];
                let tables = quantization && (huffman || arithmetic);
                return size.filter(|_| tables && !entropy.is_empty());
            }
            _ => {}
        }
        at += 2 + length;
    }
}

/// A WebP's size, once the RIFF length matches the file, its chunks are whole and it holds an
/// image: a VP8 key frame or VP8L bitstream with data after its header, first or after a VP8X
/// header (whose canvas is the size), or an animation frame.
fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(b"RIFF") || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let riff = le32(bytes, 4)? as usize;
    if riff.checked_add(8)? != bytes.len() && riff.checked_add(9)? != bytes.len() {
        return None;
    }
    let mut chunks = Vec::new();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = le32(bytes, at + 4)? as usize;
        let data = bytes.get(at + 8..(at + 8).checked_add(size)?)?;
        chunks.push((&bytes[at..at + 4], data));
        at += 8 + size + size % 2;
    }
    let image = |kind: &[u8], data: &[u8]| -> Option<(u32, u32)> {
        match kind {
            b"VP8L" if data.first() == Some(&0x2F) && data.len() > 5 => {
                let bits = le32(data, 1)?;
                (bits >> 29 == 0).then_some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
            }
            b"VP8 " if data.get(3..6)? == [0x9D, 0x01, 0x2A] && data.len() > 10 => {
                let tag = data[0] as u32 | (data[1] as u32) << 8 | (data[2] as u32) << 16;
                if tag & 1 != 0 || (tag >> 5) as usize > data.len() - 10 {
                    return None;
                }
                let side = |at: usize| {
                    Some(
                        (u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?) & 0x3FFF)
                            as u32,
                    )
                };
                Some((side(6)?, side(8)?))
            }
            b"ANMF" if data.len() > 16 => Some((1, 1)),
            _ => None,
        }
    };
    let (first, data) = *chunks.first()?;
    if first != b"VP8X" {
        return image(first, data);
    }
    let le24 = |at: usize| -> Option<u32> {
        let b = data.get(at..at + 3)?;
        Some(b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16)
    };
    let canvas = (le24(4)? + 1, le24(7)? + 1);
    chunks[1..]
        .iter()
        .find(|(kind, _)| matches!(*kind, b"VP8 " | b"VP8L" | b"ANMF"))
        .and_then(|(kind, data)| image(kind, data))
        .map(|_| canvas)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One hash of the whole package: its sorted paths and their hashes.
fn package_hash(files: &[ManifestFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(file.sha256.as_bytes());
        hasher.update(b"\n");
    }
    hex(&hasher.finalize())
}

/// Writes the package into the run's logs and the worktree's copy, then the manifest. The
/// manifest is what marks the package frozen, so it is published last, once every copy is whole.
fn freeze(
    run: &DesignPackageRun,
    scanned: &Scanned,
    scope: Option<&DesignScope>,
) -> Result<(), String> {
    let design = run.run_logs.join(RUN_DESIGN_DIR);
    let manifest = design.join(MANIFEST_FILE);
    match std::fs::remove_file(&manifest) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!(
                "the old design manifest could not be removed ({:?})",
                e.kind()
            ))
        }
        _ => {}
    }
    write_tree(&design.join(PACKAGE_DIR), scanned, false)
        .map_err(|e| format!("the design package could not be frozen: {e}"))?;
    copy_to_worktree(run, scanned)?;
    // The feature and its scope, recorded with the frozen copy; a package frozen for its own feature's
    // whole page records none.
    let scope_file = design.join(SCOPE_FILE);
    match scope {
        Some(scope) => {
            let json = serde_json::to_vec_pretty(scope)
                .map_err(|e| format!("the design scope could not be written ({e})"))?;
            std::fs::write(&scope_file, json)
                .map_err(|e| format!("the design scope could not be written ({:?})", e.kind()))?;
        }
        None => match std::fs::remove_file(&scope_file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "the old design scope could not be removed ({:?})",
                    e.kind()
                ))
            }
            _ => {}
        },
    }
    let json = serde_json::to_vec_pretty(&scanned.manifest)
        .map_err(|e| format!("the design manifest could not be written ({e})"))?;
    // Written whole under another name, then renamed into place.
    let partial = design.join(format!("{MANIFEST_FILE}.partial"));
    std::fs::write(&partial, json)
        .and_then(|()| std::fs::rename(&partial, &manifest))
        .map_err(|e| format!("the design manifest could not be written ({:?})", e.kind()))
}

/// The implementer's read-only copy at the worktree's `.orq-design/`, kept out of git.
fn copy_to_worktree(run: &DesignPackageRun, scanned: &Scanned) -> Result<(), String> {
    // Kept out of git before any file is written.
    crate::local_files::exclude_locally(run.worktree, WORKTREE_COPY_DIR)
        .map_err(|e| format!("{WORKTREE_COPY_DIR} could not be kept out of git: {e}"))?;
    write_tree(&run.worktree.join(WORKTREE_COPY_DIR), scanned, true)
        .map_err(|e| format!("the worktree's {WORKTREE_COPY_DIR} could not be written: {e}"))
}

/// Replaces `root` with the package's files; `read_only` marks each file so. Folders stay
/// writable, so a worktree holding the copy can still be removed.
fn write_tree(root: &Path, scanned: &Scanned, read_only: bool) -> Result<(), String> {
    let failed = |what: &str, e: std::io::Error| format!("{what} ({:?})", e.kind());
    match std::fs::symlink_metadata(root) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(root),
        Ok(_) => std::fs::remove_file(root),
        Err(_) => Ok(()),
    }
    .map_err(|e| failed("the old copy was not removed", e))?;
    std::fs::create_dir_all(root).map_err(|e| failed("its folder", e))?;
    for (relative, bytes) in &scanned.contents {
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| failed(relative, e))?;
        }
        std::fs::write(&target, bytes).map_err(|e| failed(relative, e))?;
        if read_only {
            let mut permissions = std::fs::metadata(&target)
                .map_err(|e| failed(relative, e))?
                .permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&target, permissions).map_err(|e| failed(relative, e))?;
        }
    }
    Ok(())
}
