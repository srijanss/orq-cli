//! Preparing a run's worktree before the agent starts: the configured setup
//! steps (for example `mcpctl setup --mcp <name>`), then the dependency
//! install the worktree's lockfile calls for. Steps are argv vectors, never
//! shell strings. A failed step stops setup; the worktree is left as it is.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPlan {
    /// Configured steps first, then the install step, in run order.
    pub steps: Vec<Vec<String>>,
    /// Why something expected was not planned.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupError {
    /// A configured step whose program is missing or blank.
    EmptyProgram,
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::EmptyProgram => {
                write!(f, "a configured setup command has no program to run")
            }
        }
    }
}

impl std::error::Error for SetupError {}

/// The step that failed, for the prepare stage to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupFailure {
    pub step: Vec<String>,
    /// `None` when the program could not be started or was killed by a signal.
    pub exit_code: Option<i32>,
    pub reason: String,
    pub log: PathBuf,
}

impl std::fmt::Display for SetupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Arguments can carry tokens, so only the program is ever shown.
        let program = self.step.first().map(String::as_str).unwrap_or("");
        write!(
            f,
            "setup step {program:?} failed: {} (log: {})",
            self.reason,
            self.log.display()
        )
    }
}

impl std::error::Error for SetupFailure {}

pub fn plan_setup(worktree: &Path, configured: &[Vec<String>]) -> Result<SetupPlan, SetupError> {
    if configured
        .iter()
        .any(|step| step.first().is_none_or(|program| program.trim().is_empty()))
    {
        return Err(SetupError::EmptyProgram);
    }
    let mut steps = configured.to_vec();
    let mut notes = Vec::new();
    if let Some(install) = install_step(worktree, &mut notes) {
        steps.push(install);
    }
    Ok(SetupPlan { steps, notes })
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

fn install_step(dir: &Path, notes: &mut Vec<String>) -> Option<Vec<String>> {
    if dir.join("uv.lock").is_file() {
        // `--locked` fails rather than re-resolving and rewriting uv.lock.
        return Some(argv(&["uv", "sync", "--locked"]));
    }
    if !dir.join("package.json").is_file() {
        return None;
    }
    let step = if dir.join("pnpm-lock.yaml").is_file() {
        argv(&["pnpm", "install", "--frozen-lockfile"])
    } else if dir.join("yarn.lock").is_file() {
        argv(&["yarn", "install", "--frozen-lockfile"])
    } else if dir.join("bun.lockb").is_file() || dir.join("bun.lock").is_file() {
        argv(&["bun", "install", "--frozen-lockfile"])
    } else if dir.join("package-lock.json").is_file() {
        argv(&["npm", "ci"])
    } else {
        notes.push(
            "package.json found but no lockfile; dependencies were not installed".to_string(),
        );
        return None;
    };
    Some(step)
}

/// Run `steps` in order with `worktree` as the working directory, appending
/// each step's stdout and stderr to `log`. Stops at the first failure.
pub fn run_setup(worktree: &Path, steps: &[Vec<String>], log: &Path) -> Result<(), SetupFailure> {
    run_setup_with_env(worktree, steps, log, &[])
}

/// `run_setup`, with `env` added to what each step sees (the worktree's own database names).
pub fn run_setup_with_env(
    worktree: &Path,
    steps: &[Vec<String>],
    log: &Path,
    env: &[(String, String)],
) -> Result<(), SetupFailure> {
    let failure = |step: &[String], exit_code: Option<i32>, reason: String| SetupFailure {
        step: step.to_vec(),
        exit_code,
        reason,
        log: log.to_path_buf(),
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|err| failure(&[], None, format!("could not open the log: {err}")))?;
    for step in steps {
        let Some((program, args)) = step
            .split_first()
            .filter(|(program, _)| !program.trim().is_empty())
        else {
            return Err(failure(
                step,
                None,
                "the step has no program to run".to_string(),
            ));
        };
        let _ = writeln!(file, "$ {program} ({} arguments)", args.len());
        let out = file
            .try_clone()
            .map_err(|err| failure(step, None, format!("could not use the log: {err}")))?;
        let err_out = file
            .try_clone()
            .map_err(|err| failure(step, None, format!("could not use the log: {err}")))?;
        // A group of its own, so a stop ends what the step started. What a step leaves running when it
        // finishes is left alone: starting a service can be what the step is for.
        let status = std::os::unix::process::CommandExt::process_group(
            Command::new(program)
                .args(args)
                .current_dir(worktree)
                .envs(env.iter().map(|(name, value)| (name, value)))
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err_out),
            0,
        )
        .spawn()
        .and_then(|mut child| {
            let _tracked = crate::child_groups::track(child.id());
            child.wait()
        });
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let reason = match status.code() {
                    Some(code) => format!("{program} exited with status {code}"),
                    None => format!("{program} was stopped by a signal"),
                };
                let _ = writeln!(file, "[{reason}]");
                return Err(failure(step, status.code(), reason));
            }
            Err(err) => {
                let reason = format!("could not start {program}: {err}");
                let _ = writeln!(file, "[{reason}]");
                return Err(failure(step, None, reason));
            }
        }
    }
    Ok(())
}
