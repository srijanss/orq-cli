//! `orq run`: the stages of one run, in order, driven through the orchestrator's
//! stage functions. A run never merges; it ends at review_ready, blocked or failed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orq_agents::{adapter_for, ImplementRequest, ModelSpec};
use orq_features::ResolvedModel;
use orq_git::{detect_provider, fetch_target_base, ProviderHosts};
use orq_orchestrator::{
    check_implement_outcome, choose_gate, cleanup_worktree, create_review_request_with, draft_review_description,
    plan_setup, push_run_branch, rebase_onto_target, record_launch, run_gate_with_fixes_env, run_implement_stage, run_setup_with_env, wait_for_ci,
    CiOutcome, CiWaitConfig, DraftSession, FixSessionSpec, GateFixOutcome, ImplementCheck, ImplementSession, PushOutcome,
    RebaseOutcome, ReviewRequestOutcome, RoleProfile, RunRecord, RunStore, StoreError,
};

/// How many times a red gate is handed to a fix session before the run fails.
const GATE_FIX_ATTEMPTS: u32 = 2;

pub struct Options<'a> {
    pub project: &'a orq_git::Project,
    pub feature: &'a str,
    pub remote: &'a str,
    pub target: &'a str,
    pub implementer: ResolvedModel,
    pub reviewer: ResolvedModel,
    /// The model of the implement session, its fix sessions and the review draft; the run setting fills in
    /// when it is not named, and either one replaces the implementer profile's own model.
    pub model: Option<&'a str>,
    pub rebase: bool,
    pub gate: bool,
    pub ci_check: bool,
    /// The id of a run whose launcher already announced it (a detached run); otherwise one is made.
    pub run_id: Option<&'a str>,
    /// Checked free text appended to the implement request.
    pub context: Option<&'a str>,
    /// A checked issue number; the review description then closes it.
    pub issue: Option<&'a str>,
    /// Who the new review request is assigned to and who reviews it; the run settings fill in
    /// whatever is left unnamed.
    pub mr_assignee: Option<&'a str>,
    pub mr_reviewer: Option<&'a str>,
    /// Publish the branch checked out in the repository: no worktree and no implement session.
    pub publish: bool,
    /// Continue the feature's latest stopped run: its branch and worktree are reused.
    pub resume: bool,
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

/// The model the run was asked for, by flag or by setting.
fn settings_or_flag_model<'a>(options: &'a Options<'_>, settings: &'a orq_orchestrator::RunSettings) -> Option<&'a str> {
    options.model.or(settings.model.as_deref())
}

/// The implementer profile with the run's model in place of its own, when one was asked for.
fn with_run_model(implementer: &ResolvedModel, options: &Options<'_>, settings: &orq_orchestrator::RunSettings) -> ResolvedModel {
    match settings_or_flag_model(options, settings) {
        Some(model) => ResolvedModel { model: model.to_string(), ..implementer.clone() },
        None => implementer.clone(),
    }
}

fn spec(model: &ResolvedModel) -> ModelSpec {
    ModelSpec { harness: model.harness.clone(), model: model.model.clone(), thinking: model.thinking.clone() }
}

