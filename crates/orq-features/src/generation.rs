use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::AgentProfile;

/// Upper bound on what Orq keeps of an agent's output.
const MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum GenerationError {
    /// Settings name no implementer harness/profile.
    NotConfigured,
    AgentNotFound { harness: String },
    Spawn(std::io::Error),
    AgentFailed { code: Option<i32> },
    TimedOut,
    /// The agent wrote more than Orq keeps; nothing was stored.
    OutputTooLarge,
    Artifact(std::io::Error),
}

impl std::fmt::Display for GenerationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenerationError::NotConfigured => f.write_str(
                "no implementation agent configured: set a harness and profile in the Settings tab",
            ),
            GenerationError::AgentNotFound { harness } => write!(
                f,
                "agent harness `{harness}` was not found: install it or choose another harness in the Settings tab"
            ),
            GenerationError::Spawn(err) => write!(f, "could not start the agent: {err}"),
            GenerationError::AgentFailed { code: Some(code) } => {
                write!(f, "the agent exited with status {code}")
            }
            GenerationError::AgentFailed { code: None } => f.write_str("the agent was terminated"),
            GenerationError::TimedOut => f.write_str("the agent did not finish in time"),
            GenerationError::OutputTooLarge => write!(
                f,
                "the agent output exceeded the {} MiB limit and was discarded",
                MAX_OUTPUT_BYTES / (1024 * 1024)
            ),
            GenerationError::Artifact(err) => write!(f, "could not store the agent output: {err}"),
        }
    }
}

impl std::error::Error for GenerationError {}

pub struct GenerationRequest<'a> {
    /// The harness executable, resolved by the caller from the harness name.
    pub program: &'a Path,
    pub profile: &'a AgentProfile,
    pub repo_root: &'a Path,
    pub spec: &'a Path,
    pub artifact_dir: &'a Path,
    pub timeout: Duration,
}

/// `$XDG_CACHE_HOME/orq/generation`, else `~/.cache/orq/generation`.
pub fn generation_artifact_dir(xdg_cache: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match xdg_cache.filter(|p| p.is_absolute()) {
        Some(dir) => dir,
        None => home?.join(".cache"),
    };
    Some(base.join("orq").join("generation"))
}

fn prompt(spec: &Path) -> String {
    format!(
        "Read the specification at {} and propose features for it. \
Reply with ONLY a JSON array on standard output. Each element is an object with \
\"featureName\" (string, required, unique, kebab-case), \"status\" (string, use \"draft\"), \
\"description\" (string, optional), \"dependsOn\" (array of featureName strings, optional), \
\"testFile\" (string, optional) and \"parallelizable\" (boolean, optional; true only when the \
feature is independent of the others). Do not modify any files, do not run commands, and do \
not read files outside the repository.",
        spec.display()
    )
}

/// Runs the configured agent non-interactively: argv only (no shell), working
/// directory the repository root, stdout stored in an owner-only artifact in
/// `artifact_dir`. The agent is never given the artifact path to write to.
pub fn run_generation(request: &GenerationRequest<'_>) -> Result<PathBuf, GenerationError> {
    let mut child = Command::new(request.program)
        .arg("--profile")
        .arg(&request.profile.profile)
        .arg("-p")
        .arg(prompt(request.spec))
        .current_dir(request.repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => GenerationError::AgentNotFound {
                harness: request.profile.harness.clone(),
            },
            _ => GenerationError::Spawn(err),
        })?;

    let stdout = child.stdout.take().expect("piped stdout");
    // The reader reports through a channel so the wait for it can be bounded:
    // a descendant that inherited stdout keeps the pipe open past the agent.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut out);
        let _ = sender.send(out);
    });

    let deadline = Instant::now() + request.timeout;
    let mut output: Option<Vec<u8>> = None;
    let status = loop {
        if output.is_none() {
            if let Ok(out) = receiver.try_recv() {
                if out.len() as u64 > MAX_OUTPUT_BYTES {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(GenerationError::OutputTooLarge);
                }
                output = Some(out);
            }
        }
        match child.try_wait().map_err(GenerationError::Spawn)? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GenerationError::TimedOut);
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let output = match output {
        Some(output) => output,
        None => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            receiver.recv_timeout(remaining).map_err(|_| GenerationError::TimedOut)?
        }
    };
    if output.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(GenerationError::OutputTooLarge);
    }
    if !status.success() {
        return Err(GenerationError::AgentFailed { code: status.code() });
    }

    fs::create_dir_all(request.artifact_dir).map_err(GenerationError::Artifact)?;
    let name = format!(
        "candidates-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let path = request.artifact_dir.join(name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(GenerationError::Artifact)?;
    std::io::Write::write_all(&mut file, &output).map_err(GenerationError::Artifact)?;
    Ok(path)
}
