use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use orq_git::ResolveError;
use orq_tui::{PopupModel, PopupView};

const USAGE: &str = "usage: orq --version | orq project inspect [--cwd <path>] | orq features --repo <path> | orq feature set-status --repo <path> --id <name> --status pending | orq feature add --repo <path> --name <name> [--description <text>] [--depends-on <name>]... [--parallelizable yes|no] [--test-file <path>] [--target-file <path>]... | orq feature edit --repo <path> --name <name> [same field flags as add] | orq ui [--snapshot] | orq doctor herdr [--cwd <path>]";

/// Width and height of the frame printed by `orq ui --snapshot`.
const SNAPSHOT_SIZE: (u16, u16) = (100, 3);

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let word = |index: usize| args.get(index).and_then(|arg| arg.to_str());

    match (word(0), word(1)) {
        (Some("--version"), None) => {
            println!("orq {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        (Some("project"), Some("inspect")) => project_inspect(&args[2..]),
        (Some("features"), _) => features(&args[1..]),
        (Some("feature"), Some("set-status")) => feature_set_status(&args[2..]),
        (Some("feature"), Some("add")) => feature_add(&args[2..]),
        (Some("feature"), Some("edit")) => feature_edit(&args[2..]),
        (Some("ui"), _) => ui(&args[1..]),
        (Some("doctor"), Some("herdr")) => doctor_herdr(&args[2..]),
        _ => usage_error(),
    }
}

fn usage_error() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// Diagnostics can carry repository-controlled text (paths, ledger names), so
/// control characters are shown escaped and never reach the terminal raw.
fn printable(text: &str) -> String {
    text.chars()
        .flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] })
        .collect()
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("orq: {}", printable(&message.to_string()));
    ExitCode::FAILURE
}

/// The directory `orq` was launched for: `--cwd`, else the Herdr active pane
/// directory, else the process working directory. Fallbacks are only consulted
/// when no higher-priority source is present, so a broken fallback never
/// defeats a valid explicit choice.
fn launch_cwd(explicit: Option<&Path>) -> Result<PathBuf, ExitCode> {
    let pane_cwd = if explicit.is_none() {
        match std::env::var_os("HERDR_ACTIVE_PANE_CWD") {
            None => None,
            Some(value) => match value.into_string() {
                Ok(value) => Some(value),
                Err(_) => return Err(fail("HERDR_ACTIVE_PANE_CWD is not valid UTF-8")),
            },
        }
    } else {
        None
    };
    let process_cwd = if explicit.is_none() && pane_cwd.is_none() {
        match std::env::current_dir() {
            Ok(dir) => dir,
            Err(err) => return Err(fail(format!("cannot determine the working directory: {err}"))),
        }
    } else {
        PathBuf::new() // never consulted: a higher-priority source is present
    };

    orq_herdr::resolve_launch_cwd(explicit, pane_cwd.as_deref(), &process_cwd)
        .map_err(|err| fail(launch_cwd_error_text(&err)))
}

fn launch_cwd_error_text(err: &orq_herdr::LaunchCwdError) -> String {
    use orq_herdr::{CwdSource, LaunchCwdError::*};
    let source = |source: &CwdSource| match source {
        CwdSource::Explicit => "--cwd",
        CwdSource::HerdrActivePaneCwd => "HERDR_ACTIVE_PANE_CWD",
        CwdSource::ProcessCwd => "the process working directory",
    };
    match err {
        Empty { source: s } => format!("invalid working directory: {} is empty", source(s)),
        NotFound { source: s, path } => {
            format!("invalid working directory: {} names {}, which is not a directory", source(s), path.display())
        }
    }
}

fn project_inspect(args: &[OsString]) -> ExitCode {
    let explicit: Option<PathBuf> = match args {
        [] => None,
        [flag, path] if flag == "--cwd" => Some(PathBuf::from(path)),
        _ => return usage_error(),
    };

    let cwd = match launch_cwd(explicit.as_deref()) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    let project = match orq_git::resolve_project(&cwd) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };

    let (Some(root), Some(common)) = (path_str(&project.root), path_str(&project.git_common_dir)) else {
        return fail("repository path is not valid UTF-8");
    };
    let json = serde_json::json!({
        "name": project.name,
        "root": root,
        "git_common_dir": common,
        "is_linked_worktree": project.is_linked_worktree,
    });
    println!("{json}");
    ExitCode::SUCCESS
}

/// `orq features --repo <path>`: the project's ledger as a JSON list. A ledger
/// that cannot be loaded prints its typed reason on stderr and no list.
fn features(args: &[OsString]) -> ExitCode {
    let repo = match args {
        [flag, path] if flag == "--repo" => PathBuf::from(path),
        _ => return usage_error(),
    };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let entries = match orq_features::load_ledger(&orq_features::ledger_path(&project)) {
        Ok(entries) => entries,
        Err(err) => return fail(ledger_error_text(&err)),
    };
    let list: Vec<serde_json::Value> = entries.iter().map(entry_json).collect();
    println!("{}", serde_json::Value::Array(list));
    ExitCode::SUCCESS
}

fn entry_json(entry: &orq_features::FeatureEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.feature_name,
        "status": status_text(&entry.status),
        "description": entry.description,
        "dependsOn": entry.depends_on,
    })
}