pub fn execute(options: &Options<'_>) -> Result<Report, StartError> {
    let repo = &options.project.root;
    let root = state_root().ok_or_else(|| other("no state location: set XDG_STATE_HOME or HOME"))?;

    if options.resume && options.publish {
        return Err(other("--resume and --publish cannot be combined"));
    }

    // Read from the user's checkout, never from the agent's worktree.
    let settings = orq_orchestrator::load_run_settings(&options.project.root, &|name| std::env::var(name).ok())
        .map_err(|e| other(format!("run settings: {e}")))?;
    let base_sha = fetch_target_base(repo, options.remote, options.target).map_err(|e| other(e.to_string()))?;
    let published = if options.publish { Some(published_branch(repo, options.target, &base_sha).map_err(other)?) } else { None };
    let store = RunStore::new(&root);
    let project_id = project_id(options.project);
    let resumed = if options.resume {
        Some(orq_orchestrator::plan_resume(&store, repo, &project_id, &slug(options.feature)).map_err(other)?)
    } else {
        None
    };
    // A resumed or published run names nothing new, so it must not need a free name either.
    let naming = if published.is_some() || resumed.is_some() {
        None
    } else {
        Some(
            orq_core::naming::run_naming(settings.branch_prefix.as_deref().unwrap_or(orq_core::naming::DEFAULT_BRANCH_PREFIX), options.feature, |n| {
                git_ok(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", n.branch)])
                    || repo.join(&n.worktree_dir).exists()
            })
            .map_err(|e| other(e.to_string()))?,
        )
    };

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
        project_id,
        feature_id: slug(options.feature),
        remote: options.remote.to_string(),
        provider: match provider {
            orq_git::Provider::GitHub => "github",
            orq_git::Provider::GitLab => "gitlab",
        }
        .to_string(),
        target_branch: options.target.to_string(),
        base_sha: resumed.as_ref().map_or_else(|| base_sha.clone(), |r| r.from.base_sha.clone()),
        branch: match (&published, &resumed) {
            (Some(branch), _) => branch.clone(),
            (None, Some(r)) => r.from.branch.clone(),
            (None, None) => naming.as_ref().expect("a new run is named").branch.clone(),
        },
        worktree: match (&published, &resumed) {
            (Some(_), _) => repo.clone(),
            (None, Some(r)) => r.from.worktree.clone(),
            (None, None) => repo.join(&naming.as_ref().expect("a new run is named").worktree_dir),
        },
        repository,
        profiles: [
            ("implementer".to_string(), profile(&with_run_model(&options.implementer, options, &settings))),
            ("reviewer".to_string(), profile(&options.reviewer)),
        ]
        .into_iter()
        .collect(),
    };
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
    // Carried over first thing: a run resumed again after failing early must still skip the agent.
    if let Some(from) = resumed.as_ref().filter(|r| r.skip_implement).map(|r| r.from.run_id.as_str()) {
        if let Err(err) = orq_orchestrator::record_inherited_implement(&store, &record, from) {
            let released = finish(&store);
            let message = format!("cannot record that the agent session already finished: {err:?}");
            if let Err(err) = orq_orchestrator::record_run_completed(&store, &record, "failed", Some(&message)) {
                eprintln!("orq: cannot record the end of {run_id}: {err:?}");
            }
            if let Err(lock) = released {
                eprintln!("orq: the feature lock of {run_id} was not released: {lock:?}");
            }
            return Err(other(message));
        }
    }
    let created = match &naming {
        Some(naming) => orq_git::create_run_worktree(repo, &naming.worktree_dir, &naming.branch, &base_sha).map(|_| ()),
        None => Ok(()),
    };
    if let Err(err) = created {
        let released = finish(&store);
        let mut message = format!("cannot create the worktree: {err:?}");
        if let Err(lock) = released {
            message.push_str(&format!("; the feature lock was not released: {lock:?}"));
        }
        if let Err(err) = orq_orchestrator::record_run_completed(&store, &record, "failed", Some(&message)) {
            eprintln!("orq: cannot record the end of {run_id}: {err:?}");
        }
        announce(&settings, repo, &record.feature_id, "failed", Some(&message), None);
        return Err(other(message));
    }

    let logs = root.join("logs").join(&run_id);
    let outcome = std::fs::create_dir_all(&logs)
        .map_err(StoreError::from)
        .and_then(|()| stages(&store, &record, options, &settings, provider.cli(), &logs, resumed.as_ref().filter(|r| r.skip_implement).map(|r| r.from.run_id.as_str())));
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
    if report.ending == Ending::Failed && settings.explain_failures != Some(false) {
        explain_failure(options, &settings, &record, &logs, report.detail.as_deref().unwrap_or("the run failed"));
    }
    announce(&settings, repo, &record.feature_id, report.ending.name(), report.detail.as_deref(), report.review_url.as_deref());
    Ok(report)
}

