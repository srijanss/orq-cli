//! The release a project's `.orq/run.toml` `[release] release_on_merge = "auto"` makes after a merge-local
//! merge: one non-interactive agent call picks the semver level (the commit subjects decide when it
//! fails), the version is bumped and the `[release]` commands run in a release worktree, never in the main
//! checkout, and the commit made there is fast-forwarded into the target. Nothing is pushed. A release
//! that cannot be made is refused with a reason and leaves the merge, and everything else, as it was.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orq_core::project_settings::{version_in, with_version, ReleaseSettings};
use orq_core::redact::redact;
use serde_json::Value;

/// How long the level call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// The most of the agent's answer that is read.
const MAX_ANSWER: u64 = 64 * 1024;
/// The most characters of the diff the agent is shown.
const MAX_DIFF_CHARS: usize = 60_000;

/// Runs the one-shot call that picks a release's level.
pub trait ReleaseAgent: Send + Sync {
    /// What `harness` printed when started with `argv`, or why it could not answer.
    fn ask(&self, harness: &str, argv: &[String]) -> Result<String, String>;
}

/// Starts the harness program itself, with no input and a time limit.
pub struct HarnessAgent;

impl ReleaseAgent for HarnessAgent {
    fn ask(&self, harness: &str, argv: &[String]) -> Result<String, String> {
        let mut child = Command::new(harness)
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("{harness} could not start: {}", error.kind()))?;
        let Some(out) = child.stdout.take() else { return Err(format!("{harness} gave no output")) };
        let reader = std::thread::spawn(move || {
            let mut answer = Vec::new();
            let _ = out.take(MAX_ANSWER).read_to_end(&mut answer);
            answer
        });
        let until = Instant::now() + CALL_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let answer = reader.join().unwrap_or_default();
                    return if status.success() { Ok(String::from_utf8_lossy(&answer).into_owned()) } else { Err(format!("{harness} failed: {status}")) };
                }
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{harness} timed out after {} seconds", CALL_TIMEOUT.as_secs()));
                }
                Err(error) => {
                    let _ = child.kill();
                    return Err(format!("{harness} could not be waited for: {}", error.kind()));
                }
            }
        }
    }
}

/// What a release needs besides its merge: where the audit goes and which agent picks the level.
#[derive(Clone)]
pub(super) struct Env {
    pub roots: orq_core::storage::StorageRoots,
    pub agent: Arc<dyn ReleaseAgent>,
    pub harness: String,
    pub model: String,
    pub thinking: Option<String>,
}

/// The merge to release.
pub(super) struct Merge<'a> {
    pub root: &'a Path,
    pub branch: &'a str,
    pub base: &'a str,
    pub merged: &'a str,
    pub feature: &'a str,
    pub run: &'a str,
}

/// A release that was made.
pub(super) struct Created {
    pub version: String,
    pub from: String,
    pub level: String,
    pub proposed: String,
    pub reason: String,
    pub decided_by: &'static str,
    pub commit: String,
}

/// What `git args` printed in `dir`, or what it said when it failed.
fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").args(args).current_dir(dir).stdin(Stdio::null()).output().map_err(|error| format!("git cannot start: {}", error.kind()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let said = String::from_utf8_lossy(&out.stderr);
        Err(format!("git {} failed: {}", args.first().unwrap_or(&""), said.trim()))
    }
}

fn parse_version(version: &str) -> Result<(u64, u64, u64), String> {
    let parts: Vec<Option<u64>> = version.split('.').map(|part| part.parse().ok()).collect();
    match parts.as_slice() {
        [Some(major), Some(minor), Some(patch)] => Ok((*major, *minor, *patch)),
        _ => Err(format!("the version {version:?} is not major.minor.patch")),
    }
}

