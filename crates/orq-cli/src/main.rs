use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: orq --version | orq project inspect [--cwd <path>]";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let word = |index: usize| args.get(index).and_then(|arg| arg.to_str());

    match (word(0), word(1)) {
        (Some("--version"), None) => {
            println!("orq {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        (Some("project"), Some("inspect")) => project_inspect(&args[2..]),
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

fn project_inspect(args: &[OsString]) -> ExitCode {
    let explicit: Option<PathBuf> = match args {
        [] => None,
        [flag, path] if flag == "--cwd" => Some(PathBuf::from(path)),
        _ => return usage_error(),
    };

    // Fallback sources are only consulted when no higher-priority source is
    // present, so a broken fallback never defeats a valid explicit choice.
    let pane_cwd = if explicit.is_none() {
        match std::env::var_os("HERDR_ACTIVE_PANE_CWD") {
            None => None,
            Some(value) => match value.into_string() {
                Ok(value) => Some(value),
                Err(_) => return fail("HERDR_ACTIVE_PANE_CWD is not valid UTF-8"),
            },
        }
    } else {
        None
    };
    let process_cwd = if explicit.is_none() && pane_cwd.is_none() {
        match std::env::current_dir() {
            Ok(dir) => dir,
            Err(err) => return fail(format!("cannot determine the working directory: {err}")),
        }
    } else {
        PathBuf::new() // never consulted: a higher-priority source is present
    };

    let cwd = match orq_herdr::resolve_launch_cwd(explicit.as_deref(), pane_cwd.as_deref(), &process_cwd) {
        Ok(cwd) => cwd,
        Err(err) => return fail(format!("invalid working directory: {err:?}")),
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

fn path_str(path: &Path) -> Option<&str> {
    path.to_str()
}
