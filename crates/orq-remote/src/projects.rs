//! Projects the server can show: the git repositories directly inside the scan root
//! (`~/code` by default). Symlinks are never followed, so nothing outside the root is read.

use std::path::{Path, PathBuf};

use orq_core::retention::PrunePlan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
}

/// The scan root: an explicit override (for tests), else `code` inside the home directory.
pub fn default_root(home: Option<&Path>, override_root: Option<&Path>) -> PathBuf {
    match (override_root, home) {
        (Some(root), _) => root.to_path_buf(),
        (None, Some(home)) => home.join("code"),
        (None, None) => PathBuf::from("code"),
    }
}

/// Scans the root now. A symlinked root is allowed on purpose and is resolved once; every project
/// is then checked to still resolve to a direct child of that real root, so a folder swapped for a
/// link to somewhere else is dropped. A missing or unreadable root, or entry, gives no project.
pub fn discover(root: &Path) -> Vec<Project> {
    let Ok(real_root) = std::fs::canonicalize(root) else { return Vec::new() };
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    collect(&real_root, "", GROUP_DEPTH, &mut found);
    found.sort();
    found.into_iter().map(|(name, path)| Project { id: project_id(&name), name, path }).collect()
}

/// What a run's event log said when it was last read, and the size and time it had then.
struct LogSummary {
    len: u64,
    modified: Option<std::time::SystemTime>,
    state: &'static str,
    ending: Option<&'static str>,
    stage: Option<String>,
    started: Option<String>,
    updated: Option<String>,
}

type LogCache = std::collections::HashMap<String, LogSummary>;

/// One listed run: its sort time, id, JSON row (with the state its log gives) and record. `None` when the run belongs to another project
/// or was removed after its record was read; an unreadable event log is an error. A log whose size
/// and time are what they were is not read again (the log only ever grows), so polling the list
/// costs a stat per run, not a replay of its history.
fn run_row(store: &orq_orchestrator::RunStore, state: &Path, cache: &mut LogCache, record: orq_orchestrator::RunRecord, key: &str) -> Result<Option<ListedRun>, String> {
    use orq_orchestrator::StoreError;
    if record.project_id != key {
        return Ok(None);
    }
    let stat = std::fs::metadata(state.join("runs").join(&record.run_id).join("events.jsonl")).ok().map(|meta| (meta.len(), meta.modified().ok()));
    let fresh = stat.is_some_and(|(len, modified)| cache.get(&record.run_id).is_some_and(|known| known.len == len && known.modified == modified && modified.is_some()));
    if !fresh {
        let events = match store.events(&record.run_id) {
            Ok(events) => events,
            Err(StoreError::NotFound(_)) => {
                cache.remove(&record.run_id);
                return Ok(None);
            }
            Err(error) => return Err(format!("run {} is unreadable: {error:?}", record.run_id)),
        };
        let summary = orq_orchestrator::summarize_run(&events);
        let learned = LogSummary {
            len: stat.map_or(0, |(len, _)| len),
            modified: stat.and_then(|(_, modified)| modified),
            state: summary.phase.as_str(),
            ending: delivered_ending(&events, summary.phase),
            stage: summary.stage,
            started: events.first().map(|event| event.occurred_at.clone()),
            updated: events.last().map(|event| event.occurred_at.clone()),
        };
        if stat.is_some() {
            cache.insert(record.run_id.clone(), learned);
        } else {
            cache.remove(&record.run_id);
            return Ok(Some(row_of(record, &learned)));
        }
    }
    let log = &cache[&record.run_id];
    Ok(Some(row_of(record, log)))
}

/// How a completed run that delivered its work without a review ended: `pushed` or `merged`
/// (into its base, locally). `None` for any other run, a run that ended in a review among them.
fn delivered_ending(events: &[orq_orchestrator::Event], phase: orq_orchestrator::RunPhase) -> Option<&'static str> {
    if phase != orq_orchestrator::RunPhase::Completed {
        return None;
    }
    let completed = events.iter().rev().find(|event| event.kind == "run.completed")?;
    match completed.payload.get("ending").and_then(|ending| ending.as_str()) {
        Some("pushed") => Some("pushed"),
        Some("merged") => Some("merged"),
        _ => None,
    }
}

type ListedRun = (Option<u64>, String, serde_json::Value, orq_orchestrator::RunRecord);

fn row_of(record: orq_orchestrator::RunRecord, log: &LogSummary) -> ListedRun {
    let sort_key = log.updated.as_deref().and_then(orq_orchestrator::parse_rfc3339);
    let row = serde_json::json!({"run_id": record.run_id, "feature": record.feature_id, "target_branch": record.target_branch, "state": log.state, "stage": log.stage, "started_at": log.started, "updated_at": log.updated, "delivery": record.delivery.as_str(), "ending": log.ending});
    (sort_key, record.run_id.clone(), row, record)
}

/// Whether a failed or review-waiting run's work is on its target, remembered for the commits its branch
/// and target were at when git was asked, so polling the list asks git again only after one of them moved.
#[derive(Default)]
struct MergedCache(std::collections::HashMap<String, (String, String, bool)>);

impl MergedCache {
    fn merged(&mut self, record: &orq_orchestrator::RunRecord, tips: &std::collections::HashMap<String, String>, check: impl FnOnce() -> bool) -> bool {
        let (Some(branch), Some(target)) = (tips.get(&record.branch), tips.get(&record.target_branch)) else {
            self.0.remove(&record.run_id);
            return false;
        };
        if let Some((known_branch, known_target, answer)) = self.0.get(&record.run_id) {
            if known_branch == branch && known_target == target {
                return *answer;
            }
        }
        let answer = check();
        self.0.insert(record.run_id.clone(), (branch.clone(), target.clone(), answer));
        answer
    }
}

/// The commit each local branch of `repo` is at, by branch name, read with one git call; empty when git cannot say.
fn branch_tips(repo: &Path) -> std::collections::HashMap<String, String> {
    let Ok(out) = std::process::Command::new("git").arg("-C").arg(repo).args(["for-each-ref", "--format=%(objectname) %(refname)", "refs/heads"]).output() else { return Default::default() };
    if !out.status.success() {
        return Default::default();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(sha, name)| Some((name.strip_prefix("refs/heads/")?.to_string(), sha.to_string())))
        .collect()
}

/// The state the popup shows: a failed or review-waiting run whose work is on its target branch in `repo`
/// is merged. It is asked of git each time, as a merge leaves the run's log as it was.
fn shown_state(repo: &Path, record: &orq_orchestrator::RunRecord, state: &'static str) -> &'static str {
    if matches!(state, "failed" | "needs_review") && orq_orchestrator::work_is_on_target(repo, record) {
        "merged"
    } else {
        state
    }
}

/// How many events one timeline answer holds; `next_cursor` continues after the last of them.
const TIMELINE_PAGE: usize = 200;
/// Lines in one page of a log, and the most of a log file that is read (and so shown).
const LOG_PAGE: usize = 500;
const LOG_READ_CAP: u64 = 8 * 1024 * 1024;

/// How many plain folders may be nested above a repository: `~/code/pitstop/eGiftit/egiftit-backend`
/// has two, and nothing deeper is looked at.
const GROUP_DEPTH: usize = 2;

/// Adds the repositories inside `dir`; a plain folder is looked into while `groups` allows it.
fn collect(dir: &Path, prefix: &str, groups: usize, found: &mut Vec<(String, PathBuf)>) {
    for name in child_folders(dir) {
        let path = dir.join(&name);
        let shown = format!("{prefix}{name}");
        if is_repository(&path) {
            found.push((shown, path));
        } else if groups > 0 {
            collect(&path, &format!("{shown}/"), groups - 1, found);
        }
    }
}

/// The names of the real directories directly inside `dir`: a link, a file or an unreadable
/// entry is left out, and so is a folder that no longer resolves to itself.
fn child_folders(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let path = dir.join(&name);
            (std::fs::canonicalize(&path).ok()? == path).then_some(name)
        })
        .collect()
}