/// The flags `feature add` takes; a flag that is not given stays `None`, so
/// the field is left out of the ledger.
#[derive(Default)]
struct FieldFlags {
    repo: Option<PathBuf>,
    name: Option<String>,
    description: Option<String>,
    depends_on: Option<Vec<String>>,
    parallelizable: Option<bool>,
    test_file: Option<String>,
    target_files: Option<Vec<String>>,
}

/// Parses `--flag value` pairs.
/// Errors are the message to show.
fn parse_field_flags(args: &[OsString]) -> Result<FieldFlags, String> {
    let mut flags = FieldFlags::default();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let flag = flag.to_str().ok_or("arguments must be valid UTF-8")?;
        let value = iter.next().ok_or(format!("{flag} needs a value"))?;
        let text = || value.to_str().map(str::to_string).ok_or("arguments must be valid UTF-8".to_string());
        match flag {
            "--repo" => flags.repo = Some(PathBuf::from(value)),
            "--name" => flags.name = Some(text()?),
            "--description" => flags.description = Some(text()?),
            "--depends-on" => flags.depends_on.get_or_insert_with(Vec::new).push(text()?),
            "--test-file" => flags.test_file = Some(text()?),
            "--target-file" => flags.target_files.get_or_insert_with(Vec::new).push(text()?),
            "--parallelizable" => {
                flags.parallelizable = Some(match text()?.as_str() {
                    "yes" => true,
                    "no" => false,
                    other => return Err(format!("--parallelizable must be yes or no, not {other:?}")),
                })
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(flags)
}

/// The saved entry as printed: the list fields plus the optional ones that are present.
fn saved_entry_json(entry: &orq_features::FeatureEntry) -> serde_json::Value {
    let mut json = entry_json(entry);
    let map = json.as_object_mut().expect("entry_json is an object");
    if let Some(flag) = entry.parallelizable {
        map.insert("parallelizable".into(), flag.into());
    }
    if let Some(test_file) = &entry.test_file {
        map.insert("testFile".into(), test_file.clone().into());
    }
    if !entry.target_files.is_empty() {
        map.insert("targetFiles".into(), entry.target_files.clone().into());
    }
    json
}

/// `orq feature add --repo <path> --name <name> [...]`: appends a draft and
/// prints it. Never starts an agent, worktree or run.
fn feature_add(args: &[OsString]) -> ExitCode {
    let flags = match parse_field_flags(args) {
        Ok(flags) => flags,
        Err(message) => return fail(message),
    };
    let Some(repo) = flags.repo else { return fail("--repo is required") };
    let Some(name) = flags.name else { return fail("--name is required") };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let path = orq_features::ledger_path(&project);
    let loaded = match orq_features::LoadedLedger::load_or_new(&path) {
        Ok(loaded) => loaded,
        Err(err) => return fail(ledger_error_text(&err)),
    };
    let draft = orq_features::FeatureDraft {
        feature_name: name,
        description: flags.description,
        depends_on: flags.depends_on,
        parallelizable: flags.parallelizable,
        test_file: flags.test_file,
        target_files: flags.target_files,
    };
    match loaded.append(&draft) {
        Ok(entry) => {
            println!("{}", saved_entry_json(&entry));
            ExitCode::SUCCESS
        }
        Err(err) => fail(err),
    }
}

/// `orq feature edit --repo <path> --name <name> [...]`: changes only the
/// given fields of an existing feature. There is no rename or status flag.
fn feature_edit(args: &[OsString]) -> ExitCode {
    let flags = match parse_field_flags(args) {
        Ok(flags) => flags,
        Err(message) => return fail(message),
    };
    let Some(repo) = flags.repo else { return fail("--repo is required") };
    let Some(name) = flags.name else { return fail("--name is required") };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let loaded = match orq_features::LoadedLedger::load(&orq_features::ledger_path(&project)) {
        Ok(loaded) => loaded,
        Err(err) => return fail(ledger_error_text(&err)),
    };
    let edits = orq_features::FieldEdits {
        description: flags.description,
        depends_on: flags.depends_on,
        parallelizable: flags.parallelizable,
        test_file: flags.test_file,
        target_files: flags.target_files,
    };
    match loaded.edit_fields(&name, &edits) {
        Ok(entry) => {
            println!("{}", saved_entry_json(&entry));
            ExitCode::SUCCESS
        }
        Err(orq_features::EditFieldsError::UnknownFeature) => fail(format!("no feature named {name:?}")),
        Err(err) => fail(err),
    }
}

/// `orq feature set-status --repo <path> --id <name> --status pending`:
/// promotes a draft and prints the updated entry. Every refusal exits
/// non-zero with a specific message and leaves the ledger untouched.
fn feature_set_status(args: &[OsString]) -> ExitCode {
    let (repo, id, status) = match args {
        [r, repo, i, id, s, status] if r == "--repo" && i == "--id" && s == "--status" => {
            match (id.to_str(), status.to_str()) {
                (Some(id), Some(status)) => (PathBuf::from(repo), id, status),
                _ => return usage_error(),
            }
        }
        _ => return usage_error(),
    };
    if status != "pending" {
        return fail(format!("cannot set status {status:?}: only --status pending is supported"));
    }
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let path = orq_features::ledger_path(&project);
    match orq_features::promote_feature(&path, id) {
        Ok(entry) => {
            println!("{}", entry_json(&entry));
            ExitCode::SUCCESS
        }
        Err(err) => fail(promote_error_text(&err, id)),
    }
}

fn promote_error_text(err: &orq_features::PromoteError, id: &str) -> String {
    use orq_features::PromoteError::*;
    match err {
        Ledger(err) => ledger_error_text(err),
        UnknownFeature(_) => format!("unknown feature: {id}"),
        UnrecognisedStatus(text) => format!("feature {id} has unrecognised status {text:?} and is left alone"),
        IllegalTransition(t) => format!("feature {id}: {:?} to {:?} is not allowed", t.from, t.to),
        Conflict => "ledger changed while it was being edited; nothing was written, try again".to_string(),
        Locked => "ledger is locked by another writer; nothing was written, try again".to_string(),
        Io(err) => format!("ledger could not be written: {err}"),
    }
}

fn status_text(status: &orq_features::LedgerStatus) -> &str {
    use orq_core::FeatureStatus::*;
    match status {
        orq_features::LedgerStatus::Known(Draft) => "draft",
        orq_features::LedgerStatus::Known(Pending) => "pending",
        orq_features::LedgerStatus::Known(InProgress) => "in_progress",
        orq_features::LedgerStatus::Known(Completed) => "completed",
        orq_features::LedgerStatus::Known(Abandoned) => "abandoned",
        orq_features::LedgerStatus::Unrecognised(text) => text,
    }
}

fn ledger_error_text(err: &orq_features::LedgerError) -> String {
    use orq_features::LedgerError::*;
    match err {
        Missing { path } => format!("ledger missing: {}", path.display()),
        Unreadable { path, source } => format!("ledger unreadable: {}: {source}", path.display()),
        Malformed { path, reason, .. } => format!("ledger malformed: {}: {reason}", path.display()),
    }
}

const HERDR_VARS: [&str; 6] = [
    "HERDR_ACTIVE_PANE_CWD",
    "HERDR_ACTIVE_PANE_ID",
    "HERDR_ACTIVE_TAB_ID",
    "HERDR_ACTIVE_WORKSPACE_ID",
    "HERDR_SOCKET_PATH",
    "HERDR_BIN_PATH",
];

/// `orq doctor herdr [--cwd <path>]`: a read-only report of what Herdr passed
/// in and what Orq resolved from it. An unusable launch directory is an error,
/// never a silent fallback to another project.
/// How long the Herdr version probe may run before it is abandoned.
const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The most version output that is read; anything longer is not a version.
const VERSION_PROBE_MAX_BYTES: u64 = 1024;

/// `<bin> --version` stdout, or `None` when it fails, cannot start or does not
/// finish in time. The child is killed and reaped on every path.
fn herdr_version(bin: &OsString) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // One byte past the limit is enough to know the output is too long.
        let _ = stdout.take(VERSION_PROBE_MAX_BYTES + 1).read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });

    // One deadline covers both the output and the process exiting: a child
    // that closes stdout and keeps running is cut off the same way.
    let deadline = std::time::Instant::now() + VERSION_PROBE_TIMEOUT;
    let bytes = rx
        .recv_timeout(VERSION_PROBE_TIMEOUT)
        .ok()
        .filter(|bytes| bytes.len() as u64 <= VERSION_PROBE_MAX_BYTES);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if bytes.is_some() && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = bytes?;
    status?.success().then(|| String::from_utf8_lossy(&bytes).trim().to_string())
}

