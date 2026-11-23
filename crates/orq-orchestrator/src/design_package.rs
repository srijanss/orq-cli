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

/// Checks the feature's package in the main checkout and freezes it for the run.
pub fn start_design_package(run: &DesignPackageRun) -> DesignPackageOutcome {
    let dir = match package_folder(run) {
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
    let request = files_needed(run, &missing);
    let Some(scanned) = scanned.filter(|_| has_reference && request.files.is_empty()) else {
        // The person is shown where the files go.
        for sub in ["references", "assets"] {
            if let Err(e) = std::fs::create_dir_all(dir.join(sub)) {
                return DesignPackageOutcome::Refused(vec![format!(
                    "{DESIGNS_DIR}/{}/{sub} could not be created ({:?})",
                    run.feature,
                    e.kind()
                )]);
            }
        }
        return DesignPackageOutcome::FilesNeeded(request);
    };
    match freeze(run, &scanned) {
        Ok(()) => DesignPackageOutcome::Frozen(scanned.manifest),
        Err(problem) => DesignPackageOutcome::Refused(vec![problem]),
    }
}

/// Checks the package again when a paused run resumes. A run that froze its package goes on only
/// while the main checkout's package is still the one frozen; a run still waiting for design files
/// checks the folder again as if it were starting.
pub fn resume_design_package(run: &DesignPackageRun) -> DesignPackageOutcome {
    let saved = run.run_logs.join(RUN_DESIGN_DIR).join(MANIFEST_FILE);
    let text = match std::fs::read(&saved) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return start_design_package(run),
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
    let current = package_folder(run).and_then(|dir| {
        if std::fs::symlink_metadata(&dir).is_err() {
            return Err(vec![format!("{DESIGNS_DIR}/{} is gone", run.feature)]);
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
        "the design package changed since this run started: {DESIGNS_DIR}/{} is not the package \
         frozen for the run ({now}). Put the package back as it was, or start a new run to use the new one.",
        run.feature
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
/// are left to the design preparer.
fn files_needed(run: &DesignPackageRun, missing: &MissingPieces) -> DesignFilesNeeded {
    let at = |path: &str| format!("{DESIGNS_DIR}/{}/{path}", run.feature);
    let mut files = Vec::new();
    for name in &missing.viewports {
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

/// `designs/<feature>` in the main checkout, refused when it or `designs` is a symlink.
fn package_folder(run: &DesignPackageRun) -> Result<PathBuf, Vec<String>> {
    let plain = !run.feature.is_empty()
        && !run.feature.starts_with('.')
        && run
            .feature
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !plain {
        return Err(vec![format!(
            "{:?} is not a usable design package name: designs/<feature> must stay inside the designs folder",
            run.feature
        )]);
    }
    let designs = run.main_checkout.join(DESIGNS_DIR);
    let dir = designs.join(run.feature);
    for (path, shown) in [
        (&designs, DESIGNS_DIR.to_string()),
        (&dir, format!("{DESIGNS_DIR}/{}", run.feature)),
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
/// `@import`) that isn't a local fragment or a data image. Read as text, case-insensitively, with
/// XML character references decoded first.
fn svg_problem(bytes: &[u8]) -> Option<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Some("is not a UTF-8 SVG".to_string());
    };
    let raw = text.to_ascii_lowercase();
    if raw.contains("<!entity") {
        return Some("is refused: it declares an <!ENTITY>".to_string());
    }
    if raw.contains("<?xml-stylesheet") {
        return Some("is refused: it links a stylesheet".to_string());
    }
    let text = decode_references(&raw).to_ascii_lowercase();
    if text.contains("<script") {
        return Some("is refused: it has a <script>".to_string());
    }
    if text.contains("<foreignobject") {
        return Some("is refused: it has a <foreignObject>".to_string());
    }
    if text.contains("@import") {
        return Some("is refused: its CSS has an @import".to_string());
    }
    for target in css_urls(&text) {
        if !local_resource(&target) {
            return Some(format!(
                "is refused: url({target}) is neither a local #fragment nor a data image"
            ));
        }
    }
    let mut styles = style_elements(&text);
    for (name, value) in svg_attributes(&text) {
        if name.starts_with("on") {
            return Some(format!("is refused: it has the event attribute {name}"));
        }
        if (name == "href" || name.ends_with(":href") || name == "src") && !local_resource(&value)
        {
            return Some(format!(
                "is refused: its {name} is neither a local #fragment nor a data image"
            ));
        }
        if name == "style" {
            styles.push(value);
        }
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

/// The target of every CSS `url(...)` in `text`, quotes removed.
fn css_urls(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("url(") {
        rest = rest[at + 4..].trim_start();
        let target = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or(""),
            _ => rest.split(')').next().unwrap_or(""),
        };
        found.push(target.trim().to_string());
    }
    found
}

/// The contents of every `<style>` element in `text`.
fn style_elements(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("<style") {
        rest = &rest[at + 6..];
        let end = rest.find("</style").unwrap_or(rest.len());
        found.push(rest[..end].to_string());
        rest = &rest[end..];
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
            // A comment holds no attributes.
            let rest: String = chars[i + 4..].iter().collect();
            i = rest
                .find("-->")
                .map_or(chars.len(), |end| i + 4 + rest[..end].chars().count() + 3);
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

/// A PNG's size, once its signature, every chunk's CRC, a leading IHDR, an IDAT and the closing
/// IEND check out.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut at = 8;
    let mut size = None;
    let mut has_data = false;
    loop {
        let length = be32(bytes, at)? as usize;
        let body = bytes.get(at + 4..(at + 8).checked_add(length)?)?;
        let crc = be32(bytes, at + 8 + length)?;
        if crc32(body) != crc {
            return None;
        }
        let (kind, data) = body.split_at(4);
        match kind {
            b"IHDR" if at == 8 && data.len() == 13 => size = Some((be32(data, 0)?, be32(data, 4)?)),
            _ if at == 8 => return None,
            b"IDAT" => has_data = true,
            b"IEND" => return has_data.then_some(size?),
            _ => {}
        }
        at += 12 + length;
    }
}

/// A JPEG's size from its frame header, once its segments up to the scan are whole and it ends
/// with EOI.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xFF, 0xD8]) || !bytes.ends_with(&[0xFF, 0xD9]) || bytes.len() < 4 {
        return None;
    }
    let mut at = 2;
    let mut size = None;
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
            // The image ends, or its scan starts: what follows is entropy-coded data.
            0xD9 | 0xDA => return size,
            _ => {}
        }
        let length = be16(bytes, at + 2)? as usize;
        if length < 2 || at + 2 + length > bytes.len() {
            return None;
        }
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            if length < 8 {
                return None;
            }
            size = Some((be16(bytes, at + 7)? as u32, be16(bytes, at + 5)? as u32));
        }
        at += 2 + length;
    }
}

/// A WebP's size from its first chunk (VP8, VP8L or VP8X), once the RIFF length matches the file.
fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(b"RIFF") || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let riff = le32(bytes, 4)? as usize;
    if riff.checked_add(8)? != bytes.len() && riff.checked_add(9)? != bytes.len() {
        return None;
    }
    let chunk = le32(bytes, 16)? as usize;
    let data = bytes.get(20..20usize.checked_add(chunk)?)?;
    let le24 = |at: usize| -> Option<u32> {
        let b = data.get(at..at + 3)?;
        Some(b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16)
    };
    match bytes.get(12..16)? {
        b"VP8L" if data.first() == Some(&0x2F) => {
            let bits = le32(data, 1)?;
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        b"VP8X" => Some((le24(4)? + 1, le24(7)? + 1)),
        b"VP8 " if data.get(3..6)? == [0x9D, 0x01, 0x2A] => {
            let side = |at: usize| {
                Some((u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?) & 0x3FFF) as u32)
            };
            Some((side(6)?, side(8)?))
        }
        _ => None,
    }
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
fn freeze(run: &DesignPackageRun, scanned: &Scanned) -> Result<(), String> {
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
