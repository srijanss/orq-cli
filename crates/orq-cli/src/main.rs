use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use orq_git::ResolveError;
use orq_tui::{PopupModel, PopupView};

const USAGE: &str = "usage: orq --version | orq project inspect [--cwd <path>] | orq features --repo <path> | orq feature set-status --repo <path> --id <name> --status pending | orq ui [--snapshot] | orq doctor herdr [--cwd <path>]";

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
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    let bytes = rx.recv_timeout(VERSION_PROBE_TIMEOUT).ok();
    if bytes.is_none() {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    let bytes = bytes?;
    status.success().then(|| String::from_utf8_lossy(&bytes).trim().to_string())
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
}

impl LedgerBackend {
    fn new(path: PathBuf) -> Self {
        Self { path, loaded: None }
    }
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
                Ok(entries)
            }
            Err(err) => {
                self.loaded = None;
                Err(orq_tui::LedgerErrorView {
                    path: self.path.to_string_lossy().into_owned(),
                    reason: ledger_error_text(&err),
                })
            }
        }
    }

    fn promote(&mut self, feature: &str) -> orq_tui::PromoteOutcome {
        let Some(loaded) = &self.loaded else {
            return orq_tui::PromoteOutcome::Failed { reason: "ledger is not loaded".to_string() };
        };
        match loaded.promote(feature, None) {
            Ok(_) => orq_tui::PromoteOutcome::Saved,
            Err(err) => orq_tui::PromoteOutcome::Failed { reason: promote_error_text(&err, feature) },
        }
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