fn doctor_herdr(args: &[OsString]) -> ExitCode {
    let explicit: Option<PathBuf> = match args {
        [] => None,
        [flag, path] if flag == "--cwd" => Some(PathBuf::from(path)),
        _ => return usage_error(),
    };

    for var in HERDR_VARS {
        match std::env::var_os(var) {
            Some(value) => println!("{var}: {}", printable(&value.to_string_lossy())),
            None => println!("{var}: (not set)"),
        }
    }
    let herdr_bin = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| OsString::from("herdr"));
    match herdr_version(&herdr_bin) {
        Some(version) => println!("herdr version: {}", printable(&version)),
        None => println!("herdr version: unavailable ({})", printable(&herdr_bin.to_string_lossy())),
    }

    let cwd = match launch_cwd(explicit.as_deref()) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    println!("launch directory: {}", printable(&cwd.to_string_lossy()));
    let project = match orq_git::resolve_project(&cwd) {
        Ok(project) => project,
        Err(ResolveError::NotARepository { .. }) => {
            println!("repo root: none (not a Git repository)");
            return ExitCode::FAILURE;
        }
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    println!("repo root: {}", printable(&project.root.to_string_lossy()));
    println!("linked worktree: {}", if project.is_linked_worktree { "yes" } else { "no" });
    let ledger = orq_features::ledger_path(&project);
    println!("ledger: {}", printable(&ledger.to_string_lossy()));
    match orq_features::load_ledger(&ledger) {
        Ok(entries) => println!("ledger status: ok ({} features)", entries.len()),
        Err(orq_features::LedgerError::Missing { .. }) => println!("ledger status: missing"),
        Err(orq_features::LedgerError::Unreadable { source, .. }) => {
            println!("ledger status: unreadable ({})", printable(&source.to_string()));
        }
        Err(orq_features::LedgerError::Malformed { reason, .. }) => {
            println!("ledger status: malformed ({})", printable(&reason));
        }
    }
    ExitCode::SUCCESS
}