/// A `.git` directory, or a `.git` file as linked worktrees have, that is not itself a symlink.
fn is_repository(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).map(|meta| !meta.file_type().is_symlink()).unwrap_or(false)
}

fn slug(name: &str) -> String {
    let cleaned: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let parts: Vec<&str> = cleaned.split('-').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() { "project".to_string() } else { parts.join("-") }
}

/// The id comes from the folder name alone, so it never changes when other folders come or go.
/// A name that is already a plain lowercase slug is its own id. Any other name gets its readable
/// slug plus `_` and the hex of the whole name; a plain slug never contains `_`, so the two
/// kinds cannot meet and two different names can never share an id.
fn project_id(name: &str) -> String {
    let readable = slug(name);
    if readable == name && name.len() <= MAX_ID_LEN {
        return readable;
    }
    let hex: String = name.bytes().map(|byte| format!("{byte:02x}")).collect();
    if readable.len() + 1 + hex.len() <= MAX_ID_LEN {
        return format!("{readable}_{hex}");
    }
    // A long name would overflow what the page accepts: keep a short readable start and a fixed
    // 128-bit digest instead. The `__` never occurs in the other two kinds, so they cannot meet.
    let start: String = readable.chars().take(40).collect();
    format!("{}__{:016x}{:016x}", start.trim_end_matches('-'), fnv1a(name, 0xcbf2_9ce4_8422_2325), fnv1a(name, 0x8422_2325_cbf2_9ce4))
}

/// The longest id the page accepts is 128 characters; stay well under it.
const MAX_ID_LEN: usize = 100;

