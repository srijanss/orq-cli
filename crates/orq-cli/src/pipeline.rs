//! `orq run`: the stages of one run, in order, driven through the orchestrator's
//! stage functions. A run never merges; it ends at review_ready, blocked or failed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orq_agents::{adapter_for, FixRequest, ImplementRequest, ModelSpec};
use orq_features::ResolvedModel;
use orq_git::{detect_provider, fetch_target_base, ProviderHosts};
use orq_orchestrator::{
    check_implement_outcome, choose_gate, cleanup_worktree, create_review_request_with, draft_review_description,
    plan_setup, push_run_branch, rebase_onto_target, record_launch, run_ci_fix_loop, run_gate_with_fixes_env, run_implement_stage, run_setup_with_env, wait_for_ci_logged,
    CiFixConfig, CiFixOutcome, CiFixSteps, CiOutcome, CiWaitConfig, DraftSession, FixSessionSpec, GateFixOutcome, ImplementCheck, ImplementSession, PushOutcome,
    RebaseOutcome, ReviewRequestOutcome, RoleProfile, RunRecord, RunStore, StoreError,
};

/// How many times a red gate is handed to a fix session before the run fails.
const GATE_FIX_ATTEMPTS: u32 = 2;
const CI_FIX_ATTEMPTS: u32 = 2;

#[derive(Clone)]
pub struct Options<'a> {
    pub project: &'a orq_git::Project,
    /// The features of the run in the order to do them; the first names the branch and holds the lock.
    pub features: &'a [String],
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
    /// On resume: the role continues on the profile named for it; otherwise on the paused run's own.
    pub switch_implementer: bool,
    pub switch_reviewer: bool,
    /// What the run does once its work is ready. A resumed run keeps the paused run's unless
    /// `delivery_named` says the flag named this one.
    pub delivery: orq_core::project_settings::Delivery,
    pub delivery_named: bool,
    /// The implement session is given orq's own terminal, so the agent runs interactively in it.
    pub interactive: bool,
}

/// A run id no other run has: `run-<seconds>-<pid>-<sequence>`, the sequence telling apart the
/// runs one long-lived process (the popup) names within the same second.
pub(crate) fn new_run_id() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "run-{}-{}-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// The run that already holds the lock of `feature` in `project`, if any. A detached launch asks
/// first, so a busy feature is refused by the launcher instead of by a runner nobody is watching.
pub fn active_run(project: &orq_git::Project, feature: &str) -> Result<Option<String>, String> {
    let root = state_root().ok_or_else(|| "no state location: set XDG_STATE_HOME or HOME".to_string())?;
    RunStore::new(&root).active_run(&project_id(project), &slug(feature)).map_err(|e| e.to_string())
}

/// The first of `features` that is held by a run, with that run's id.
pub fn active_group_run(project: &orq_git::Project, features: &[String]) -> Result<Option<(String, String)>, String> {
    for feature in features {
        if let Some(run) = active_run(project, feature)? {
            return Ok(Some((feature.clone(), run)));
        }
    }
    Ok(None)
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
    /// The implement session's review loop escalated: a human reviews again, accepts or discards.
    NeedsReview,
    /// Stopped at a stage boundary because a pause was requested; the run can be resumed.
    Paused,
    /// The run's delivery is push: its branch was pushed and nothing else was asked for.
    Pushed,
}

impl Ending {
    pub fn name(self) -> &'static str {
        match self {
            Ending::ReviewReady => "review_ready",
            Ending::Blocked => "blocked",
            Ending::Failed => "failed",
            Ending::NeedsReview => "needs_review",
            Ending::Paused => "paused",
            Ending::Pushed => "pushed",
        }
    }
}

pub struct Report {
    pub run_id: String,
    pub ending: Ending,
    pub detail: Option<String>,
    pub review_url: Option<String>,
    /// For a review that is ready: whether CI was watched and green (`false`: it was not watched).
    pub ci_green: Option<bool>,
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
    orq_git::project_id(project)
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
    // A resumed run's implementer is already the effective one: the paused run's, or the switched-to.
    if options.resume {
        return implementer.clone();
    }
    match settings_or_flag_model(options, settings) {
        Some(model) => ResolvedModel { model: model.to_string(), ..implementer.clone() },
        None => implementer.clone(),
    }
}

fn spec(model: &ResolvedModel) -> ModelSpec {
    ModelSpec { harness: model.harness.clone(), model: model.model.clone(), thinking: model.thinking.clone() }
}

pub fn execute(options: &Options<'_>) -> Result<Report, StartError> {
    if !options.resume {
        return execute_run(options);
    }
    // A resumed run keeps the paused run's profiles exactly, unless a switch named another for a role.
    let mut adjusted = options.clone();
    if let Some(root) = state_root() {
        let store = RunStore::new(&root);
        let members: Vec<String> = options.features.iter().map(|f| slug(f)).collect();
        if let Ok(plan) = orq_orchestrator::plan_group_resume(&store, &options.project.root, &project_id(options.project), &members) {
            let stored = |role: &str, named: &ResolvedModel| match plan.from.profiles.get(role) {
                Some(p) => ResolvedModel { harness: p.harness.clone(), model: p.model.clone(), thinking: p.thinking.clone() },
                None => named.clone(),
            };
            if !options.switch_implementer {
                adjusted.implementer = stored("implementer", &options.implementer);
            }
            if !options.switch_reviewer {
                adjusted.reviewer = stored("reviewer", &options.reviewer);
            }
        }
    }
    execute_run(&adjusted)
}

/// Write `handoff.json` for `record`: the feature plan, the diff of the paused run's branch against its base, and the
/// gate result it stopped with.
fn write_handoff_record(
    store: &RunStore,
    record: &orq_orchestrator::RunRecord,
    from: &orq_orchestrator::RunRecord,
    project: &orq_git::Project,
) -> Result<(), String> {
    // A project without a ledger has no plan; a ledger that cannot be read or parsed is an error, not an empty plan.
    let plan = match orq_features::load_ledger(&orq_features::ledger_path(project)) {
        Ok(entries) => entries
            .into_iter()
            .map(|e| orq_orchestrator::PlanItem { id: e.feature_name, description: e.description, status: e.status.as_str().to_string() })
            .collect(),
        Err(orq_features::LedgerError::Missing { .. }) => Vec::new(),
        Err(err) => return Err(format!("the feature ledger: {err:?}")),
    };
    let evidence = orq_git::diff_evidence(&from.worktree, &from.base_sha, "HEAD").map_err(|e| format!("{e:?}"))?;
    let diff = orq_orchestrator::HandoffDiff {
        base_sha: evidence.base_sha,
        head_sha: evidence.head_sha,
        files: evidence
            .files
            .into_iter()
            .map(|f| orq_orchestrator::DiffFile { path: f.path, old_path: f.old_path, added: f.added, deleted: f.deleted })
            .collect(),
    };
    orq_orchestrator::write_handoff(store, record, &from.run_id, plan, diff).map(|_| ()).map_err(|e| format!("{e:?}"))
}

