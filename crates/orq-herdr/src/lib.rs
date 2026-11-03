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
        Err(failure_reason(&String::from_utf8_lossy(&output.stdout), &String::from_utf8_lossy(&output.stderr)))
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

/// One shared tab that several runs' log panes are tiled into as a golden spiral: the first run
/// takes the tab's root pane and each further run splits the newest pane, alternating right and
/// down so the existing pane keeps 61.8%.
pub struct GridTab {
    label: String,
    /// The pane the next run splits, once the tab exists.
    newest: Option<PaneLocation>,
    next_direction: &'static str,
}

impl GridTab {
    pub fn new(label: &str) -> Self {
        GridTab { label: label.to_string(), newest: None, next_direction: "right" }
    }
}

/// Puts the run's log into the grid `grid`, opening its tab on first use. A split Herdr refuses
/// gives that run a tab of its own instead, and the grid carries on from the last pane in it.
pub fn open_grid_pane(
    herdr: &dyn HerdrCli,
    grid: &mut GridTab,
    workspace: Option<&str>,
    cwd: &Path,
    label: &str,
    log: &Path,
) -> Result<PaneLocation, String> {
    let text = |value: &Path| value.to_string_lossy().into_owned();
    let command = format!("tail -n +1 -f {}", shell_quote(&text(log)));
    let rename = |pane: &PaneLocation| {
        let _ = herdr.run(&["pane".into(), "rename".into(), pane.pane_id.clone(), label.to_string()]);
    };
    let Some(newest) = grid.newest.clone() else {
        let mut create: Vec<String> = vec!["tab".into(), "create".into()];
        if let Some(workspace) = workspace {
            create.extend(["--workspace".to_string(), workspace.to_string()]);
        }
        create.extend(["--cwd".into(), text(cwd), "--label".into(), grid.label.clone(), "--no-focus".into()]);
        let reply = herdr.run(&create).map_err(|err| format!("cannot open a Herdr tab: {err}"))?;
        let location = parse_location(&reply)?;
        rename(&location);
        if let Err(err) = herdr.run(&["pane".into(), "run".into(), location.pane_id.clone(), command]) {
            let _ = herdr.run(&["tab".into(), "close".into(), location.tab_id.clone()]);
            return Err(format!("cannot start the log viewer in the Herdr tab: {err}"));
        }
        grid.newest = Some(location.clone());
        return Ok(location);
    };
    let split: Vec<String> = [
        "pane", "split", "--pane", newest.pane_id.as_str(), "--direction", grid.next_direction, "--ratio", "0.618", "--cwd",
        text(cwd).as_str(), "--no-focus",
    ]
    .iter()
    .map(|part| part.to_string())
    .collect();
    let created = herdr.run(&split).map_err(|err| err.to_string()).and_then(|reply| parse_split(&reply, &newest));
    let location = match created {
        Ok(location) => location,
        Err(_) => return open_run_pane(herdr, workspace, cwd, label, log),
    };
    rename(&location);
    if let Err(err) = herdr.run(&["pane".into(), "run".into(), location.pane_id.clone(), command]) {
        let _ = herdr.run(&["pane".into(), "close".into(), location.pane_id.clone()]);
        return Err(format!("cannot start the log viewer in the Herdr pane: {err}"));
    }
    grid.next_direction = if grid.next_direction == "right" { "down" } else { "right" };
    grid.newest = Some(location.clone());
    Ok(location)
}

fn parse_split(reply: &str, near: &PaneLocation) -> Result<PaneLocation, String> {
    let bad = || "Herdr's reply did not name the new pane".to_string();
    let value: serde_json::Value = serde_json::from_str(reply).map_err(|_| bad())?;
    let pane = value.pointer("/result/pane").ok_or_else(bad)?;
    let field = |name: &str, fallback: &str| {
        pane.get(name).and_then(|v| v.as_str()).map(str::to_string).unwrap_or_else(|| fallback.to_string())
    };
    let pane_id = pane.get("pane_id").and_then(|v| v.as_str()).map(str::to_string).ok_or_else(bad)?;
    Ok(PaneLocation { workspace_id: field("workspace_id", &near.workspace_id), tab_id: field("tab_id", &near.tab_id), pane_id })
}

fn parse_location(reply: &str) -> Result<PaneLocation, String> {
    let bad = || "Herdr's reply did not name the new pane".to_string();
    let value: serde_json::Value = serde_json::from_str(reply).map_err(|_| bad())?;
    let pane = value.pointer("/result/root_pane").ok_or_else(bad)?;
    let field = |name: &str| pane.get(name).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(bad);
    Ok(PaneLocation { workspace_id: field("workspace_id")?, tab_id: field("tab_id")?, pane_id: field("pane_id")? })
}

/// Why a `herdr` call failed. Herdr prints its error as JSON on stdout, `code: message`; anything
/// else falls back to the first line of stderr.
pub fn failure_reason(stdout: &str, stderr: &str) -> String {
    let json_error = serde_json::from_str::<serde_json::Value>(stdout).ok().and_then(|value| {
        let error = value.get("error")?;
        let code = error.get("code")?.as_str()?;
        let message = error.get("message")?.as_str()?;
        Some(format!("{code}: {message}"))
    });
    json_error
        .or_else(|| stderr.lines().map(str::trim).find(|line| !line.is_empty()).map(str::to_string))
        .unwrap_or_else(|| "herdr failed".to_string())
}

/// What bringing a run's pane to the front came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusOutcome {
    Focused,
    /// The tab or pane no longer exists in Herdr.
    Gone(String),
    /// Herdr could not be asked, or refused for another reason.
    Failed(String),
}

/// Shows the pane's tab, then the pane in it.
pub fn focus_pane(herdr: &dyn HerdrCli, pane: &PaneLocation) -> FocusOutcome {
    let steps = [["tab", "focus", pane.tab_id.as_str()], ["pane", "focus", pane.pane_id.as_str()]];
    for step in steps {
        let args: Vec<String> = step.iter().map(|part| part.to_string()).collect();
        if let Err(reason) = herdr.run(&args) {
            let gone = reason.split(':').next().is_some_and(|code| matches!(code, "tab_not_found" | "pane_not_found"));
            return if gone { FocusOutcome::Gone(reason) } else { FocusOutcome::Failed(reason) };
        }
    }
    FocusOutcome::Focused
}