/// FNV-1a: a stable hash that never changes between releases, unlike the standard hasher.
fn fnv1a(text: &str, seed: u64) -> u64 {
    text.bytes().fold(seed, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// The projects provider the server hands to the handler: it rescans the scan root on each
/// request and reads counts from the same storage the CLI writes. A project it cannot read is
/// listed as unavailable, never dropped.
pub struct StoreProjects {
    roots: orq_core::storage::StorageRoots,
    scan_root: std::path::PathBuf,
    launcher: Option<std::sync::Arc<dyn RunLauncher + Send + Sync>>,
    agent_options: Option<AgentOptions>,
    storage: Option<std::sync::Arc<dyn StorageCleanup + Send + Sync>>,
    run_logs: std::sync::Mutex<LogCache>,
    merged: std::sync::Mutex<MergedCache>,
    log_snapshot: std::sync::Mutex<Option<LogSnapshot>>,
    accept_tests: AcceptTests,
    accepting: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

/// Runs the tests of a run's worktree when it is accepted; the error is what failed.
pub type AcceptTests = std::sync::Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// The redacted text of the log file last paged through, so the next page of an unchanged file is
/// cut from it instead of reading and redacting the file again.
struct LogSnapshot {
    path: std::path::PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    truncated: bool,
    text: std::sync::Arc<str>,
}

impl StoreProjects {
    pub fn new(roots: orq_core::storage::StorageRoots, scan_root: std::path::PathBuf) -> Self {
        StoreProjects {
            roots,
            scan_root,
            launcher: None,
            agent_options: None,
            storage: None,
            run_logs: Default::default(),
            merged: Default::default(),
            log_snapshot: Default::default(),
            accept_tests: std::sync::Arc::new(|worktree: &Path| orq_orchestrator::run_worktree_tests(worktree, None)),
            accepting: Default::default(),
        }
    }

    /// Accepting a run runs `tests` in its worktree instead of what its stacks call for.
    pub fn with_accept_tests(mut self, tests: AcceptTests) -> Self {
        self.accept_tests = tests;
        self
    }

    /// Lets this provider start runs through `launcher`; without one it says it cannot.
    pub fn with_launcher(mut self, launcher: std::sync::Arc<dyn RunLauncher + Send + Sync>) -> Self {
        self.launcher = Some(launcher);
        self
    }

    /// Project `id`'s git project and the main checkout its own settings are kept under, so a linked
    /// worktree shares them; `None` when there is no such project.
    fn settings_key(&self, id: &str) -> Result<Option<(orq_git::Project, std::path::PathBuf)>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let root = orq_git::main_checkout_root(&git).unwrap_or_else(|| git.root.clone());
        Ok(Some((git, root)))
    }

    /// Project `id`'s git and its own settings in this machine's orq.db, keyed by its main checkout.
    /// orq.db is only read; without it the project has no record (revision 0). None for an unknown project.
    fn stored_settings_of(&self, id: &str) -> Result<Option<(orq_git::Project, orq_core::project_settings::StoredProjectSettings)>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        let database = self.roots.database();
        let stored = if database.exists() {
            orq_core::store::Store::read_project_settings(&database, &root).map_err(|error| error.to_string())?
        } else {
            orq_core::project_settings::StoredProjectSettings { settings: Default::default(), revision: 0 }
        };
        Ok(Some((git, stored)))
    }

    /// Project `git`'s `stored` settings as the web reads them: each role's own choice and what a
    /// run would use if it named none, with where that comes from, all redacted. It cannot fail: a
    /// fallback that cannot be worked out is null.
    fn settings_answer(&self, git: &orq_git::Project, stored: &orq_core::project_settings::StoredProjectSettings) -> serde_json::Value {
        use orq_features::AgentRole;
        // A fallback that cannot be worked out (a broken config a run may never read) is unknown, so
        // the project's own settings can still be seen and changed. Each role needs only its own
        // sources: the implementer never reads .tdd-config.json, and the reviewer's reviewers.default
        // comes before every profile.
        let profiles = self.model_profiles(&git.root).ok();
        let reviewers = orq_features::load_reviewer_config(&git.root.join(".tdd-config.json")).ok();
        let fallback_of = |role| {
            let (profiles, reviewers) = match role {
                AgentRole::Implementer => (profiles.clone()?, orq_features::TddReviewerConfig::default()),
                AgentRole::Reviewer => {
                    let reviewers = reviewers.clone()?;
                    match profiles.clone() {
                        Some(profiles) => (profiles, reviewers),
                        None if reviewers.default_reviewer.is_some() => (orq_features::ModelProfiles::default(), reviewers),
                        None => return None,
                    }
                }
            };
            orq_features::role_fallback(&profiles, &reviewers, role).ok()
        };
        let name = |text: &str| orq_core::redact::redact(text);
        let effort = |effort: Option<&String>| effort.map(|effort| name(effort));
        let role = |chosen: &Option<orq_core::project_settings::ProjectAgent>, role| {
            let fallback = fallback_of(role);
            let fallback = fallback.map(|(model, source)| serde_json::json!({ "harness": name(&model.harness), "model": name(&model.model), "effort": effort(model.thinking.as_ref()), "source": source.as_str() }));
            let chosen = chosen.as_ref().map(|agent| serde_json::json!({ "harness": name(&agent.harness), "model": name(&agent.model), "effort": effort(agent.effort.as_ref()) }));
            serde_json::json!({ "project": chosen, "fallback": fallback })
        };
        serde_json::json!({
            "implementer": role(&stored.settings.implementer, AgentRole::Implementer),
            "reviewer": role(&stored.settings.reviewer, AgentRole::Reviewer),
            "delivery": stored.settings.delivery.as_str(),
            "revision": stored.revision,
        })
    }

    /// Lets the web choose the implementer's and reviewer's harness and model from what `source`
    /// finds: each harness with the models it allows. It is asked once, on first use.
    pub fn with_agent_options(mut self, source: std::sync::Arc<AgentOptionsSource>) -> Self {
        self.agent_options = Some(AgentOptions { source, found: Default::default() });
        self
    }

    /// Lets the web clean up storage through `storage`, as `orq storage prune` does; without one it cannot.
    pub fn with_storage(mut self, storage: std::sync::Arc<dyn StorageCleanup + Send + Sync>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Why `run` cannot be acted on while this server is accepting it: its tests are running in its worktree,
    /// and the run must not be resumed, stopped, paused, switched or discarded under them.
    fn being_accepted(&self, run: &str) -> Option<String> {
        let accepting = self.accepting.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        accepting.contains(run).then(|| format!("{run} is being accepted; try again when its tests have finished"))
    }

    /// The popup's Accept: the run is checked now, as the popup checks it, and its tests then run in the
    /// background, since they can take as long as the project's whole suite. A run already being accepted is
    /// refused; one that passes ends review ready, and one that fails stays as it was, its reason logged here.
    fn accept(&self, store: orq_orchestrator::RunStore, run: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let mut accepting = self.accepting.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if accepting.contains(run) {
            return Ok(RunControlOutcome::Refused(format!("{run} is already being accepted")));
        }
        // A failed git command is for this machine's operator; any other answer is why the run cannot be accepted.
        match orq_orchestrator::accept_check(&store, run) {
            Ok(()) => {}
            Err(why) if why.starts_with("git ") => return Err(why),
            Err(why) => return Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why))),
        }
        accepting.insert(run.to_string());
        drop(accepting);
        let (tests, in_flight, run_id) = (self.accept_tests.clone(), self.accepting.clone(), run.to_string());
        std::thread::spawn(move || {
            if let Err(why) = orq_orchestrator::accept_run(&store, &run_id, &*tests) {
                eprintln!("orq serve: accepting {run_id} failed: {}", orq_core::redact::redact(&why));
            }
            in_flight.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&run_id);
        });
        Ok(RunControlOutcome::Done(run.to_string()))
    }

    /// The model profiles of this machine's config and the `.orq.toml` of the project at `root`.
    /// Starts a run of `feature` in project `id`, with the chosen profiles or the launch defaults. The CLI's
    /// eligibility rule comes first; a chosen profile must be one the launch picker lists.
    fn launch(&self, id: &str, feature: &str, profiles: Option<&orq_core::remote_inbox::LaunchProfiles>) -> Result<orq_core::remote_inbox::RunStart, String> {
        use orq_core::remote_inbox::RunStart;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(RunStart::Unknown) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let ledger = read_ledger(&orq_features::ledger_path(&git))?;
        match orq_features::check_run_eligibility(&ledger, feature) {
            Ok(_) => {}
            Err(orq_features::Ineligible::NotFound) => return Ok(RunStart::Unknown),
            Err(why) => return Ok(RunStart::Refused(orq_core::redact::redact(&why.to_string()))),
        }
        if let Some(chosen) = profiles {
            let listed = self.model_profiles(&git.root)?;
            for name in [&chosen.implementer, &chosen.reviewer] {
                if !listed.profiles().iter().any(|profile| &profile.name == name) {
                    return Ok(RunStart::Refused(orq_core::redact::redact(&format!("the profile {name} is not defined in the model profiles"))));
                }
            }
        }
        let Some(launcher) = &self.launcher else { return Ok(RunStart::Refused("this server cannot start runs".to_string())) };
        let started = match profiles {
            Some(chosen) => launcher.start_with(&project.path, feature, chosen),
            None => launcher.start(&project.path, feature),
        };
        match started {
            Ok(run) => Ok(match launcher.start_warning(&project.path) {
                Some(warning) => RunStart::StartedWithWarning { run, warning: orq_core::redact::redact(&warning) },
                None => RunStart::Started(run),
            }),
            Err(LaunchError::Refused(why)) => Ok(RunStart::Refused(orq_core::redact::redact(&why))),
            Err(LaunchError::Failed(why)) => Err(why),
        }
    }

    fn model_profiles(&self, root: &Path) -> Result<orq_features::ModelProfiles, String> {
        orq_features::load_model_profiles(&self.roots.config_file(), &root.join(".orq.toml")).map_err(|error| error.to_string())
    }

    /// Project `id` and the run store, when `run` is a run of that project; only such a run can be
    /// acted on through it. `None` when either is unknown.
    /// Where the feature ledger of project `id` is; `None` when there is no such project.
    fn ledger_of(&self, id: &str) -> Result<Option<PathBuf>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        Ok(Some(orq_features::ledger_path(&git)))
    }

    fn own_run(&self, id: &str, run: &str) -> Result<Option<(Project, orq_orchestrator::RunStore)>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        match store.load(run) {
            Ok(record) if record.project_id == key => Ok(Some((project, store))),
            Ok(_) | Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => Ok(None),
            Err(error) => Err(format!("run {run} is unreadable: {error:?}")),
        }
    }
}

/// Finds the harnesses on this machine and the models each allows.
pub type AgentOptionsSource = dyn Fn() -> Vec<(String, Vec<String>)> + Send + Sync;

/// The harness options and what they were found to be, once asked.
struct AgentOptions {
    source: std::sync::Arc<AgentOptionsSource>,
    found: std::sync::OnceLock<serde_json::Value>,
}

/// Starts a run the way the CLI does. The server never starts one by itself: whoever builds it supplies this.
pub trait RunLauncher {
    /// Starts a run of `feature` in the project at `repo` and returns its id.
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError>;

    /// Starts a run as `start` does, with the chosen implementer and reviewer instead of the launch defaults.
    fn start_with(&self, _repo: &Path, _feature: &str, _profiles: &orq_core::remote_inbox::LaunchProfiles) -> Result<String, LaunchError> {
        Err(LaunchError::Refused("this server cannot choose the profiles".to_string()))
    }

    /// What whoever started a run in the project at `repo` should know about it, such as commits only the
    /// local target branch has, which the run will not see; `None` when there is nothing to say.
    fn start_warning(&self, _repo: &Path) -> Option<String> {
        None
    }

    /// Continues run `run` of the project at `repo` and returns the id of the run that carries on.
    fn resume(&self, _repo: &Path, _run: &str) -> Result<String, LaunchError> {
        Err(LaunchError::Refused("this server cannot resume runs".to_string()))
    }
}

/// Cleans up storage the way the CLI's prune does. The server never prunes by itself: whoever builds it supplies this.
pub trait StorageCleanup {
    /// What a prune would remove now: the lines a person reads, and the plan itself.
    fn preview(&self) -> Result<(Vec<String>, PrunePlan), String>;

    /// Removes what `plan` names and says, in lines a person reads, what was freed.
    fn prune(&self, plan: &PrunePlan) -> Result<Vec<String>, CleanupError>;
}