fn ui(args: &[OsString]) -> ExitCode {
    let snapshot = match args {
        [] => false,
        [flag] if flag == "--snapshot" => true,
        _ => return usage_error(),
    };

    let cwd = match launch_cwd(None) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    let view = match view_for(&cwd) {
        Ok(view) => view,
        Err(code) => return code,
    };
    if !snapshot {
        let model = match view {
            PopupView::Project(model) => model,
            PopupView::NotARepository { path } => PopupModel {
                project_name: "not a Git repository".to_string(),
                repo_path: path,
            },
        };
        let result = match orq_git::resolve_project(&cwd) {
            Ok(project) => {
                let mut backend = LedgerBackend::new(orq_features::ledger_path(&project));
                orq_tui::run_browser_on_terminal(&model, &mut backend)
            }
            Err(_) => orq_tui::run_on_terminal(&model).map(|_| ()),
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(err),
        };
    }
    let (width, height) = SNAPSHOT_SIZE;
    println!("{}", orq_tui::render_snapshot(&view, width, height));
    ExitCode::SUCCESS
}

/// The popup's data source: the project's own ledger, loaded when the popup
/// opens (and on reload) and promoted against exactly those loaded bytes, so a
/// file changed behind the popup's back is a conflict rather than overwritten.
struct LedgerBackend {
    path: PathBuf,
    loaded: Option<orq_features::LoadedLedger>,
    /// The last load failed only because the file does not exist yet.
    absent: bool,
    /// The user-level settings file, when `XDG_CONFIG_HOME` or `HOME` give one.
    settings_path: Option<PathBuf>,
    /// The settings as last loaded for the tab: saves are checked against
    /// this snapshot, so an outside edit is a conflict, not an overwrite.
    settings_snapshot: Option<orq_features::Settings>,
    /// The last generated proposal, kept so accepting names what was reviewed.
    staged: Vec<orq_features::ReviewedCandidate>,
    /// Replaces the PATH lookup of the harness program (tests).
    program_override: Option<PathBuf>,
}