/// Start a detached Claude session in the run's worktree that explains the failure and can be reached from
/// the Claude app. Best effort: it never changes how the run ended and the run does not wait for it.
fn explain_failure(options: &Options<'_>, settings: &orq_orchestrator::RunSettings, record: &RunRecord, logs: &Path, detail: &str) {
    if options.implementer.harness != "claude" || !record.worktree.is_dir() {
        return;
    }
    let Some(program) = find_program("claude") else { return };
    let model = with_run_model(&options.implementer, options, settings).model;
    let name = format!("{}-explain", record.branch);
    let Some(logs_dir) = logs.to_str() else { return };
    let argv = match orq_agents::explain_failure_argv(&model, &name, detail, logs_dir) {
        Ok(argv) => argv,
        Err(err) => {
            eprintln!("orq: cannot start the failure explainer: {err:?}");
            return;
        }
    };
    let log = std::fs::File::create(logs.join("explain.log"));
    let mut command = Command::new(program);
    command.args(&argv).current_dir(&record.worktree).stdin(std::process::Stdio::null());
    if let Ok(file) = log.and_then(|f| f.try_clone().map(|c| (f, c))) {
        command.stdout(file.0).stderr(file.1);
    }
    if let Err(err) = command.spawn() {
        eprintln!("orq: cannot start the failure explainer: {err}");
    }
}

/// Tell the configured notifier how a run ended. Best effort: a notifier that fails never
/// changes how the run ended.
fn announce(settings: &orq_orchestrator::RunSettings, cwd: &Path, feature: &str, ending: &str, detail: Option<&str>, url: Option<&str>) {
    let Some(argv) = settings.notify_cmd.as_deref() else { return };
    if !orq_orchestrator::wants_notification(settings.notify_on.as_deref(), ending) {
        return;
    }
    let message = orq_orchestrator::notification_message(feature, ending, detail, url);
    if let Err(err) = orq_orchestrator::send_notification_with(argv, cwd, &message, Duration::from_secs(30), settings.notify_env.as_deref().unwrap_or(&[])) {
        eprintln!("orq: the notification command failed: {err}");
    }
}