/// Why a cleanup did not run.
#[derive(Debug)]
pub enum CleanupError {
    /// It may not run now, such as while another prune runs; the reason is meant to be read by the person who asked.
    Refused(String),
    /// Anything else; the reason is for this machine's operator and is never sent to the web.
    Failed(String),
}

/// The hash of a cleanup set: the files (run, path, size) and the runs whose metadata would go, in a stable
/// order, so a prune can tell whether the set is still the one previewed.
fn cleanup_hash(plan: &PrunePlan) -> String {
    let mut files: Vec<String> = plan.candidates.iter().map(|c| format!("file\0{}\0{}\0{}\n", c.artifact.run_id, c.artifact.rel.display(), c.artifact.bytes)).collect();
    files.sort();
    let mut metadata: Vec<String> = plan.metadata.iter().map(|run| format!("metadata\0{run}\n")).collect();
    metadata.sort();
    files.extend(metadata);
    ledger_version(files.concat().as_bytes())
}

/// Why a launch did not start a run.
#[derive(Debug)]
pub enum LaunchError {
    /// The run's own checks said no; the reason is meant to be read by the person who asked.
    Refused(String),
    /// Anything else; the reason is for this machine's operator and is never sent to the web.
    Failed(String),
}

/// Failures older than this are not counted as needing attention; the API states it with the count.
pub const FAILED_WINDOW_HOURS: u64 = 24;

#[derive(Clone, Copy, Default)]
struct RunCounts {
    active: usize,
    failed: usize,
}

struct Counts {
    features: usize,
    active_runs: usize,
    failed_runs: usize,
    reviews_waiting: usize,
}

impl orq_core::remote_inbox::ProjectsProvider for StoreProjects {
    fn review_scope(&self, id: &str) -> Result<Option<orq_core::remote_inbox::ReviewScope>, String> {
        // Only a project found by the scan counts; the id is matched, never used as a path.
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        Ok(Some(orq_core::remote_inbox::ReviewScope { review_key: orq_git::project_id(&git), name: project.name }))
    }

    fn settings(&self) -> Result<Option<serde_json::Value>, String> {
        orq_core::remote_inbox::public_settings(&self.roots.config_file()).map(Some)
    }

    fn setting_options(&self) -> Result<Option<serde_json::Value>, String> {
        let Some(options) = &self.agent_options else { return Ok(None) };
        if let Some(found) = options.found.get() {
            return Ok(Some(found.clone()));
        }
        let listed = (options.source)();
        // A harness with no models may only have failed to answer this time, so such a list is not kept.
        let complete = listed.iter().all(|(_, models)| !models.is_empty());
        // A harness whose levels are not known lists none, so a save leaves its effort alone.
        let option = |(harness, models): (String, Vec<String>)| {
            let levels = orq_agents::effort_levels(&harness);
            let mut option = serde_json::json!({"harness": harness, "models": models});
            if !levels.is_empty() {
                option["efforts"] = serde_json::json!(levels);
            }
            option
        };
        let found: serde_json::Value = listed.into_iter().map(option).collect();
        if complete {
            let _ = options.found.set(found.clone());
        }
        Ok(Some(found))
    }

    fn start_run(&self, id: &str, feature: &str) -> Result<orq_core::remote_inbox::RunStart, String> {
        self.launch(id, feature, None)
    }

    fn start_run_with(&self, id: &str, feature: &str, profiles: &orq_core::remote_inbox::LaunchProfiles) -> Result<orq_core::remote_inbox::RunStart, String> {
        self.launch(id, feature, Some(profiles))
    }

