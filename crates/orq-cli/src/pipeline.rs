//! `orq run`: the stages of one run, in order, driven through the orchestrator's
//! stage functions. A run never merges; it ends at review_ready, blocked or failed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orq_agents::{adapter_for, ImplementRequest, ModelSpec};
use orq_features::ResolvedModel;
use orq_git::{detect_provider, fetch_target_base, ProviderHosts};
use orq_orchestrator::{
    check_implement_outcome, choose_gate, cleanup_worktree, create_review_request, draft_review_description,
    plan_setup, push_run_branch, rebase_onto_target, record_launch, run_gate_stage, run_implement_stage, run_setup, wait_for_ci,
    CiOutcome, CiWaitConfig, DraftSession, GateOutcome, ImplementCheck, ImplementSession, PushOutcome,
    RebaseOutcome, ReviewRequestOutcome, RoleProfile, RunRecord, RunStore, StoreError,
};

pub struct Options<'a> {
    pub project: &'a orq_git::Project,
    pub feature: &'a str,
    pub remote: &'a str,
    pub target: &'a str,
    pub implementer: ResolvedModel,
    pub reviewer: ResolvedModel,
    pub rebase: bool,
    pub gate: bool,
    pub ci_check: bool,
    /// The id of a run whose launcher already announced it (a detached run); otherwise one is made.
    pub run_id: Option<&'a str>,
    /// Checked free text appended to the implement request.
    pub context: Option<&'a str>,
    /// A checked issue number; the review description then closes it.
    pub issue: Option<&'a str>,
}

fn new_run_id() -> String {
    format!(
        "run-{}-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        std::process::id()
    )
}

/// The run that already holds the lock of `feature` in `project`, if any. A detached launch asks
/// first, so a busy feature is refused by the launcher instead of by a runner nobody is watching.
pub fn active_run(project: &orq_git::Project, feature: &str) -> Result<Option<String>, String> {
    let root = state_root().ok_or_else(|| "no state location: set XDG_STATE_HOME or HOME".to_string())?;
    RunStore::new(&root).active_run(&project_id(project), &slug(feature)).map_err(|e| e.to_string())
}

/// Start `orq run` again as a detached runner: a process group of its own, no terminal input,
/// its output in the run's log. Returns the run id and the log path without waiting for the run.
/// `args` are the `orq run` arguments, without `--detach`.
pub fn launch_detached(args: &[std::ffi::OsString]) -> Result<(String, PathBuf), String> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    let root = state_root().ok_or_else(|| "no state location: set XDG_STATE_HOME or HOME".to_string())?;
    let run_id = new_run_id();
    let logs = root.join("logs").join(&run_id);
    // The log can hold what the agents and git printed: the user's alone, never a shared file.
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&logs)
        .map_err(|e| format!("cannot create the run log directory: {e}"))?;
    let log = logs.join("runner.log");
    let out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log)
        .map_err(|e| format!("cannot create the run log: {e}"))?;
    let err = out.try_clone().map_err(|e| format!("cannot open the run log: {e}"))?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the orq executable: {e}"))?;
    let mut runner = Command::new(exe);
    runner
        .arg("run")
        .args(args)
        .arg("--run-id")
        .arg(&run_id)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    // SAFETY: the closure runs between fork and exec and calls only `setsid`, which is
    // async-signal-safe and touches no memory of the parent.
    unsafe {
        // A session of its own: no controlling terminal, so closing the launcher's terminal
        // (or its process group) never signals the runner or the agents under it.
        runner.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    runner
        .spawn()
        .map_err(|e| format!("cannot start the runner: {e}"))?;
    Ok((run_id, log))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    ReviewReady,
    Blocked,
    Failed,
}

impl Ending {
    pub fn name(self) -> &'static str {
        match self {
            Ending::ReviewReady => "review_ready",
            Ending::Blocked => "blocked",
            Ending::Failed => "failed",
        }
    }
}

pub struct Report {
    pub run_id: String,
    pub ending: Ending,
    pub detail: Option<String>,
    pub review_url: Option<String>,
}

/// Why no run was started.
pub enum StartError {
    /// Another run of the feature is active.
    Busy(String),
    Other(String),
}

impl From<StoreError> for StartError {
    fn from(err: StoreError) -> Self {
        StartError::Other(format!("{err:?}"))
    }
}

fn other(message: impl Into<String>) -> StartError {
    StartError::Other(message.into())
}

pub(crate) fn state_root() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(base.join("orq"))
}

pub(crate) fn find_program(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    // Sessions run in the worktree, so the program must not depend on this process's cwd:
    // relative and empty PATH entries are anchored to it here.
    let here = std::env::current_dir().ok()?;
    std::env::split_paths(&path)
        .map(|dir| if dir.is_absolute() { dir } else { here.join(dir) })
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

fn git_ok(repo: &Path, args: &[&str]) -> bool {
    Command::new("git").arg("-C").arg(repo).args(args).output().is_ok_and(|o| o.status.success())
}

/// The id runs and locks are filed under: the project's name plus a hash of its git common
/// directory, so two repositories with the same name are different projects while a repository
/// and its linked worktrees are one.
pub(crate) fn project_id(project: &orq_git::Project) -> String {
    // FNV-1a: stable across releases, unlike the std hashers.
    let hash = project.git_common_dir.as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3));
    format!("{}-{:08x}", slug(&project.name), hash >> 32)
}

