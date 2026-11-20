//! Pushing a project's target branch to `origin`, only when the person asks for it (Push on the
//! web, or `orq push`). Nothing in a run, the queue, a clean-up or a watchdog calls this: runs keep
//! going through the push stage, which refuses target branches. The push is never forced: it only
//! happens when origin's branch is an ancestor of the local one, and it pushes the commit the
//! person confirmed, from a refspec `allowed_target_push` checks.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::store::{Event, RunStore};

/// The only remote a target is pushed to.
pub const REMOTE: &str = "origin";

/// Why a GitLab project's target is never pushed from orq: it is delivered by MR review.
pub const GITLAB_REFUSAL: &str = "GitLab: deliver by MR review";

/// How long the fetch before a push may take.
const FETCH_LIMIT: Duration = Duration::from_secs(60);

/// What a target push needs to know: the project's main checkout and target, and where its runs are.
pub struct TargetPush<'a> {
    /// The project's main checkout.
    pub root: &'a Path,
    pub project_id: &'a str,
    pub target: &'a str,
    /// The project's forge setting (`gh`, `glab`, ...), which wins over the remote's host.
    pub forge: Option<&'a str>,
    pub store: &'a RunStore,
    /// The `gh` program the branch lookup runs.
    pub gh: PathBuf,
}

/// One commit a push would send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushCommit {
    pub sha: String,
    pub short: String,
    /// The subject, redacted.
    pub subject: String,
    /// The orq run the commit came from; `None` for a commit not from orq.
    pub run: Option<String>,
}

/// Whether the target may be pushed now, and what a push would send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetPushState {
    /// `github` or `gitlab`, as orq decides it for the project; `None` when it cannot tell. Only a
    /// GitHub project is ever offered the push.
    pub provider: Option<String>,
    pub remote: String,
    pub target: String,
    /// The local target's commit, the one a push sends.
    pub sha: Option<String>,
    /// Origin's target, as just fetched.
    pub origin_sha: Option<String>,
    /// How many commits the local target has that origin's does not.
    pub count: usize,
    pub commits: Vec<PushCommit>,
    /// Why the push is not offered now; `None` when it is.
    pub reason: Option<String>,
}

/// What [`push_target`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetPushOutcome {
    /// Origin's target moved from `from` to `to`, carrying the commits of `runs`, which each
    /// recorded `target.pushed`.
    Pushed { from: String, to: String, count: usize, runs: Vec<String> },
    /// Nothing was pushed, for this reason.
    Refused(String),
}

/// The checks before a push, run again each time, and what a push would send.
pub fn target_push_state(push: &TargetPush) -> TargetPushState {
    let mut state = TargetPushState {
        provider: None,
        remote: REMOTE.to_string(),
        target: push.target.to_string(),
        sha: None,
        origin_sha: None,
        count: 0,
        commits: Vec::new(),
        reason: None,
    };
    state.reason = check(push, &mut state).err();
    state
}

fn check(push: &TargetPush, state: &mut TargetPushState) -> Result<(), String> {
    let target = push.target;
    let plain = !target.is_empty()
        && !target.starts_with('-')
        && target.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
        && target.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
    if !plain {
        return Err(format!("{target:?} is not a plain branch name"));
    }
    let url = origin_push_url(push.root)?;
    match orq_git::detect_provider(&url, &orq_git::ProviderHosts::default(), push.forge) {
        Ok(orq_git::Provider::GitHub) => state.provider = Some("github".to_string()),
        Ok(orq_git::Provider::GitLab) => {
            state.provider = Some("gitlab".to_string());
            return Err(GITLAB_REFUSAL.to_string());
        }
        Err(error) => return Err(format!("only a GitHub project's target is pushed from orq: {}", redacted(&error.to_string()))),
    }
    let address = crate::review_request::normalize_remote(&url).ok_or_else(|| format!("{REMOTE} has no repository address"))?;
    if let Some(pinned) = runs_repository(push)? {
        if repository_identity(&address) != repository_identity(&pinned) {
            return Err(format!("{REMOTE} pushes to {address}, not {pinned} that this project's runs use"));
        }
    }
    let local = rev_parse(push.root, &format!("refs/heads/{target}"))
        .ok_or_else(|| format!("the main checkout has no branch {target}"))?;
    state.sha = Some(local.clone());
    fetch(push.root, target)?;
    let tracking = format!("refs/remotes/{REMOTE}/{target}");
    let origin = rev_parse(push.root, &tracking).ok_or_else(|| format!("{REMOTE}/{target} could not be read"))?;
    state.origin_sha = Some(origin.clone());
    let ancestor = git(push.root, &["merge-base", "--is-ancestor", &origin, &local])?;
    if !ancestor.success {
        return Err(format!("{REMOTE}/{target} has commits you don't have: pull first"));
    }
    let log = git(push.root, &["log", "--format=%H%x00%s", &format!("{origin}..{local}"), "--"])?;
    if !log.success {
        return Err(format!("the commits on {target} could not be listed: {}", log.stderr));
    }
    state.commits = log
        .stdout
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .map(|(sha, subject)| PushCommit { sha: sha.to_string(), short: sha.chars().take(7).collect(), subject: orq_core::redact::redact(subject), run: None })
        .collect();
    link_runs(push, &mut state.commits);
    state.count = state.commits.len();
    if state.count == 0 {
        return Err(format!("{target} has nothing {REMOTE} does not have"));
    }
    let repository = owner_name(&url)?;
    if github_protects(push, &repository)? {
        return Err(format!("{target} is protected on GitHub: deliver by review instead"));
    }
    Ok(())
}