/// `text` on one line, so a reason cannot add lines to a commit message.
fn one_line(text: &str) -> String {
    redact(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The description of `feature` in the project's ledger, if it has one.
fn described(root: &Path, feature: &str) -> String {
    let Ok(text) = std::fs::read_to_string(root.join(".tdd-features.json")) else { return String::new() };
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&text) else { return String::new() };
    entries.iter().find(|entry| entry["featureName"] == feature).and_then(|entry| entry["description"].as_str()).unwrap_or_default().to_string()
}

/// Asks the agent for the level of the change in `evidence`.
fn ask_level(env: &Env, evidence: &str) -> Result<orq_agents::ReleaseLevel, String> {
    let argv = orq_agents::release_level_argv(&env.harness, &env.model, env.thinking.as_deref(), evidence).map_err(|error| error.to_string())?;
    let answer = env.agent.ask(&env.harness, &argv)?;
    orq_agents::parse_release_level(&answer).ok_or_else(|| "its answer was not a level and a reason".to_string())
}

/// Makes the release of `merge`: the commit that carries the next version, fast-forwarded into the target.
pub(super) fn release(env: &Env, settings: &ReleaseSettings, merge: &Merge<'_>) -> Result<Created, String> {
    let root = merge.root;
    match run_git(root, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        Ok(on) if on.trim() == merge.branch => {}
        Ok(on) => return Err(format!("the checkout is on {}, not {}", on.trim(), merge.branch)),
        Err(_) => return Err("the checkout is on a detached HEAD".to_string()),
    }
    let head = run_git(root, &["rev-parse", "HEAD"])?;
    if head.trim() != merge.merged {
        return Err(format!("the target moved since the merge: it is at {}, not {}", head.trim(), merge.merged));
    }
    if !run_git(root, &["status", "--porcelain", "--untracked-files=no"])?.trim().is_empty() {
        return Err("the checkout has uncommitted changes".to_string());
    }
    let file = Path::new(&settings.version_file);
    if file.is_absolute() || file.components().any(|part| !matches!(part, Component::Normal(_))) {
        return Err(format!("the version file {} is not a path inside the project", settings.version_file));
    }
    let current = std::fs::read_to_string(root.join(file)).map_err(|error| format!("{} cannot be read: {}", settings.version_file, error.kind()))?;
    let from = version_in(&settings.version_file, &current, &settings.version_key)?;
    let (major, minor, patch) = parse_version(&from)?;

    let base = if merge.base.is_empty() { format!("{}^", merge.merged) } else { merge.base.to_string() };
    let range = format!("{base}..{}", merge.merged);
    let subjects = run_git(root, &["log", "--format=%s", &range]).unwrap_or_default();
    let stat = run_git(root, &["diff", "--stat", &base, merge.merged]).unwrap_or_default();
    let diff: String = run_git(root, &["diff", &base, merge.merged]).unwrap_or_default().chars().take(MAX_DIFF_CHARS).collect();
    let evidence = redact(&format!(
        "Feature: {}\nDescription: {}\nCurrent version: {from}\n\nCommit subjects:\n{subjects}\nDiff stat:\n{stat}\nDiff:\n{diff}",
        merge.feature,
        described(root, merge.feature)
    ));
    let (proposed, reason, decided_by) = match ask_level(env, &evidence) {
        Ok(level) => (level.level, level.reason, "claude"),
        Err(why) => {
            let level = if subjects.lines().any(|subject| subject.starts_with("feat")) { "minor" } else { "patch" };
            (level.to_string(), format!("chosen by commit subjects: {why}"), "fallback")
        }
    };
    let reason = one_line(&reason);
    // Major is applied as minor; what was proposed stays on the record.
    let (level, next) = match proposed.as_str() {
        "patch" => ("patch", (major, minor, patch + 1)),
        _ => ("minor", (major, minor + 1, 0)),
    };
    let version = format!("{}.{}.{}", next.0, next.1, next.2);

    let worktree: PathBuf = root.join(".claude/worktrees").join(format!("release-{version}"));
    let branch = format!("release/{version}");
    let worktree_arg = worktree.to_string_lossy().into_owned();
    run_git(root, &["worktree", "add", "-q", "-b", &branch, &worktree_arg, merge.merged])?;
    let made = (|| {
        let path = worktree.join(file);
        let text = std::fs::read_to_string(&path).map_err(|error| format!("{} cannot be read: {}", settings.version_file, error.kind()))?;
        let bumped = with_version(&settings.version_file, &text, &settings.version_key, &version)?;
        std::fs::write(&path, bumped).map_err(|error| format!("{} cannot be written: {}", settings.version_file, error.kind()))?;
        let (results, ok) = super::after_merge::run_commands(&worktree, &settings.commands);
        if !ok {
            let failed = results.iter().find(|result| result.get("skipped").is_none() && result["exit_code"] != 0).map_or(Value::Null, |result| result["argv"][0].clone());
            return Err(format!("the release command {failed} failed"));
        }
        run_git(&worktree, &["add", "-A"])?;
        let message = format!("level: {level}\nreason: {reason}\nfeature: {}\nrun: {}", one_line(merge.feature), merge.run);
        let subject = format!("chore(release): {} {version}", one_line(merge.feature));
        let configured = run_git(&worktree, &["config", "user.email"]).is_ok_and(|email| !email.trim().is_empty());
        let mut commit = vec!["-c", "user.name=orq", "-c", "user.email=orq@localhost", "commit", "-q", "-m", &subject, "-m", &message];
        if configured {
            commit.drain(..4);
        }
        run_git(&worktree, &commit)?;
        let commit = run_git(&worktree, &["rev-parse", "HEAD"])?.trim().to_string();
        run_git(root, &["merge", "--ff-only", "-q", &branch])?;
        Ok(commit)
    })();
    let _ = run_git(root, &["worktree", "remove", "--force", &worktree_arg]);
    let _ = run_git(root, &["branch", "-D", &branch]);
    let commit = made?;
    Ok(Created { version, from, level: level.to_string(), proposed, reason, decided_by, commit })
}
