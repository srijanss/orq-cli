use std::path::{Path, PathBuf};

/// Why a launch working directory could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchCwdError {
    /// The chosen source supplied an empty path.
    Empty { source: CwdSource },
    /// The chosen source named a directory that does not exist.
    NotFound { source: CwdSource, path: PathBuf },
}

/// Where a launch working directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdSource {
    Explicit,
    HerdrActivePaneCwd,
    ProcessCwd,
}

/// Resolve the directory `orq` should treat as the launch context.
///
/// Precedence: an explicit `--cwd`, then `HERDR_ACTIVE_PANE_CWD`, then the
/// process working directory.
pub fn resolve_launch_cwd(
    explicit: Option<&Path>,
    herdr_active_pane_cwd: Option<&str>,
    process_cwd: &Path,
) -> Result<PathBuf, LaunchCwdError> {
    // The first source that is present is authoritative: if it is unusable
    // that is an error, never a silent fallback to another directory.
    if let Some(path) = explicit {
        return validate(path, CwdSource::Explicit);
    }
    if let Some(path) = herdr_active_pane_cwd {
        return validate(Path::new(path), CwdSource::HerdrActivePaneCwd);
    }
    validate(process_cwd, CwdSource::ProcessCwd)
}

fn validate(path: &Path, source: CwdSource) -> Result<PathBuf, LaunchCwdError> {
    if path.as_os_str().is_empty() {
        return Err(LaunchCwdError::Empty { source });
    }
    if !path.is_dir() {
        return Err(LaunchCwdError::NotFound {
            source,
            path: path.to_path_buf(),
        });
    }
    Ok(path.to_path_buf())
}

/// Where a run's Herdr pane lives. Navigation only: the run never depends on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneLocation {
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
}

/// The public `herdr` CLI: each call takes the arguments after `herdr` and returns its stdout.
pub trait HerdrCli {
    fn run(&self, args: &[String]) -> Result<String, String>;
}

/// The installed `herdr` binary: `$HERDR_BIN_PATH` when set, else `herdr` on the path.
pub struct HerdrBinary;

impl HerdrCli for HerdrBinary {
    fn run(&self, args: &[String]) -> Result<String, String> {
        let program = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
        let output = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|err| format!("cannot run herdr: {err}"))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("herdr failed");
        Err(reason.to_string())
    }
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Opens a new tab in `workspace` (Herdr's choice when `None`) at `cwd`, labelled `label`, that
/// follows the run's log. Without focus, so the popup's user stays where they are. If the tab
/// opens but the command cannot be started in it, the tab is closed again.
pub fn open_run_pane(
    herdr: &dyn HerdrCli,
    workspace: Option<&str>,
    cwd: &Path,
    label: &str,
    log: &Path,
) -> Result<PaneLocation, String> {
    let text = |value: &Path| value.to_string_lossy().into_owned();
    let mut create: Vec<String> = vec!["tab".into(), "create".into()];
    if let Some(workspace) = workspace {
        create.extend(["--workspace".to_string(), workspace.to_string()]);
    }
    create.extend(["--cwd".into(), text(cwd), "--label".into(), label.to_string(), "--no-focus".into()]);
    let reply = herdr.run(&create).map_err(|err| format!("cannot open a Herdr tab: {err}"))?;
    let location = parse_location(&reply)?;
    let command = format!("tail -n +1 -f {}", shell_quote(&text(log)));
    if let Err(err) = herdr.run(&["pane".into(), "run".into(), location.pane_id.clone(), command]) {
        let _ = herdr.run(&["tab".into(), "close".into(), location.tab_id.clone()]);
        return Err(format!("cannot start the log viewer in the Herdr tab: {err}"));
    }
    Ok(location)
}

fn parse_location(reply: &str) -> Result<PaneLocation, String> {
    let bad = || "Herdr's reply did not name the new pane".to_string();
    let value: serde_json::Value = serde_json::from_str(reply).map_err(|_| bad())?;
    let pane = value.pointer("/result/root_pane").ok_or_else(bad)?;
    let field = |name: &str| pane.get(name).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(bad);
    Ok(PaneLocation { workspace_id: field("workspace_id")?, tab_id: field("tab_id")?, pane_id: field("pane_id")? })
}