/// The executable for `harness`: a bare name found on PATH, never a path.
fn find_program(harness: &str) -> Option<PathBuf> {
    if harness.is_empty() || harness.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(harness)).find(|candidate| {
        use std::os::unix::fs::PermissionsExt;
        candidate.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

impl LedgerBackend {
    fn new(path: PathBuf) -> Self {
        let settings_path = orq_features::settings_path(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        );
        Self {
            path,
            loaded: None,
            absent: false,
            settings_path,
            settings_snapshot: None,
            staged: Vec::new(),
            program_override: None,
        }
    }

    fn settings_file(&self) -> Result<&Path, String> {
        self.settings_path
            .as_deref()
            .ok_or_else(|| "no settings location: set XDG_CONFIG_HOME or HOME to an absolute path".to_string())
    }
}

fn agent_setting(profile: Option<orq_features::AgentProfile>) -> orq_tui::AgentSetting {
    profile
        .map(|p| orq_tui::AgentSetting { harness: p.harness, profile: p.profile })
        .unwrap_or_default()
}

impl orq_tui::BrowserBackend for LedgerBackend {
    fn load(&mut self) -> Result<Vec<orq_tui::BrowserEntry>, orq_tui::LedgerErrorView> {
        match orq_features::LoadedLedger::load(&self.path) {
            Ok(loaded) => {
                let entries = loaded
                    .entries()
                    .iter()
                    .map(|entry| orq_tui::BrowserEntry {
                        name: entry.feature_name.clone(),
                        description: entry.description.clone(),
                        status: status_text(&entry.status).to_string(),
                        depends_on: entry.depends_on.clone(),
                        test_file: entry.test_file.clone(),
                    })
                    .collect();
                self.loaded = Some(loaded);
                self.absent = false;
                Ok(entries)
            }
            Err(err) => {
                self.absent = matches!(err, orq_features::LedgerError::Missing { .. });
                // An absent file is snapshotted as "absent", so a file that
                // appears before the first add is a conflict, not an append.
                self.loaded = self.absent.then(|| orq_features::LoadedLedger::absent(&self.path));
                Err(orq_tui::LedgerErrorView {
                    path: self.path.to_string_lossy().into_owned(),
                    reason: ledger_error_text(&err),
                })
            }
        }
    }

    fn ledger_absent(&self) -> bool {
        self.absent
    }

    fn promote(&mut self, feature: &str) -> orq_tui::PromoteOutcome {
        let Some(loaded) = &self.loaded else {
            return orq_tui::PromoteOutcome::Failed { reason: "ledger is not loaded".to_string() };
        };
        match loaded.promote(feature, None) {
            Ok(_) => {
                // Our own write made the snapshot stale; re-snapshot so the next
                // promote in a bulk run is not a conflict with ourselves.
                self.loaded = orq_features::LoadedLedger::load(&self.path).ok();
                orq_tui::PromoteOutcome::Saved
            }
            Err(err) => orq_tui::PromoteOutcome::Failed { reason: promote_error_text(&err, feature) },
        }
    }

    fn feature_fields(&mut self, name: &str) -> Option<orq_tui::FeatureFields> {
        let entry = self.loaded.as_ref()?.entries().iter().find(|e| e.feature_name == name)?;
        Some(orq_tui::FeatureFields {
            name: entry.feature_name.clone(),
            description: entry.description.clone(),
            depends_on: entry.depends_on.clone(),
            parallelizable: entry.parallelizable,
            test_file: entry.test_file.clone(),
            target_files: entry.target_files.clone(),
        })
    }

    fn edit_feature(&mut self, fields: &orq_tui::FeatureFields) -> orq_tui::FormSaveOutcome {
        use orq_features::{EditFieldsError, SaveError};
        use orq_tui::FormSaveOutcome;
        let Some(loaded) = self.loaded.take() else {
            return FormSaveOutcome::Failed { reason: "ledger is not loaded".to_string() };
        };
        let Some(current) = loaded.entries().iter().find(|e| e.feature_name == fields.name).cloned() else {
            self.loaded = Some(loaded);
            return FormSaveOutcome::Failed { reason: format!("no feature named {}", fields.name) };
        };
        // Only what differs is sent; blank text leaves a field as it is.
        let changed_text = |new: &Option<String>, old: &Option<String>| match new {
            Some(value) if new != old => Some(value.clone()),
            _ => None,
        };
        let edits = orq_features::FieldEdits {
            description: changed_text(&fields.description, &current.description),
            depends_on: (fields.depends_on != current.depends_on).then(|| fields.depends_on.clone()),
            parallelizable: match fields.parallelizable {
                Some(value) if Some(value) != current.parallelizable => Some(value),
                _ => None,
            },
            test_file: changed_text(&fields.test_file, &current.test_file),
            target_files: (!fields.target_files.is_empty() && fields.target_files != current.target_files)
                .then(|| fields.target_files.clone()),
        };
        let outcome = match loaded.edit_fields(&fields.name, &edits) {
            Ok(_) => FormSaveOutcome::Saved,
            Err(err @ EditFieldsError::Invalid(_)) => FormSaveOutcome::Invalid { reason: err.to_string() },
            Err(err @ EditFieldsError::Save(SaveError::Conflict)) => {
                FormSaveOutcome::Conflict { reason: err.to_string() }
            }
            Err(err) => FormSaveOutcome::Failed { reason: err.to_string() },
        };
        // As for add: a refused save keeps its snapshot, anything else
        // reloads against the file as it now is.
        self.loaded = match outcome {
            FormSaveOutcome::Invalid { .. } | FormSaveOutcome::Failed { .. } => Some(loaded),
            _ => orq_features::LoadedLedger::load(&self.path).ok(),
        };
        outcome
    }

    fn add_feature(&mut self, fields: &orq_tui::FeatureFields) -> orq_tui::FormSaveOutcome {
        use orq_features::{AppendError, SaveError};
        use orq_tui::FormSaveOutcome;
        // Appended against the snapshot the popup showed, so a file changed
        // behind its back is a conflict; an absent file is created.
        let loaded = match self.loaded.take() {
            Some(loaded) => loaded,
            None => match orq_features::LoadedLedger::load_or_new(&self.path) {
                Ok(loaded) => loaded,
                Err(err) => return FormSaveOutcome::Failed { reason: ledger_error_text(&err) },
            },
        };
        let draft = orq_features::FeatureDraft {
            feature_name: fields.name.clone(),
            description: fields.description.clone(),
            depends_on: (!fields.depends_on.is_empty()).then(|| fields.depends_on.clone()),
            parallelizable: fields.parallelizable,
            test_file: fields.test_file.clone(),
            target_files: (!fields.target_files.is_empty()).then(|| fields.target_files.clone()),
        };
        let outcome = match loaded.append(&draft) {
            Ok(_) => FormSaveOutcome::Saved,
            Err(err @ AppendError::Invalid(_)) => FormSaveOutcome::Invalid { reason: err.to_string() },
            Err(err @ AppendError::Save(SaveError::Conflict)) => {
                FormSaveOutcome::Conflict { reason: err.to_string() }
            }
            Err(err) => FormSaveOutcome::Failed { reason: err.to_string() },
        };
        // A refused save wrote nothing, so the snapshot still stands and a
        // retry is checked against it; after a write or a conflict the
        // browser reloads, so take a fresh snapshot of the file as it is.
        self.loaded = match outcome {
            FormSaveOutcome::Invalid { .. } | FormSaveOutcome::Failed { .. } => Some(loaded),
            _ => orq_features::LoadedLedger::load(&self.path).ok(),
        };
        outcome
    }

    fn generate_candidates(&mut self) -> Result<Vec<orq_tui::CandidateView>, String> {
        use orq_features::{CandidateProblem, Verdict};
        use orq_tui::{CandidateKind, CandidateView};
        self.staged.clear();
        let repo_root = self.path.parent().ok_or("the feature file has no directory")?.to_path_buf();
        let spec = orq_features::spec_file_candidates(&repo_root)
            .into_iter()
            .next()
            .ok_or("no Markdown spec file was found at the repository root")?;
        let settings = orq_features::Settings::load(self.settings_file()?).map_err(|e| e.to_string())?;
        let profile = settings
            .generation_agent()
            .map_err(|e| e.to_string())?
            .ok_or(orq_features::GenerationError::NotConfigured.to_string())?;
        let program = match &self.program_override {
            Some(program) => program.clone(),
            None => find_program(&profile.harness).ok_or_else(|| {
                orq_features::GenerationError::AgentNotFound { harness: profile.harness.clone() }.to_string()
            })?,
        };
        let artifact_dir = orq_features::generation_artifact_dir(
            std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
        .ok_or("no cache location: set XDG_CACHE_HOME or HOME")?;
        let request = orq_features::GenerationRequest {
            program: &program,
            profile: &profile,
            repo_root: &repo_root,
            spec: &spec,
            artifact_dir: &artifact_dir,
            timeout: std::time::Duration::from_secs(600),
        };
        let artifact = orq_features::run_generation(&request).map_err(|e| e.to_string())?;
        let text = std::fs::read_to_string(&artifact)
            .map_err(|e| format!("could not read the agent output {}: {e}", artifact.display()))?;
        let candidates = orq_features::extract_candidates(&text)
            .map_err(|e| format!("{e} (agent output kept at {})", artifact.display()))?;
        let existing = match orq_features::LoadedLedger::load_or_new(&self.path) {
            Ok(loaded) => loaded.entries().to_vec(),
            Err(err) => return Err(ledger_error_text(&err)),
        };
        let reviewed = orq_features::review_candidates(&existing, candidates);
        let views = reviewed
            .iter()
            .map(|r| {
                let draft = &r.candidate.draft;
                let kind = match &r.verdict {
                    Verdict::New => CandidateKind::New,
                    Verdict::DuplicateOfExisting => CandidateKind::Duplicate("already in the feature file".into()),
                    Verdict::DuplicateInBatch => CandidateKind::Duplicate("repeated in this output".into()),
                    Verdict::Invalid(problems) => CandidateKind::Invalid(
                        problems
                            .iter()
                            .map(|p| match p {
                                CandidateProblem::UnknownDependency(dep) => format!("depends on unknown \"{dep}\""),
                                CandidateProblem::Cycle => "dependency cycle".to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                };
                CandidateView {
                    name: draft.feature_name.clone(),
                    description: draft.description.clone(),
                    depends_on: draft.depends_on.clone().unwrap_or_default(),
                    kind,
                    suggests_parallel: r.suggests_parallel,
                    ignored_fields: r.candidate.ignored_fields.clone(),
                }
            })
            .collect();
        self.staged = reviewed;
        Ok(views)
    }

    fn accept_candidates(&mut self, accepted: &[orq_tui::AcceptedCandidate]) -> orq_tui::FormSaveOutcome {
        use orq_features::{AppendError, SaveError, Verdict};
        use orq_tui::FormSaveOutcome;
        let mut drafts = Vec::new();
        for pick in accepted {
            let staged = self
                .staged
                .iter()
                .find(|r| r.verdict == Verdict::New && r.candidate.draft.feature_name == pick.name);
            let Some(staged) = staged else {
                return FormSaveOutcome::Failed { reason: format!("{} is not a new generated candidate", pick.name) };
            };
            let mut draft = staged.candidate.draft.clone();
            // Only a kept suggestion is written; anything else stays absent.
            draft.parallelizable = pick.parallelizable.filter(|_| staged.suggests_parallel);
            drafts.push(draft);
        }
        let loaded = match self.loaded.take() {
            Some(loaded) => loaded,
            None => match orq_features::LoadedLedger::load_or_new(&self.path) {
                Ok(loaded) => loaded,
                Err(err) => return FormSaveOutcome::Failed { reason: ledger_error_text(&err) },
            },
        };
        let outcome = match loaded.append_many(&drafts) {
            Ok(_) => FormSaveOutcome::Saved,
            Err(err @ AppendError::Invalid(_)) => FormSaveOutcome::Invalid { reason: err.to_string() },
            Err(err @ AppendError::Save(SaveError::Conflict)) => {
                FormSaveOutcome::Conflict { reason: err.to_string() }
            }
            Err(err) => FormSaveOutcome::Failed { reason: err.to_string() },
        };
        self.loaded = match outcome {
            FormSaveOutcome::Invalid { .. } | FormSaveOutcome::Failed { .. } => Some(loaded),
            _ => {
                self.staged.clear();
                orq_features::LoadedLedger::load(&self.path).ok()
            }
        };
        outcome
    }

    fn load_settings(&mut self) -> orq_tui::SettingsView {
        let failed = |error: String| orq_tui::SettingsView { error: Some(error), ..Default::default() };
        let path = match self.settings_file() {
            Ok(path) => path,
            Err(error) => return failed(error),
        };
        let settings = match orq_features::Settings::load(path) {
            Ok(settings) => settings,
            Err(error) => {
                self.settings_snapshot = None;
                return failed(error.to_string());
            }
        };
        let roles = (
            settings.agent(orq_features::AgentRole::Implementer),
            settings.agent(orq_features::AgentRole::Reviewer),
        );
        self.settings_snapshot = Some(settings);
        let (implementer, reviewer) = match roles {
            (Ok(implementer), Ok(reviewer)) => (implementer, reviewer),
            (Err(error), _) | (_, Err(error)) => return failed(error.to_string()),
        };
        let mut view = orq_tui::SettingsView {
            implementer: agent_setting(implementer),
            reviewer: agent_setting(reviewer.clone()),
            ..Default::default()
        };
        // .tdd-config.json stays authoritative for the reviewer; it is only read.
        let config_path = self.path.with_file_name(".tdd-config.json");
        match orq_features::load_reviewer_config(&config_path) {
            Ok(config) => {
                let resolved = orq_features::resolve_reviewer(reviewer.as_ref(), &config);
                if let (true, orq_features::EffectiveReviewer::Config(p)) =
                    (resolved.overrides_settings, &resolved.effective)
                {
                    view.reviewer_override = Some(format!("{} / {}", p.harness, p.profile));
                }
                view.error = resolved.invalid;
            }
            Err(error) => view.error = Some(error.to_string()),
        }
        view
    }

    fn save_settings(
        &mut self,
        implementer: &orq_tui::AgentSetting,
        reviewer: &orq_tui::AgentSetting,
    ) -> Result<(), String> {
        // Work on the stored snapshot in place so no error path drops it: the
        // baseline it was loaded against stays, and an outside edit is still
        // a conflict on the next save.
        if self.settings_snapshot.is_none() {
            let path = self.settings_file()?;
            self.settings_snapshot = Some(orq_features::Settings::load(path).map_err(|e| e.to_string())?);
        }
        let settings = self.settings_snapshot.as_mut().expect("snapshot was just ensured");
        let roles = [
            (orq_features::AgentRole::Implementer, implementer),
            (orq_features::AgentRole::Reviewer, reviewer),
        ];
        for (role, value) in roles {
            // A role left empty stays unset rather than being written blank.
            if value.harness.trim().is_empty() && value.profile.trim().is_empty() {
                continue;
            }
            settings.set_agent(role, &value.harness, &value.profile).map_err(|e| e.to_string())?;
        }
        // The snapshot is kept even after a conflict: it stays stale until the
        // tab reloads, so the same stale values are never written later.
        settings.save().map_err(|error| match error {
            orq_features::SettingsError::Conflict { .. } => {
                "settings.toml changed since the Settings tab was loaded; nothing was written, reopen the tab to reload"
                    .to_string()
            }
            other => other.to_string(),
        })
    }
}

/// What the popup should show for `cwd`: its project, or why there is none.
fn view_for(cwd: &Path) -> Result<PopupView, ExitCode> {
    match orq_git::resolve_project(cwd) {
        Ok(project) => Ok(PopupView::Project(PopupModel {
            project_name: project.name,
            repo_path: project.root.to_string_lossy().into_owned(),
        })),
        Err(ResolveError::NotARepository { path }) => Ok(PopupView::NotARepository {
            path: path.to_string_lossy().into_owned(),
        }),
        Err(err) => Err(fail(format!("cannot resolve project: {err:?}"))),
    }
}

fn path_str(path: &Path) -> Option<&str> {
    path.to_str()
}

#[cfg(test)]
mod ledger_backend_tests {
    use orq_tui::{BrowserBackend, PromoteOutcome};

    use super::LedgerBackend;

    fn ledger(names: &[&str]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("orq-backend-{}-{}", std::process::id(), names.join("-")));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".tdd-features.json");
        let entries: Vec<String> =
            names.iter().map(|n| format!(r#"{{"featureName":"{n}","status":"draft"}}"#)).collect();
        std::fs::write(&path, format!("[{}]", entries.join(","))).unwrap();
        (dir, path)
    }

    #[test]
    fn generated_candidates_are_reviewed_and_accepted_in_one_save() {
        use orq_tui::{AcceptedCandidate, CandidateKind, FormSaveOutcome};
        let (dir, path) = ledger(&["base"]);
        std::fs::write(dir.join("SPEC.md"), "# spec\n").unwrap();
        let output = r#"[{"featureName":"base"},{"featureName":"a","parallelizable":true},{"featureName":"b","dependsOn":["a"]},{"featureName":"c","dependsOn":["ghost"]}]"#;
        let agent = orq_test_support::FakeAgent::builder().stdout(output).build();
        let mut backend = LedgerBackend::new(path.clone());
        backend.settings_path = Some(dir.join("cfg").join("orq").join("settings.toml"));
        backend.program_override = Some(agent.path().to_path_buf());
        let none = orq_tui::AgentSetting::default();
        let writer = orq_tui::AgentSetting { harness: "claude".into(), profile: "writer".into() };
        backend.save_settings(&writer, &none).unwrap();
        // The cache is chosen from XDG_CACHE_HOME or HOME; the test only
        // needs the agent to run, so it relies on whichever is set.
        let views = match backend.generate_candidates() {
            Ok(views) => views,
            Err(reason) => panic!("generation failed: {reason}"),
        };
        assert_eq!(views.len(), 4);
        assert!(matches!(views[0].kind, CandidateKind::Duplicate(_)));
        assert_eq!(views[1].kind, CandidateKind::New);
        assert!(views[1].suggests_parallel);
        assert!(matches!(views[3].kind, CandidateKind::Invalid(_)));
        let before = std::fs::read_to_string(&path).unwrap();

        let refused = backend.accept_candidates(&[AcceptedCandidate { name: "c".into(), parallelizable: None }]);
        assert!(matches!(refused, FormSaveOutcome::Failed { .. }));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        let saved = backend.accept_candidates(&[
            AcceptedCandidate { name: "a".into(), parallelizable: Some(true) },
            AcceptedCandidate { name: "b".into(), parallelizable: None },
        ]);
        assert!(matches!(saved, FormSaveOutcome::Saved));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""featureName":"a""#) && text.contains(r#""parallelizable":true"#));
        assert_eq!(orq_features::load_ledger(&path).unwrap().len(), 3);
    }

    #[test]
    fn settings_round_trip_through_the_backend_and_a_half_filled_role_is_refused() {
        let (dir, path) = ledger(&["s"]);
        let mut backend = LedgerBackend::new(path);
        backend.settings_path = Some(dir.join("cfg").join("orq").join("settings.toml"));
        let implementer = orq_tui::AgentSetting { harness: "claude".into(), profile: "opus".into() };
        let none = orq_tui::AgentSetting::default();

        backend.save_settings(&implementer, &none).unwrap();
        let view = backend.load_settings();
        assert_eq!(view.implementer, implementer);
        assert_eq!(view.reviewer, none);
        assert_eq!(view.error, None);

        let half = orq_tui::AgentSetting { harness: "pi".into(), profile: String::new() };
        assert!(backend.save_settings(&implementer, &half).is_err());
    }

    #[test]
    fn tdd_config_overrides_the_settings_reviewer_and_is_never_written() {
        let (dir, path) = ledger(&["override"]);
        let config = dir.join(".tdd-config.json");
        let text = r#"{"reviewers":{"default":{"reviewer":"pi","model":"gpt-6"},"allowedModels":["gpt-6"]}}"#;
        std::fs::write(&config, text).unwrap();
        let mut backend = LedgerBackend::new(path);
        backend.settings_path = Some(dir.join("cfg").join("orq").join("settings.toml"));
        let none = orq_tui::AgentSetting::default();
        let reviewer = orq_tui::AgentSetting { harness: "claude".into(), profile: "opus".into() };
        backend.save_settings(&none, &reviewer).unwrap();

        let view = backend.load_settings();

        assert_eq!(view.reviewer_override.as_deref(), Some("pi / gpt-6"));
        assert_eq!(view.error, None);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    }

    #[test]
    fn saving_after_an_outside_edit_is_a_conflict_not_an_overwrite() {
        let (dir, path) = ledger(&["stale"]);
        let mut backend = LedgerBackend::new(path);
        let settings = dir.join("cfg").join("orq").join("settings.toml");
        backend.settings_path = Some(settings.clone());
        let none = orq_tui::AgentSetting::default();
        let opus = orq_tui::AgentSetting { harness: "claude".into(), profile: "opus".into() };
        backend.save_settings(&opus, &none).unwrap();
        backend.load_settings();
        let outside = "[implementer]\nharness = \"claude\"\nprofile = \"opus\"\n[reviewer]\nharness = \"pi\"\nprofile = \"gpt\"\n";
        std::fs::write(&settings, outside).unwrap();

        let sonnet = orq_tui::AgentSetting { harness: "claude".into(), profile: "sonnet".into() };
        let result = backend.save_settings(&sonnet, &none);

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), outside);
    }

    #[test]
    fn a_rejected_value_does_not_let_a_later_save_overwrite_an_outside_edit() {
        let (dir, path) = ledger(&["rejected-then-stale"]);
        let mut backend = LedgerBackend::new(path);
        let settings = dir.join("cfg").join("orq").join("settings.toml");
        backend.settings_path = Some(settings.clone());
        let none = orq_tui::AgentSetting::default();
        let opus = orq_tui::AgentSetting { harness: "claude".into(), profile: "opus".into() };
        backend.save_settings(&opus, &none).unwrap();
        backend.load_settings();
        let bad = orq_tui::AgentSetting { harness: "bad\nname".into(), profile: "x".into() };
        assert!(backend.save_settings(&bad, &none).is_err());
        let outside = "[implementer]\nharness = \"pi\"\nprofile = \"gpt\"\n";
        std::fs::write(&settings, outside).unwrap();

        let sonnet = orq_tui::AgentSetting { harness: "claude".into(), profile: "sonnet".into() };
        let result = backend.save_settings(&sonnet, &none);

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), outside);
    }

    #[test]
    fn promoting_several_features_in_a_row_saves_each_one() {
        let (dir, path) = ledger(&["a", "b", "c"]);
        let mut backend = LedgerBackend::new(path.clone());
        backend.load().unwrap();

        let outcomes: Vec<bool> =
            ["a", "b", "c"].iter().map(|n| matches!(backend.promote(n), PromoteOutcome::Saved)).collect();

        assert_eq!(outcomes, vec![true, true, true]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(r#""status":"pending""#).count(), 3, "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_external_edit_after_the_last_load_is_still_a_conflict() {
        let (dir, path) = ledger(&["a", "b"]);
        let mut backend = LedgerBackend::new(path.clone());
        backend.load().unwrap();
        std::fs::write(&path, r#"[{"featureName":"a","status":"draft"},{"featureName":"b","status":"draft"} ]"#).unwrap();

        let outcome = backend.promote("a");

        assert!(matches!(outcome, PromoteOutcome::Failed { .. }));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn fields(name: &str) -> orq_tui::FeatureFields {
        orq_tui::FeatureFields { name: name.into(), ..Default::default() }
    }

    #[test]
    fn a_file_created_after_an_absent_load_is_a_conflict_not_an_append() {
        let (dir, path) = ledger(&["x"]);
        std::fs::remove_file(&path).unwrap();
        let mut backend = LedgerBackend::new(path.clone());
        assert!(backend.load().is_err());
        let outside = r#"[{"featureName":"theirs","status":"draft"}]"#;
        std::fs::write(&path, outside).unwrap();

        let outcome = backend.add_feature(&fields("mine"));

        assert!(matches!(outcome, orq_tui::FormSaveOutcome::Conflict { .. }), "{outcome:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), outside);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_write_leaves_the_bytes_alone_and_a_retry_succeeds() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = ledger(&["a"]);
        let mut backend = LedgerBackend::new(path.clone());
        backend.load().unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root and DAC-override environments can still write into a 0555
        // directory; the permission fault cannot be injected there.
        let probe = dir.join(".probe");
        if std::fs::File::create(&probe).is_ok() {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = std::fs::remove_dir_all(dir);
            return;
        }

        let first = backend.add_feature(&fields("b"));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(first, orq_tui::FormSaveOutcome::Failed { .. }), "{first:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let retry = backend.add_feature(&fields("b"));
        assert!(matches!(retry, orq_tui::FormSaveOutcome::Saved), "{retry:?}");
        assert!(std::fs::read_to_string(&path).unwrap().contains(r#""b""#));
        let _ = std::fs::remove_dir_all(dir);
    }
}
