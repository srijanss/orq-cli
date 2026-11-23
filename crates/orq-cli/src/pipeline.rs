//! `orq run`: the stages of one run, in order, driven through the orchestrator's
//! stage functions. A run never merges; it ends at review_ready, blocked or failed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orq_agents::{adapter_for, FixRequest, ImplementRequest, ModelSpec};
use orq_features::ResolvedModel;
use orq_git::{detect_provider, local_target_base, ProviderHosts};
use orq_orchestrator::{
    check_implement_outcome, choose_gate, cleanup_worktree, create_review_request_with, draft_review_description,
    plan_setup, push_run_branch, rebase_onto_target, record_launch, run_ci_fix_loop, run_gate_with_fixes_then, run_implement_stage, run_setup_with_env, wait_for_ci_logged,
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
    /// Nobody watches the run (the queue started it): its session runs headless and is told never to
    /// ask, and the run never waits for an answer.
    pub unattended: bool,
    /// A merge-local delivery blocked by uncommitted changes to the run's files sets them aside while
    /// it merges, then puts them back (`orq run merge --set-aside`).
    pub set_aside: bool,
    /// On resume of a run blocked mid-rebase on a conflict: a session of the implementer resolves it.
    pub fix_conflicts: bool,
    /// On resume (`orq run resume --review-again`): review the run again, a failed one from its last review.
    pub review_again: bool,
    /// On resume (`orq run resume --answer`): what the person resuming answered, given to the implement session.
    pub answer: Option<&'a str>,
    /// The project's settings: at most this many of its runs active at once (a resume does not
    /// count against it), and this much free disk, in GB, to start or resume a run.
    pub max_active_runs: Option<u8>,
    pub min_free_disk_gb: Option<u32>,
    /// The machine's settings, `[runs]` in the user's config: at most this many runs active at
    /// once across every project, and this much free disk to start or resume a run.
    pub machine: orq_features::MachineRunLimits,
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
    /// The run's delivery is merge-local: its base branch was fast-forwarded to it, nothing pushed.
    Merged,
    /// A frontend run waits for the design files its events list; resumed once they are there.
    DesignFilesNeeded,
}

