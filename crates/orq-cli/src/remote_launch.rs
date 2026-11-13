//! Starts runs for the web server: the same `orq run` the command line uses, checked first and then detached.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use orq_remote::projects::{LaunchError, RunLauncher};

/// How long one `orq run` call may take before it is stopped.
const DEADLINE: Duration = Duration::from_secs(60);
/// How much of a call's output is kept; the rest is read and dropped.
const OUTPUT_CAP: usize = 64 * 1024;

pub struct CliLauncher {
    exe: PathBuf,
    user_config: Option<PathBuf>,
    deadline: Duration,
}

/// Reads `source` to its end, keeping only the first `OUTPUT_CAP` bytes.
fn read_capped(mut source: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (sender, received) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut kept, mut chunk) = (Vec::new(), [0u8; 8192]);
        while let Ok(n) = source.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let room = OUTPUT_CAP.saturating_sub(kept.len()).min(n);
            kept.extend_from_slice(&chunk[..room]);
        }
        let _ = sender.send(kept);
    });
    received
}

/// `text` with every word that holds a path replaced, so a machine's layout never reaches the web.
fn without_paths(text: &str) -> String {
    text.split_whitespace().map(|word| if word.contains('/') { "<path>" } else { word }).collect::<Vec<_>>().join(" ")
}

/// Ends the call and everything it started: the call leads a process group of its own.
fn stop(child: &mut std::process::Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl CliLauncher {
    pub fn new(exe: PathBuf, user_config: Option<PathBuf>) -> Self {
        CliLauncher { exe, user_config, deadline: DEADLINE }
    }

    #[cfg(test)]
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Runs the program for at most the deadline; one that is still going then is killed.
    fn run(&self, args: &[std::ffi::OsString]) -> Result<(bool, String, String), LaunchError> {
        let failed = |what: &str| LaunchError::Failed(format!("cannot start orq run: {what}"));
        let mut child = std::process::Command::new(&self.exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .process_group(0)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|err| failed(&err.to_string()))?;
        let stdout = read_capped(child.stdout.take().ok_or_else(|| failed("no output"))?);
        let stderr = read_capped(child.stderr.take().ok_or_else(|| failed("no output"))?);
        let end = Instant::now() + self.deadline;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    stop(&mut child);
                    return Err(failed("it took too long"));
                }
            }
        };
        let text = |received: mpsc::Receiver<Vec<u8>>| received.recv_timeout(end.saturating_duration_since(Instant::now())).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        match (text(stdout), text(stderr)) {
            (Ok(out), Ok(err)) => Ok((status.success(), out, err)),
            _ => {
                stop(&mut child);
                Err(failed("it kept its output open"))
            }
        }
    }
}

impl CliLauncher {
    /// Checks and then starts a detached `orq run` of `feature`, with the chosen profiles or, without a
    /// choice, none named, so the run picks its agents as it does on the command line: the project's own,
    /// else the machine's.
    fn launch(&self, repo: &Path, feature: &str, chosen: Option<&orq_core::remote_inbox::LaunchProfiles>) -> Result<String, LaunchError> {
        let refused = |reason: String| LaunchError::Refused(without_paths(&reason));
        let project = orq_git::resolve_project(repo).map_err(|err| LaunchError::Failed(format!("cannot resolve project: {err:?}")))?;
        let request = orq_features::LaunchRequest {
            repo: project.root.clone(),
            feature: feature.to_string(),
            remote: "origin".to_string(),
            target: crate::configured_target_branch(&project.root).map_err(refused)?,
            implementer: chosen.map(|chosen| chosen.implementer.clone()),
            reviewer: chosen.map(|chosen| chosen.reviewer.clone()),
            context: None,
            issue: None,
        };
        if chosen.is_some() {
            let user_config = self.user_config.as_deref().unwrap_or(Path::new(""));
            let profiles = orq_features::load_model_profiles(user_config, &project.root.join(".orq.toml")).map_err(|err| LaunchError::Failed(err.to_string()))?;
            let reviewers = orq_features::load_reviewer_config(&project.root.join(".tdd-config.json")).map_err(|err| LaunchError::Failed(err.to_string()))?;
            request.validate(&profiles, &reviewers).map_err(|err| refused(err.to_string()))?;
        }
        let args = crate::without_detach_flag(&request.args());
        let mut check = args.clone();
        check.push("--check".into());
        let (checked, _, check_err) = self.run(&check)?;
        if !checked {
            let reason = crate::launch_outcome(false, "", &check_err).unwrap_or_else(|reason| reason);
            return Err(refused(reason));
        }
        let mut detached = args;
        detached.push("--detach".into());
        // While Herdr is running the run goes in a pane of its own, where its session can be answered.
        detached.push("--in-herdr".into());
        let (ok, out, err) = self.run(&detached)?;
        crate::launch_outcome(ok, &out, &err).map_err(LaunchError::Failed)
    }
}

impl RunLauncher for CliLauncher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        self.launch(repo, feature, None)
    }

    fn start_with(&self, repo: &Path, feature: &str, profiles: &orq_core::remote_inbox::LaunchProfiles) -> Result<String, LaunchError> {
        self.launch(repo, feature, Some(profiles))
    }

    fn start_warning(&self, repo: &Path) -> Option<String> {
        let project = orq_git::resolve_project(repo).ok()?;
        let target = crate::configured_target_branch(&project.root).ok()?;
        crate::target_ahead_warning(&project.root, "origin", &target)
    }

    fn resume(&self, repo: &Path, run: &str) -> Result<String, LaunchError> {
        // The command validates the run itself; a run id is never allowed to read as an option.
        if run.is_empty() || run.starts_with('-') {
            return Err(LaunchError::Refused("not a run id".to_string()));
        }
        // While Herdr is running the resumed run goes in a pane of its own, as a started one does.
        let args: Vec<std::ffi::OsString> = vec!["run".into(), "resume".into(), run.into(), "--yes".into(), "--detach".into(), "--in-herdr".into(), "--repo".into(), repo.into()];
        let (ok, out, err) = self.run(&args)?;
        if !ok {
            let reason = crate::launch_outcome(false, "", &err).unwrap_or_else(|reason| reason);
            // Only the command's own "cannot resume <run>: <why>" is a reason for the web; anything else is the operator's.
            return Err(if reason.starts_with("cannot resume ") { LaunchError::Refused(without_paths(&reason)) } else { LaunchError::Failed(reason) });
        }
        crate::launch_outcome(true, &out, &err).map_err(LaunchError::Failed)
    }
}

#[cfg(test)]
mod tests;