    fn control_run(&self, id: &str, run: &str, control: orq_core::remote_inbox::RunControl) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::{RunControl, RunControlOutcome};
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        if let (false, Some(busy)) = (matches!(control, RunControl::Accept), self.being_accepted(run)) {
            return Ok(RunControlOutcome::Refused(busy));
        }
        match control {
            // The orchestrator words its own refusals "<run> is ..."; any other failure is for this machine's operator.
            RunControl::Pause => match orq_orchestrator::request_pause(&store, run) {
                Ok(()) => Ok(RunControlOutcome::Done(run.to_string())),
                Err(why) if why.starts_with(&format!("{run} is ")) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why))),
                Err(why) => Err(why),
            },
            // The popup's Stop: the owner process is ended only when its recorded start time proves it is the run's.
            RunControl::Stop => match orq_orchestrator::stop_run(&store, run, &orq_orchestrator::owner_state, &orq_orchestrator::terminate_process, &std::thread::sleep) {
                Ok(()) => Ok(RunControlOutcome::Done(run.to_string())),
                Err(why) if why.starts_with(&format!("{run} is ")) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why))),
                Err(why) => Err(why),
            },
            RunControl::Resume => {
                let Some(launcher) = &self.launcher else { return Ok(RunControlOutcome::Refused("this server cannot resume runs".to_string())) };
                match launcher.resume(&project.path, run) {
                    Ok(carried_on) => Ok(RunControlOutcome::Done(carried_on)),
                    Err(LaunchError::Refused(why)) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&crate::discard::paths_within_project(&why, &project.path)))),
                    Err(LaunchError::Failed(why)) => Err(why),
                }
            }
            RunControl::Accept => self.accept(store, run),
        }
    }

    fn resume_check(&self, id: &str, run: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        // The orchestrator's "cannot ..." is a failure on this machine; any other answer is why the run cannot resume.
        match orq_orchestrator::resume_check(&store, &project.path, run) {
            Ok(next) => Ok(RunControlOutcome::Done(orq_core::redact::redact(&next))),
            Err(why) if why.starts_with("cannot ") => Err(why),
            Err(why) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&crate::discard::paths_within_project(&why, &project.path)))),
        }
    }

    /// The popup's launch picker: the profiles of this machine's config and the project's `.orq.toml`, and the
    /// implementer and reviewer it preselects. A role whose default cannot be worked out has none.
    fn launch_profiles(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let root = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?.root;
        let user_config = self.roots.config_file();
        let profiles = self.model_profiles(&root)?;
        let reviewers = orq_features::load_reviewer_config(&root.join(".tdd-config.json")).unwrap_or_default();
        let settings = orq_features::Settings::load(&user_config).ok();
        let agent = |role| settings.as_ref().and_then(|settings| settings.agent(role).ok().flatten());
        let (machine_implementer, machine_reviewer) = orq_features::default_role_profiles(
            &profiles,
            &reviewers,
            agent(orq_features::AgentRole::Implementer).as_ref(),
            agent(orq_features::AgentRole::Reviewer).as_ref(),
        );
        // The project's own agent for a role is what a run naming no profile uses, so it comes first:
        // the profile that runs as it does, with its harness, model and effort as the thinking level,
        // or none, and then only "Project default" starts with it.
        let own = self.stored_settings_of(id)?.map(|(_, stored)| stored.settings).unwrap_or_default();
        let mut project_default = false;
        let mut pick = |own: Option<orq_core::project_settings::ProjectAgent>, machine: Option<String>| match own {
            None => machine,
            Some(agent) => {
                let found = profiles.profiles().iter().find(|p| p.harness == agent.harness && p.model == agent.model && p.thinking == agent.effort).map(|p| p.name.clone());
                project_default |= found.is_none();
                found
            }
        };
        let implementer = pick(own.implementer, machine_implementer);
        let reviewer = pick(own.reviewer, machine_reviewer);
        let names: Vec<&str> = profiles.profiles().iter().map(|profile| profile.name.as_str()).collect();
        Ok(Some(serde_json::json!({ "profiles": names, "implementer": implementer, "reviewer": reviewer, "project_default": project_default })))
    }

    /// The project's own settings in this machine's orq.db, keyed by its main checkout, and for each
    /// role what a run would use if the project named none, with where it comes from. orq.db is only
    /// read; without it the project has no record (revision 0).
    fn project_settings(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let Some((git, stored)) = self.stored_settings_of(id)? else { return Ok(None) };
        Ok(Some(self.settings_answer(&git, &stored)))
    }

    /// Saves the project's own settings in this machine's orq.db, keyed by its main checkout like a
    /// read, if its record is still at `revision`; then gives the saved settings as a read gives them.
    fn save_project_settings(&self, id: &str, revision: u64, settings: &orq_core::project_settings::ProjectSettings) -> Result<orq_core::remote_inbox::ProjectSettingsSave, String> {
        use orq_core::project_settings::SaveOutcome;
        use orq_core::remote_inbox::ProjectSettingsSave;
        let Some((git, root)) = self.settings_key(id)? else { return Ok(ProjectSettingsSave::Unknown) };
        let mut store = orq_core::store::Store::open(&self.roots.database()).map_err(|error| error.to_string())?;
        match store.save_project_settings(&root, revision, settings).map_err(|error| error.to_string())? {
            SaveOutcome::Conflict => Ok(ProjectSettingsSave::Conflict),
            // Once committed, the answer is built from what was saved and the project already found,
            // so nothing read afterwards can report a save that happened as refused.
            SaveOutcome::Saved { revision } => {
                let saved = orq_core::project_settings::StoredProjectSettings { settings: settings.clone(), revision };
                Ok(ProjectSettingsSave::Saved(self.settings_answer(&git, &saved)))
            }
        }
    }

    /// The popup's editor fields of one feature, and the version of the ledger they were read from.
    fn feature_fields(&self, id: &str, feature: &str) -> Result<Option<serde_json::Value>, String> {
        use orq_core::redact::redact;
        let Some(path) = self.ledger_of(id)? else { return Ok(None) };
        let Some(bytes) = read_ledger_bytes(&path)? else { return Ok(None) };
        let version = ledger_version(&bytes);
        let Some(entry) = parse_ledger_bytes(bytes)?.into_iter().find(|entry| entry.feature_name == feature) else { return Ok(None) };
        let fields = serde_json::json!({
            "name": redact(&entry.feature_name),
            "description": entry.description.as_deref().map(redact),
            "depends_on": entry.depends_on.iter().map(|name| redact(name)).collect::<Vec<_>>(),
            "parallelizable": entry.parallelizable,
            "test_file": entry.test_file.as_deref().map(redact),
            "target_files": entry.target_files.iter().map(|path| redact(path)).collect::<Vec<_>>(),
        });
        Ok(Some(serde_json::json!({ "fields": fields, "version": version })))
    }

    /// The popup's Promote: draft to pending under the ledger's lock. A refusal is worded as the CLI's
    /// `orq feature promote` words it; a ledger that cannot be read or written is this machine's failure.
    fn promote_feature(&self, id: &str, feature: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        use orq_features::{LedgerError, PromoteError};
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(RunControlOutcome::Unknown) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let refused = |why: String| Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why)));
        match orq_features::promote_feature(&orq_features::ledger_path(&git), feature) {
            Ok(_) => Ok(RunControlOutcome::Done(feature.to_string())),
            Err(PromoteError::UnknownFeature(_)) | Err(PromoteError::Ledger(LedgerError::Missing { .. })) => Ok(RunControlOutcome::Unknown),
            Err(PromoteError::UnrecognisedStatus(text)) => refused(format!("feature {feature} has unrecognised status {text:?} and is left alone")),
            Err(PromoteError::IllegalTransition(t)) => refused(format!("feature {feature}: {:?} to {:?} is not allowed", t.from, t.to)),
            Err(PromoteError::Conflict) => refused("ledger changed while it was being edited; nothing was written, try again".to_string()),
            Err(PromoteError::Locked) => refused("ledger is locked by another writer; nothing was written, try again".to_string()),
            Err(error) => Err(format!("{error:?}")),
        }
    }

    /// The popup's Add and Edit for the web's editor, saved against the ledger exactly as the editor read it:
    /// a version that no longer matches, or a file changed before the write, is a conflict and nothing is written.
    fn save_feature(&self, id: &str, feature: Option<&str>, fields: &orq_core::remote_inbox::FeatureInput, version: Option<&str>) -> Result<orq_core::remote_inbox::FeatureSave, String> {
        use orq_core::remote_inbox::FeatureSave;
        use orq_features::{AppendError, EditFieldsError, SaveError};
        let Some(path) = self.ledger_of(id)? else { return Ok(FeatureSave::Unknown) };
        let changed = || Ok(FeatureSave::Conflict("The ledger changed since you opened it; nothing was written".to_string()));
        let refused = |why: String| Ok(FeatureSave::Invalid(orq_core::redact::redact(&why)));
        let loaded = match (read_ledger_bytes(&path)?, version) {
            (None, _) if feature.is_some() => return Ok(FeatureSave::Unknown),
            (None, Some(_)) => return changed(),
            (None, None) => orq_features::LoadedLedger::absent(&path),
            (Some(bytes), Some(version)) if ledger_version(&bytes) != version => return changed(),
            (Some(bytes), _) => orq_features::LoadedLedger::from_bytes(&path, bytes).map_err(|error| format!("{error:?}"))?,
        };
        let save_error = |error: SaveError| match error {
            SaveError::Conflict => changed(),
            SaveError::Locked => Ok(FeatureSave::Conflict("the ledger is locked by another writer; nothing was written, try again".to_string())),
            SaveError::Io(error) => Err(format!("{error}")),
        };
        let Some(feature) = feature else {
            let name = fields.name.clone().unwrap_or_default();
            if !plain_feature_name(&name) {
                return refused("a feature name may use only letters, digits, '-', '_' and '.', and may not start with '.'".to_string());
            }
            let draft = orq_features::FeatureDraft {
                feature_name: name,
                description: fields.description.clone(),
                depends_on: fields.depends_on.clone().filter(|names| !names.is_empty()),
                parallelizable: fields.parallelizable,
                test_file: fields.test_file.clone(),
                target_files: fields.target_files.clone().filter(|paths| !paths.is_empty()),
            };
            return match loaded.append(&draft) {
                Ok(_) => Ok(FeatureSave::Saved),
                Err(AppendError::Invalid(why)) => refused(why.to_string()),
                Err(AppendError::Save(error)) => save_error(error),
            };
        };
        let Some(current) = loaded.entries().iter().find(|entry| entry.feature_name == feature) else { return Ok(FeatureSave::Unknown) };
        // As the popup's Edit: only what differs is sent, a field left out stays as it is, and one sent empty is cleared.
        // The editor was shown redacted values, so one sent back as shown keeps the original it stands for.
        let text = |new: &Option<String>, old: &Option<String>| new.as_ref().map(|value| unredacted(value, old.iter()));
        let list = |new: &Option<Vec<String>>, old: &[String]| new.as_ref().map(|values| values.iter().map(|value| unredacted(value, old.iter())).collect::<Vec<_>>());
        let differs = |new: Option<String>, old: &Option<String>| new.filter(|value| Some(value) != old.as_ref());
        let edits = orq_features::FieldEdits {
            description: differs(text(&fields.description, &current.description), &current.description),
            depends_on: list(&fields.depends_on, &current.depends_on).filter(|names| *names != current.depends_on),
            parallelizable: fields.parallelizable.filter(|value| Some(*value) != current.parallelizable),
            test_file: differs(text(&fields.test_file, &current.test_file), &current.test_file),
            target_files: list(&fields.target_files, &current.target_files).filter(|paths| *paths != current.target_files),
        };
        match loaded.edit_fields(feature, &edits) {
            Ok(_) => Ok(FeatureSave::Saved),
            Err(EditFieldsError::UnknownFeature) => Ok(FeatureSave::Unknown),
            Err(EditFieldsError::Invalid(why)) => refused(why.to_string()),
            Err(EditFieldsError::Save(error)) => save_error(error),
            Err(EditFieldsError::Malformed) => Err("the ledger could not be edited safely".to_string()),
        }
    }

    fn switch_model(&self, id: &str, run: &str, role: &str, profile: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        if let Some(busy) = self.being_accepted(run) {
            return Ok(RunControlOutcome::Refused(busy));
        }
        let root = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?.root;
        // Only a profile the launch picker offers, as the popup's Switch model allows.
        if self.model_profiles(&root)?.get(profile).is_none() {
            return Ok(RunControlOutcome::Refused(orq_core::redact::redact(&format!("the profile {profile} is not defined in the model profiles"))));
        }
        match orq_orchestrator::request_model_switch(&store, run, role, profile) {
            Ok(()) => Ok(RunControlOutcome::Done(run.to_string())),
            Err(why) if why.starts_with(&format!("{run} is ")) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why))),
            Err(why) => Err(why),
        }
    }

    fn discard_warning(&self, id: &str, run: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let Some((project, _)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        Ok(RunControlOutcome::Done(orq_core::redact::redact(&crate::discard::discard_warning(&self.roots.state, run, &project.path))))
    }

    fn storage_preview(&self) -> Result<Option<serde_json::Value>, String> {
        let Some(storage) = &self.storage else { return Ok(None) };
        let (lines, plan) = storage.preview()?;
        let runs: std::collections::BTreeSet<&str> = plan.candidates.iter().map(|c| c.artifact.run_id.as_str()).collect();
        let bytes = plan.total_bytes();
        Ok(Some(serde_json::json!({
            "lines": lines.iter().map(|line| orq_core::redact::redact(line)).collect::<Vec<_>>(),
            "files": plan.candidates.len(),
            "bytes": bytes,
            "size": orq_core::retention::human_bytes(bytes),
            "runs": runs.len(),
            "metadata": plan.metadata.len(),
            "hash": cleanup_hash(&plan),
        })))
    }

    fn storage_prune(&self, hash: &str) -> Result<Option<orq_core::remote_inbox::StoragePrune>, String> {
        use orq_core::remote_inbox::StoragePrune;
        let Some(storage) = &self.storage else { return Ok(None) };
        // Planned again now, so only the set the person saw is removed.
        let (_, plan) = storage.preview()?;
        if cleanup_hash(&plan) != hash {
            return Ok(Some(StoragePrune::Changed));
        }
        match storage.prune(&plan) {
            Ok(lines) => Ok(Some(StoragePrune::Pruned(lines.iter().map(|line| orq_core::redact::redact(line)).collect()))),
            Err(CleanupError::Refused(why)) => Ok(Some(StoragePrune::Refused(orq_core::redact::redact(&why)))),
            Err(CleanupError::Failed(why)) => {
                eprintln!("orq serve: storage cleanup failed: {}", orq_core::redact::redact(&why));
                Err(why)
            }
        }
    }

    fn discard_run(&self, id: &str, run: &str) -> Result<orq_core::remote_inbox::RunDiscard, String> {
        use orq_core::remote_inbox::RunDiscard;
        let Some((project, _)) = self.own_run(id, run)? else { return Ok(RunDiscard::Unknown) };
        if let Some(busy) = self.being_accepted(run) {
            return Ok(RunDiscard::Refused(busy));
        }
        // The orchestrator words why a run cannot go as "<run> ...", "... refusing ..." or "... is not the
        // repository of <run>"; any other failure is for this machine's operator.
        match crate::discard::discard_and_reopen(&self.roots.state, run, &project.path) {
            Ok(done) => Ok(RunDiscard::Discarded { note: orq_core::redact::redact(&done.web_note), cleared: done.merged }),
            Err(why) if why.starts_with(&format!("{run} ")) || why.contains("refusing") || why.ends_with(&format!("is not the repository of {run}")) => {
                Ok(RunDiscard::Refused(orq_core::redact::redact(&why)))
            }
            Err(why) => Err(why),
        }
    }

    fn features(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let list = read_ledger(&orq_features::ledger_path(&git))?
            .into_iter()
            .map(|entry| serde_json::json!({"name": orq_core::redact::redact(&entry.feature_name), "description": entry.description.as_deref().map(orq_core::redact::redact), "status": orq_core::redact::redact(entry.status.as_str()), "depends_on": entry.depends_on.iter().map(|name| orq_core::redact::redact(name)).collect::<Vec<_>>(), "test_file": entry.test_file.as_deref().map(orq_core::redact::redact), "recorded_at": entry.recorded_at.as_deref().map(orq_core::redact::redact)}))
            .collect();
        Ok(Some(serde_json::Value::Array(list)))
    }

    fn runs(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let mut rows = Vec::new();
        // A poisoned lock only means an earlier request panicked; what it holds is still valid to re-check.
        let mut cache = self.run_logs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let run_ids = store.run_ids().map_err(|error| format!("{error:?}"))?;
        // Runs that are gone are forgotten, so the cache never outgrows the store.
        let live: std::collections::HashSet<&String> = run_ids.iter().collect();
        cache.retain(|id, _| live.contains(id));
        for run_id in run_ids {
            let record = match store.load(&run_id) {
                Ok(record) => record,
                Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
                Err(error) => return Err(format!("run {run_id} is unreadable: {error:?}")),
            };
            if let Some(row) = run_row(&store, &self.roots.state, &mut cache, record, &key)? {
                rows.push(row);
            }
        }
        drop(cache);
        // A merge leaves a run's log as it was, so a failed or review-waiting run is checked against git,
        // which is asked again only for a run whose branch or target has moved since it last was.
        if rows.iter().any(|row| matches!(row.2["state"].as_str(), Some("failed" | "needs_review"))) {
            let tips = branch_tips(&project.path);
            let mut merged = self.merged.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for row in &mut rows {
                if matches!(row.2["state"].as_str(), Some("failed" | "needs_review")) && merged.merged(&row.3, &tips, || orq_orchestrator::work_is_on_target(&project.path, &row.3)) {
                    row.2["state"] = "merged".into();
                }
            }
        }
        // Newest first; a run with no readable time goes last, ties by run id.
        rows.sort_by(|a, b| match (a.0, b.0) {
            (Some(x), Some(y)) => y.cmp(&x).then_with(|| a.1.cmp(&b.1)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.1.cmp(&b.1),
        });
        Ok(Some(serde_json::Value::Array(rows.into_iter().map(|row| row.2).collect())))
    }

    fn run_logs(&self, id: &str, run: &str, file: Option<&str>, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        use std::io::Read;
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let record = match store.load(run) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        if record.project_id != key {
            return Ok(None);
        }
        let logs_root = self.roots.state.join("logs");
        let dir = logs_root.join(run);
        // A link in place of either directory would lead outside the run's own logs.
        let real_dir = |path: &std::path::Path| match path.symlink_metadata() {
            Ok(meta) => Some(meta.is_dir()),
            Err(_) => None,
        };
        if real_dir(&logs_root) == Some(false) || real_dir(&dir) == Some(false) {
            return Ok(None);
        }
        // Only plain, non-hidden `.log` files that are not links are logs.
        let plain = |name: &str| name.ends_with(".log") && !name.starts_with('.') && !name.contains(['/', '\\']);
        let Some(file) = file else {
            let mut files = Vec::new();
            // A run that has written no logs has no directory; any other failure is an error.
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry.map_err(|error| format!("logs of {run} are unreadable: {error}"))?;
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let meta = entry.path().symlink_metadata().map_err(|error| format!("log {name} is unreadable: {error}"))?;
                        if meta.is_file() && plain(&name) {
                            files.push(serde_json::json!({"name": name, "bytes": meta.len()}));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("logs of {run} are unreadable: {error}")),
            }
            files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            return Ok(Some(serde_json::json!({"files": files})));
        };
        if !plain(file) {
            return Ok(None);
        }
        let skip: usize = match after {
            None => 0,
            Some(cursor) => match cursor.parse() {
                Ok(lines) => lines,
                Err(_) => return Ok(None),
            },
        };
        let path = dir.join(file);
        match path.symlink_metadata() {
            Ok(meta) if meta.is_file() => {}
            _ => return Ok(None),
        }
        let meta = path.symlink_metadata().map_err(|error| format!("log {file} is unreadable: {error}"))?;
        let (len, modified) = (meta.len(), meta.modified().ok());
        let mut slot = self.log_snapshot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = match slot.as_ref().filter(|known| after.is_some() && known.path == path && known.len == len && known.modified == modified && modified.is_some()) {
            Some(known) => (known.text.clone(), known.truncated),
            None => {
                let mut bytes = Vec::new();
                std::fs::File::open(&path).and_then(|f| f.take(LOG_READ_CAP + 1).read_to_end(&mut bytes)).map_err(|error| format!("log {file} is unreadable: {error}"))?;
                let truncated = bytes.len() as u64 > LOG_READ_CAP;
                bytes.truncate(LOG_READ_CAP as usize);
                // The whole readable text is redacted at once so a secret that spans lines is found.
                let text: std::sync::Arc<str> = String::from_utf8_lossy(&orq_core::redact::redact_bytes(&bytes)).into_owned().into();
                *slot = Some(LogSnapshot { path: path.clone(), len, modified, truncated, text: text.clone() });
                (text, truncated)
            }
        };
        drop(slot);
        let (text, truncated) = snapshot;
        let total = text.lines().count();
        if skip > total {
            return Ok(None);
        }
        let page: Vec<&str> = text.lines().skip(skip).take(LOG_PAGE).collect();
        let next = if skip + page.len() < total { Some((skip + page.len()).to_string()) } else { None };
        Ok(Some(serde_json::json!({"name": file, "lines": page, "next_cursor": next, "truncated": truncated})))
    }

    fn run_timeline(&self, id: &str, run: &str, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let record = match store.load(run) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        if record.project_id != key {
            return Ok(None);
        }
        let events = match store.events(run) {
            Ok(events) => events,
            Err(StoreError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        let start = match after {
            None => 0,
            Some(cursor) => match events.iter().position(|event| event.event_id == cursor) {
                Some(at) => at + 1,
                None => return Ok(None),
            },
        };
        let summary = orq_orchestrator::summarize_run(&events);
        let page: Vec<_> = events.iter().skip(start).take(TIMELINE_PAGE).collect();
        let more = start + page.len() < events.len();
        let next_cursor = if more { page.last().map(|event| event.event_id.clone()) } else { None };
        // The payload is left out: it can hold reasons and paths the page has no need of.
        // A review round starts with each review stage start, counted over the whole run so a later page
        // still names its round; only review events carry one.
        let advance = |round: u64, event: &orq_orchestrator::Event| if event.kind == "stage.started" || round == 0 { round + 1 } else { round };
        let is_review = |event: &orq_orchestrator::Event| event.stage.as_deref() == Some("review");
        let mut round = events[..start].iter().filter(|event| is_review(event)).fold(0, advance);
        let listed: Vec<_> = page
            .iter()
            .map(|event| {
                let mut listed = serde_json::json!({"event_id": event.event_id, "kind": event.kind, "stage": event.stage, "attempt": event.attempt, "occurred_at": event.occurred_at});
                if is_review(event) {
                    round = advance(round, event);
                    listed["review_round"] = serde_json::json!(round);
                }
                listed
            })
            .collect();
        // How many review rounds the whole run has, so live events can carry on counting.
        let review_rounds = events.iter().filter(|event| is_review(event)).fold(0, advance);
        Ok(Some(serde_json::json!({
            "run": {"run_id": record.run_id, "feature": record.feature_id, "target_branch": record.target_branch, "state": shown_state(&project.path, &record, summary.phase.as_str()), "stage": summary.stage, "started_at": events.first().map(|event| &event.occurred_at), "updated_at": events.last().map(|event| &event.occurred_at), "delivery": record.delivery.as_str(), "ending": delivered_ending(&events, summary.phase)},
            "events": listed,
            "review_rounds": review_rounds,
            "next_cursor": next_cursor,
        })))
    }

    fn projects(&self) -> Result<serde_json::Value, String> {
        use serde_json::json;
        let reviews = if self.roots.database().exists() {
            Some(orq_core::store::Store::open_read_only(&self.roots.database()).map_err(|error| format!("cannot open the review index: {error}"))?)
        } else {
            None
        };
        let active = active_runs_by_project(&orq_orchestrator::RunStore::new(&self.roots.state));
        let list = discover(&self.scan_root)
            .into_iter()
            .map(|project| match counts(&project, reviews.as_ref(), &active) {
                Ok(counts) => json!({"id": project.id, "name": project.name, "available": true, "features": counts.features, "active_runs": counts.active_runs, "reviews_waiting": counts.reviews_waiting, "failed_runs": counts.failed_runs, "failed_window_hours": FAILED_WINDOW_HOURS}),
                Err(_) => json!({"id": project.id, "name": project.name, "available": false, "features": null, "active_runs": null, "reviews_waiting": null, "failed_runs": null, "failed_window_hours": FAILED_WINDOW_HOURS}),
            })
            .collect::<Vec<_>>();
        Ok(serde_json::Value::Array(list))
    }
}

fn counts(project: &Project, reviews: Option<&orq_core::store::Store>, active: &Result<std::collections::HashMap<String, RunCounts>, String>) -> Result<Counts, String> {
    let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
    let key = orq_git::project_id(&git);
    let features = ledger_entries(&orq_features::ledger_path(&git))?;
    let runs = active.as_ref().map_err(Clone::clone)?.get(&key).copied().unwrap_or_default();
    let reviews_waiting = match reviews {
        Some(store) => waiting_reviews(store, &key)?,
        None => 0,
    };
    Ok(Counts { features, active_runs: runs.active, failed_runs: runs.failed, reviews_waiting })
}

/// The runs of `project` whose latest report is still waiting on a human: no lifecycle recorded for
/// that report's head yet, or one that is neither merged, closed nor resolved. A lifecycle that
/// cannot be read is an error, never a guess.
fn waiting_reviews(store: &orq_core::store::Store, project: &str) -> Result<usize, String> {
    use orq_core::review_lifecycle::{Gate, ProviderState};
    // Records come oldest first, so a later one for the same run replaces the earlier.
    let mut latest: std::collections::HashMap<String, orq_core::store::ReviewRecord> = std::collections::HashMap::new();
    for record in store.reviews_for_project(project).map_err(|error| error.to_string())? {
        latest.insert(record.run_id.clone(), record);
    }
    let mut waiting = 0;
    for record in latest.values() {
        let lifecycles = store.lifecycles_for_run(&record.run_id).map_err(|error| error.to_string())?;
        let finished = lifecycles
            .iter()
            .rev()
            .find(|lifecycle| lifecycle.reported_head_sha == record.head_sha)
            .is_some_and(|lifecycle| matches!(lifecycle.state, ProviderState::Merged | ProviderState::Closed) || lifecycle.gate == Gate::Resolved);
        if !finished {
            waiting += 1;
        }
    }
    Ok(waiting)
}

/// Active runs per project id, read in one pass over the run store. A record or event log that
/// cannot be read has no known owner, so no project's count can be trusted and this is an error.
fn active_runs_by_project(runs: &orq_orchestrator::RunStore) -> Result<std::collections::HashMap<String, RunCounts>, String> {
    use orq_orchestrator::{RunPhase, StoreError};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut counts = std::collections::HashMap::new();
    for run_id in runs.run_ids().map_err(|error| format!("{error:?}"))? {
        let record = match runs.load(&run_id) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
            Err(error) => return Err(format!("run {run_id} is unreadable: {error:?}")),
        };
        let events = runs.events(&run_id).map_err(|error| format!("run {run_id} is unreadable: {error:?}"))?;
        let entry: &mut RunCounts = counts.entry(record.project_id).or_default();
        let summary = orq_orchestrator::summarize_run(&events);
        match summary.phase {
            RunPhase::Running | RunPhase::PauseRequested | RunPhase::Paused => entry.active += 1,
            RunPhase::Failed if failed_recently(summary.phase_at.as_deref(), now) => entry.failed += 1,
            _ => {}
        }
    }
    Ok(counts)
}

/// Whether the event that failed the run is no older than the window. A failure with no readable
/// time, or one from the future, is not counted.
fn failed_recently(failed_at: Option<&str>, now: u64) -> bool {
    let Some(at) = failed_at.and_then(orq_orchestrator::parse_rfc3339) else { return false };
    at <= now && now - at <= FAILED_WINDOW_HOURS * 3600
}

/// The most a feature ledger may weigh before the server refuses to read it.
const LEDGER_LIMIT: u64 = 1024 * 1024;

/// How many features the ledger at `path` lists; none when it does not exist. The file is opened
/// without following a link and without waiting on a pipe, must be a regular file, and is read
/// only up to a limit, so a ledger planted in a repository cannot reach elsewhere or stall a request.
fn ledger_entries(path: &Path) -> Result<usize, String> {
    read_ledger(path).map(|entries| entries.len())
}

/// The ledger's entries; a missing ledger has none. It is opened without following a link and
/// read only up to a limit, so a hostile file cannot stall or exhaust the server.
fn read_ledger(path: &Path) -> Result<Vec<orq_features::FeatureEntry>, String> {
    match read_ledger_bytes(path)? {
        Some(bytes) => parse_ledger_bytes(bytes),
        None => Ok(Vec::new()),
    }
}

fn parse_ledger_bytes(bytes: Vec<u8>) -> Result<Vec<orq_features::FeatureEntry>, String> {
    let text = String::from_utf8(bytes).map_err(|_| "the ledger is not text".to_string())?;
    orq_features::parse_ledger(&text).map_err(|error| format!("the ledger is malformed: {error}"))
}

/// The ledger's bytes, read once without following a link and within the size limit; `None` when there is none.
fn read_ledger_bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("the ledger cannot be opened: {:?}", error.kind())),
    };
    let meta = file.metadata().map_err(|error| format!("the ledger cannot be inspected: {:?}", error.kind()))?;
    if !meta.is_file() {
        return Err("the ledger is not a regular file".to_string());
    }
    if meta.len() > LEDGER_LIMIT {
        return Err("the ledger is too large".to_string());
    }
    let mut bytes = Vec::new();
    file.take(LEDGER_LIMIT + 1).read_to_end(&mut bytes).map_err(|error| format!("the ledger cannot be read: {:?}", error.kind()))?;
    if bytes.len() as u64 > LEDGER_LIMIT {
        return Err("the ledger is too large".to_string());
    }
    Ok(Some(bytes))
}