pub(crate) fn slug(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect()
}

fn profile(model: &ResolvedModel) -> RoleProfile {
    RoleProfile { harness: model.harness.clone(), model: model.model.clone(), thinking: model.thinking.clone() }
}

fn spec(model: &ResolvedModel) -> ModelSpec {
    ModelSpec { harness: model.harness.clone(), model: model.model.clone(), thinking: model.thinking.clone() }
}

pub fn execute(options: &Options<'_>) -> Result<Report, StartError> {
    let repo = &options.project.root;
    let root = state_root().ok_or_else(|| other("no state location: set XDG_STATE_HOME or HOME"))?;

    // Read from the user's checkout, never from the agent's worktree.
    let settings = orq_orchestrator::load_run_settings(&options.project.root, &|name| std::env::var(name).ok())
        .map_err(|e| other(format!("run settings: {e}")))?;
    let base_sha = fetch_target_base(repo, options.remote, options.target).map_err(|e| other(e.to_string()))?;
    let naming = orq_core::naming::run_naming(orq_core::naming::DEFAULT_BRANCH_PREFIX, options.feature, |n| {
        git_ok(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", n.branch)])
            || repo.join(&n.worktree_dir).exists()
    })
    .map_err(|e| other(e.to_string()))?;

    let url = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", "--", options.remote])
        .output()
        .map_err(|e| other(e.to_string()))?;
    let url = String::from_utf8_lossy(&url.stdout).trim().to_string();
    let provider = detect_provider(&url, &ProviderHosts::default(), settings.forge.as_deref())
        .map_err(|e| other(format!("{e:?}")))?;

    // Pinned now: the session's worktree can repoint the remote later, and provider calls must
    // still only reach the host the user chose.
    let repository = orq_orchestrator::resolve_repository_url(repo, options.remote).map_err(|e| other(format!("{e:?}")))?;

    let run_id = options.run_id.map_or_else(new_run_id, str::to_string);
    let record = RunRecord {
        run_id: run_id.clone(),
        project_id: project_id(options.project),
        feature_id: slug(options.feature),
        remote: options.remote.to_string(),
        provider: match provider {
            orq_git::Provider::GitHub => "github",
            orq_git::Provider::GitLab => "gitlab",
        }
        .to_string(),
        target_branch: options.target.to_string(),
        base_sha: base_sha.clone(),
        branch: naming.branch.clone(),
        worktree: repo.join(&naming.worktree_dir),
        repository,
        profiles: [
            ("implementer".to_string(), profile(&options.implementer)),
            ("reviewer".to_string(), profile(&options.reviewer)),
        ]
        .into_iter()
        .collect(),
    };
    let store = RunStore::new(&root);
    match store.start_run(&record) {
        Ok(()) => {}
        Err(StoreError::FeatureBusy { active_run_id }) => return Err(StartError::Busy(active_run_id)),
        Err(err) => return Err(other(format!("{err:?}"))),
    }
    if options.run_id.is_some() {
        let log = root.join("logs").join(&run_id).join("runner.log");
        if let Err(err) = record_launch(&store, &run_id, std::process::id(), &log) {
            eprintln!("orq: cannot record the launch of {run_id}: {err}");
        }
    }
    let finish = |store: &RunStore| store.finish_run(&record.project_id, &record.feature_id, &record.run_id);
    if let Err(err) = orq_git::create_run_worktree(repo, &naming.worktree_dir, &naming.branch, &base_sha) {
        let released = finish(&store);
        let mut message = format!("cannot create the worktree: {err:?}");
        if let Err(lock) = released {
            message.push_str(&format!("; the feature lock was not released: {lock:?}"));
        }
        if let Err(err) = orq_orchestrator::record_run_completed(&store, &record, "failed", Some(&message)) {
            eprintln!("orq: cannot record the end of {run_id}: {err:?}");
        }
        return Err(other(message));
    }

    let logs = root.join("logs").join(&run_id);
    let outcome = std::fs::create_dir_all(&logs)
        .map_err(StoreError::from)
        .and_then(|()| stages(&store, &record, options, &settings, provider.cli(), &logs));
    let released = finish(&store);
    let mut report = match outcome {
        Ok(mut report) => {
            report.run_id = run_id;
            report
        }
        Err(err) => Report { run_id, ending: Ending::Failed, detail: Some(format!("{err:?}")), review_url: None },
    };
    // A lock left behind blocks the next run of the feature, so it is never reported as success.
    if let Err(lock) = released {
        report.ending = Ending::Failed;
        let note = format!("the feature lock was not released: {lock:?}");
        report.detail = Some(match report.detail.take() {
            Some(detail) => format!("{detail}; {note}"),
            None => note,
        });
    }
    if let Err(err) = orq_orchestrator::record_run_completed(&store, &record, report.ending.name(), report.detail.as_deref()) {
        eprintln!("orq: cannot record the end of {}: {err:?}", report.run_id);
    }
    Ok(report)
}

fn stopped(ending: Ending, detail: impl Into<String>) -> Result<Report, StoreError> {
    Ok(Report { run_id: String::new(), ending, detail: Some(detail.into()), review_url: None })
}

fn stages(
    store: &RunStore,
    record: &RunRecord,
    options: &Options<'_>,
    settings: &orq_orchestrator::RunSettings,
    provider_cli: &str,
    logs: &Path,
) -> Result<Report, StoreError> {
    let run_id = record.run_id.as_str();
    let worktree = record.worktree.as_path();

    let plan = match plan_setup(worktree, &settings.setup) {
        Ok(plan) => plan,
        Err(err) => return stopped(Ending::Failed, format!("setup: {err:?}")),
    };
    if let Err(failure) = run_setup(worktree, &plan.steps, &logs.join("setup.log")) {
        return stopped(Ending::Failed, format!("setup failed: {}", failure.reason));
    }

    let Some(claude) = find_program(&options.implementer.harness) else {
        return stopped(Ending::Failed, format!("the {} harness is not on PATH", options.implementer.harness));
    };
    let Some(claude_program) = claude.to_str() else {
        return stopped(Ending::Failed, format!("the {} harness path is not valid UTF-8", options.implementer.harness));
    };
    let adapter = adapter_for(&options.implementer.harness).map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
    let args = adapter
        .implement_argv(&ImplementRequest {
            feature: options.feature,
            implementer: spec(&options.implementer),
            reviewer: spec(&options.reviewer),
            context: options.context,
        })
        .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
    let session = ImplementSession {
        program: claude_program,
        args: &args,
        env: &[],
        cwd: worktree,
        session_id: None,
        log: &logs.join("implement.log"),
        terminal: false,
    };
    if !run_implement_stage(store, run_id, &session)?.success {
        return stopped(Ending::Failed, "the implement session failed");
    }
    if let ImplementCheck::Blocked { reason } = check_implement_outcome(store, run_id)? {
        return stopped(Ending::Blocked, reason);
    }

    if let RebaseOutcome::Conflict { reason } = rebase_onto_target(store, run_id, options.rebase)? {
        return stopped(Ending::Blocked, reason);
    }

    let gate = choose_gate(worktree, settings.gate.as_deref()).map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
    if let GateOutcome::Failed { step, exit_code } =
        run_gate_stage(store, run_id, &gate, options.gate, &logs.join("gate.log"))?
    {
        return stopped(Ending::Failed, format!("the gate failed at step {step} (exit {exit_code:?})"));
    }

    let head_sha = match push_run_branch(store, run_id, &logs.join("push.log"))? {
        PushOutcome::Pushed { head_sha } => head_sha,
        PushOutcome::Failed { reason } => return stopped(Ending::Failed, format!("push failed: {reason}")),
    };

    let Some(provider) = find_program(provider_cli) else {
        return stopped(Ending::Failed, format!("{provider_cli} is not on PATH"));
    };
    let draft = draft_review_description(store, run_id, Duration::from_secs(120), |prompt| DraftSession {
        program: claude.to_string_lossy().into_owned(),
        args: vec!["-p".to_string(), prompt.to_string()],
        env: Vec::new(),
        cwd: worktree.to_path_buf(),
    })?;
    let draft = match options.issue {
        Some(number) => orq_orchestrator::link_issue(draft, number),
        None => draft,
    };
    let review = match create_review_request(store, run_id, &provider, &draft, Duration::from_secs(60))? {
        ReviewRequestOutcome::Created(review) | ReviewRequestOutcome::Adopted(review) => review,
        ReviewRequestOutcome::Failed { reason } => {
            return stopped(Ending::Failed, format!("review request failed: {reason}"))
        }
    };

    let ci = wait_for_ci(
        store,
        run_id,
        &provider,
        &head_sha,
        &CiWaitConfig {
            enabled: options.ci_check,
            poll_interval: Duration::from_secs(15),
            timeout: settings.ci_timeout.unwrap_or(Duration::from_secs(30 * 60)),
            command_timeout: Duration::from_secs(60),
        },
    )?;
    let ready = matches!(ci, CiOutcome::Passed | CiOutcome::Skipped);
    cleanup_worktree(store, run_id, ready, &ci)?;
    if !ready {
        return Ok(Report {
            run_id: String::new(),
            ending: Ending::Failed,
            detail: Some(format!("CI did not pass: {ci:?}")),
            review_url: Some(review.url),
        });
    }
    Ok(Report { run_id: String::new(), ending: Ending::ReviewReady, detail: None, review_url: Some(review.url) })
}