/// Marks each of `commits` with the project's run it came from: a run whose recorded local merge
/// landed one of them owns the commits between where it started and that merge.
fn link_runs(push: &TargetPush, commits: &mut [PushCommit]) {
    let listed: std::collections::HashSet<String> = commits.iter().map(|commit| commit.sha.clone()).collect();
    let mut owner: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for id in push.store.run_ids().unwrap_or_default() {
        let Ok(record) = push.store.load(&id) else { continue };
        if record.project_id != push.project_id || record.base_sha.is_empty() {
            continue;
        }
        let Ok(events) = push.store.events(&id) else { continue };
        let merged = events.iter().rev().filter(|event| event.kind == "merge.completed").find_map(|event| event.payload["head_sha"].as_str().map(str::to_string));
        let Some(head) = merged.filter(|head| listed.contains(head)) else { continue };
        let Ok(out) = git(push.root, &["rev-list", &format!("{}..{head}", record.base_sha), "--"]) else { continue };
        if !out.success {
            continue;
        }
        for sha in out.stdout.lines().filter(|sha| listed.contains(*sha)) {
            owner.entry(sha.to_string()).or_insert_with(|| id.clone());
        }
    }
    for commit in commits {
        commit.run = owner.get(&commit.sha).cloned();
    }
}

/// Records `target.pushed` on each run whose commits went in the push of `sha`; the runs, sorted.
fn record_pushed(push: &TargetPush, commits: &[PushCommit], sha: &str) -> Vec<String> {
    let mut runs: Vec<String> = commits.iter().filter_map(|commit| commit.run.clone()).collect();
    runs.sort();
    runs.dedup();
    for run in &runs {
        let Ok(record) = push.store.load(run) else { continue };
        let n = push.store.events(run).map_or(0, |events| events.len());
        let event = Event {
            schema_version: 1,
            event_id: format!("{run}-target-pushed-{n}"),
            run_id: run.clone(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: "target.pushed".to_string(),
            stage: None,
            attempt: 1,
            occurred_at: crate::implement::now_rfc3339(),
            payload: serde_json::json!({ "sha": sha, "remote": REMOTE, "target": push.target }),
        };
        if let Err(error) = push.store.append_event(&event) {
            eprintln!("orq: cannot record the push of {} on {run}: {error:?}", push.target);
        }
    }
    runs
}

/// The repository the project's newest run that pinned one was started against; `None` when no
/// run of the project pinned one.
fn runs_repository(push: &TargetPush) -> Result<Option<String>, String> {
    let ids = push.store.run_ids().map_err(|error| format!("the project's runs could not be read: {error:?}"))?;
    Ok(ids
        .iter()
        .rev()
        .filter_map(|id| push.store.load(id).ok())
        .find(|record| record.project_id == push.project_id && !record.repository.is_empty())
        .map(|record| record.repository))
}

/// `.git` and a trailing slash name the same repository.
fn repository_identity(url: &str) -> &str {
    let url = url.trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url)
}

/// The raw url origin is pushed to, as configured (its pushurls, else its urls). `git push origin`
/// sends to every one of them, so an origin with more than one is refused: only the one address
/// checked here may get the commits.
fn origin_push_url(dir: &Path) -> Result<String, String> {
    for key in [format!("remote.{REMOTE}.pushurl"), format!("remote.{REMOTE}.url")] {
        let out = git(dir, &["config", "--get-all", &key])?;
        let urls: Vec<&str> = if out.success { out.stdout.lines().map(str::trim).filter(|url| !url.is_empty()).collect() } else { Vec::new() };
        match urls.as_slice() {
            [] => continue,
            [url] => return Ok(url.to_string()),
            _ => return Err(format!("{REMOTE} pushes to more than one address: give it one push address first")),
        }
    }
    Err(format!("the main checkout has no remote {REMOTE}"))
}