/// What a run learns from the checks it makes before it fetches, records or names anything.
struct Preflight {
    settings: orq_orchestrator::RunSettings,
    resumed: Option<orq_orchestrator::ResumePlan>,
    naming: Option<orq_core::naming::RunNaming>,
    provider: orq_git::Provider,
    repository: String,
}

/// The refusals a run makes before it starts anything: shared by the run itself and by
/// `orq run --check`, so a check that says ready is one the run would not refuse here.
fn preflight(options: &Options<'_>, store: &RunStore) -> Result<Preflight, StartError> {
    let repo = &options.project.root;
    if options.resume && options.publish {
        return Err(other("--resume and --publish cannot be combined"));
    }

    // Read from the user's checkout, never from the agent's worktree.
    let settings = orq_orchestrator::load_run_settings(&options.project.root, &|name| std::env::var(name).ok())
        .map_err(|e| other(format!("run settings: {e}")))?;
    if let Some(file) = &settings.gate_from_script {
        eprintln!("orq: using GATE_CMD from {file} as the gate");
    }
    let resumed = if options.resume {
        let members: Vec<String> = options.features.iter().map(|f| slug(f)).collect();
        Some(orq_orchestrator::plan_group_resume(store, repo, &project_id(options.project), &members).map_err(other)?)
    } else {
        None
    };
    // A resumed or published run names nothing new, so it must not need a free name either.
    let naming = if options.publish || resumed.is_some() {
        None
    } else {
        Some(
            orq_core::naming::run_naming(settings.branch_prefix.as_deref().unwrap_or(orq_core::naming::DEFAULT_BRANCH_PREFIX), &options.features[0], |n| {
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
    Ok(Preflight { settings, resumed, naming, provider, repository })
}

/// Every refusal [`execute`] makes before it fetches or records anything, with nothing started.
pub fn check(options: &Options<'_>) -> Result<(), StartError> {
    let root = state_root().ok_or_else(|| other("no state location: set XDG_STATE_HOME or HOME"))?;
    preflight(options, &RunStore::new(&root)).map(|_| ())
}

fn execute_run(options: &Options<'_>) -> Result<Report, StartError> {
    let repo = &options.project.root;
    let root = state_root().ok_or_else(|| other("no state location: set XDG_STATE_HOME or HOME"))?;
    let store = RunStore::new(&root);
    let Preflight { settings, resumed, naming, provider, repository } = preflight(options, &store)?;
    let base_sha = fetch_target_base(repo, options.remote, options.target).map_err(|e| other(e.to_string()))?;
    let published = if options.publish { Some(published_branch(repo, options.target, &base_sha).map_err(other)?) } else { None };
    let project_id = project_id(options.project);

    let run_id = options.run_id.map_or_else(new_run_id, str::to_string);
    let record = RunRecord {
        delivery: match &resumed {
            Some(r) if !options.delivery_named => r.from.delivery,
            _ => options.delivery,
        },
        run_id: run_id.clone(),
        project_id,
        feature_id: slug(&options.features[0]),
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
    let members: Vec<String> = options.features.iter().map(|f| slug(f)).collect();
    match store.start_group_run(&record, &members) {
        Ok(()) => {}
        Err(StoreError::FeatureBusy { active_run_id }) => return Err(StartError::Busy(active_run_id)),
        Err(err) => return Err(other(format!("{err:?}"))),
    }
    orq_orchestrator::end_children_with_this_process();
    // Every run names its owner process (detached or not), so a killed one can be told from a live
    // one. Only a detached run has a runner log to name.
    let recorded = if options.run_id.is_some() {
        let log = root.join("logs").join(&run_id).join("runner.log");
        record_launch(&store, &run_id, std::process::id(), &log)
    } else {
        orq_orchestrator::record_owner(&store, &run_id, std::process::id())
    };
    if let Err(err) = recorded {
        eprintln!("orq: cannot record the launch of {run_id}: {err}");
    }
    let finish = |store: &RunStore| store.finish_group_run(&record.project_id, &members, &record.run_id);
    // A resume that skips the rebase still sits on the commit the paused run rebased onto, and a gate
    // told otherwise would judge the wrong changes. Without it the resume stops before it does
    // anything. Its run is removed, not failed: a resume continues a feature's newest run, and that
    // has to stay the one it could not continue.
    if let Some(from) = resumed.as_ref().map(|r| r.from.run_id.as_str()) {
        if let Err(err) = orq_orchestrator::carry_rebase(&store, &record, from) {
            let mut message = format!("cannot carry the rebase of {from} into the resumed run: {err:?}; {from} was left as it was, resume it again");
            if let Err(lock) = finish(&store) {
                message.push_str(&format!("; the feature locks were not released: {lock:?}"));
            }
            if let Err(removal) = store.remove_run(&run_id) {
                message.push_str(&format!("; {run_id} was not removed: {removal:?}"));
            }
            return Err(other(message));
        }
    }
    // Which features this run is for, so that only a run of the same group can be resumed.
    if members.len() > 1 {
        if let Err(err) = orq_orchestrator::record_group(&store, &record, &members) {
            let released = finish(&store);
            let message = format!("cannot record the features of the group: {err:?}");
            if let Err(err) = orq_orchestrator::record_run_completed(&store, &record, "failed", Some(&message)) {
                eprintln!("orq: cannot record the end of {run_id}: {err:?}");
            }
            if let Err(lock) = released {
                eprintln!("orq: the feature locks of {run_id} were not released: {lock:?}");
            }
            return Err(other(message));
        }
    }
    // A model that differs from the paused run's is a switch: record what it was and what it is.
    if let Some(from) = resumed.as_ref().map(|r| &r.from) {
        let mut switched = false;
        for (role, now) in &record.profiles {
            let Some(was) = from.profiles.get(role) else { continue };
            if was == now {
                continue;
            }
            switched = true;
            if let Err(err) = orq_orchestrator::record_model_switched(&store, &record, role, was, now) {
                eprintln!("orq: cannot record the model switch of {run_id}: {err:?}");
            }
        }
        // Whoever continues after a switch starts from state, not from the old conversation.
        if switched {
            if let Err(err) = write_handoff_record(&store, &record, from, options.project) {
                eprintln!("orq: cannot write the handoff of {run_id}: {err}");
            }
        }
    }
    // The sessions the paused run already ran stay queryable from the resumed run, whatever model it continues on.
    if let Some(from) = resumed.as_ref().map(|r| r.from.run_id.as_str()) {
        if let Err(err) = orq_orchestrator::carry_session_references(&store, &record, from) {
            eprintln!("orq: cannot carry the sessions of {from} into {run_id}: {err:?}");
        }
    }
    // A paused run handed its checkout to this run; it no longer holds it.
    if let Some(from) = resumed.as_ref().map(|r| &r.from) {
        let was_paused = store.events(&from.run_id).map(|e| orq_orchestrator::summarize_run(&e).phase == orq_orchestrator::RunPhase::Paused).unwrap_or(false);
        if was_paused {
            if let Err(err) = orq_orchestrator::supersede_paused(&store, from, &run_id) {
                eprintln!("orq: cannot mark {} as resumed: {err:?}", from.run_id);
            }
        }
    }
    // A new harness continues the work from the handoff, so only an implementer on the same harness skips its finished session.
    let inherited = resumed.as_ref().filter(|r| {
        r.skip_implement && r.from.profiles.get("implementer").map(|p| &p.harness) == record.profiles.get("implementer").map(|p| &p.harness)
    });
    // Carried over first thing: a run resumed again after failing early must still skip the agent.
    if let Some(from) = inherited.map(|r| r.from.run_id.as_str()) {
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
        announce(&settings, repo, &options.features.join(","), "failed", Some(&message), None);
        return Err(other(message));
    }

    // New implementation work invalidates what the checkpoint recorded as done after it: the gate and the push
    // must cover the new commits. Only the review request, already opened, is kept so it is not opened twice.
    let checkpoint = resumed.as_ref().and_then(|r| r.checkpoint.clone()).map(|mut c| {
        if resumed.as_ref().is_some_and(|r| r.skip_implement) && inherited.is_none() {
            c.completed_stages.retain(|s| s == "review_request");
            c.push_ref = None;
        }
        c
    });
    // A run that waited for a review decision is resumed as another review of what came after the escalated head.
    let review_again = resumed.as_ref().and_then(|r| r.review_again.clone());
    let logs = root.join("logs").join(&run_id);
    let resumed_from = Resumed { implement_inherited_from: inherited.map(|r| r.from.run_id.as_str()), paused_at: checkpoint.as_ref(), review_again: review_again.as_ref() };
    let outcome = std::fs::create_dir_all(&logs)
        .map_err(StoreError::from)
        .and_then(|()| stages(&store, &record, options, &settings, provider.cli(), &logs, &resumed_from));
    let mut report = match outcome {
        Ok(mut report) => {
            report.run_id = run_id;
            report
        }
        Err(err) => Report { run_id, ending: Ending::Failed, detail: Some(format!("{err:?}")), review_url: None, ci_green: None },
    };
    // Reopened while the run still holds the locks, so a run launched next is never reset.
    if matches!(report.ending, Ending::Blocked | Ending::Failed) && !has_commits(&record) {
        // Nothing was built, so nothing continues the features: they may be launched again.
        if let Err(err) = orq_features::reopen_features(&orq_features::ledger_path(options.project), options.features) {
            eprintln!("orq: cannot put {} back to pending: {err}", options.features.join(","));
        }
    }
    let released = finish(&store);
    // A lock left behind blocks the next run of the feature, so it is never reported as success.
    if let Err(lock) = released {
        report.ending = Ending::Failed;
        let note = format!("the feature lock was not released: {lock:?}");
        report.detail = Some(match report.detail.take() {
            Some(detail) => format!("{detail}; {note}"),
            None => note,
        });
    }
    // A paused run has not ended: its `run.paused` event is already recorded.
    if report.ending != Ending::Paused {
        if let Err(err) = orq_orchestrator::record_run_completed(&store, &record, report.ending.name(), report.detail.as_deref()) {
            eprintln!("orq: cannot record the end of {}: {err:?}", report.run_id);
        }
    }
    if report.ending == Ending::Failed && settings.explain_failures != Some(false) {
        explain_failure(options, &settings, &record, &logs, report.detail.as_deref().unwrap_or("the run failed"));
    }
    match (report.ending, report.review_url.as_deref(), report.ci_green) {
        (Ending::ReviewReady, Some(url), Some(ci_green)) => {
            let repo_name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let message = orq_orchestrator::review_ready_message(&repo_name, &record.provider, ci_green, url, &record.branch);
            deliver(&settings, repo, report.ending.name(), &message);
        }
        _ => announce(&settings, repo, &options.features.join(","), report.ending.name(), report.detail.as_deref(), report.review_url.as_deref()),
    }
    Ok(report)
}

/// The review in `worktree` that escalated, if one did. A ledger that cannot be read is said and
/// counts as none, so the run fails as it would have.
fn escalated(worktree: &Path, base: &str) -> Option<orq_orchestrator::EscalatedReview> {
    orq_orchestrator::escalated_review(worktree, base)
        .inspect_err(|err| eprintln!("orq: cannot read the review ledger: {err}"))
        .ok()
        .flatten()
}

fn escalation_reason(review: &orq_orchestrator::EscalatedReview) -> String {
    let open = review.findings.iter().filter(|f| f.status == "open").count();
    let rounds = if review.rounds == 1 { "round" } else { "rounds" };
    let findings = if open == 1 { "finding" } else { "findings" };
    format!("the review escalated after {} {rounds} with {open} {findings} open", review.rounds)
}

/// The findings still open in an escalated review, one line each, for the session that reviews again.
fn open_findings_text(review: &orq_orchestrator::EscalatedReview) -> String {
    let lines: Vec<String> = review
        .findings
        .iter()
        .filter(|f| f.status == "open")
        .map(|f| {
            let place = match (&f.file, f.line) {
                (Some(file), Some(line)) => format!(", {file}:{line}"),
                (Some(file), None) => format!(", {file}"),
                _ => String::new(),
            };
            format!("- {} ({}{place}): {}", f.id, f.severity, f.claim)
        })
        .collect();
    if lines.is_empty() {
        "(none: each was fixed, rejected or deferred since)".to_string()
    } else {
        lines.join("\n")
    }
}

/// Whether the run's worktree has a commit since the run's base. A worktree that is gone has none.
fn has_commits(record: &RunRecord) -> bool {
    if !record.worktree.is_dir() {
        return false;
    }
    let output = Command::new("git").arg("-C").arg(&record.worktree).args(["rev-list", "--count", &format!("{}..HEAD", record.base_sha)]).output();
    // When git cannot tell, the work is assumed to be there, so the feature is not reopened.
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() != "0",
        _ => true,
    }
}

/// Start a detached Claude session in the run's worktree that explains the failure and can be reached from
/// the Claude app. Best effort: it never changes how the run ended and the run does not wait for it.
fn explain_failure(options: &Options<'_>, settings: &orq_orchestrator::RunSettings, record: &RunRecord, logs: &Path, detail: &str) {
    if options.implementer.harness != "claude" || !record.worktree.is_dir() {
        return;
    }
    let Some(program) = find_program("claude") else { return };
    let model = with_run_model(&options.implementer, options, settings).model;
    // The explainer is reachable from the app only when the run's own session is.
    let name = (settings.remote_control != Some(false)).then(|| format!("{}-explain", record.branch));
    let Some(logs_dir) = logs.to_str() else { return };
    let argv = match orq_agents::explain_failure_argv(&model, name.as_deref(), detail, logs_dir) {
        Ok(argv) => argv,
        Err(err) => {
            eprintln!("orq: cannot start the failure explainer: {err:?}");
            return;
        }
    };
    // Output goes to the log; when the log cannot be opened it is dropped, never inherited from the runner.
    let mut command = Command::new(program);
    command.args(&argv).current_dir(&record.worktree).stdin(std::process::Stdio::null());
    match std::fs::File::create(logs.join("explain.log")).and_then(|f| f.try_clone().map(|c| (f, c))) {
        Ok((out, err)) => command.stdout(out).stderr(err),
        Err(_) => command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()),
    };
    if let Err(err) = command.spawn() {
        eprintln!("orq: cannot start the failure explainer: {err}");
    }
}

/// Tell the configured notifier how a run ended. Best effort: a notifier that fails never
/// changes how the run ended.
pub(crate) fn announce(settings: &orq_orchestrator::RunSettings, cwd: &Path, feature: &str, ending: &str, detail: Option<&str>, url: Option<&str>) {
    let message = orq_orchestrator::notification_message(feature, ending, detail, url);
    deliver(settings, cwd, ending, &message);
}

/// Send `message` to the configured notifier and the Claude app, unless `notify_on` excludes `ending`.
fn deliver(settings: &orq_orchestrator::RunSettings, cwd: &Path, ending: &str, message: &str) {
    if !orq_orchestrator::wants_notification(settings.notify_on.as_deref(), ending) {
        return;
    }
    if let Some(argv) = settings.notify_cmd.as_deref() {
        if let Err(err) = orq_orchestrator::send_notification_with(argv, cwd, message, Duration::from_secs(30), settings.notify_env.as_deref().unwrap_or(&[])) {
            eprintln!("orq: the notification command failed: {err}");
        }
    }
    // The push goes through Remote Control, so a project that turned that off gets none.
    if settings.claude_push != Some(false) && settings.remote_control != Some(false) {
        push_to_claude_app(cwd, message);
    }
}

/// Where the detached sender finds the text, which stays out of the process list this way.
pub(crate) const PUSH_TEXT: &str = "ORQ_PUSH_TEXT";
const PUSH_SESSION_LIMIT: Duration = Duration::from_secs(60);
/// Where sessions auto-compact when the run settings do not say, as in the orchestrate script.
const DEFAULT_COMPACT_WINDOW: u32 = 150_000;
/// What the session gets to finish sending once the tool has run.
const PUSH_SESSION_LINGER: Duration = Duration::from_secs(3);

/// Send `message` to the Claude app from a detached `orq run push-send`. Best effort: it never changes
/// how the run ended, and the run does not wait for it.
fn push_to_claude_app(cwd: &Path, message: &str) {
    use std::process::Stdio;
    if find_program("claude").is_none() {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("orq: cannot send the Claude push notification: cannot find orq's own path: {err}");
            return;
        }
    };
    let mut sender = Command::new(exe);
    sender.args(["run", "push-send", "--repo"]).arg(cwd).env(PUSH_TEXT, message).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // In a process group of its own, so the end of the run's terminal session does not take it down.
    std::os::unix::process::CommandExt::process_group(&mut sender, 0);
    if let Err(err) = sender.spawn() {
        eprintln!("orq: cannot send the Claude push notification: {err}");
    }
}

/// Runs the short Claude session that sends `message`. Claude only reaches the app from an interactive
/// session with Remote Control on, and only tells a phone when it does not think somebody is watching
/// the terminal, so the session gets a terminal of its own and that check is switched off. It runs in
/// the repository because a folder Claude does not trust stops at a question nobody answers; project
/// memory and project settings stay out of it, and its only tool is the push.
pub(crate) fn send_push(cwd: &Path, message: &str) -> Result<(), String> {
    let program = find_program("claude").ok_or("claude is not on PATH")?;
    let dir = private_session_dir().map_err(|err| format!("no private directory for the session: {err}"))?;
    let marker = dir.join("sent");
    let name = format!("orq-push-{}", std::process::id());
    let argv = orq_agents::push_notification_argv(message, &name, &marker).map_err(|err| format!("{err:?}"))?;
    let mut command = Command::new(program);
    command
        .args(&argv)
        .current_dir(cwd)
        .env("CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK", "1")
        .env("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")
        .env("TERM", "xterm-256color")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env_remove(PUSH_TEXT);
    let ran = orq_orchestrator::run_in_terminal(command, &marker, PUSH_SESSION_LIMIT, PUSH_SESSION_LINGER);
    let delivered = marker.exists();
    let _ = std::fs::remove_dir_all(&dir);
    ran.map_err(|err| err.to_string())?;
    if delivered { Ok(()) } else { Err("the push notification session ended without sending it".to_string()) }
}

/// A new empty directory under the temp dir that only this user can enter. Created exclusively, so one
/// somebody else made first is never reused.
fn private_session_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    for attempt in 0..16u32 {
        let dir = std::env::temp_dir().join(format!("orq-push-{}-{nanos}-{attempt}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no unused directory name"))
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

fn paused(next_stage: &str) -> Result<Report, StoreError> {
    Ok(Report { run_id: String::new(), ending: Ending::Paused, detail: Some(format!("paused before {next_stage}")), review_url: None, ci_green: None })
}

fn at_boundary(store: &RunStore, run_id: &str, done: &[&str], next: &str, push_ref: Option<&str>, review_ref: Option<&str>) -> Result<bool, StoreError> {
    orq_orchestrator::pause_at_boundary(store, run_id, done, next, push_ref, review_ref)
}

fn stopped(ending: Ending, detail: impl Into<String>) -> Result<Report, StoreError> {
    Ok(Report { run_id: String::new(), ending, detail: Some(detail.into()), review_url: None, ci_green: None })
}

/// A group's review request names every feature in its title and lists them in its description.
fn group_draft(mut draft: orq_orchestrator::ReviewDraft, features: &[String]) -> orq_orchestrator::ReviewDraft {
    if features.len() < 2 {
        return draft;
    }
    let names = features.join(", ");
    draft.title = format!("{names}: {}", draft.title).chars().take(120).collect();
    let list: String = features.iter().map(|f| format!("- {f}\n")).collect();
    draft.body = format!("Features in this review:\n{list}\n{}", draft.body);
    draft
}

/// What a gate runs with: the run's variables, and `GATE_BASE`, the commit the branch was last
/// rebased onto, or the one it started from when it was not rebased. A pause or a resume cannot lose
/// it: it is read from the run's events, and a resumed run carries it over.
fn gate_env(store: &RunStore, record: &RunRecord, run_vars: &[(String, String)]) -> Result<Vec<(String, String)>, StoreError> {
    let base = orq_orchestrator::rebased_onto(store, &record.run_id)?.unwrap_or_else(|| record.base_sha.clone());
    let mut env = run_vars.to_vec();
    env.push(("GATE_BASE".to_string(), base));
    Ok(env)
}

/// What a resumed run takes over from the run it continues; all `None` for a new run.
#[derive(Clone, Copy)]
struct Resumed<'a> {
    /// That run's agent session finished, so this run does not start one.
    implement_inherited_from: Option<&'a str>,
    /// That run paused here, having done these stages.
    paused_at: Option<&'a orq_orchestrator::Checkpoint>,
    /// That run's review escalated, so this run's session reviews again from where it stopped.
    review_again: Option<&'a orq_orchestrator::EscalatedReview>,
}

fn stages(
    store: &RunStore,
    record: &RunRecord,
    options: &Options<'_>,
    settings: &orq_orchestrator::RunSettings,
    provider_cli: &str,
    logs: &Path,
    resumed: &Resumed<'_>,
) -> Result<Report, StoreError> {
    let Resumed { implement_inherited_from, paused_at, review_again } = *resumed;
    let run_id = record.run_id.as_str();
    // A run continued from a pause does not redo what the checkpoint records as done: the push and
    // the review request are outward-facing and must happen once.
    let done = |stage: &str| paused_at.is_some_and(|c| c.completed_stages.iter().any(|d| d == stage));
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
    // Every session of the run, and its settings file, is told where to auto-compact: where the
    // settings say, else where the orchestrate script does.
    let tokens = settings.compact_window.unwrap_or(DEFAULT_COMPACT_WINDOW);
    run_vars.push(("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), tokens.to_string()));
    let mut notes = databases.notes.clone();
    if !options.publish {
        let carried = orq_orchestrator::carry_local_files(&options.project.root, worktree, settings.local_files.as_deref());
        run_vars.extend(carried.env);
        notes.extend(carried.notes);
        notes.extend(orq_orchestrator::exclude_worktrees_dir(&options.project.root));
        let mcpctl = find_program("mcpctl");
        notes.extend(orq_orchestrator::prepare_mcp_servers(worktree, &run_vars, settings.mcp_servers.as_deref(), mcpctl.as_deref()));
        // Only a session with a terminal has someone to ask.
        if options.interactive {
            match std::env::current_exe() {
                Ok(orq) => {
                    let command = format!(
                        "{} run needs-input {} --repo {}",
                        crate::sh_quote(&orq.to_string_lossy()),
                        crate::sh_quote(&run_id),
                        crate::sh_quote(&options.project.root.to_string_lossy())
                    );
                    notes.extend(orq_orchestrator::write_needs_input_hook(worktree, &command));
                }
                Err(err) => notes.push(format!("the session will not say when it needs an answer: orq's own path is unknown ({:?})", err.kind())),
            }
        } else {
            notes.extend(orq_orchestrator::remove_needs_input_hook(worktree));
        }
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
            let features: Vec<&str> = options.features.iter().map(String::as_str).collect();
            let adapter = adapter_for(&options.implementer.harness).map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
            // The session is asked to leave this file when it is finished, and is closed once it does.
            let marker_path = logs.join("implement.done");
            let marker = (settings.auto_exit != Some(false)).then(|| marker_path.to_str()).flatten();
            // After a model switch the new session is told where the work stands, in words, never as the old conversation.
            let handoff = orq_orchestrator::read_handoff(&store, &run_id)
                .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?
                .map(|h| orq_orchestrator::handoff_prompt(&h));
            // Remote control is asked for unless the project turned it off; a harness without it runs without and says so.
            let wants_remote_control = settings.remote_control != Some(false);
            if wants_remote_control && !adapter.supports(orq_agents::Capability::RemoteControl) {
                let capability = orq_agents::Capability::RemoteControl.name();
                if let Err(err) = orq_orchestrator::record_capability_skipped(&store, &record, "implement_and_review", &options.implementer.harness, capability) {
                    eprintln!("orq: cannot record the skipped {capability} of {run_id}: {err:?}");
                }
            }
            let open_findings = review_again.map(open_findings_text);
            let args = adapter
                .implement_argv(&ImplementRequest {
                    features: &features,
                    implementer: spec(&implementer),
                    reviewer: spec(&options.reviewer),
                    context: options.context,
                    exit_marker: marker,
                    attended: options.interactive,
                    remote_control: (settings.remote_control != Some(false)).then_some(record.branch.as_str()),
                    handoff: handoff.as_deref(),
                    review_again: review_again.zip(open_findings.as_deref()).map(|(review, findings)| orq_agents::ReviewAgain { from: &review.reviewed_head, findings }),
                })
                .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
            let session = ImplementSession {
                program: claude_program,
                args: &args,
                env: database_env,
                cwd: worktree,
                session_id: None,
                log: &logs.join("implement.log"),
                terminal: options.interactive,
                exit_marker: marker.map(std::path::Path::new),
            };
            let outcome = run_implement_stage(store, run_id, &session)?;
            if !outcome.success {
                // A session that stopped for its escalated review has done what it could; the
                // decision is a human's, so the run waits for it instead of failing.
                if outcome.reported.is_some() {
                    if let Some(review) = escalated(worktree, &record.base_sha) {
                        return stopped(Ending::NeedsReview, escalation_reason(&review));
                    }
                }
                return stopped(Ending::Failed, match outcome.reported {
                    Some(problem) => format!("the implement session reported a problem: {problem}"),
                    None => "the implement session failed".to_string(),
                });
            }
            if let ImplementCheck::Blocked { reason } = check_implement_outcome(store, run_id)? {
                return stopped(Ending::Blocked, reason);
            }
        }
    }

    // The agent session has the terminal while it runs; after it, short lines keep the pane alive.
    let _progress = (options.interactive && !options.publish)
        .then(|| logs.parent().and_then(Path::parent).map(|root| crate::progress::Progress::start(root.to_path_buf(), run_id.to_string(), logs.to_path_buf())))
        .flatten();

    if !done("gate") {
        if at_boundary(store, run_id, &["implement_and_review"], "rebase", None, None)? {
            return paused("rebase");
        }
        if let RebaseOutcome::Conflict { reason } = rebase_onto_target(store, run_id, options.rebase)? {
            return stopped(Ending::Blocked, reason);
        }

        let gate = choose_gate(worktree, settings.gate.as_deref()).map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
        let gate_log = logs.join("gate.log");
        let fix_log = logs.join("gate-fix.log");
        // Where a fix session says that the failure is not the run's; the session is told in its environment too.
        let unrelated_report = logs.join(orq_orchestrator::UNRELATED_FAILURE_FILE);
        let mut fix_env = database_env.to_vec();
        fix_env.push(("ORQ_UNRELATED_FAILURE_FILE".to_string(), unrelated_report.to_string_lossy().into_owned()));
        // A red gate gets a fix session with the run's environment, then runs again. A published branch is
        // the user's own checkout: it is only checked, never edited by an agent.
        let fix_attempts = if options.publish { 0 } else { GATE_FIX_ATTEMPTS };
        // Someone watching the run gets the session on the terminal, closed once it says it is finished.
        let fix_marker = (options.interactive && settings.auto_exit != Some(false)).then(|| logs.join("gate-fix.done"));
        let fix_args = if options.publish {
            Vec::new()
        } else {
            let task = format!(
                "The local gate failed after your changes. Its log is at {}. Read it, fix the cause in this worktree and commit the fix. \
                 If the failure has nothing to do with your changes (a flaky test, a test already broken on the target branch, a broken environment), do not fix it: \
                 write a line saying what fails and why it is not caused by your changes to {} and stop.",
                gate_log.display(),
                unrelated_report.display()
            );
            let logs_dir = logs.to_str().ok_or_else(|| StoreError::Corrupt(format!("the logs folder {} is not valid text", logs.display())))?;
            adapter_for(&options.implementer.harness)
                .and_then(|adapter| {
                    adapter.fix_argv(&FixRequest {
                        implementer: spec(&implementer),
                        task: &task,
                        logs_dir,
                        exit_marker: fix_marker.as_deref().and_then(Path::to_str),
                        attended: options.interactive,
                    })
                })
                .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?
        };
        let gate_env = gate_env(store, record, database_env)?;
        let gate_outcome =
            run_gate_with_fixes_env(store, run_id, &gate, options.gate, &gate_log, fix_attempts, &gate_env, |_, _| FixSessionSpec {
                program: claude_program.to_string(),
                args: fix_args.clone(),
                env: fix_env.clone(),
                cwd: worktree.to_path_buf(),
                session_id: None,
                log: fix_log.clone(),
                terminal: options.interactive,
                exit_marker: fix_marker.clone(),
            })?;
        match gate_outcome {
            GateFixOutcome::Failed { step, exit_code, .. } => {
                return stopped(Ending::Failed, format!("the gate failed at step {step} (exit {exit_code:?})"));
            }
            // Left alone on purpose: the person decides whether it is fixed first, in its own change.
            GateFixOutcome::Unrelated { summary } => {
                return stopped(
                    Ending::Blocked,
                    format!("the gate fails for a reason unrelated to this run: {summary}. Get that fixed separately and merged into the target branch, then resume this run"),
                );
            }
            // A repair that was not committed would leave the pushed commit unfixed.
            GateFixOutcome::Passed { fix_attempts } if fix_attempts > 0 => {
                if let ImplementCheck::Blocked { reason } = check_implement_outcome(store, run_id)? {
                    return stopped(Ending::Blocked, reason);
                }
            }
            _ => {}
        }
    }

    if !done("push") && at_boundary(store, run_id, &["implement_and_review", "rebase", "gate"], "push", None, None)? {
        return paused("push");
    }
    let head_sha = match paused_at.filter(|_| done("push")) {
        Some(checkpoint) => checkpoint.head_sha.clone(),
        None => match push_run_branch(store, run_id, &logs.join("push.log"))? {
            PushOutcome::Pushed { head_sha } => head_sha,
            PushOutcome::Failed { reason } => return stopped(Ending::Failed, format!("push failed: {reason}")),
        },
    };
    // The worktree of a run whose work is delivered goes, and its test databases with it; a kept
    // worktree may still need them.
    let clean_up = |ready: bool, ci: &CiOutcome| -> Result<(), StoreError> {
        if options.publish {
            return Ok(());
        }
        if cleanup_worktree(store, run_id, ready, ci)? == orq_orchestrator::CleanupOutcome::Removed {
            let keep = settings.keep_test_db.unwrap_or(false);
            let dropped = orq_orchestrator::drop_test_databases(&options.project.root, &databases.names,settings.test_db_drop_cmd.as_deref(), keep);
            if !dropped.is_empty() {
                use std::io::Write;
                let log = std::fs::OpenOptions::new().create(true).append(true).open(logs.join("databases.log"));
                let _ = log.and_then(|mut file| file.write_all((dropped.join("\n") + "\n").as_bytes()));
            }
        }
        Ok(())
    };
    // The steps a passing gate ran; the report counts them for a commit only when the gate passed for it.
    let gate_steps = match choose_gate(worktree, settings.gate.as_deref()) {
        Ok(orq_orchestrator::Gate::Steps { steps, .. }) => steps,
        _ => Vec::new(),
    };
    if record.delivery == orq_core::project_settings::Delivery::Push {
        // The pushed head's report is built while its worktree is still there to read.
        build_review_report(store, record, options, &gate_steps, &head_sha, None, logs);
        clean_up(true, &CiOutcome::Skipped)?;
        return Ok(Report { run_id: String::new(), ending: Ending::Pushed, detail: None, review_url: None, ci_green: None });
    }

    let Some(provider) = find_program(provider_cli) else {
        return stopped(Ending::Failed, format!("{provider_cli} is not on PATH"));
    };
    if !done("review_request") && at_boundary(store, run_id, &["implement_and_review", "rebase", "gate", "push"], "review_request", Some(&record.branch), None)? {
        return paused("review_request");
    }
    let existing = paused_at.and_then(|c| c.review_request_ref.clone());
    let review_url = match existing {
        Some(url) => url,
        None => {
                let draft = draft_review_description(store, run_id, Duration::from_secs(120), |prompt| DraftSession {
                    program: claude.to_string_lossy().into_owned(),
                    args: match settings_or_flag_model(options, settings) {
                        Some(model) => vec!["-p".to_string(), "--model".to_string(), model.to_string(), prompt.to_string()],
                        None => vec!["-p".to_string(), prompt.to_string()],
                    },
                    env: database_env.to_vec(),
                    cwd: worktree.to_path_buf(),
                })?;
                let draft = group_draft(draft, options.features);
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
                let created = match create_review_request_with(store, run_id, &provider, &draft, &people, Duration::from_secs(60))? {
                    ReviewRequestOutcome::Created(review) | ReviewRequestOutcome::Adopted(review) => review.url,
                    ReviewRequestOutcome::Failed { reason } => {
                        return stopped(Ending::Failed, format!("review request failed: {reason}"))
                    }
                };
            created
        }
    };
    build_review_report(store, record, options, &gate_steps, &head_sha, None, logs);

    if at_boundary(store, run_id, &["implement_and_review", "rebase", "gate", "push", "review_request"], "ci", Some(&record.branch), Some(&review_url))? {
        return paused("ci");
    }
    let ci_config = CiWaitConfig {
        enabled: options.ci_check,
        poll_interval: Duration::from_secs(15),
        timeout: settings.ci_timeout.unwrap_or(Duration::from_secs(30 * 60)),
        command_timeout: Duration::from_secs(60),
        no_ci_grace: Duration::from_secs(90),
    };
    let ci_log = logs.join("ci.log");
    // A published branch is the user's own checkout, and no agent edits it: its CI is only reported.
    let ci = if options.publish || !options.ci_check {
        let ci = wait_for_ci_logged(store, run_id, &provider, &head_sha, &ci_config, Some(&ci_log))?;
        if ci != CiOutcome::Skipped {
            build_review_report(store, record, options, &gate_steps, &head_sha, Some(&ci), logs);
        }
        ci
    } else {
        let gate = choose_gate(worktree, settings.gate.as_deref()).map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
        // Where a fix session says that the failure is not the run's; the session is told in its environment too.
        let unrelated_report = logs.join(orq_orchestrator::UNRELATED_FAILURE_FILE);
        let mut fix_env = database_env.to_vec();
        fix_env.push(("ORQ_UNRELATED_FAILURE_FILE".to_string(), unrelated_report.to_string_lossy().into_owned()));
        let task = format!(
            "CI failed on the pushed branch. The newest ci-failure-*.log in {} lists the failing checks. Read it, get each check's log with {provider_cli}, fix the cause in this worktree and commit the fix. \
             If the failure has nothing to do with your changes (a flaky check, a check already failing on the target branch, a broken CI environment), do not fix it: \
             write a line saying what fails and why it is not caused by your changes to {} and stop.",
            logs.display(),
            unrelated_report.display()
        );
        let logs_dir = logs.to_str().ok_or_else(|| StoreError::Corrupt(format!("the logs folder {} is not valid text", logs.display())))?;
        // Someone watching the run gets the session on the terminal, closed once it says it is finished.
        let fix_marker = (options.interactive && settings.auto_exit != Some(false)).then(|| logs.join("ci-fix.done"));
        let fix_args = adapter_for(&options.implementer.harness)
            .and_then(|adapter| {
                adapter.fix_argv(&FixRequest {
                    implementer: spec(&implementer),
                    task: &task,
                    logs_dir,
                    exit_marker: fix_marker.as_deref().and_then(Path::to_str),
                    attended: options.interactive,
                })
            })
            .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
        let mut steps = PipelineCiSteps {
            store,
            run_id,
            provider: &provider,
            provider_cli,
            config: &ci_config,
            ci_log: &ci_log,
            push_log: logs.join("push.log"),
            report: &|head, ci| build_review_report(store, record, options, &gate_steps, head, ci, logs),
            session: FixSessionSpec {
                program: claude_program.to_string(),
                args: fix_args,
                env: fix_env,
                cwd: worktree.to_path_buf(),
                session_id: None,
                log: logs.join("ci-fix.log"),
                terminal: options.interactive,
                exit_marker: fix_marker,
            },
        };
        let gate_env = gate_env(store, record, database_env)?;
        let config = CiFixConfig {
            gate: &gate,
            gate_enabled: options.gate,
            gate_log: &logs.join("gate.log"),
            log_dir: logs,
            max_attempts: CI_FIX_ATTEMPTS,
            env: &gate_env,
        };
        match run_ci_fix_loop(store, run_id, &head_sha, &config, &mut steps)? {
            // Left alone on purpose: the person decides whether it is fixed first, in its own change.
            CiFixOutcome::Unrelated { summary, .. } => {
                return stopped(
                    Ending::Blocked,
                    format!("CI fails for a reason unrelated to this run: {summary}. Get that fixed separately and merged into the target branch, then resume this run"),
                );
            }
            outcome => ci_verdict(outcome),
        }
    };
    let ready = matches!(ci, CiOutcome::Passed | CiOutcome::Skipped);
    clean_up(ready, &ci)?;
    if !ready {
        return Ok(Report {
            run_id: String::new(),
            ending: Ending::Failed,
            detail: Some(format!("CI did not pass: {ci:?}")),
            review_url: Some(review_url),
            ci_green: None,
        });
    }
    let ci_green = Some(matches!(ci, CiOutcome::Passed));
    Ok(Report { run_id: String::new(), ending: Ending::ReviewReady, detail: None, review_url: Some(review_url), ci_green })
}

/// How long a run waits for its review report before moving on without it.
const REVIEW_REPORT_TIMEOUT: Duration = Duration::from_secs(30);

/// The wait for the report; `ORQ_REVIEW_BUILD_TIMEOUT_MS` shortens it for tests.
fn review_report_timeout() -> Duration {
    std::env::var("ORQ_REVIEW_BUILD_TIMEOUT_MS").ok().and_then(|ms| ms.parse().ok()).map(Duration::from_millis).unwrap_or(REVIEW_REPORT_TIMEOUT)
}

/// Leaves the report of the pushed commit for the Reviews tab. Best effort: a report that fails or takes too long
/// never changes how the run ends, and is only noted in the run's logs.
///
/// The report carries what the run knows about the commit: the gate steps, when the latest gate run passed (the gate
/// runs before every push, so that is the pushed commit's), what `ci` said about it, and the review rounds the
/// implement session recorded for the run's commits.
fn build_review_report(
    store: &RunStore,
    record: &RunRecord,
    options: &Options<'_>,
    gate_steps: &[Vec<String>],
    head_sha: &str,
    ci: Option<&CiOutcome>,
    logs: &Path,
) {
    let gate_passed = store.events(&record.run_id).is_ok_and(|events| {
        events
            .iter()
            .rev()
            .find(|e| e.stage.as_deref() == Some("verify") && matches!(e.kind.as_str(), "stage.completed" | "stage.failed" | "stage.skipped"))
            .is_some_and(|e| e.kind == "stage.completed")
    });
    let tests = crate::run_evidence::tests_evidence(head_sha, if gate_passed { gate_steps } else { &[] }, ci);
    let built = orq_orchestrator::rebased_onto(store, &record.run_id)
        .map_err(|err| format!("cannot read what the branch was rebased onto: {err}"))
        .and_then(|rebased| {
            // Rebased, the branch no longer sits on the commit the run started from; the diff would claim the target's newer commits.
            let base = rebased.unwrap_or_else(|| record.base_sha.clone());
            let (repo, run_id, feature, head) = (options.project.root.clone(), record.run_id.clone(), record.feature_id.clone(), head_sha.to_string());
            let (worktree, started_from) = (record.worktree.clone(), record.base_sha.clone());
            let (sender, receiver) = std::sync::mpsc::channel();
            // Taken before the worker starts, so a late finish of this build cannot overwrite a later build's report.
            let turn = crate::run_evidence::report_turn(&run_id);
            // Detached on a timeout, so a late finish normally still leaves a report. If the run exits while it publishes,
            // the report can be left unindexed until the next build repairs it.
            std::thread::spawn(move || {
                // The rounds are of the commits the session made from where the run started, before any rebase.
                let review = crate::run_evidence::review_from_ledger(&worktree, &started_from);
                let _ = sender.send(crate::review::build_for_run(&repo, &run_id, &feature, &base, &head, &tests, review.as_ref(), &turn));
            });
            match receiver.recv_timeout(review_report_timeout()) {
                Ok(built) => built,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err("the build did not finish in time".to_string()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the build stopped unexpectedly".to_string()),
            }
        });
    let note = match built {
        Ok(path) => format!("report for {head_sha}: {}", path.display()),
        Err(err) => format!("no report for {head_sha}: {err}"),
    };
    use std::io::Write;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(logs.join("review-report.log"));
    let _ = log.and_then(|mut file| writeln!(file, "{note}"));
}

/// How the pipeline talks to CI and the remote while the fix loop runs.
struct PipelineCiSteps<'a> {
    store: &'a RunStore,
    run_id: &'a str,
    provider: &'a Path,
    provider_cli: &'a str,
    config: &'a CiWaitConfig,
    ci_log: &'a Path,
    push_log: PathBuf,
    /// Leaves the review report of a commit the loop just pushed, and again with what its CI said.
    report: &'a dyn Fn(&str, Option<&CiOutcome>),
    session: FixSessionSpec,
}

impl CiFixSteps for PipelineCiSteps<'_> {
    fn wait(&mut self, head_sha: &str) -> Result<CiOutcome, StoreError> {
        let ci = wait_for_ci_logged(self.store, self.run_id, self.provider, head_sha, self.config, Some(self.ci_log))?;
        if ci != CiOutcome::Skipped {
            (self.report)(head_sha, Some(&ci));
        }
        Ok(ci)
    }

    fn push(&mut self) -> Result<PushOutcome, StoreError> {
        // A push publishes commits only: a fix left uncommitted passed the gate but would never reach CI.
        if let ImplementCheck::Blocked { reason } = check_implement_outcome(self.store, self.run_id)? {
            return Ok(PushOutcome::Failed { reason });
        }
        let pushed = push_run_branch(self.store, self.run_id, &self.push_log)?;
        if let PushOutcome::Pushed { head_sha } = &pushed {
            (self.report)(head_sha, None);
        }
        Ok(pushed)
    }

    fn failure_log(&mut self, failing: &[String]) -> String {
        ci_failure_text(self.provider_cli, failing)
    }

    fn fix_session(&mut self, _attempt: u32, _failure_log: &Path) -> FixSessionSpec {
        self.session.clone()
    }
}

/// What a fix session starts from: orq knows the names of the failing checks, and the session
/// fetches their logs with the provider's own CLI.
fn ci_failure_text(provider_cli: &str, failing: &[String]) -> String {
    let hint = if provider_cli == "glab" {
        "`glab ci view` shows the pipeline and `glab ci trace <job>` prints a job's log."
    } else {
        "`gh pr checks` lists the checks and `gh run view <run-id> --log-failed` prints a failed run's log."
    };
    format!("CI failed on the pushed branch.\n\nFailing checks:\n- {}\n\n{hint}\n", failing.join("\n- "))
}

/// The fix loop's ending, as what the rest of the pipeline reports and cleans up after.
fn ci_verdict(outcome: CiFixOutcome) -> CiOutcome {
    match outcome {
        CiFixOutcome::Passed { .. } => CiOutcome::Passed,
        CiFixOutcome::Skipped => CiOutcome::Skipped,
        CiFixOutcome::TimedOut { .. } => CiOutcome::TimedOut,
        CiFixOutcome::Errored { reason, .. } => CiOutcome::Errored { reason },
        CiFixOutcome::Failed { failing, .. } => CiOutcome::Failed { failing },
        CiFixOutcome::GateFailed { fix_attempts, step, exit_code } => CiOutcome::Errored {
            reason: format!("the gate failed at step {step} (exit {exit_code:?}) after CI fix attempt {fix_attempts}; nothing was pushed"),
        },
        CiFixOutcome::PushFailed { fix_attempts, reason } => CiOutcome::Errored {
            reason: format!("pushing CI fix attempt {fix_attempts} failed: {reason}"),
        },
        // The pipeline stops on this before it gets here; a verdict is still never a pass.
        CiFixOutcome::Unrelated { summary, .. } => CiOutcome::Errored { reason: format!("CI fails for a reason unrelated to this run: {summary}") },
    }
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

    fn record_in(worktree: std::path::PathBuf) -> orq_orchestrator::RunRecord {
        orq_orchestrator::RunRecord {
            delivery: Default::default(),
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: String::new(),
            provider: String::new(),
            target_branch: "main".into(),
            base_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            branch: "orq/F".into(),
            worktree,
            repository: String::new(),
            profiles: Default::default(),
        }
    }

    #[test]
    fn has_commits_assumes_the_work_is_there_when_git_cannot_tell_and_none_when_the_worktree_is_gone() {
        let dir = std::env::temp_dir().join(format!("orq-has-commits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // An existing directory git cannot read as a worktree: the feature must not be reopened.
        assert!(super::has_commits(&record_in(dir.clone())));
        assert!(!super::has_commits(&record_in(dir.join("gone"))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