impl Ending {
    pub fn name(self) -> &'static str {
        match self {
            Ending::ReviewReady => "review_ready",
            Ending::Blocked => "blocked",
            Ending::Failed => "failed",
            Ending::NeedsReview => "needs_review",
            Ending::Paused => "paused",
            Ending::Merged => "merged",
            Ending::DesignFilesNeeded => "design_files_needed",
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
    /// The run's own pre-start checks refused it; nothing was created or started.
    Refused(String),
    /// The project's run limit or disk floor refused it; nothing was created or started.
    Limit(String),
    Other(String),
}

/// A refusal of the pre-start checks, told apart from a failure once the run has started.
fn refused(err: StartError) -> StartError {
    match err {
        StartError::Other(message) => StartError::Refused(message),
        err => err,
    }
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

/// Why a run cannot start `harness`: it is not on this PATH, named in full. Under `orq serve install`'s unit
/// the PATH is the one install wrote, so it says how to write it again.
fn harness_missing(harness: &str) -> String {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut reason = format!("the {harness} harness is not on PATH ({path})");
    if crate::remote_serve::own_unit().is_some() {
        reason.push_str(&format!("; set it with `orq serve install` from a shell where `{harness}` is found"));
    }
    reason
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
    project_limits(options, store)?;
    refuse_grouped_frontend(options)?;
    let resumed = if options.resume {
        let members: Vec<String> = options.features.iter().map(|f| slug(f)).collect();
        let planned = if options.review_again {
            orq_orchestrator::plan_group_resume_reviewing_again(store, repo, &project_id(options.project), &members)
        } else {
            orq_orchestrator::plan_group_resume_fixing(store, repo, &project_id(options.project), &members, options.fix_conflicts)
        };
        let planned = planned.map_err(other)?;
        // An answer is for the session: a resume that inherits the finished one refuses it rather than dropping it.
        let same_harness = planned.from.profiles.get("implementer").map(|p| &p.harness) == Some(&options.implementer.harness);
        if same_harness {
            orq_orchestrator::check_answer_reaches_session(&planned, options.answer).map_err(other)?;
        }
        Some(planned)
    } else {
        None
    };
    // A run started before push was removed carries on only once another delivery is named.
    if resumed.as_ref().is_some_and(|r| !options.delivery_named && r.from.delivery == orq_core::project_settings::Delivery::Push) {
        return Err(other(orq_core::project_settings::PUSH_REMOVED));
    }
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

/// The features of this run marked frontend in orq.db (kept under the project's main checkout). A machine
/// without orq.db has none.
fn frontend_features(options: &Options<'_>) -> Result<std::collections::HashSet<String>, StartError> {
    let Ok(roots) = orq_core::storage::StorageRoots::resolve(&|name| std::env::var(name).ok()) else {
        return Ok(Default::default());
    };
    let root = orq_git::main_checkout_root(options.project).unwrap_or_else(|| options.project.root.clone());
    orq_remote::projects::frontend_features_at(&roots.database(), &root).map_err(other)
}

/// A grouped run with a frontend feature is refused for now: the design review step runs for one feature.
fn refuse_grouped_frontend(options: &Options<'_>) -> Result<(), StartError> {
    if options.features.len() < 2 {
        return Ok(());
    }
    let frontend = frontend_features(options)?;
    match options.features.iter().find(|f| frontend.contains(f.as_str())) {
        Some(feature) => Err(other(format!("{feature} is a frontend feature and cannot run in a group yet: run it on its own"))),
        None => Ok(()),
    }
}

/// The refusals the project's and the machine's settings make: too little free disk where the
/// run's worktree is made, or, for a new run, as many active runs as the machine or the project
/// allows. The limits are checked again when the run is recorded, under the machine's and the
/// project's locks.
fn project_limits(options: &Options<'_>, store: &RunStore) -> Result<(), StartError> {
    let name = &options.project.name;
    let machine = options.machine;
    if options.min_free_disk_gb.is_some() || machine.min_free_disk_gb.is_some() {
        // A resume continues in the worktree its run recorded; a new run is made under the project's.
        let resumed = if options.resume { resumed_worktree(options, store)? } else { None };
        let measured = resumed.as_deref().unwrap_or(&options.project.root);
        let free = match &resumed {
            Some(worktree) => orq_orchestrator::free_disk(worktree.ancestors().find(|path| path.exists()).unwrap_or(worktree)),
            None => orq_orchestrator::worktrees_free_disk(&options.project.root),
        }
        .map_err(|err| other(format!("cannot measure the free disk of {}: {err}", measured.display())))?;
        let refusal = orq_orchestrator::machine_disk_refusal(machine.min_free_disk_gb, &free)
            .or_else(|| orq_orchestrator::disk_refusal(name, options.min_free_disk_gb, &free));
        if let Some(reason) = refusal {
            return Err(StartError::Limit(reason));
        }
    }
    // A blocked run holds no slot, so resuming one is a start; a paused one's resume keeps its slot.
    let counted = options.max_active_runs.is_some() || machine.max_active.is_some();
    if counted && (!options.resume || resume_takes_a_slot(options, store)?) {
        if machine.max_active.is_some() {
            let active = store.active_machine_runs()?;
            if let Some(reason) = orq_orchestrator::machine_limit_refusal(machine.max_active, &active) {
                return Err(StartError::Limit(reason));
            }
        }
        if options.max_active_runs.is_some() {
            let active = store.active_project_runs(&project_id(options.project))?;
            if let Some(reason) = orq_orchestrator::limit_refusal(name, options.max_active_runs, &active) {
                return Err(StartError::Limit(reason));
            }
        }
    }
    Ok(())
}

/// The worktree recorded by the run a resume of `options` would continue, read without changing
/// anything; `None` when there is no such run.
fn resumed_worktree(options: &Options<'_>, store: &RunStore) -> Result<Option<PathBuf>, StartError> {
    let Some(run_id) = resumed_run(options, store)? else {
        return Ok(None);
    };
    Ok(store.load(&run_id).ok().map(|record| record.worktree))
}

/// Whether the resume of `options` is held to the run limit: the run it continues is blocked.
fn resume_takes_a_slot(options: &Options<'_>, store: &RunStore) -> Result<bool, StartError> {
    match resumed_run(options, store)? {
        Some(run) => orq_orchestrator::resume_takes_a_slot(store, &run).map_err(other),
        None => Ok(false),
    }
}

/// The run a resume of `options` would continue; `None` when there is no such run.
fn resumed_run(options: &Options<'_>, store: &RunStore) -> Result<Option<String>, StartError> {
    orq_orchestrator::newest_run_of(store, &project_id(options.project), &slug(&options.features[0])).map_err(other)
}

/// [`project_limits`] on their own, for a launcher that refuses before it spawns the run.
pub fn check_project_limits(options: &Options<'_>) -> Result<(), StartError> {
    let root = state_root().ok_or_else(|| other("no state location: set XDG_STATE_HOME or HOME"))?;
    project_limits(options, &RunStore::new(&root)).map_err(refused)
}

/// Every refusal [`execute`] makes before it fetches or records anything, with nothing started.
pub fn check(options: &Options<'_>) -> Result<(), StartError> {
    let root = state_root().ok_or_else(|| other("no state location: set XDG_STATE_HOME or HOME"))?;
    preflight(options, &RunStore::new(&root)).map(|_| ()).map_err(refused)
}

fn execute_run(options: &Options<'_>) -> Result<Report, StartError> {
    let repo = &options.project.root;
    let root = state_root().ok_or_else(|| other("no state location: set XDG_STATE_HOME or HOME"))?;
    let store = RunStore::new(&root);
    let Preflight { settings, resumed, naming, provider, repository } = preflight(options, &store).map_err(refused)?;
    // Read before the resumed run is marked as resumed, which ends its failure.
    let review_again_failed = resumed.as_ref().is_some_and(|r| r.review_again.is_some() && resumed_failed(&store, &r.from));
    // A paused run's resume continues a run that was already counted; a new run, and the resume of a
    // run that holds no slot (blocked, failed, waiting on review), are held to the limit.
    // Read with the rest of the resume's checks, before anything else is read or started.
    let takes_a_slot = match &resumed {
        Some(r) => orq_orchestrator::resume_takes_a_slot(&store, &r.from.run_id).map_err(other)?,
        None => true,
    };
    // A person's fresh start of a review project pulls its target first; one that cannot be pulled
    // still starts, from the local target, and the run records why.
    let not_pulled = pull_before_start(options, resumed.is_none());
    // A run starts from the local target branch in the project root, with whatever it has that its
    // remote does not: the project as it is here, not as last pushed.
    let base_sha = local_target_base(repo, options.target).map_err(|e| other(e.to_string()))?;
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
    let max_active = options.max_active_runs.filter(|_| takes_a_slot);
    let limits = orq_orchestrator::RunLimits {
        project: max_active.map(usize::from),
        machine: options.machine.max_active.filter(|_| takes_a_slot),
    };
    match store.start_group_run_within(&record, &members, limits) {
        Ok(()) => {}
        Err(StoreError::FeatureBusy { active_run_id }) => return Err(StartError::Busy(active_run_id)),
        Err(StoreError::MachineAtLimit { active }) => {
            let reason = orq_orchestrator::machine_limit_refusal(limits.machine, &active)
                .unwrap_or_else(|| "this machine is at its limit of active runs".to_string());
            return Err(StartError::Limit(reason));
        }
        Err(StoreError::ProjectAtLimit { active }) => {
            let reason = orq_orchestrator::limit_refusal(&options.project.name, max_active, &active)
                .unwrap_or_else(|| format!("{} is at its limit of active runs", options.project.name));
            return Err(StartError::Limit(reason));
        }
        Err(err) => return Err(other(format!("{err:?}"))),
    }
    orq_orchestrator::end_children_with_this_process();
    // Every run names its owner process (detached or not), so a killed one can be told from a live
    // one. Only a detached run has a runner log to name.
    // A resumed run's launch says whether the resume went with an answer for its session (never the text).
    let answer = options.answer.map(str::trim).filter(|answer| !answer.is_empty());
    let runner_log = options.run_id.is_some().then(|| root.join("logs").join(&run_id).join("runner.log"));
    let recorded = match (&resumed, &runner_log) {
        (Some(_), log) => orq_orchestrator::record_resumed_launch(&store, &run_id, std::process::id(), log.as_deref(), answer.is_some()),
        (None, Some(log)) => record_launch(&store, &run_id, std::process::id(), log),
        (None, None) => orq_orchestrator::record_owner(&store, &run_id, std::process::id()),
    };
    if let Err(err) = recorded {
        eprintln!("orq: cannot record the launch of {run_id}: {err}");
    }
    // The answer itself is kept in the run's logs, for its page to show, never through a symlink in that path.
    if let Some(answer) = answer.filter(|_| resumed.is_some()) {
        let kept = Path::new("logs").join(&run_id).join(orq_orchestrator::RESUME_ANSWER_LOG);
        if let Err(err) = orq_core::storage::check_run_id(&run_id).and_then(|()| orq_core::storage::write_atomic(&root, &kept, answer.as_bytes())) {
            eprintln!("orq: cannot keep the answer {run_id} was resumed with: {err}");
        }
    }
    if let Some(outcome) = &not_pulled {
        if let Some(note) = record_pull_warning(&store, &record, outcome) {
            eprintln!("orq: {note}");
        }
    }
    let finish = |store: &RunStore| store.finish_group_run(&record.project_id, &members, &record.run_id);
    // A resume that skips the rebase still sits on the commit the paused run rebased onto, and a gate
    // told otherwise would judge the wrong changes. Without it the resume stops before it does
    // anything. Its run is removed, not failed: a resume continues a feature's newest run, and that
    // has to stay the one it could not continue.
    // A rebase a person finished after the run blocked on its conflict is the resumed run's to record;
    // one still stopped on it is the resumed run's conflict.
    if let Some(resumed) = resumed.as_ref() {
        let from = resumed.from.run_id.as_str();
        let carried = orq_orchestrator::carry_rebase(&store, &record, from).and_then(|()| match &resumed.finished_by_hand {
            Some(onto) => orq_orchestrator::record_rebase_finished_by_hand(&store, &record, onto),
            None if resumed.mid_rebase => orq_orchestrator::carry_conflict(&store, &record, from),
            None => Ok(()),
        })
        // A frontend run's design rounds, budget and frozen package go on in the run that resumes it.
        .and_then(|()| orq_orchestrator::carry_design(&store, &record, from, &root.join("logs").join(from), &root.join("logs").join(&run_id)));
        if let Err(err) = carried {
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
        // What its sessions completed stays theirs: it is not handed back, and the group check counts it.
        carry_session_completed(&store, &record, from);
        // The features its fix sessions started are this run's to close now.
        if let Err(err) = orq_orchestrator::carry_fix_features(&store, &record, from) {
            eprintln!("orq: cannot carry the features the fix sessions of {from} started into {run_id}: {err:?}");
        }
    }
    // The stopped run handed its branch and worktree to this run; it no longer holds them nor offers actions of its own.
    // A new harness continues the work from the handoff, so only an implementer on the same harness skips its finished session.
    let inherited = resumed.as_ref().filter(|r| {
        r.skip_implement && r.from.profiles.get("implementer").map(|p| &p.harness) == record.profiles.get("implementer").map(|p| &p.harness)
    });
    if let Some(plan) = resumed.as_ref() {
        // Whether the session was started or inherited, and why, goes on the stopped run's `run.resumed`.
        let (session, why) = match inherited {
            Some(plan) => plan.session(),
            None if plan.skip_implement => ("started", "the implementer's harness changed, so the new one starts from the handoff".to_string()),
            None => plan.session(),
        };
        if let Err(err) = orq_orchestrator::supersede_resumed_answered(&store, &plan.from, &run_id, session, &why, answer.is_some()) {
            eprintln!("orq: cannot mark {} as resumed: {err:?}", plan.from.run_id);
        }
    }
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
    let resumed_from = Resumed {
        from: resumed.as_ref().map(|r| &r.from),
        implement_inherited_from: inherited.map(|r| r.from.run_id.as_str()),
        paused_at: checkpoint.as_ref(), review_again: review_again.as_ref(), review_again_failed };
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
    if let Some(note) = extra_features_note(&store, &report.run_id) {
        add_note(&mut report, note);
    }
    if let Some(note) = undeclared_note(&store, &report.run_id) {
        add_note(&mut report, note);
    }
    if let Some(note) = followups_note(&store, &report.run_id) {
        add_note(&mut report, note);
    }
    if matches!(report.ending, Ending::Merged | Ending::ReviewReady) {
        if let Some(note) = flaky_note(&store, &report.run_id) {
            add_note(&mut report, note);
        }
    }
    // The features its fix sessions started are the run's own: delivered, they are finished. Closed while
    // the run still holds the locks, like the reopening below.
    if matches!(report.ending, Ending::Merged | Ending::ReviewReady) {
        close_fix_features(&store, &record, &orq_features::ledger_path(options.project));
        complete_delivered_features(&store, &record, &orq_features::ledger_path(options.project), options.features);
    }
    // Reopened while the run still holds the locks, so a run launched next is never reset.
    if matches!(report.ending, Ending::Blocked | Ending::Failed | Ending::Paused) {
        reopen_undelivered(&store, &record, &report.ending, &orq_features::ledger_path(options.project), options.features);
    }
    let released = finish(&store);
    // A lock left behind blocks the next run of the feature, so it is never reported as success.
    if let Err(lock) = released {
        report.ending = Ending::Failed;
        add_note(&mut report, format!("the feature lock was not released: {lock:?}"));
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

/// The event a run records when its start could not pull its target and went ahead from the local one.
pub const TARGET_PULL_WARNING_EVENT: &str = "target.pull_warning";

/// Pulls the target of a review project before a person's fresh start, when its
/// `pull_target_before_runs` setting says to; what came of a pull that may not start a queued run,
/// which a person's start goes ahead without. The queue pulls before its own starts.
fn pull_before_start(options: &Options<'_>, fresh: bool) -> Option<orq_git::PullOutcome> {
    let review = options.delivery == orq_core::project_settings::Delivery::Review;
    if !fresh || options.publish || options.unattended || !review {
        return None;
    }
    if !crate::project_settings(options.project).ok()?.pull_target_before_runs {
        return None;
    }
    let outcome = orq_git::pull_target(&options.project.root, options.remote, options.target);
    (!outcome.may_start()).then_some(outcome)
}

/// Records on the run why its target was not pulled before it started, so its page can say so; a
/// note saying what was not pulled, and the event too when it cannot be written.
fn record_pull_warning(store: &RunStore, record: &RunRecord, outcome: &orq_git::PullOutcome) -> Option<String> {
    let target = &record.target_branch;
    let message = orq_core::redact::redact(&outcome.describe(&record.remote, target));
    let event = orq_orchestrator::Event {
        schema_version: 1,
        event_id: format!("{}-target-pull", record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: TARGET_PULL_WARNING_EVENT.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: crate::review::utc_now(),
        payload: serde_json::json!({ "outcome": outcome.kind(), "target": target, "message": message }),
    };
    let note = format!("{target} was not pulled before the run: {message}; the run starts from the local {target}");
    Some(match store.append_event(&event) {
        Ok(()) => note,
        Err(err) => format!("{note}; the run's events do not record it: {err:?}"),
    })
}

/// Records on the run that it runs unattended, so its record and the runs lists say so; a note
/// when the event cannot be written.
fn record_unattended(store: &RunStore, record: &RunRecord) -> Option<String> {
    let event = orq_orchestrator::Event {
        schema_version: 1,
        event_id: format!("{}-unattended", record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: orq_orchestrator::UNATTENDED_EVENT.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: crate::review::utc_now(),
        payload: serde_json::json!({ "started_by": "queue" }),
    };
    store.append_event(&event).err().map(|err| format!("the run's events do not record that it runs unattended: {err:?}"))
}

/// How an unattended session's reported problem says a person has to decide something.
const NEEDS_A_DECISION: &str = "needs a decision:";

/// The decision an unattended session's reported problem says a person has to make, from
/// [`NEEDS_A_DECISION`] on, if it names one.
fn needed_decision(reported: Option<&str>) -> Option<String> {
    let problem = reported?;
    problem.find(NEEDS_A_DECISION).map(|at| problem[at..].to_string())
}

/// Why an unattended run stops when its session asked for something nobody can give: the last need its
/// needs-input hook put on the run, if one did.
fn unanswered_need(store: &RunStore, run_id: &str) -> Option<String> {
    let events = store.events(run_id).inspect_err(|err| eprintln!("orq: cannot tell whether {run_id}'s session asked a question: {err:?}")).ok()?;
    let need = events.iter().rev().find(|e| e.kind == "run.needs_input")?;
    let text = need.payload["text"].as_str().unwrap_or_default();
    Some(match need.payload["kind"].as_str() {
        Some("permission") => format!("waited for a permission in an unattended run: {text}"),
        _ => format!("asked a question in an unattended run: {text}"),
    })
}

/// For an unattended run whose review escalated: each finding still open becomes a draft ledger entry
/// `review-followup-<feature>-<finding id>-<hash>` that says what was found, recorded as the run's
/// `run.followups_drafted` event. One already in the ledger is kept as it is. A finding at high
/// severity or above, or in the security category, is not left for later: `Err` names them, and
/// nothing is drafted.
fn draft_followups(store: &RunStore, record: &RunRecord, ledger: &Path, review: &orq_orchestrator::EscalatedReview) -> Result<(), String> {
    let open: Vec<&orq_orchestrator::EscalatedFinding> = review.findings.iter().filter(|f| f.status == "open").collect();
    let serious: Vec<String> = open
        .iter()
        .filter(|f| matches!(f.severity.as_str(), "high" | "critical") || f.category.as_deref() == Some("security"))
        .map(|f| match &f.category {
            Some(category) => format!("{} ({}, {category})", f.id, f.severity),
            None => format!("{} ({})", f.id, f.severity),
        })
        .collect();
    if !serious.is_empty() {
        return Err(format!("{}, and an unattended run does not leave a high-severity or security finding for later: {}", escalation_reason(review), serious.join(", ")));
    }
    let drafts: Vec<orq_features::FeatureDraft> = open
        .iter()
        .map(|f| orq_features::FeatureDraft {
            description: Some(followup_description(&record.feature_id, f)),
            depends_on: Some(Vec::new()),
            ..orq_features::FeatureDraft::new(followup_name(&record.feature_id, &f.id))
        })
        .collect();
    let names: Vec<String> = drafts.iter().map(|d| d.feature_name.clone()).collect();
    if names.is_empty() {
        return Ok(());
    }
    for _ in 0..5 {
        let loaded = orq_features::LoadedLedger::load_or_new(ledger).map_err(|err| format!("{}, and its follow-ups cannot be drafted: the feature ledger: {err:?}", escalation_reason(review)))?;
        let existing: std::collections::HashSet<&str> = loaded.entries().iter().map(|e| e.feature_name.as_str()).collect();
        let new: Vec<orq_features::FeatureDraft> = drafts.iter().filter(|d| !existing.contains(d.feature_name.as_str())).cloned().collect();
        match loaded.append_many(&new) {
            Ok(_) => {
                record_ledger(store, record, "run.followups_drafted", &names);
                return Ok(());
            }
            Err(orq_features::AppendError::Save(orq_features::SaveError::Conflict)) => continue,
            Err(err) => return Err(format!("{}, and its follow-ups cannot be drafted: {err}", escalation_reason(review))),
        }
    }
    Err(format!("{}, and its follow-ups cannot be drafted: the feature ledger kept changing", escalation_reason(review)))
}

/// The ledger name of the follow-up of `finding` in a run of `feature`: only letters, digits, `-` and
/// `_`, whatever the reviewer named the finding. Every name ends in a stable hash of the pair, so no two
/// pairs share one: not ids that clean to the same text (`a/b`, `a:b`, `a-b`), not a safe id that
/// spells out another's name, and not pairs a `-` splits two ways (`A-B`/`C`, `A`/`B-C`).
fn followup_name(feature: &str, finding: &str) -> String {
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let clean = |text: &str| text.chars().map(|c| if allowed(c) { c } else { '-' }).collect::<String>();
    // FNV-1a of the pair as it was, the two kept apart by a byte neither holds, so the name is the
    // same in every run and every build and tells apart what the readable part cannot.
    let hash = feature.bytes().chain([0]).chain(finding.bytes()).fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("review-followup-{}-{}-{:012x}", clean(feature), clean(finding), hash & 0xffff_ffff_ffff)
}

fn followup_description(feature: &str, f: &orq_orchestrator::EscalatedFinding) -> String {
    let place = match (&f.file, f.line) {
        (Some(file), Some(line)) => format!(" at {file}:{line}"),
        (Some(file), None) => format!(" at {file}"),
        _ => String::new(),
    };
    let category = f.category.as_deref().map(|c| format!(", {c}")).unwrap_or_default();
    let suggestion = f.suggestion.as_deref().map(|s| format!(" Suggested: {s}")).unwrap_or_default();
    format!("Review follow-up of {feature}, finding {} ({}{category}){place}, left open when its unattended run's review escalated: {}{suggestion}", f.id, f.severity, f.claim)
}

/// The ending's note on the follow-ups the run drafted, if it drafted any.
fn followups_note(store: &RunStore, run_id: &str) -> Option<String> {
    let drafted = features_in_events(store, run_id, "run.followups_drafted").ok()?;
    (!drafted.is_empty()).then(|| format!("Follow-ups drafted: {}", drafted.join(", ")))
}

/// The findings still open in an escalated review, one line each, for the session that reviews again.
/// Whether the run being resumed failed, rather than waiting on a review decision: a review asked of
/// it again starts from its last review, not from one that escalated.
fn resumed_failed(store: &RunStore, from: &RunRecord) -> bool {
    store.events(&from.run_id).ok()
        .is_some_and(|events| orq_orchestrator::summarize_run(&events).phase == orq_orchestrator::RunPhase::Failed)
}

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

/// Those of `features` no run launched after `record` took, which are still this run's to change. When
/// that cannot be told it is said, and none are.
fn features_no_later_run_took(store: &RunStore, record: &RunRecord, features: &[String]) -> Vec<String> {
    orq_orchestrator::features_no_later_run_took(store, &record.run_id, features).unwrap_or_else(|err| {
        eprintln!("orq: cannot tell whether a later run took {}: {err}", features.join(","));
        Vec::new()
    })
}

/// Takes the features of a run that ended without delivering out of `completed`. What a session of
/// the run marked `completed` was held `in_progress` once its work went on to the gate
/// (`hold_until_delivery`); this catches one the run stopped before holding, marked since, or the
/// hold could not move. A feature
/// a later run took is left to that run. A
/// blocked or failed run that built nothing puts its features back to `pending` (an `in_progress`
/// one too), so they may be launched again; otherwise the run is resumed, so they go back to
/// `in_progress`. Best effort: a failure is said and changes nothing else about how the run ended.
fn reopen_undelivered(store: &RunStore, record: &RunRecord, ending: &Ending, ledger: &Path, features: &[String]) {
    let free = features_no_later_run_took(store, record, features);
    let (reopened, to) = if matches!(ending, Ending::Blocked | Ending::Failed) && !has_commits(record) {
        (orq_features::reopen_discarded_features(ledger, features, &free), "pending")
    } else {
        // In a group, what a session of the run completed at the head the branch is still on is
        // finished work, which the group check counts from the `ledger.session_completed` events,
        // not the ledger; every feature is completed in the ledger only at delivery.
        let kept = kept_by_group(store, record, features);
        let still: Vec<String> = free.into_iter().filter(|name| !kept.contains(name)).collect();
        (orq_features::reopen_unfinished_features(ledger, &still), "in_progress")
    };
    if let Err(err) = reopened {
        eprintln!("orq: cannot put {} back to {to}: {err}", features.join(","));
    }
}

/// Before a resumed run's session: each of `features` its agent left `in_progress` goes back to
/// `pending`, so the session can start it again under the same name; a feature a later run took is
/// left to that run. Best effort: a failure is said and changes nothing else about the run.
fn hand_back_features(store: &RunStore, record: &RunRecord, ledger: &Path, features: &[String]) {
    // A group feature a session of this run, or of a run it resumes, completed at the head the branch
    // is still on is not started again.
    let completed = kept_by_group(store, record, features);
    let free: Vec<String> = features_no_later_run_took(store, record, features).into_iter().filter(|name| !completed.contains(name)).collect();
    match orq_features::reopen_features(ledger, &free) {
        Ok(handed_back) => record_ledger(store, record, "ledger.handed_back", &handed_back),
        Err(err) => eprintln!("orq: cannot hand {} back to the session: {err}", free.join(",")),
    }
}

/// At a delivered ending: each of the run's `features` still `pending` or `in_progress` becomes
/// `completed`, its work being delivered; a feature a later run took is left to that run. Best effort:
/// a failure is said and changes nothing else about how the run ended.
fn complete_delivered_features(store: &RunStore, record: &RunRecord, ledger: &Path, features: &[String]) {
    let free = features_no_later_run_took(store, record, features);
    match orq_features::complete_run_features(ledger, &free, &crate::review::utc_now()) {
        Ok(completed) => record_ledger(store, record, "ledger.completed_by_run", &completed),
        Err(err) => eprintln!("orq: cannot set {} completed: {err}", free.join(",")),
    }
}

/// The names in the ledger with their statuses, none when it is missing; one that cannot be read is
/// said and gives nothing, so what `session` started is not told.
fn ledger_statuses(ledger: &Path, session: &str) -> Option<Vec<(String, orq_features::LedgerStatus)>> {
    match orq_features::load_ledger(ledger) {
        Ok(entries) => Some(entries.into_iter().map(|entry| (entry.feature_name, entry.status)).collect()),
        Err(orq_features::LedgerError::Missing { .. }) => Some(Vec::new()),
        Err(err) => {
            eprintln!("orq: cannot read the feature ledger to tell which features {session} started: {err:?}");
            None
        }
    }
}

/// The name, phase and depth of the feature the worktree's `.tdd-state.json` has open (not always the
/// run's own: it may be a review-finding feature), none when no feature is open or the file is missing,
/// unreadable, or its phase is not a plain word.
fn open_tdd_state(worktree: &Path) -> Option<(String, String, usize)> {
    let text = std::fs::read_to_string(worktree.join(".tdd-state.json")).ok()?;
    let state: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = state.get("featureName")?.as_str()?;
    let stack = state.get("stack").and_then(|s| s.as_array()).map_or(&[][..], |s| s.as_slice());
    let phase = state.get("phase").and_then(|p| p.as_str()).or_else(|| stack.last()?.get("phase")?.as_str())?;
    let plain = !phase.is_empty() && phase.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    plain.then(|| (name.to_string(), phase.to_string(), stack.len().max(1)))
}

/// Each of `features` in the ledger with the files it declares, none when the ledger cannot be read.
fn ledger_target_files(ledger: &Path, features: &[String]) -> Option<Vec<(String, Vec<String>)>> {
    let entries = orq_features::load_ledger(ledger).ok()?;
    Some(entries.into_iter().filter(|e| features.contains(&e.feature_name)).map(|e| (e.feature_name, e.target_files)).collect())
}

/// After a session: a declared file (from `before`) a TDD tool dropped from a feature's `targetFiles`
/// is put back, the files the session added kept, and `ledger.target_files_restored {feature, restored}`
/// is recorded. Best effort: a failure is said and changes nothing else about the run.
fn restore_target_files(store: &RunStore, record: &RunRecord, ledger: &Path, before: Option<&[(String, Vec<String>)]>) {
    let Some(before) = before else { return };
    // One load serves the whole check; it is repeated only after a save or a conflict made it stale.
    let mut conflicts = 0;
    let mut gone: Vec<&String> = Vec::new();
    loop {
        let loaded = match orq_features::LoadedLedger::load(ledger) {
            Ok(loaded) => loaded,
            Err(orq_features::LedgerError::Missing { .. }) => {
                if before.iter().any(|(_, declared)| !declared.is_empty()) {
                    eprintln!("orq: the feature ledger is gone, so the files its features declared are not put back");
                }
                return;
            }
            Err(err) => {
                eprintln!("orq: cannot read the feature ledger to put back the files its features declared: {err:?}");
                return;
            }
        };
        let mut dropped = None;
        for (feature, declared) in before {
            if gone.contains(&feature) {
                continue;
            }
            let Some(entry) = loaded.entries().iter().find(|e| &e.feature_name == feature) else {
                if !declared.is_empty() {
                    eprintln!("orq: {feature} is no longer in the feature ledger, so the files it declared are not put back");
                }
                gone.push(feature);
                continue;
            };
            let restored: Vec<String> = declared.iter().filter(|f| !entry.target_files.contains(f)).cloned().collect();
            if !restored.is_empty() {
                let mut files = declared.clone();
                files.extend(entry.target_files.iter().filter(|f| !declared.contains(f)).cloned());
                dropped = Some((feature, restored, files));
                break;
            }
        }
        let Some((feature, restored, files)) = dropped else { return };
        let edits = orq_features::FieldEdits { target_files: Some(files), ..Default::default() };
        match loaded.edit_fields(feature, &edits) {
            Ok(_) => record_ledger_event(store, record, "ledger.target_files_restored", serde_json::json!({ "feature": feature, "restored": restored })),
            Err(orq_features::EditFieldsError::Save(orq_features::SaveError::Conflict)) if conflicts < 5 => conflicts += 1,
            Err(err) => {
                eprintln!("orq: cannot put the files {feature} declared back in the ledger: {err}");
                return;
            }
        }
    }
}

/// Appends a ledger event with `payload`; a failure is said.
fn record_ledger_event(store: &RunStore, record: &RunRecord, kind: &str, payload: serde_json::Value) {
    let event = orq_orchestrator::Event {
        schema_version: 1,
        event_id: format!("{}-{}-{}", record.run_id, kind.replace('.', "-"), store.events(&record.run_id).map_or(0, |e| e.len())),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: crate::review::utc_now(),
        payload,
    };
    if let Err(err) = store.append_event(&event) {
        eprintln!("orq: {}: cannot record {kind}: {err:?}", record.run_id);
    }
}

/// The event naming the run's features a session marked `completed`, with the head it left.
pub(crate) const SESSION_COMPLETED: &str = "ledger.session_completed";

/// After a session: those of `features` the ledger now has `completed` that it did not have
/// `completed` when the session started (`before`) are recorded as `ledger.session_completed
/// {features, head}`, `head` being the commit the run's worktree is on. When the ledger or the head
/// cannot be read nothing is recorded and that is said. Best effort. Returns the names recorded.
fn record_session_completed(
    store: &RunStore,
    record: &RunRecord,
    ledger: &Path,
    features: &[String],
    before: Option<&[(String, orq_features::LedgerStatus)]>,
) -> Vec<String> {
    let Some(before) = before else { return Vec::new() };
    let Some(after) = ledger_statuses(ledger, "the implement session") else { return Vec::new() };
    let names = newly_completed(features, before, &after);
    if names.is_empty() {
        return names;
    }
    let Some(head) = worktree_head(record) else {
        eprintln!("orq: {}: cannot read the head of its worktree, so the features its session completed are not recorded", record.run_id);
        return Vec::new();
    };
    let event = orq_orchestrator::Event {
        schema_version: 1,
        event_id: format!("{}-session-completed-{}", record.run_id, store.events(&record.run_id).map_or(0, |e| e.len())),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: SESSION_COMPLETED.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: crate::review::utc_now(),
        payload: serde_json::json!({ "features": names, "head": head }),
    };
    if let Err(err) = store.append_event(&event) {
        eprintln!("orq: {}: cannot record that its session completed {}: {err:?}", record.run_id, names.join(","));
        return Vec::new();
    }
    names
}

/// Those of `features` that `after` has `completed` and `before` did not.
fn newly_completed(features: &[String], before: &[(String, orq_features::LedgerStatus)], after: &[(String, orq_features::LedgerStatus)]) -> Vec<String> {
    let completed = |statuses: &[(String, orq_features::LedgerStatus)], name: &str| {
        statuses.iter().any(|(entry, status)| entry == name && status.known() == Some(orq_core::FeatureStatus::Completed))
    };
    features.iter().filter(|name| completed(after, name) && !completed(before, name)).cloned().collect()
}

/// Each of `features` a session of the run marked `completed` goes back to `in_progress` until the
/// run delivers, which completes it (`complete_delivered_features`); a feature a later run took is
/// left to that run. Recorded as `ledger.held_until_delivery`. Best effort: a failure is said.
fn hold_until_delivery(store: &RunStore, record: &RunRecord, ledger: &Path, features: &[String]) {
    if features.is_empty() {
        return;
    }
    let free = features_no_later_run_took(store, record, features);
    match orq_features::reopen_unfinished_features(ledger, &free) {
        Ok(held) => record_ledger(store, record, "ledger.held_until_delivery", &held),
        Err(err) => eprintln!("orq: cannot put {} back to in_progress until the run delivers: {err}", free.join(",")),
    }
}

/// Before an implement session: each finding the run's review ledger (in its worktree) has open whose
/// feature `ledger` records as completed goes back to pending, recorded as `ledger.finding_reopened
/// {feature, finding, round}`. Best effort: what cannot be read or saved is said and reopens nothing.
fn reopen_open_findings(store: &RunStore, record: &RunRecord, ledger: &Path) -> Vec<orq_features::ReopenedFinding> {
    let mut record_event = |kind: &str, payload: serde_json::Value| {
        let event = orq_orchestrator::Event {
            schema_version: 1,
            event_id: format!("{}-finding-reopened-{}", record.run_id, store.events(&record.run_id).map_or(0, |e| e.len())),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: kind.to_string(),
            stage: None,
            attempt: 1,
            occurred_at: crate::review::utc_now(),
            payload,
        };
        if let Err(err) = store.append_event(&event) {
            eprintln!("orq: {}: cannot record a reopened finding: {err:?}", record.run_id);
        }
    };
    match orq_features::reopen_open_findings(ledger, &record.worktree, &mut record_event) {
        Ok(reopened) => reopened,
        Err(err) => {
            eprintln!("orq: {}: cannot reopen the findings its review has open again: {err}", record.run_id);
            Vec::new()
        }
    }
}

/// Each feature a session of `run_id` (or of a run it was resumed from) completed, with the head that
/// session left, as the run's `ledger.session_completed` events record them. Events that cannot be
/// read are said and give none.
fn session_completed(store: &RunStore, run_id: &str) -> Vec<(String, String)> {
    let events = match store.events(run_id) {
        Ok(events) => events,
        Err(err) => {
            eprintln!("orq: {run_id}: cannot read which features its sessions completed: {err:?}");
            return Vec::new();
        }
    };
    let mut completed = Vec::new();
    for event in events.iter().filter(|e| e.kind == SESSION_COMPLETED) {
        let Some(head) = event.payload.get("head").and_then(|h| h.as_str()) else { continue };
        let names = event.payload.get("features").and_then(|f| f.as_array()).into_iter().flatten().filter_map(|n| n.as_str());
        completed.extend(names.map(|name| (name.to_string(), head.to_string())));
    }
    completed
}

/// The features of a group run a session of it (or of a run it resumes) completed at the head its
/// worktree is on now; a completion at an earlier head is not finished work any more, the branch
/// having changed after it. None for a single feature, whose ledger is left to its reopening and
/// hand-back as before, nor when the head cannot be read.
fn kept_by_group(store: &RunStore, record: &RunRecord, features: &[String]) -> Vec<String> {
    if features.len() < 2 {
        return Vec::new();
    }
    let Some(head) = worktree_head(record) else { return Vec::new() };
    session_completed(store, &record.run_id).into_iter().filter(|(_, at)| *at == head).map(|(name, _)| name).collect()
}

/// Copies the `ledger.session_completed` events of `from` (its own and those it carried) into `record`'s
/// run, so a resumed run knows what the sessions before it completed. Best effort: a failure is said.
fn carry_session_completed(store: &RunStore, record: &RunRecord, from: &str) {
    let events = match store.events(from) {
        Ok(events) => events,
        Err(err) => {
            eprintln!("orq: cannot carry which features the sessions of {from} completed into {}: {err:?}", record.run_id);
            return;
        }
    };
    for (index, event) in events.into_iter().filter(|e| e.kind == SESSION_COMPLETED).enumerate() {
        let carried = orq_orchestrator::Event {
            event_id: format!("{}-session-completed-from-{from}-{index}", record.run_id),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            ..event
        };
        if let Err(err) = store.append_event(&carried) {
            eprintln!("orq: cannot carry which features the sessions of {from} completed into {}: {err:?}", record.run_id);
        }
    }
}

/// The commit the run's worktree is on, none when git cannot tell.
fn worktree_head(record: &RunRecord) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(&record.worktree).args(["rev-parse", "HEAD"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|head| !head.is_empty())
}

/// The TDD session log the run's sessions write in its worktree; its `init_feature` events name the
/// features they started.
const SESSION_LOG: &str = ".tdd-session.log";

/// After a session stage: the features the run's own sessions started, as the session log in its
/// worktree names them, that are not the run's `features` nor already credited to it, and that the
/// ledger has `in_progress` or `completed`, are recorded as `ledger.extra_features` and left as they
/// are. The shared ledger is never diffed for them: another run's session adds to it too. Without a
/// session log nothing is credited and that is said. Best effort.
fn record_extra_features(store: &RunStore, record: &RunRecord, ledger: &Path, features: &[String], stage: &str) {
    let log = record.worktree.join(SESSION_LOG);
    let started = match std::fs::read_to_string(&log) {
        Ok(text) => {
            let mut foreign = match lines_not_the_runs(store, record) {
                Ok(foreign) => foreign,
                Err(err) => {
                    eprintln!("orq: {}: cannot tell which features the {stage} session started, so none are credited to the run: {err}", record.run_id);
                    return;
                }
            };
            // Each line a commit the branch sat on already had is not the run's, however a rebase
            // interleaved the run's own lines with them.
            features_started_in(text.lines().filter(|line| match foreign.get_mut(*line) {
                Some(left) if *left > 0 => {
                    *left -= 1;
                    false
                }
                _ => true,
            }))
        }
        Err(err) => {
            eprintln!(
                "orq: {}: cannot tell which features the {stage} session started, so none are credited to the run: cannot read {}: {err}",
                record.run_id,
                log.display()
            );
            return;
        }
    };
    let credited = extra_features_recorded(store, &record.run_id).unwrap_or_default();
    let candidates: Vec<String> = started.into_iter().filter(|name| !features.contains(name) && !credited.contains(name)).collect();
    if candidates.is_empty() {
        return;
    }
    let Some(statuses) = ledger_statuses(ledger, &format!("the {stage} session")) else { return };
    use orq_core::FeatureStatus::{Completed, InProgress};
    let added: Vec<String> = candidates
        .into_iter()
        .filter(|name| statuses.iter().any(|(entry, status)| entry == name && matches!(status.known(), Some(InProgress | Completed))))
        .collect();
    record_ledger(store, record, "ledger.extra_features", &added);
}

/// The event naming the files a session stage changed outside its features' files.
const UNDECLARED_FILES: &str = "session.undeclared_files";

/// After the session stage `stage` (`implement_and_review`, `gate_fix` or `rebase_fix`): the files the
/// run's commits changed since its base that no feature of the run declares, and that no earlier stage
/// already named, are recorded as `session.undeclared_files {files, stage}`. Information only: nothing
/// is blocked, and what cannot be read is said and records nothing.
fn record_undeclared_files(store: &RunStore, record: &RunRecord, ledger: &Path, features: &[String], generated: &[String], stage: &str) {
    let events = match store.events(&record.run_id) {
        Ok(events) => events,
        Err(err) => {
            eprintln!("orq: {}: cannot tell which files the {} session changed outside its features' files: {err:?}", record.run_id, stage_words(stage));
            return;
        }
    };
    let changed = match changed_files(record, &events) {
        Ok(changed) => changed,
        Err(err) => {
            eprintln!("orq: {}: cannot tell which files the {} session changed outside its features' files: {err}", record.run_id, stage_words(stage));
            return;
        }
    };
    let Some(declared) = declared_files(store, record, ledger, features, generated) else { return };
    let named = undeclared_recorded(&events);
    let outside: Vec<String> = changed
        .into_iter()
        .filter(|file| !declared.contains(file) && !exempt(file) && !named.iter().any(|(named, _)| named == file))
        .collect();
    if outside.is_empty() {
        return;
    }
    // Recorded under the stage's latest attempt, so the run page lists the files on that step.
    let attempt = events.iter().rev().find(|e| e.stage.as_deref() == Some(stage)).map_or(1, |e| e.attempt);
    let event = orq_orchestrator::Event {
        schema_version: 1,
        event_id: format!("{}-undeclared-{}", record.run_id, events.len()),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: UNDECLARED_FILES.to_string(),
        stage: Some(stage.to_string()),
        attempt,
        occurred_at: crate::review::utc_now(),
        payload: serde_json::json!({ "files": outside, "stage": stage }),
    };
    if let Err(err) = store.append_event(&event) {
        eprintln!("orq: {}: cannot record the files changed outside its features' files ({}): {err:?}", record.run_id, outside.join(","));
    }
}

/// After the session stage `stage`: a Rust file outside the features' files whose committed change is
/// formatting only is put back in a commit of its own and recorded as `session.format_reverted`, so it
/// is no longer among the files changed outside them. What cannot be done is said, and blocks nothing.
fn undo_formatting_outside(store: &RunStore, record: &RunRecord, ledger: &Path, features: &[String], generated: &[String], stage: &str) {
    let events = match store.events(&record.run_id) {
        Ok(events) => events,
        Err(err) => {
            eprintln!("orq: {}: cannot check the {} session's formatting outside its features' files: {err:?}", record.run_id, stage_words(stage));
            return;
        }
    };
    let base = changes_base(record, &events);
    let Some(declared) = declared_files(store, record, ledger, features, generated) else { return };
    if let Err(err) = orq_orchestrator::undo_formatting_outside(store, record, &base, &declared, stage) {
        eprintln!("orq: {}: cannot undo the {} session's formatting outside its features' files: {err}", record.run_id, stage_words(stage));
    }
}

/// The files the implement session is told: those `features` declare, and those this run, or the run
/// it resumes, already recorded as changed outside them. A name a prompt cannot hold is left out.
fn session_files(store: &RunStore, record: &RunRecord, ledger: &Path, features: &[String], resumed_from: Option<&str>) -> (Vec<String>, Vec<String>) {
    let promptable = |file: &String| !file.is_empty() && !file.chars().any(char::is_control);
    let declared: Vec<String> = declared_files(store, record, ledger, features, &[]).unwrap_or_default().into_iter().filter(promptable).collect();
    let mut undeclared: Vec<String> = Vec::new();
    for run_id in std::iter::once(record.run_id.as_str()).chain(resumed_from) {
        for (file, _) in store.events(run_id).map(|events| undeclared_recorded(&events)).unwrap_or_default() {
            if promptable(&file) && !undeclared.contains(&file) {
                undeclared.push(file);
            }
        }
    }
    (declared, undeclared)
}

/// How the ending note and messages say a session stage.
fn stage_words(stage: &str) -> &str {
    match stage {
        "implement_and_review" => "implement",
        "gate_fix" => "gate fix",
        "rebase_fix" => "rebase fix",
        other => other,
    }
}

/// The commit the run's changes are counted from: the one its branch was last rebased (or stopped
/// rebasing) onto, else its base.
fn changes_base(record: &RunRecord, events: &[orq_orchestrator::Event]) -> String {
    let onto = events.iter().rev().find_map(|e| match e.kind.as_str() {
        "rebase.completed" | "run.rebase_carried" => e.payload["target_sha"].as_str(),
        "rebase.conflict" => e.payload["onto"].as_str(),
        _ => None,
    });
    onto.unwrap_or(&record.base_sha).to_string()
}

/// The files the run's commits changed: since the commit its branch was last rebased (or stopped
/// rebasing) onto, else since its base.
fn changed_files(record: &RunRecord, events: &[orq_orchestrator::Event]) -> Result<Vec<String>, String> {
    let base = changes_base(record, events);
    let output = Command::new("git")
        .arg("-C")
        .arg(&record.worktree)
        .args(["diff", "--name-only", "--no-renames", "-z", &format!("{base}...HEAD"), "--"])
        .output()
        .map_err(|err| format!("cannot run git: {err}"))?;
    if !output.status.success() {
        return Err(format!("git diff {base}...HEAD: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect())
}

/// The files the run's features declare: the `targetFiles` and test files of its own features and of
/// those its sessions started, and the files `generated` from them. None when the ledger cannot be
/// read, which is said.
fn declared_files(store: &RunStore, record: &RunRecord, ledger: &Path, features: &[String], generated: &[String]) -> Option<std::collections::BTreeSet<String>> {
    let entries = match orq_features::load_ledger(ledger) {
        Ok(entries) => entries,
        Err(orq_features::LedgerError::Missing { .. }) => Vec::new(),
        Err(err) => {
            eprintln!("orq: {}: cannot read the feature ledger to tell the features' files: {err:?}", record.run_id);
            return None;
        }
    };
    let mut names: Vec<String> = features.to_vec();
    names.extend(extra_features_recorded(store, &record.run_id).unwrap_or_default());
    names.extend(orq_orchestrator::fix_features(store, &record.run_id).unwrap_or_default());
    let mut declared = std::collections::BTreeSet::new();
    for entry in entries.iter().filter(|entry| names.contains(&entry.feature_name)) {
        declared.extend(entry.target_files.iter().cloned());
        // A test target is a path or an expression holding paths (`path::name`, options).
        for word in entry.test_file.iter().flat_map(|t| t.split_whitespace()).filter(|w| !w.starts_with('-')) {
            declared.insert(word.split("::").next().unwrap_or(word).to_string());
        }
    }
    // A web bundle is rebuilt from its sources: a declared source declares what it builds.
    let bundles: Vec<String> = declared.iter().filter_map(|file| file.split_once("web-src/").map(|(crate_dir, _)| crate_dir.to_string())).collect();
    for dir in bundles {
        for built in ["web/app.react.js", "web/app.react.js.stamp", "web/sw.js"] {
            declared.insert(format!("{dir}{built}"));
        }
    }
    declared.extend(generated.iter().cloned());
    Some(declared)
}

/// The spec (ledger description) of each of the run's `features` that has one, in the run's order.
/// An unreadable ledger gives none: the session still has the feature names.
fn feature_specs(ledger: &Path, features: &[String]) -> Vec<(String, String)> {
    let Ok(entries) = orq_features::load_ledger(ledger) else { return Vec::new() };
    features
        .iter()
        .filter_map(|name| {
            let entry = entries.iter().find(|entry| &entry.feature_name == name)?;
            Some((name.clone(), entry.description.clone()?))
        })
        .collect()
}

/// A file whose change is normal whatever the features declare: an existing test under a `tests/`
/// directory, a lockfile, or the TDD workflow's own records (`.tdd-*`, such as its ledger and log).
fn exempt(file: &str) -> bool {
    const LOCKFILES: [&str; 4] = ["Cargo.lock", "package-lock.json", "pnpm-lock.yaml", "uv.lock"];
    let name = file.rsplit('/').next().unwrap_or(file);
    LOCKFILES.contains(&name) || name.starts_with(".tdd-") || file.split('/').rev().skip(1).any(|dir| dir == "tests")
}

/// The files the run's `session.undeclared_files` events name, each once, with the stage that first
/// named it.
fn undeclared_recorded(events: &[orq_orchestrator::Event]) -> Vec<(String, String)> {
    let mut named: Vec<(String, String)> = Vec::new();
    for event in events.iter().filter(|e| e.kind == UNDECLARED_FILES) {
        let stage = event.payload["stage"].as_str().unwrap_or_default();
        for file in event.payload["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()) {
            if !named.iter().any(|(n, _)| n == file) {
                named.push((file.to_string(), stage.to_string()));
            }
        }
    }
    named
}

/// The ending's note on the files the run changed outside its features' files, if any were recorded.
fn undeclared_note(store: &RunStore, run_id: &str) -> Option<String> {
    let events = store.events(run_id).ok()?;
    let named = undeclared_recorded(&events);
    (!named.is_empty()).then(|| {
        let listed: Vec<String> = named.iter().map(|(file, stage)| format!("{file} ({})", stage_words(stage))).collect();
        format!("changed outside its features' files: {}", listed.join(", "))
    })
}

/// The session log lines that are not the run's, each with how many times it is there: a project that
/// keeps the log in git hands the worktree the events of other sessions, at the run's base and again
/// with each commit a rebase put the branch onto. Every run that shared the worktree (a resumed run
/// keeps it) is looked at, since its rebases moved this branch too. A line counts as many times as the
/// commit with most of it has it. An error when what one of those commits or runs holds cannot be
/// read, or a run's record is gone while its events remain, since then no line can be told to be the
/// run's.
fn lines_not_the_runs(store: &RunStore, record: &RunRecord) -> Result<std::collections::HashMap<String, usize>, String> {
    let mut commits = vec![record.base_sha.clone()];
    for run_id in store.run_ids().map_err(|err| format!("cannot list the runs: {err:?}"))? {
        if run_id != record.run_id {
            match store.load(&run_id) {
                Ok(other) if other.worktree == record.worktree => {}
                Ok(_) => continue,
                // A run with neither a record nor events is still starting: it has rebased nothing.
                Err(StoreError::NotFound(_)) if !store.has_event_log(&run_id).map_err(|err| format!("cannot read the run {run_id}: {err:?}"))? => continue,
                Err(err) => return Err(format!("cannot read the run {run_id}, which may have rebased this branch: {err:?}")),
            }
        }
        for event in store.events(&run_id).map_err(|err| format!("cannot read the events of {run_id}: {err:?}"))? {
            let onto = match event.kind.as_str() {
                "rebase.completed" => event.payload["target_sha"].as_str(),
                "rebase.conflict" => event.payload["onto"].as_str(),
                _ => None,
            };
            if let Some(sha) = onto.filter(|sha| !commits.iter().any(|c| c == sha)) {
                commits.push(sha.to_string());
            }
        }
    }
    let mut foreign: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for commit in &commits {
        let mut here: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let text = log_at(record, commit)?.unwrap_or_default();
        for line in text.lines() {
            *here.entry(line).or_default() += 1;
        }
        for (line, count) in here {
            let most = foreign.entry(line.to_string()).or_default();
            *most = (*most).max(count);
        }
    }
    Ok(foreign)
}

/// The session log as `commit` has it; None when the commit has none. An error when git cannot say,
/// such as when the commit or the log's contents cannot be read.
fn log_at(record: &RunRecord, commit: &str) -> Result<Option<String>, String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&record.worktree)
            .args(args)
            .output()
            .map_err(|err| format!("cannot run git: {err}"))?;
        if !output.status.success() {
            return Err(format!("cannot read {SESSION_LOG} at {commit}: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    if git(&["ls-tree", "--name-only", commit, "--", SESSION_LOG])?.trim().is_empty() {
        return Ok(None);
    }
    git(&["cat-file", "blob", &format!("{commit}:{SESSION_LOG}")]).map(Some)
}

/// The features the `init_feature` events among a TDD session log's `lines` name, each once, in the
/// order first started. A line that is not such an event is skipped.
fn features_started_in<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut started: Vec<String> = Vec::new();
    for line in lines {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if event["event"] != "init_feature" {
            continue;
        }
        if let Some(name) = event["featureName"].as_str().filter(|name| !started.iter().any(|s| s == name)) {
            started.push(name.to_string());
        }
    }
    started
}

/// The features a session started: the names in `after` that were not in `before` and that the
/// session's TDD workflow left `in_progress` or `completed`. A new `draft`, `pending` or `abandoned`
/// entry is someone else's, added to the shared ledger while the session ran.
fn started_by_session(before: &[(String, orq_features::LedgerStatus)], after: Vec<(String, orq_features::LedgerStatus)>) -> Vec<String> {
    use orq_core::FeatureStatus::{Completed, InProgress};
    after
        .into_iter()
        .filter(|(name, status)| {
            matches!(status.known(), Some(InProgress | Completed)) && !before.iter().any(|(old, _)| old == name)
        })
        .map(|(name, _)| name)
        .collect()
}

/// The commits on the branch since `base`, one short line each, newest first, at most 50; none when
/// git cannot tell.
fn short_log(worktree: &Path, base: &str) -> String {
    match Command::new("git").arg("-C").arg(worktree).args(["log", "--oneline", "--no-decorate", "--max-count=50", &format!("{base}..HEAD")]).output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).into_owned(),
        _ => String::new(),
    }
}

/// Add `note` to what the report says about its ending.
fn add_note(report: &mut Report, note: String) {
    report.detail = Some(match report.detail.take() {
        Some(detail) => format!("{detail}; {note}"),
        None => note,
    });
}

/// The ending's note on the features the run's sessions added beside its own, if any were recorded.
fn extra_features_note(store: &RunStore, run_id: &str) -> Option<String> {
    let added = match extra_features_recorded(store, run_id) {
        Ok(added) => added,
        Err(err) => {
            eprintln!("orq: cannot read which features the session of {run_id} added: {err:?}");
            return None;
        }
    };
    (!added.is_empty()).then(|| format!("the session also added: {}", added.join(", ")))
}

/// The features the run's `ledger.extra_features` events credit it with, each once.
fn extra_features_recorded(store: &RunStore, run_id: &str) -> Result<Vec<String>, StoreError> {
    features_in_events(store, run_id, "ledger.extra_features")
}

/// The features the run's `kind` events name, each once, in the order first named.
fn features_in_events(store: &RunStore, run_id: &str, kind: &str) -> Result<Vec<String>, StoreError> {
    let mut added: Vec<String> = Vec::new();
    for event in store.events(run_id)?.iter().filter(|e| e.kind == kind) {
        for name in event.payload["features"].as_array().into_iter().flatten().filter_map(|n| n.as_str()) {
            if !added.iter().any(|a| a == name) {
                added.push(name.to_string());
            }
        }
    }
    Ok(added)
}

/// The ending's note on failures reported unrelated that passed on a re-run, so a delivered run still
/// says what was flaky. Each summary is told once.
fn flaky_note(store: &RunStore, run_id: &str) -> Option<String> {
    let events = store.events(run_id).ok()?;
    let mut flaky: Vec<&str> = Vec::new();
    for summary in events
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "gate.flaky" | "ci.flaky"))
        .filter_map(|e| e.payload["summary"].as_str())
    {
        if !flaky.contains(&summary) {
            flaky.push(summary);
        }
    }
    (!flaky.is_empty()).then(|| format!("Flaky: {}", flaky.join("; ")))
}

/// Record a `kind` event naming the `features` a ledger step changed; none changed records nothing.
/// Best effort: a failure is said and changes nothing else about the run.
fn record_ledger(store: &RunStore, record: &RunRecord, kind: &str, features: &[String]) {
    if features.is_empty() {
        return;
    }
    if let Err(err) = orq_orchestrator::record_ledger_features(store, record, kind, features) {
        eprintln!("orq: cannot record {kind} for {}: {err:?}", features.join(","));
    }
}

/// Set the features the run's fix sessions started `completed`, but for those a later run took. One
/// that is not `in_progress` any more was closed or reopened by someone else and is left as it is.
fn close_fix_features(store: &RunStore, record: &RunRecord, ledger: &Path) {
    let started = orq_orchestrator::fix_features(store, &record.run_id).unwrap_or_else(|err| {
        eprintln!("orq: cannot read which features the fix sessions of {} started: {err:?}", record.run_id);
        Vec::new()
    });
    if started.is_empty() {
        return;
    }
    let free = features_no_later_run_took(store, record, &started);
    if let Err(err) = orq_features::close_features(ledger, &free, orq_core::FeatureStatus::Completed, &crate::review::utc_now()) {
        eprintln!("orq: cannot set {} completed: {err}", free.join(","));
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
    // The explainer reads copies of the run's logs in the worktree, so it needs nothing outside it. It
    // outlives the run, so its copies stay until the worktree goes or a later session replaces them.
    let copies = match copy_run_logs(&record.worktree, logs) {
        Ok(copies) => copies,
        Err(err) => {
            eprintln!("orq: cannot start the failure explainer: {err}");
            return;
        }
    };
    let Some(logs_dir) = copies.to_str() else { return };
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

/// Copy the run's logs in `logs` into `worktree`'s session files, but for the explainer's own log,
/// which it is writing. Returns where the copies are.
fn copy_run_logs(worktree: &Path, logs: &Path) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(logs).map_err(|err| format!("cannot read {}: {err}", logs.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "log") && path.file_name().is_some_and(|name| name != "explain.log"))
        .collect();
    files.sort();
    let files: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    orq_orchestrator::copy_session_files(worktree, &files)
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

/// Why a group's implement session is not done: the first member, in group order, that the ledger
/// does not show completed and no session of the run (`session_completed`, carried across resumes)
/// completed at the head the branch is on. One the session set `abandoned` (it was not `abandoned` in `before`, the ledger as the
/// session found it) is said to be so. A single feature is not checked.
fn unfinished_group_member(
    project: &orq_git::Project,
    features: &[String],
    session_completed: &[String],
    before: Option<&[(String, orq_features::LedgerStatus)]>,
) -> Option<String> {
    if features.len() < 2 {
        return None;
    }
    let entries = match orq_features::load_ledger(&orq_features::ledger_path(project)) {
        Ok(entries) => entries,
        Err(err) => return Some(format!("cannot tell whether the group's features were completed: the feature ledger: {err:?}")),
    };
    let completed = |feature: &str| {
        entries.iter().any(|e| e.feature_name == feature && e.status.known() == Some(orq_core::FeatureStatus::Completed))
            || session_completed.iter().any(|done| done == feature)
    };
    let unfinished = features.iter().find(|feature| !completed(feature))?;
    let abandoned = orq_features::LedgerStatus::Known(orq_core::FeatureStatus::Abandoned);
    let now_abandoned = entries.iter().any(|e| &e.feature_name == unfinished && e.status == abandoned);
    let was_abandoned = before.is_none_or(|before| before.iter().any(|(name, status)| name == unfinished && *status == abandoned));
    if now_abandoned && !was_abandoned {
        return Some(format!("the implement session set {unfinished} abandoned, which does not finish it"));
    }
    Some(format!("the implement session ended before {unfinished} was completed"))
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

/// Writes what the run's sessions may do without asking, checked against the run's branch as it is
/// now: a script the run changed since the commit it was last rebased onto (or started from) is no
/// longer run without asking. Called before each session, so a change made by one is seen by the next.
/// Writes the session permissions again before a fix session, saying on stderr what was left out:
/// the fix session runs on what the earlier sessions changed. A rule that should be left out but that
/// the settings still allow starts no session: the error says which.
/// Returns the command rules the written settings allow, for the fix session's prompt.
/// Points the worktree's needs-input hook at the attended session about to start: one that leaves
/// `marker` when it is finished is also heard stopping without it. Returns notes.
fn hook_needs_input(worktree: &Path, run_id: &str, root: &Path, marker: Option<&Path>) -> Vec<String> {
    let orq = match std::env::current_exe() {
        Ok(orq) => orq,
        Err(err) => return vec![format!("the session will not say when it needs an answer: orq's own path is unknown ({:?})", err.kind())],
    };
    // The session's environment is cleared, so the hook is told where the run's state is when orq was.
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(|dir| format!("XDG_STATE_HOME={} ", crate::sh_quote(&dir.to_string_lossy())))
        .unwrap_or_default();
    let mut command = format!(
        "{state}{} run needs-input {} --repo {}",
        crate::sh_quote(&orq.to_string_lossy()),
        crate::sh_quote(run_id),
        crate::sh_quote(&root.to_string_lossy())
    );
    if let Some(marker) = marker {
        command += &format!(" --marker {}", crate::sh_quote(&marker.to_string_lossy()));
    }
    orq_orchestrator::write_needs_input_hook(worktree, &command)
}

fn recheck_permissions(store: &RunStore, record: &RunRecord, settings: &orq_orchestrator::RunSettings, logs: &Path) -> Result<Vec<String>, StoreError> {
    let permissions = write_permissions(store, record, settings, logs)?;
    permissions.notes.iter().for_each(|note| eprintln!("orq: {note}"));
    Ok(permissions.allowed_commands)
}

fn write_permissions(store: &RunStore, record: &RunRecord, settings: &orq_orchestrator::RunSettings, logs: &Path) -> Result<orq_orchestrator::RunPermissions, StoreError> {
    let base = orq_orchestrator::rebased_onto(store, &record.run_id)?.unwrap_or_else(|| record.base_sha.clone());
    let extra = settings.allowed_tools.as_deref().unwrap_or_default();
    orq_orchestrator::refresh_run_permissions(&record.worktree, extra, logs, &base).map_err(StoreError::Corrupt)
}

/// What a resumed run takes over from the run it continues; all `None` for a new run.
#[derive(Clone, Copy)]
struct Resumed<'a> {
    /// The run this one continues.
    from: Option<&'a RunRecord>,
    /// That run's agent session finished, so this run does not start one.
    implement_inherited_from: Option<&'a str>,
    /// That run paused here, having done these stages.
    paused_at: Option<&'a orq_orchestrator::Checkpoint>,
    /// That run's review escalated, so this run's session reviews again from where it stopped.
    review_again: Option<&'a orq_orchestrator::EscalatedReview>,
    /// That run failed after its last review, rather than waiting on a decision after its review escalated.
    review_again_failed: bool,
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
    let Resumed { from: resumed_from, implement_inherited_from, paused_at, review_again, review_again_failed } = *resumed;
    let run_id = record.run_id.as_str();
    // Settled when the stages end, however they end, so the last fix session's features are recorded too.
    let fix_features = FixFeatureWatch::new(store, record, orq_features::ledger_path(options.project), options.features);
    // A run continued from a pause does not redo what the checkpoint records as done: the push and
    // the review request are outward-facing and must happen once.
    let done = |stage: &str| paused_at.is_some_and(|c| c.completed_stages.iter().any(|d| d == stage));
    let worktree = record.worktree.as_path();
    let implementer = with_run_model(&options.implementer, options, settings);
    // A frontend run (one feature marked frontend) has the design package and design review steps.
    let frontend_run = !options.publish && options.features.len() == 1 && {
        let frontend = frontend_features(options).map_err(|e| StoreError::Corrupt(start_error_text(e)))?;
        frontend.contains(&options.features[0])
    };

    let Some(claude) = find_program(&options.implementer.harness) else {
        return stopped(Ending::Failed, harness_missing(&options.implementer.harness));
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
    // The command rules the session settings allow, so the session's prompt can list them.
    let mut allowed_commands: Vec<String> = Vec::new();
    if !options.publish {
        let carried = orq_orchestrator::carry_local_files(&options.project.root, worktree, settings.local_files.as_deref());
        run_vars.extend(carried.env);
        notes.extend(carried.notes);
        notes.extend(orq_orchestrator::exclude_worktrees_dir(&options.project.root));
        let mcpctl = find_program("mcpctl");
        let mcp = orq_orchestrator::prepare_project_mcp(&options.project.root, worktree, &run_vars, settings.mcp_servers.as_deref(), mcpctl.as_deref(), &logs.join("mcp-setup.log"));
        if !mcp.versions.is_empty() || !mcp.notes.is_empty() {
            if let Err(err) = orq_orchestrator::record_mcp_setup(store, record, &mcp) {
                notes.push(format!("the run's events do not record how its MCP servers were set up: {err}"));
            }
        }
        notes.extend(mcp.notes);
        // Nobody may be there to answer a permission prompt, and the exit marker goes into the logs.
        let permissions = write_permissions(store, record, settings, logs)?;
        notes.extend(permissions.notes);
        allowed_commands = permissions.allowed_commands;
        // Only a session with a terminal has someone to ask.
        if options.interactive {
            let marker = (settings.auto_exit != Some(false)).then(|| logs.join("implement.done"));
            notes.extend(hook_needs_input(worktree, run_id, &options.project.root, marker.as_deref()));
        } else if options.unattended {
            // Nobody can answer an unattended session: the hook only puts on the run that it asked, and
            // the run ends blocked with what it asked.
            notes.extend(hook_needs_input(worktree, run_id, &options.project.root, None));
            notes.extend(record_unattended(store, record));
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
        if implement_inherited_from.is_none() && frontend_run {
            if let Some(stop) = design_package(store, run_id, options, settings, logs, worktree)? {
                return Ok(stop);
            }
        }
        if implement_inherited_from.is_none() {
            if resumed_from.is_some() {
                hand_back_features(store, record, &orq_features::ledger_path(options.project), options.features);
            }
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
            // A finding the review has open again whose feature a session already completed could not be
            // fixed under its own name: it goes back to pending first, and the session is told.
            let reopened = reopen_open_findings(store, record, &orq_features::ledger_path(options.project));
            let reopened_findings: Vec<&str> = reopened.iter().map(|r| r.finding.as_str()).collect();
            let open_findings = review_again.map(open_findings_text);
            // A resumed session carries on with the features the run it resumes started; a review asked
            // for again reviews instead.
            let continued_log = resumed_from.filter(|_| review_again.is_none()).map(|_| short_log(worktree, &record.base_sha));
            let open_tdd = continued_log.as_ref().and_then(|_| open_tdd_state(worktree));
            let continuing = resumed_from.zip(continued_log.as_deref()).map(|(from, log)| orq_agents::Continuing {
                run: &from.run_id,
                base: &record.base_sha,
                log,
                open_feature: open_tdd.as_ref().map(|(name, phase, depth)| orq_agents::OpenFeature { name, phase, depth: *depth }),
            });
            // The session is told the features' files, and on a resumed run those already changed outside them.
            let (declared_files, undeclared_files) = session_files(store, record, &orq_features::ledger_path(options.project), options.features, resumed_from.map(|from| from.run_id.as_str()));
            let specs = feature_specs(&orq_features::ledger_path(options.project), options.features);
            let spec_refs: Vec<(&str, &str)> = specs.iter().map(|(name, spec)| (name.as_str(), spec.as_str())).collect();
            let args = adapter
                .implement_argv(&ImplementRequest {
                    feature_specs: &spec_refs,
                    features: &features,
                    implementer: spec(&implementer),
                    reviewer: spec(&options.reviewer),
                    context: options.context,
                    exit_marker: marker,
                    attended: options.interactive,
                    unattended: options.unattended,
                    remote_control: (settings.remote_control != Some(false)).then_some(record.branch.as_str()),
                    handoff: handoff.as_deref(),
                    continuing,
                    review_again: review_again.zip(open_findings.as_deref()).map(|(review, findings)| orq_agents::ReviewAgain { from: &review.reviewed_head, findings, failed: review_again_failed }),
                    // The gate runs the full suite after the session, so the session need not; a project
                    // whose gate is not a full suite opts out.
                    leave_full_suite_to_gate: options.gate && settings.full_suite_in_session != Some(true),
                    allowed_commands: &allowed_commands,
                    declared_files: &declared_files,
                    answer: options.answer,
                    undeclared_files: &undeclared_files,
                    reopened_findings: &reopened_findings,
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
            let before = ledger_statuses(&orq_features::ledger_path(options.project), "the implement session");
            let files_before = ledger_target_files(&orq_features::ledger_path(options.project), options.features);
            let outcome = run_implement_stage(store, run_id, &session)?;
            restore_target_files(store, record, &orq_features::ledger_path(options.project), files_before.as_deref());
            // What the session completed is recorded, so the group check counts it.
            let completed = record_session_completed(store, record, &orq_features::ledger_path(options.project), options.features, before.as_deref());
            record_extra_features(store, record, &orq_features::ledger_path(options.project), options.features, "implement");
            undo_formatting_outside(store, record, &orq_features::ledger_path(options.project), options.features, settings.rebase_generated.as_deref().unwrap_or_default(), "implement_and_review");
            record_undeclared_files(store, record, &orq_features::ledger_path(options.project), options.features, settings.rebase_generated.as_deref().unwrap_or_default(), "implement_and_review");
            if options.unattended {
                if let Some(reason) = unanswered_need(store, run_id) {
                    return stopped(Ending::Blocked, reason);
                }
                // What the session could not decide waits for a person, whatever else it left: a
                // review that escalated too is drafted only once that is decided.
                if let Some(decision) = needed_decision(outcome.reported.as_deref()) {
                    return stopped(Ending::Blocked, decision);
                }
            }
            // Nobody decides for an unattended run whose review escalated, however its session
            // reported: what is left open becomes a draft follow-up and the run carries on, unless a
            // finding is one a person must see first. A session that left no report crashed: whatever
            // review it left, it has failed, and nothing is drafted from it.
            let drafted = options.unattended
                && (outcome.success || outcome.reported.is_some())
                && match escalated(worktree, &record.base_sha) {
                    Some(review) => {
                        if let Err(reason) = draft_followups(store, record, &orq_features::ledger_path(options.project), &review) {
                            return stopped(Ending::Blocked, reason);
                        }
                        true
                    }
                    None => false,
                };
            if !outcome.success && !drafted {
                // A session that stopped for its escalated review has done what it could; the
                // decision is a human's, so the run waits for it instead of failing.
                let review = (!options.unattended).then(|| outcome.reported.as_ref().and_then(|_| escalated(worktree, &record.base_sha))).flatten();
                match review {
                    Some(review) => return stopped(Ending::NeedsReview, escalation_reason(&review)),
                    None => {
                        return stopped(Ending::Failed, match outcome.reported {
                            Some(problem) => format!("the implement session reported a problem: {problem}"),
                            None => "the implement session failed".to_string(),
                        })
                    }
                }
            }
            if let ImplementCheck::Blocked { reason } = check_implement_outcome(store, run_id)? {
                return stopped(Ending::Blocked, reason);
            }
            if let Some(reason) = unfinished_group_member(options.project, options.features, &kept_by_group(store, record, options.features), before.as_deref()) {
                // Recorded as the session's stage failing, so resuming starts the session again.
                orq_orchestrator::record_implement_unfinished(store, run_id, &reason)?;
                return stopped(Ending::Failed, reason);
            }
            // The session's work goes on to the gate and review: what it completed is held
            // `in_progress` until the run delivers it. A run that stopped above is left to
            // `reopen_undelivered`, as before.
            hold_until_delivery(store, record, &orq_features::ledger_path(options.project), &completed);
        }
    }

    // The agent session has the terminal while it runs; after it, short lines keep the pane alive.
    let _progress = (options.interactive && !options.publish)
        .then(|| logs.parent().and_then(Path::parent).map(|root| crate::progress::Progress::start(root.to_path_buf(), run_id.to_string(), logs.to_path_buf())))
        .flatten();

    // The steps of the gate this process last ran, for the report of the head it checked.
    // The gate this process last ran: the one the run's head passed, which a later check of it runs too.
    let ran_gate: std::cell::RefCell<Option<orq_orchestrator::Gate>> = std::cell::RefCell::new(None);
    // Runs the gate, with its fix sessions, on the run's branch as it is now: `Some` with how the run
    // stops when the branch does not pass.
    let check = || -> Result<Option<Report>, StoreError> {
        let stopped_now = |ending: Ending, detail: String| stopped(ending, detail).map(Some);
        let gate = choose_gate(worktree, settings.gate.as_deref()).map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
        *ran_gate.borrow_mut() = Some(gate.clone());
        let gate_log = logs.join("gate.log");
        let fix_log = logs.join("gate-fix.log");
        // The session reads a copy of the gate log in its worktree, and says there that the failure is not
        // the run's; it is told the report's place in its environment too.
        let session_files = worktree.join(orq_orchestrator::SESSION_FILES_DIR);
        let unrelated_report = session_files.join(orq_orchestrator::UNRELATED_FAILURE_FILE);
        let mut fix_env = database_env.to_vec();
        fix_env.push(("ORQ_UNRELATED_FAILURE_FILE".to_string(), unrelated_report.to_string_lossy().into_owned()));
        // A red gate gets a fix session with the run's environment, then runs again. A published branch is
        // the user's own checkout: it is only checked, never edited by an agent.
        let fix_attempts = if options.publish { 0 } else { GATE_FIX_ATTEMPTS };
        // Someone watching the run gets the session on the terminal, closed once it says it is finished.
        let fix_marker = (options.interactive && settings.auto_exit != Some(false)).then(|| logs.join("gate-fix.done"));
        let task = format!(
            "The local gate failed after your changes. Its log is at {}. Read it, fix the cause in this worktree and commit the fix. \
             If the failure has nothing to do with your changes (a flaky test, a test already broken on the target branch, a broken environment), do not fix it: \
             write a line saying what fails and why it is not caused by your changes to {} and stop.",
            session_files.join("gate.log").display(),
            unrelated_report.display()
        );
        let gate_env = gate_env(store, record, database_env)?;
        let gate_outcome =
            run_gate_with_fixes_then(store, run_id, &gate, options.gate, &gate_log, fix_attempts, &gate_env, |_, _| {
                fix_features.before("gate_fix");
                orq_orchestrator::copy_session_files(worktree, &[gate_log.as_path()]).map_err(StoreError::Corrupt)?;
                // A script the run has changed since is no longer allowed to the fix session, nor listed to it.
                let allowed_commands = recheck_permissions(store, record, settings, logs)?;
                if options.interactive {
                    hook_needs_input(worktree, run_id, &options.project.root, fix_marker.as_deref()).iter().for_each(|note| eprintln!("orq: {note}"));
                }
                let args = adapter_for(&options.implementer.harness)
                    .and_then(|adapter| {
                        adapter.fix_argv(&FixRequest {
                            implementer: spec(&implementer),
                            task: &task,
                            exit_marker: fix_marker.as_deref().and_then(Path::to_str),
                            attended: options.interactive,
                            allowed_commands: &allowed_commands,
                        })
                    })
                    .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
                Ok(FixSessionSpec {
                    program: claude_program.to_string(),
                    args,
                    env: fix_env.clone(),
                    cwd: worktree.to_path_buf(),
                    session_id: None,
                    log: fix_log.clone(),
                    terminal: options.interactive,
                    exit_marker: fix_marker.clone(),
                })
            }, || {
                fix_features.settle();
                record_extra_features(store, record, &orq_features::ledger_path(options.project), options.features, "gate fix");
            record_undeclared_files(store, record, &orq_features::ledger_path(options.project), options.features, settings.rebase_generated.as_deref().unwrap_or_default(), "gate_fix");
                // The report the session left in its copies is kept where the gate loop reads it.
                if let Err(err) = orq_orchestrator::finish_session_files(worktree, logs) {
                    eprintln!("orq: {err}");
                }
            })?;
        match gate_outcome {
            GateFixOutcome::Failed { step, exit_code, .. } => {
                return stopped_now(Ending::Failed, format!("the gate failed at step {step} (exit {exit_code:?})"));
            }
            // Left alone on purpose: the person decides whether it is fixed first, in its own change.
            // Mostly a flaky test: the gate runs once more, unchanged. A pass carries the run on, noted as a flake.
            GateFixOutcome::Unrelated { fix_attempts, step, summary } => {
                if !orq_orchestrator::rerun_gate(store, run_id, &gate, options.gate, &gate_log, &gate_env, fix_attempts, step, &summary)? {
                    return stopped_now(
                        Ending::Blocked,
                        format!("the gate fails for a reason unrelated to this run: {summary}; the re-run failed too. Get that fixed separately and merged into the target branch, then resume this run"),
                    );
                }
                if let ImplementCheck::Blocked { reason } = check_implement_outcome(store, run_id)? {
                    return stopped_now(Ending::Blocked, reason);
                }
            }
            // A repair that was not committed would leave the pushed commit unfixed.
            GateFixOutcome::Passed { fix_attempts } if fix_attempts > 0 => {
                if let ImplementCheck::Blocked { reason } = check_implement_outcome(store, run_id)? {
                    return stopped_now(Ending::Blocked, reason);
                }
            }
            _ => {}
        }
        Ok(None)
    };
    // A rebase stopped on a conflict: a session of the implementer resolves it and finishes the
    // rebase; `Some` with how the run stops when it does not. A published branch is the user's own
    // checkout, never edited by an agent.
    let resolve_conflict = |reason: String| -> Result<Option<Report>, StoreError> {
        if options.publish {
            return stopped(Ending::Blocked, reason).map(Some);
        }
        let task = orq_orchestrator::conflict_fix_task(
            store,
            run_id,
            settings.rebase_generated.as_deref().unwrap_or_default(),
            settings.rebase_rebuild.as_deref(),
        )?;
        let allowed_commands = recheck_permissions(store, record, settings, logs)?;
        let marker = (options.interactive && settings.auto_exit != Some(false)).then(|| logs.join("rebase-fix.done"));
        if options.interactive {
            hook_needs_input(worktree, run_id, &options.project.root, marker.as_deref()).iter().for_each(|note| eprintln!("orq: {note}"));
        }
        let args = adapter_for(&options.implementer.harness)
            .and_then(|adapter| {
                adapter.fix_argv(&FixRequest {
                    implementer: spec(&implementer),
                    task: &task,
                    exit_marker: marker.as_deref().and_then(Path::to_str),
                    attended: options.interactive,
                    allowed_commands: &allowed_commands,
                })
            })
            .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
        let session = ImplementSession {
            program: claude_program,
            args: &args,
            env: database_env,
            cwd: worktree,
            session_id: None,
            log: &logs.join("rebase-fix.log"),
            terminal: options.interactive,
            exit_marker: marker.as_deref(),
        };
        let outcome = orq_orchestrator::fix_rebase_conflicts(store, run_id, &session)?;
        record_extra_features(store, record, &orq_features::ledger_path(options.project), options.features, "rebase fix");
            record_undeclared_files(store, record, &orq_features::ledger_path(options.project), options.features, settings.rebase_generated.as_deref().unwrap_or_default(), "rebase_fix");
        match outcome {
            orq_orchestrator::ConflictFixOutcome::Resolved => Ok(None),
            orq_orchestrator::ConflictFixOutcome::StillBlocked { reason } => stopped(Ending::Blocked, reason).map(Some),
        }
    };
    // A frontend run's design review step: `Some` with how the run stops when its page does not pass. The step
    // carries on from what the run's events record, and a pass holds only for the head it reviewed, so calling
    // it again after a later change reviews once more, to verify.
    let design =|| -> Result<Option<Report>, StoreError> {
        if !frontend_run {
            return Ok(None);
        }
        let needs_review = |reason: String| stopped(Ending::NeedsReview, reason).map(Some);
        let Some(design_settings) = settings.design.as_ref() else {
            return needs_review("this frontend feature's project has no design settings: add a [design] table to .orq/run.toml".to_string());
        };
        let agents = match design_agents(options.project) {
            Ok(agents) => agents,
            Err(reason) => return needs_review(format!("the design review cannot run: {reason}")),
        };
        let main_checkout = crate::kind_root(options.project);
        let skill = orq_agents::design_skill("design-review").unwrap_or_default();
        let max = u32::from(design_settings.max_correction_rounds);
        let review = |round: u32| {
            let run = orq_orchestrator::DesignReviewRun {
                run_id,
                main_checkout: &main_checkout,
                feature: &options.features[0],
                settings: design_settings,
                run_logs: logs,
                worktree,
                round,
                reviewer: &agents.0,
                verifier: &agents.1,
                skill,
                schema: orq_agents::DESIGN_REVIEW_SCHEMA,
                agent_timeout: DESIGN_AGENT_TIMEOUT,
            };
            orq_orchestrator::review_design(&run, &orq_orchestrator::capture_design)
        };
        let correct = |batch: u32, issues: &[orq_orchestrator::ReviewIssue], suggestions: &[String]| {
            let task = orq_orchestrator::design_fix_task(batch, max, issues, suggestions);
            let allowed_commands = recheck_permissions(store, record, settings, logs)?;
            let marker = (options.interactive && settings.auto_exit != Some(false)).then(|| logs.join("design-fix.done"));
            let args = adapter_for(&options.implementer.harness)
                .and_then(|adapter| {
                    adapter.fix_argv(&FixRequest {
                        implementer: spec(&implementer),
                        task: &task,
                        exit_marker: marker.as_deref().and_then(Path::to_str),
                        attended: options.interactive,
                        allowed_commands: &allowed_commands,
                    })
                })
                .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
            let session = ImplementSession {
                program: claude_program,
                args: &args,
                env: database_env,
                cwd: worktree,
                session_id: None,
                log: &logs.join("design-fix.log"),
                terminal: options.interactive,
                exit_marker: marker.as_deref(),
            };
            fix_features.before("design_fix");
            let outcome = orq_orchestrator::run_design_correction(store, run_id, &session, batch);
            fix_features.settle();
            record_extra_features(store, record, &orq_features::ledger_path(options.project), options.features, "design fix");
            record_undeclared_files(store, record, &orq_features::ledger_path(options.project), options.features, settings.rebase_generated.as_deref().unwrap_or_default(), "design_fix");
            outcome
        };
        // The fix commits of a batch go through the project's code review again (review-and-fix over them).
        let review_code = |batch: u32, from: &str| {
            let reviewer = spec(&options.reviewer);
            let mut reviewer_options = format!("--reviewer {} --model {}", reviewer.harness, reviewer.model);
            if let Some(thinking) = &reviewer.thinking {
                reviewer_options.push_str(&format!(" --thinking {thinking}"));
            }
            let skill = orq_agents::skill_command(&options.implementer.harness, "review-and-fix").map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
            let Some(task) = orq_orchestrator::design_code_review_task(&skill, from, &reviewer_options, batch) else {
                return Ok(orq_orchestrator::CorrectionOutcome::Stopped(format!("design correction round {batch} recorded no head to review its fix commits from")));
            };
            let allowed_commands = recheck_permissions(store, record, settings, logs)?;
            let marker = (options.interactive && settings.auto_exit != Some(false)).then(|| logs.join("design-code-review.done"));
            let args = adapter_for(&options.implementer.harness)
                .and_then(|adapter| {
                    adapter.fix_argv(&FixRequest {
                        implementer: spec(&implementer),
                        task: &task,
                        exit_marker: marker.as_deref().and_then(Path::to_str),
                        attended: options.interactive,
                        allowed_commands: &allowed_commands,
                    })
                })
                .map_err(|e| StoreError::Corrupt(format!("{e:?}")))?;
            let session = ImplementSession {
                program: claude_program,
                args: &args,
                env: database_env,
                cwd: worktree,
                session_id: None,
                log: &logs.join("design-code-review.log"),
                terminal: options.interactive,
                exit_marker: marker.as_deref(),
            };
            fix_features.before("design_code_review");
            let outcome = orq_orchestrator::run_design_code_review(store, run_id, &session, batch);
            fix_features.settle();
            record_extra_features(store, record, &orq_features::ledger_path(options.project), options.features, "design code review");
            record_undeclared_files(store, record, &orq_features::ledger_path(options.project), options.features, settings.rebase_generated.as_deref().unwrap_or_default(), "design_code_review");
            outcome
        };
        let mut steps = PipelineDesignSteps { worktree, review: &review, correct: &correct, review_code: &review_code };
        match orq_orchestrator::run_design_loop(store, run_id, max, &mut steps)? {
            orq_orchestrator::DesignLoopOutcome::Passed { .. } => Ok(None),
            orq_orchestrator::DesignLoopOutcome::NeedsReview { reason, report } => needs_review(match report {
                Some(report) => format!("{reason}; the report is at {}", report.display()),
                None => reason,
            }),
            orq_orchestrator::DesignLoopOutcome::Stopped { reason } => stopped(Ending::Failed, reason).map(Some),
        }
    };
    if !done("gate") {
        if at_boundary(store, run_id, &["implement_and_review"], "rebase", None, None)? {
            return paused("rebase");
        }
        // A run resumed to have its stopped rebase resolved first finishes that one.
        if options.fix_conflicts {
            if let Some(stop) = resolve_conflict(format!("{run_id}: the rebase stopped on a conflict"))? {
                return Ok(stop);
            }
        }
        if let RebaseOutcome::Conflict { reason } = rebase_onto_target(store, run_id, options.rebase)? {
            if let Some(stop) = resolve_conflict(reason)? {
                return Ok(stop);
            }
        }
        if let Some(stop) = design()? {
            return Ok(stop);
        }
        if let Some(stop) = check()? {
            return Ok(stop);
        }
    }
    // Evidence is bound to the commit: a gate fix since the design passed gets a verification review first.
    if let Some(stop) = design()? {
        return Ok(stop);
    }

    // The worktree of a run whose work is delivered goes, and its test databases with it; a kept
    // worktree may still need them.
    let clean_up = |ready: bool, ci: &CiOutcome| -> Result<(), StoreError> {
        if options.publish {
            return Ok(());
        }
        // Copies a fix session could not have removed go before the worktree is kept or removed.
        if let Err(err) = orq_orchestrator::remove_session_files(worktree) {
            eprintln!("orq: {err}");
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
    // The gate last run, which catching up with a base that moved may have changed and a fix may not;
    // a resumed run whose gate ran before it stopped reads it from its worktree.
    let last_gate = || match ran_gate.borrow().clone() {
        Some(gate) => Ok(gate),
        None => choose_gate(worktree, settings.gate.as_deref()).map_err(|e| StoreError::Corrupt(format!("{e:?}"))),
    };
    // The steps a passing gate ran; the report counts them for a commit only when the gate passed for it.
    let gate_steps = || last_gate().map(|gate| steps_of(&gate)).unwrap_or_default();
    // A run whose delivery ends before any PR/MR: the delivered head's report is built while its
    // worktree is still there to read, then the worktree goes.
    let delivered = |head_sha: &str, ending: Ending| -> Result<Report, StoreError> {
        build_review_report(store, record, options, &gate_steps(), head_sha, None, logs);
        if ending == Ending::Merged {
            // Merged here, so no PR/MR will ever close its review: the merge resolves it.
            if let Err(err) = crate::review::record_local_merge(&record.run_id, head_sha) {
                note_review_report(logs, &format!("cannot resolve the review of the merged run: {err}"));
            }
        }
        clean_up(true, &CiOutcome::Skipped)?;
        Ok(Report { run_id: String::new(), ending, detail: None, review_url: None, ci_green: None })
    };
    if record.delivery == orq_core::project_settings::Delivery::MergeLocal {
        // Delivered in this repository: the base branch is fast-forwarded and nothing is pushed.
        // A base branch that cannot take the run as it is stops it, with its branch and worktree kept.
        // A base branch that moved on since the run was rebased is caught up with: the run is
        // rebased onto where it is now and checked again, a few times at most.
        // Asked to set aside uncommitted changes in the way: they are stashed, the run merged, and the
        // changes put back (or named in their stash when they conflict with the run). A conflict
        // resolved here is checked by the gate again before the next merge attempt.
        let landed = orq_orchestrator::land_on_target(store, run_id, options.set_aside, options.rebase, &mut |reason| resolve_conflict(reason), &mut || match check()? {
            // A catch-up rebase moves the head: the page is verified again before it lands.
            None => design(),
            stop => Ok(stop),
        })?;
        return match landed {
            orq_orchestrator::Landing::Merged { head_sha, note } => {
                let mut report = delivered(&head_sha, Ending::Merged)?;
                report.detail = note;
                Ok(report)
            }
            orq_orchestrator::Landing::Blocked { reason } => stopped(Ending::Blocked, reason),
            orq_orchestrator::Landing::Stopped(stop) => Ok(stop),
        };
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
                let (review, adopted) = match create_review_request_with(store, run_id, &provider, &draft, &people, Duration::from_secs(60))? {
                    ReviewRequestOutcome::Created(review) => (review, false),
                    ReviewRequestOutcome::Adopted(review) => (review, true),
                    ReviewRequestOutcome::Failed { reason } => {
                        return stopped(Ending::Failed, format!("review request failed: {reason}"))
                    }
                };
                // Kept so the review page links it; a failure is noted and never fails delivery.
                if let Err(err) = crate::review::record_created_review(run_id, &record.provider, &review, &head_sha, adopted) {
                    note_review_report(logs, &format!("cannot record the review request: {err}"));
                }
                review.url
        }
    };
    build_review_report(store, record, options, &gate_steps(), &head_sha, None, logs);

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
            build_review_report(store, record, options, &gate_steps(), &head_sha, Some(&ci), logs);
        }
        ci
    } else {
        // A CI fix is checked with the gate the pushed head passed, whatever a fix changes in what a gate would detect.
        let gate = last_gate()?;
        // The session reads a copy of the CI failure in its worktree, and says there that the failure is not
        // the run's; it is told the report's place in its environment too.
        let session_files = worktree.join(orq_orchestrator::SESSION_FILES_DIR);
        let unrelated_report = session_files.join(orq_orchestrator::UNRELATED_FAILURE_FILE);
        let mut fix_env = database_env.to_vec();
        fix_env.push(("ORQ_UNRELATED_FAILURE_FILE".to_string(), unrelated_report.to_string_lossy().into_owned()));
        let task = format!(
            "CI failed on the pushed branch. The ci-failure-*.log in {} lists the failing checks. Read it, get each check's log with {provider_cli}, fix the cause in this worktree and commit the fix. \
             If the failure has nothing to do with your changes (a flaky check, a check already failing on the target branch, a broken CI environment), do not fix it: \
             write a line saying what fails and why it is not caused by your changes to {} and stop.",
            session_files.display(),
            unrelated_report.display()
        );
        // Someone watching the run gets the session on the terminal, closed once it says it is finished.
        let fix_marker = (options.interactive && settings.auto_exit != Some(false)).then(|| logs.join("ci-fix.done"));
        let session_args = || {
            fix_features.before("ci_fix");
            // A script the run has changed since is no longer allowed to the fix session, nor listed to it.
            let allowed_commands = recheck_permissions(store, record, settings, logs)?;
            if options.interactive && !options.publish {
                hook_needs_input(worktree, run_id, &options.project.root, fix_marker.as_deref()).iter().for_each(|note| eprintln!("orq: {note}"));
            }
            adapter_for(&options.implementer.harness)
                .and_then(|adapter| {
                    adapter.fix_argv(&FixRequest {
                        implementer: spec(&implementer),
                        task: &task,
                        exit_marker: fix_marker.as_deref().and_then(Path::to_str),
                        attended: options.interactive,
                        allowed_commands: &allowed_commands,
                    })
                })
                .map_err(|e| StoreError::Corrupt(format!("{e:?}")))
        };
        let mut steps = PipelineCiSteps {
            store,
            run_id,
            provider: &provider,
            provider_cli,
            config: &ci_config,
            ci_log: &ci_log,
            push_log: logs.join("push.log"),
            report: &|head, ci| build_review_report(store, record, options, &steps_of(&gate), head, ci, logs),
            session_args: &session_args,
            session_ended: &|| {
                fix_features.settle();
                // The report the session left in its copies is kept where the CI fix loop reads it.
                if let Err(err) = orq_orchestrator::finish_session_files(worktree, logs) {
                    eprintln!("orq: {err}");
                }
            },
            session: FixSessionSpec {
                program: claude_program.to_string(),
                args: Vec::new(),
                env: fix_env,
                cwd: worktree.to_path_buf(),
                session_id: None,
                log: logs.join("ci-fix.log"),
                terminal: options.interactive,
                exit_marker: fix_marker.clone(),
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
            CiFixOutcome::Unrelated { summary, rerun, .. } => {
                return stopped(
                    Ending::Blocked,
                    format!("CI fails for a reason unrelated to this run: {summary}; {rerun}. Get that fixed separately and merged into the target branch, then resume this run"),
                );
            }
            outcome => ci_verdict(outcome),
        }
    };
    let ready = matches!(ci, CiOutcome::Passed | CiOutcome::Skipped);
    // A CI fix since the design passed gets a verification review before the review is ready.
    if ready {
        if let Some(stop) = design()? {
            return Ok(stop);
        }
    }
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

/// A frontend run's `design_package` step, before any agent: the feature's package is checked and frozen. `Some`
/// with how the run stops: waiting for design files (listed in its events), or a package that cannot be used.
fn design_package(
    store: &RunStore,
    run_id: &str,
    options: &Options<'_>,
    settings: &orq_orchestrator::RunSettings,
    logs: &Path,
    worktree: &Path,
) -> Result<Option<Report>, StoreError> {
    let Some(design) = settings.design.as_ref() else {
        return stopped(Ending::NeedsReview, "this frontend feature's project has no design settings: add a [design] table to .orq/run.toml").map(Some);
    };
    orq_orchestrator::record_design_package(store, run_id, "stage.started", serde_json::json!({}))?;
    let main_checkout = crate::kind_root(options.project);
    let run = orq_orchestrator::DesignPackageRun {
        main_checkout: &main_checkout,
        feature: &options.features[0],
        settings: design,
        run_logs: logs,
        worktree,
        limits: Default::default(),
    };
    // A package already frozen for the run (carried from the run it resumes) is checked unchanged, never frozen again.
    let files_needed = |needed: orq_orchestrator::DesignFilesNeeded| {
        let files: Vec<serde_json::Value> = needed.files.iter().map(|f| serde_json::json!({"what": f.what, "path": f.path})).collect();
        orq_orchestrator::record_design_package(store, run_id, "design.files_needed", serde_json::json!({"feature": needed.feature, "files": files}))?;
        stopped(Ending::DesignFilesNeeded, needed.message()).map(Some)
    };
    match orq_orchestrator::resume_design_package(&run) {
        orq_orchestrator::DesignPackageOutcome::Frozen(manifest) => {
            // Infer mode: the design preparer writes what the package lacks before the implementer.
            if design.mode == orq_orchestrator::DesignMode::Infer {
                let preparer_agent = match design_agents(options.project) {
                    Ok((_, _, preparer)) => preparer,
                    Err(reason) => return stopped(Ending::NeedsReview, format!("the design preparer cannot run: {reason}")).map(Some),
                };
                let skill = orq_agents::design_skill("design-prepare").unwrap_or_default();
                let mut preparer = orq_orchestrator::AgentPreparer {
                    agent: &preparer_agent,
                    worktree,
                    skill: &skill,
                    log: logs.join("design/preparer.log"),
                    timeout: DESIGN_AGENT_TIMEOUT,
                };
                match orq_orchestrator::prepare_design(&run, &manifest, &mut preparer) {
                    orq_orchestrator::DesignPrepareOutcome::Skipped | orq_orchestrator::DesignPrepareOutcome::Prepared(_) => {}
                    orq_orchestrator::DesignPrepareOutcome::FilesNeeded(needed) => return files_needed(needed),
                    orq_orchestrator::DesignPrepareOutcome::Failed(reason) => return stopped(Ending::Failed, reason).map(Some),
                }
            }
            orq_orchestrator::record_design_package(store, run_id, "stage.completed", serde_json::json!({}))?;
            Ok(None)
        }
        orq_orchestrator::DesignPackageOutcome::FilesNeeded(needed) => files_needed(needed),
        orq_orchestrator::DesignPackageOutcome::Refused(problems) => {
            stopped(Ending::Failed, format!("the design package cannot be used: {}", problems.join("; "))).map(Some)
        }
        orq_orchestrator::DesignPackageOutcome::Changed(reason) => stopped(Ending::NeedsReview, reason).map(Some),
    }
}

/// How long one design agent (preparer, reviewer or verifier) may take.
const DESIGN_AGENT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The design reviewer, verifier and preparer the project's settings name.
fn design_agents(
    project: &orq_git::Project,
) -> Result<(orq_orchestrator::DesignAgent, orq_orchestrator::DesignAgent, orq_orchestrator::DesignAgent), String> {
    let settings = crate::project_settings(project)?;
    let uses_pi = [&settings.design_reviewer, &settings.design_verifier, &settings.design_preparer]
        .iter()
        .any(|agent| agent.as_ref().is_some_and(|agent| agent.harness == "pi"));
    let listing = if uses_pi { find_program("pi").and_then(|program| crate::pi_model_listing(&program, Duration::from_secs(5))) } else { None };
    let agents = orq_core::project_settings::design_agents(&settings, orq_agents::effort_levels, listing.as_deref())?;
    let agent = |agent: &orq_core::project_settings::ProjectAgent| orq_orchestrator::DesignAgent {
        program: PathBuf::from(&agent.harness),
        harness: agent.harness.clone(),
        model: agent.model.clone(),
        thinking: agent.effort.clone(),
    };
    Ok((agent(&agents.reviewer), agent(&agents.verifier), agent(&agents.preparer)))
}

/// The design loop's steps on the run's worktree: the real capture and review, and the implementer's fix session.
struct PipelineDesignSteps<'a> {
    worktree: &'a Path,
    review: &'a dyn Fn(u32) -> orq_orchestrator::DesignReviewOutcome,
    correct: &'a dyn Fn(u32, &[orq_orchestrator::ReviewIssue], &[String]) -> Result<orq_orchestrator::CorrectionOutcome, StoreError>,
    review_code: &'a dyn Fn(u32, &str) -> Result<orq_orchestrator::CorrectionOutcome, StoreError>,
}

impl orq_orchestrator::DesignLoopSteps for PipelineDesignSteps<'_> {
    fn head(&mut self) -> Result<String, StoreError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.worktree)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|e| StoreError::Corrupt(format!("cannot read the run's head: {e}")))?;
        if !out.status.success() {
            return Err(StoreError::Corrupt("cannot read the run's head".to_string()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn review(&mut self, round: u32) -> Result<orq_orchestrator::DesignReviewOutcome, StoreError> {
        Ok((self.review)(round))
    }

    fn correct(&mut self, batch: u32, issues: &[orq_orchestrator::ReviewIssue], suggestions: &[String]) -> Result<orq_orchestrator::CorrectionOutcome, StoreError> {
        (self.correct)(batch, issues, suggestions)
    }

    fn review_code(&mut self, batch: u32, from: &str) -> Result<orq_orchestrator::CorrectionOutcome, StoreError> {
        (self.review_code)(batch, from)
    }
}

/// A refusal's text, for a run that has already started.
fn start_error_text(err: StartError) -> String {
    match err {
        StartError::Busy(text) | StartError::Refused(text) | StartError::Limit(text) | StartError::Other(text) => text,
    }
}

/// How long a run waits for its review report before moving on without it.
const REVIEW_REPORT_TIMEOUT: Duration = Duration::from_secs(30);

/// The wait for the report; `ORQ_REVIEW_BUILD_TIMEOUT_MS` shortens it for tests.
fn review_report_timeout() -> Duration {
    std::env::var("ORQ_REVIEW_BUILD_TIMEOUT_MS").ok().and_then(|ms| ms.parse().ok()).map(Duration::from_millis).unwrap_or(REVIEW_REPORT_TIMEOUT)
}

/// The commands `gate` runs; none for a gate with nothing to check.
fn steps_of(gate: &orq_orchestrator::Gate) -> Vec<Vec<String>> {
    match gate {
        orq_orchestrator::Gate::Steps { steps, .. } => steps.clone(),
        _ => Vec::new(),
    }
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
    note_review_report(logs, &note);
}

/// Builds, for orq serve's repair, the report a run ended without: from the gate and CI results its events hold, with
/// the review rounds and diff base the run itself would have used. A gate recorded before gates named their commands
/// counts as one passed command.
pub(crate) fn repair_review_report(repair: &orq_remote::projects::ReportRepair) -> Result<PathBuf, String> {
    let repo = repair.repo.as_deref().ok_or("the run's project is not among the projects serve sees")?;
    let state = state_root().ok_or("no state location: set XDG_STATE_HOME or HOME")?;
    let store = RunStore::new(&state);
    let record = store.load(&repair.run_id).map_err(|err| format!("cannot read the run: {err:?}"))?;
    let gate_steps = match (&repair.evidence.gate_passed, &repair.evidence.gate_steps) {
        (false, _) => Vec::new(),
        (true, Some(steps)) => steps.clone(),
        (true, None) => vec![vec!["the run's gate".to_string()]],
    };
    let tests = crate::run_evidence::tests_evidence(&repair.head_sha, &gate_steps, repair.evidence.ci.as_ref());
    let rebased = orq_orchestrator::rebased_onto(&store, &record.run_id).map_err(|err| format!("cannot read what the branch was rebased onto: {err:?}"))?;
    let base = rebased.unwrap_or_else(|| record.base_sha.clone());
    let review = crate::run_evidence::review_from_ledger(&record.worktree, &record.base_sha);
    let turn = crate::run_evidence::report_turn(&record.run_id);
    crate::review::build_for_run(repo, &record.run_id, &record.feature_id, &base, &repair.head_sha, &tests, review.as_ref(), &turn)
}

/// Adds a line to the run's review-report log; a log that cannot be written is let go.
fn note_review_report(logs: &std::path::Path, note: &str) {
    use std::io::Write;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(logs.join("review-report.log"));
    let _ = log.and_then(|mut file| writeln!(file, "{note}"));
}

/// Tells which ledger features each fix session added: the ledger's names are taken before a
/// session starts and compared with them as soon as it ends, before the gate or anything else of
/// the run runs, so a feature added after the session is not taken for one it started. The run
/// records the added ones so it closes them when it ends. A feature of the run the session marked
/// `completed` is held `in_progress` until the run delivers.
/// A ledger that cannot be read is said and records nothing; it never fails the run.
struct FixFeatureWatch<'a> {
    store: &'a RunStore,
    record: &'a RunRecord,
    ledger: PathBuf,
    /// The run's own features.
    features: &'a [String],
    /// The session running and the names in the ledger before it started.
    running: std::cell::RefCell<Option<(&'static str, Vec<(String, orq_features::LedgerStatus)>)>>,
}

impl<'a> FixFeatureWatch<'a> {
    fn new(store: &'a RunStore, record: &'a RunRecord, ledger: PathBuf, features: &'a [String]) -> Self {
        Self { store, record, ledger, features, running: std::cell::RefCell::new(None) }
    }

    /// A fix session of kind `session` is about to start.
    fn before(&self, session: &'static str) {
        self.settle();
        *self.running.borrow_mut() = self.names().map(|names| (session, names));
    }

    /// Record what the session that ran added.
    fn settle(&self) {
        let Some((session, before)) = self.running.borrow_mut().take() else { return };
        let Some(after) = self.names() else { return };
        hold_until_delivery(self.store, self.record, &self.ledger, &newly_completed(self.features, &before, &after));
        let added = started_by_session(&before, after);
        if added.is_empty() {
            return;
        }
        if let Err(err) = orq_orchestrator::record_fix_features_added(self.store, self.record, session, &added) {
            eprintln!("orq: cannot record the features the {session} session started ({}): {err:?}", added.join(","));
        }
    }

    fn names(&self) -> Option<Vec<(String, orq_features::LedgerStatus)>> {
        ledger_statuses(&self.ledger, "a fix session")
    }
}

impl Drop for FixFeatureWatch<'_> {
    fn drop(&mut self) {
        self.settle();
    }
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
    /// Run before each fix session: brings the session's allowed commands up to date with the branch
    /// and returns the session's arguments, whose prompt lists them.
    session_args: &'a dyn Fn() -> Result<Vec<String>, StoreError>,
    /// Run as soon as a fix session ends, before the gate.
    session_ended: &'a dyn Fn(),
    /// The fix session, but for its arguments.
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

    fn fix_session(&mut self, _attempt: u32, failure_log: &Path) -> Result<FixSessionSpec, StoreError> {
        orq_orchestrator::copy_session_files(&self.session.cwd, &[failure_log]).map_err(StoreError::Corrupt)?;
        let args = (self.session_args)()?;
        Ok(FixSessionSpec { args, ..self.session.clone() })
    }

    fn fix_session_ended(&mut self) {
        (self.session_ended)();
    }

    fn retry(&mut self, head_sha: &str) -> Result<(), String> {
        orq_orchestrator::retry_ci(self.store, self.run_id, self.provider, head_sha, self.config)
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

    #[test]
    fn the_run_features_specs_are_their_ledger_descriptions_in_the_runs_order() {
        let dir = orq_test_support::process_temp_dir().join(format!("orq-feature-specs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join(".tdd-features.json");
        std::fs::write(
            &ledger,
            r#"[{"featureName":"A","description":"do a","status":"pending"},{"featureName":"B","status":"pending"},{"featureName":"C","description":"do c","status":"pending"}]"#,
        )
        .unwrap();
        let features = vec!["C".to_string(), "B".to_string(), "A".to_string(), "gone".to_string()];
        assert_eq!(super::feature_specs(&ledger, &features), vec![("C".to_string(), "do c".to_string()), ("A".to_string(), "do a".to_string())]);
        // An unreadable ledger gives no specs rather than failing the run.
        assert!(super::feature_specs(&dir.join("missing.json"), &features).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
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
        let dir = orq_test_support::process_temp_dir().join(format!("orq-has-commits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // An existing directory git cannot read as a worktree: the feature must not be reopened.
        assert!(super::has_commits(&record_in(dir.clone())));
        assert!(!super::has_commits(&record_in(dir.join("gone"))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Distinct (feature, finding) pairs never share a follow-up name: not when cleaning makes two ids
    /// alike, not when a safe id spells out another's name, and not when `-` splits a pair two ways.
    #[test]
    fn distinct_findings_never_share_a_follow_up_name() {
        use super::followup_name;
        let cleaned = followup_name("COM-01", "a/b");
        let spelled = cleaned.trim_start_matches("review-followup-COM-01-");
        assert_ne!(followup_name("COM-01", spelled), cleaned, "a safe id spelling out another's name");
        assert_ne!(followup_name("A-B", "C"), followup_name("A", "B-C"), "the `-` between feature and finding");
        assert_ne!(followup_name("COM-01", "a/b"), followup_name("COM-01", "a-b"));
        assert_eq!(followup_name("COM-01", "a/b"), cleaned, "the same pair always has the same name");
        assert!(followup_name("COM-01", "off-by-one").starts_with("review-followup-COM-01-off-by-one-"));
    }
}