/// The `owner/name` on its host of the repository at push url `url`.
fn owner_name(url: &str) -> Result<String, String> {
    let unreadable = || format!("{REMOTE}'s address is not an owner/name repository");
    let normalized = crate::review_request::normalize_remote(url).ok_or_else(unreadable)?;
    let path = normalized.split_once("://").and_then(|(_, rest)| rest.split_once('/')).map(|(_, path)| path).ok_or_else(unreadable)?;
    let path = path.trim_end_matches('/');
    Ok(path.strip_suffix(".git").unwrap_or(path).to_string())
}

/// Whether GitHub protects the target, from its read-only branch lookup. A lookup that fails or is
/// not understood is a reason not to push: protection is never assumed away.
fn github_protects(push: &TargetPush, repository: &str) -> Result<bool, String> {
    let failed = |why: String| format!("GitHub could not say whether {} is protected: {why}", push.target);
    let (_, args) = orq_core::provider_observation::branch_protection_command(repository, push.target).map_err(failed)?;
    let out = Command::new(&push.gh)
        .args(&args)
        .current_dir(push.root)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| failed(format!("gh could not run: {error}")))?;
    if !out.status.success() {
        let stderr = redacted(&String::from_utf8_lossy(&out.stderr));
        return Err(failed(if stderr.is_empty() { "gh failed".to_string() } else { stderr }));
    }
    orq_core::provider_observation::parse_branch_protection(&String::from_utf8_lossy(&out.stdout), push.target).map_err(failed)
}

/// Pushes the target at `sha`, the commit the person confirmed, once every check passes again.
pub fn push_target(push: &TargetPush, sha: &str) -> TargetPushOutcome {
    let state = target_push_state(push);
    if let Some(reason) = state.reason {
        return TargetPushOutcome::Refused(reason);
    }
    let (Some(local), Some(origin)) = (state.sha, state.origin_sha) else {
        return TargetPushOutcome::Refused("the target could not be read".to_string());
    };
    if local != sha {
        return TargetPushOutcome::Refused("the branch changed: review again".to_string());
    }
    // --no-follow-tags: a checkout's push.followTags would otherwise send tags nobody confirmed.
    let args = vec!["push".to_string(), "--no-follow-tags".to_string(), REMOTE.to_string(),format!("{sha}:refs/heads/{}", push.target)];
    if let Err(why) = orq_core::no_merge::allowed_target_push(&args, sha, push.target) {
        return TargetPushOutcome::Refused(why.to_string());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match git(push.root, &args) {
        Ok(out) if out.success => {
            let runs = record_pushed(push, &state.commits, &local);
            TargetPushOutcome::Pushed { from: origin, to: local, count: state.count, runs }
        }
        Ok(out) => TargetPushOutcome::Refused(out.stderr),
        Err(why) => TargetPushOutcome::Refused(why),
    }
}

struct GitOut {
    success: bool,
    stdout: String,
    /// Redacted.
    stderr: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<GitOut, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("git could not run: {error}"))?;
    Ok(GitOut {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: redacted(&String::from_utf8_lossy(&out.stderr)),
    })
}

fn redacted(text: &str) -> String {
    orq_core::redact::redact(text.trim())
}

fn rev_parse(dir: &Path, name: &str) -> Option<String> {
    let out = git(dir, &["rev-parse", "--verify", "--quiet", &format!("{name}^{{commit}}")]).ok()?;
    out.success.then(|| out.stdout.trim().to_string())
}

/// Fetches origin's `target` into its tracking ref, within [`FETCH_LIMIT`].
fn fetch(dir: &Path, target: &str) -> Result<(), String> {
    let refspec = format!("+refs/heads/{target}:refs/remotes/{REMOTE}/{target}");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["fetch", "--quiet", "--no-tags", "--", REMOTE, &refspec])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("git could not run: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read;
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("{REMOTE}/{target} could not be fetched: {}", redacted(&stderr)))
                };
            }
            Ok(None) if started.elapsed() >= FETCH_LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{REMOTE}/{target} could not be fetched within {} seconds", FETCH_LIMIT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("git could not be waited for: {error}")),
        }
    }
}