/// The stored value a redacted one stands for: `value` if it is not the redacted form of one of `stored`.
fn unredacted<'a>(value: &str, mut stored: impl Iterator<Item = &'a String>) -> String {
    stored.find(|original| original.as_str() != value && orq_core::redact::redact(original) == value).cloned().unwrap_or_else(|| value.to_string())
}

/// Whether `name` can only be a feature name, never a path: the web adds no feature it could not then name.
fn plain_feature_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !name.starts_with('.')
}

/// The version of a ledger as it was read, for a later save to compare against: the SHA-256 of its bytes.
fn ledger_version(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod run_row_tests {
    use super::run_row;
    use orq_orchestrator::{RunRecord, RunStore};

    #[test]
    fn run_row_skips_a_run_removed_after_its_record_was_read() {
        let dir = orq_test_support::process_temp_dir().join(format!("orq-run-row-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = RunStore::new(&dir);
        let record = RunRecord { run_id: "gone".into(), project_id: "p".into(), feature_id: "f".into(), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: "b".into(), worktree: dir.join("w"), repository: String::new(), profiles: Default::default(), delivery: Default::default() };
        store.create(&record).unwrap();
        let loaded = store.load("gone").unwrap();
        // Cleanup removes the run between the two reads.
        std::fs::remove_dir_all(dir.join("runs")).ok();
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
        assert!(matches!(run_row(&store, &dir, &mut Default::default(), loaded, "p"), Ok(None)));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod merged_cache_tests {
    use super::MergedCache;
    use orq_orchestrator::RunRecord;
    use std::collections::HashMap;

    fn record(run: &str) -> RunRecord {
        RunRecord { run_id: run.into(), project_id: "p".into(), feature_id: "f".into(), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: "w".into(), repository: String::new(), profiles: Default::default(), delivery: Default::default() }
    }

    fn tips(branch: &str, main: &str) -> HashMap<String, String> {
        HashMap::from([("orq/r-1".to_string(), branch.to_string()), ("main".to_string(), main.to_string())])
    }

    #[test]
    fn merged_cache_asks_git_again_only_when_the_branch_or_its_target_has_moved() {
        let mut cache = MergedCache::default();
        let mut asked = 0;
        let mut check = |answer: bool| {
            asked += 1;
            answer
        };
        assert!(!cache.merged(&record("r-1"), &tips("b1", "m1"), || check(false)));
        assert!(!cache.merged(&record("r-1"), &tips("b1", "m1"), || check(true)), "the same commits give the same answer");
        assert!(cache.merged(&record("r-1"), &tips("b1", "m2"), || check(true)), "the target moved, so git is asked again");
        assert!(cache.merged(&record("r-1"), &tips("b1", "m2"), || check(false)));
        assert!(!cache.merged(&record("r-1"), &HashMap::new(), || check(true)), "a branch that is gone is not merged");
        assert_eq!(asked, 2);
    }
}
