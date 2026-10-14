use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use orq_git::ResolveError;
use orq_tui::{PopupModel, PopupView};

const USAGE: &str =
    "usage: orq --version | orq project inspect [--cwd <path>] | orq ui [--snapshot]";

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
        (Some("ui"), _) => ui(&args[1..]),
        _ => usage_error(),
    }
}

fn usage_error() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("orq: {message}");
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
        .map_err(|err| fail(format!("invalid working directory: {err:?}")))
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
        return match orq_tui::run_on_terminal(&model) {
            Ok(_) => ExitCode::SUCCESS,
            Err(err) => fail(err),
        };
    }
    let (width, height) = SNAPSHOT_SIZE;
    println!("{}", orq_tui::render_snapshot(&view, width, height));
    ExitCode::SUCCESS
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