/// The number `git rev-list --count` printed. A failed git or unreadable output is an error: an
/// unestablished count must never read as "there are commits to publish".
fn ahead_count(output: &std::process::Output) -> Result<u64, String> {
    if !output.status.success() {
        return Err(format!("cannot count the commits to publish: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "cannot count the commits to publish: git printed no number".to_string())
}

/// The branch `--publish` pushes: the one checked out in `repo`. It must be a real branch other
/// than the target, carry commits the target lacks, and leave the working tree clean.
fn published_branch(repo: &Path, target: &str, base_sha: &str) -> Result<String, String> {
    let head = Command::new("git").arg("-C").arg(repo).args(["symbolic-ref", "--quiet", "--short", "HEAD"]).output().map_err(|e| e.to_string())?;
    let branch = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if !head.status.success() || branch.is_empty() {
        return Err("nothing to publish: HEAD is detached; check out the branch to publish".to_string());
    }
    if branch == target {
        return Err(format!("nothing to publish: {branch} is the target branch"));
    }
    let status = Command::new("git").arg("-C").arg(repo).args(["status", "--porcelain", "--untracked-files=no"]).output().map_err(|e| e.to_string())?;
    if !status.status.success() || !status.stdout.is_empty() {
        return Err("the working tree has uncommitted changes; commit or stash them before publishing".to_string());
    }
    let ahead = Command::new("git").arg("-C").arg(repo).args(["rev-list", "--count", &format!("{base_sha}..HEAD")]).output().map_err(|e| e.to_string())?;
    if ahead_count(&ahead)? == 0 {
        return Err(format!("nothing to publish: {branch} has no commits beyond {target}"));
    }
    Ok(branch)
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
    implement_inherited_from: Option<&str>,
) -> Result<Report, StoreError> {
    let run_id = record.run_id.as_str();
    let worktree = record.worktree.as_path();
    let implementer = with_run_model(&options.implementer, options, settings);

    let Some(claude) = find_program(&options.implementer.harness) else {
        return stopped(Ending::Failed, format!("the {} harness is not on PATH", options.implementer.harness));
    };
    let Some(claude_program) = claude.to_str() else {
        return stopped(Ending::Failed, format!("the {} harness path is not valid UTF-8", options.implementer.harness));
    };
    let databases = {
        let named = settings.test_db_vars.is_some();
        let vars = settings.test_db_vars.clone().unwrap_or_else(|| vec!["DB_NAME".to_string(), "B2C_DB_NAME".to_string()]);
        orq_orchestrator::derive_worktree_databases_with(&options.project.root, &vars, &record.branch, named, settings.db_name_template.as_deref())
    };
    let mut run_vars = databases.env.clone();
    // Every session of the run, and its settings file, is told where to auto-compact.
    if let Some(tokens) = settings.compact_window {
        run_vars.push(("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), tokens.to_string()));
    }
    let mut notes = databases.notes.clone();
    if !options.publish {
        let carried = orq_orchestrator::carry_local_files(&options.project.root, worktree, settings.local_files.as_deref());
        run_vars.extend(carried.env);
        notes.extend(carried.notes);
        notes.extend(orq_orchestrator::exclude_worktrees_dir(&options.project.root));
        notes.extend(orq_orchestrator::write_session_settings(worktree, &run_vars, settings.mcp_servers.as_deref()));
        let mcpctl = find_program("mcpctl");
        notes.extend(orq_orchestrator::setup_mcp_servers(worktree, settings.mcp_servers.as_deref().unwrap_or(&[]), &run_vars, mcpctl.as_deref()));
        notes.extend(orq_orchestrator::record_orq_written(worktree));
    }
    let database_env = run_vars.as_slice();
    if !options.publish {
        let plan = match plan_setup(worktree, &settings.setup) {
            Ok(plan) => plan,
            Err(err) => return stopped(Ending::Failed, format!("setup: {err:?}")),
        };
        if !notes.is_empty() {
            let _ = std::fs::write(logs.join("databases.log"), notes.join("\n") + "\n");
        }
        if let Err(failure) = run_setup_with_env(worktree, &plan.steps, &logs.join("setup.log"), database_env) {
            return stopped(Ending::Failed, format!("setup failed: {}", failure.reason));
        }

        // A resumed run whose agent session already finished goes on from the next stage; that was
        // recorded when the run began, so it survives a failure anywhere before here.
        if implement_inherited_from.is_none() {
            let adapter = adapter_for(&options.implementer.harness).map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
            // The session is asked to leave this file when it is finished, and is closed once it does.
            let marker_path = logs.join("implement.done");
            let marker = (settings.auto_exit != Some(false)).then(|| marker_path.to_str()).flatten();
            let args = adapter
                .implement_argv(&ImplementRequest {
                    feature: options.feature,
                    implementer: spec(&implementer),
                    reviewer: spec(&options.reviewer),
                    context: options.context,
                    exit_marker: marker,
                    remote_control: (settings.remote_control != Some(false)).then_some(record.branch.as_str()),
                })
                .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
            let session = ImplementSession {
                program: claude_program,
                args: &args,
                env: database_env,
                cwd: worktree,
                session_id: None,
                log: &logs.join("implement.log"),
                terminal: false,
                exit_marker: marker.map(std::path::Path::new),
            };
            if !run_implement_stage(store, run_id, &session)?.success {
                return stopped(Ending::Failed, "the implement session failed");
            }
            if let ImplementCheck::Blocked { reason } = check_implement_outcome(store, run_id)? {
                return stopped(Ending::Blocked, reason);
            }
        }
    }

    if let RebaseOutcome::Conflict { reason } = rebase_onto_target(store, run_id, options.rebase)? {
        return stopped(Ending::Blocked, reason);
    }

    let gate = choose_gate(worktree, settings.gate.as_deref()).map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
    let gate_log = logs.join("gate.log");
    let fix_model = implementer.model.clone();
    let fix_log = logs.join("gate-fix.log");
    // A red gate gets a fix session with the run's environment, then runs again. A published branch is
    // the user's own checkout: it is only checked, never edited by an agent.
    let fix_attempts = if options.publish { 0 } else { GATE_FIX_ATTEMPTS };
    let gate_outcome =
        run_gate_with_fixes_env(store, run_id, &gate, options.gate, &gate_log, fix_attempts, database_env, |_, log| FixSessionSpec {
            program: claude_program.to_string(),
            args: vec![
                "-p".to_string(),
                "--model".to_string(),
                fix_model.clone(),
                format!(
                    "The local gate failed after your changes. Its log is at {}. Read it, fix the cause in this worktree and commit the fix.",
                    log.display()
                ),
            ],
            env: database_env.to_vec(),
            cwd: worktree.to_path_buf(),
            session_id: None,
            log: fix_log.clone(),
            terminal: false,
        })?;
    match gate_outcome {
        GateFixOutcome::Failed { step, exit_code, .. } => {
            return stopped(Ending::Failed, format!("the gate failed at step {step} (exit {exit_code:?})"));
        }
        // A repair that was not committed would leave the pushed commit unfixed.
        GateFixOutcome::Passed { fix_attempts } if fix_attempts > 0 => {
            if let ImplementCheck::Blocked { reason } = check_implement_outcome(store, run_id)? {
                return stopped(Ending::Blocked, reason);
            }
        }
        _ => {}
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
        args: match settings_or_flag_model(options, settings) {
            Some(model) => vec!["-p".to_string(), "--model".to_string(), model.to_string(), prompt.to_string()],
            None => vec!["-p".to_string(), prompt.to_string()],
        },
        env: database_env.to_vec(),
        cwd: worktree.to_path_buf(),
    })?;
    let draft = match options.issue {
        Some(number) => orq_orchestrator::link_issue(draft, number),
        None => draft,
    };
    let people = match orq_orchestrator::ReviewPeople::checked(
        options.mr_assignee.or(settings.mr_assignee.as_deref()),
        options.mr_reviewer.or(settings.mr_reviewer.as_deref()),
    ) {
        Ok(people) => people,
        Err(reason) => return stopped(Ending::Failed, reason),
    };
    let review = match create_review_request_with(store, run_id, &provider, &draft, &people, Duration::from_secs(60))? {
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
    if !options.publish {
        // The test databases go with the worktree; a kept worktree may still need them.
        if cleanup_worktree(store, run_id, ready, &ci)? == orq_orchestrator::CleanupOutcome::Removed {
            let keep = settings.keep_test_db.unwrap_or(false);
            let dropped = orq_orchestrator::drop_test_databases(&options.project.root, &databases.names,settings.test_db_drop_cmd.as_deref(), keep);
            if !dropped.is_empty() {
                use std::io::Write;
                let log = std::fs::OpenOptions::new().create(true).append(true).open(logs.join("databases.log"));
                let _ = log.and_then(|mut file| file.write_all((dropped.join("\n") + "\n").as_bytes()));
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::ahead_count;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    fn output(code: i32, stdout: &str) -> Output {
        Output { status: ExitStatus::from_raw(code << 8), stdout: stdout.as_bytes().to_vec(), stderr: b"fatal: bad object\n".to_vec() }
    }

    #[test]
    fn the_commit_count_must_come_from_a_successful_git_and_be_a_number() {
        assert_eq!(ahead_count(&output(0, "3\n")), Ok(3));
        assert_eq!(ahead_count(&output(0, "0\n")), Ok(0));
        assert!(ahead_count(&output(128, "")).unwrap_err().contains("bad object"));
        assert!(ahead_count(&output(0, "")).is_err());
        assert!(ahead_count(&output(0, "many\n")).is_err());
    }
}
