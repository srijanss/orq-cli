//! The commands a project's `.orq/run.toml` `[after_merge] commands` runs once the queue settles a
//! merge-local run's merge, in the project's main checkout, one after the other, stopping at the first
//! failure. A merge gets at most one go: none for a run that already recorded `after_merge.started` or
//! `after_merge.commands`. A merge the server did not watch land (it restarted as the run ended) gets
//! its go from the next round, unless it left the target branch or a release commit already follows it,
//! which is recorded as not ran with the reason. The commands run on a thread of their own per
//! project, so a round only starts a go and returns; merges landing during one go fold into one more.
//! Every argv, error and reason the run records is redacted.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex, MutexGuard};

use orq_core::project_settings::{AfterMergeCommands, Delivery, ReleaseOnMerge, ReleaseSettings};
use orq_core::redact::redact;
use orq_orchestrator::{Event, RunStore};
use serde_json::{json, Value};

use super::release::{self, Env};

/// Written before the first command of a go.
const STARTED: &str = "after_merge.started";
/// Written when a go ended, ran or not, or was found interrupted.
const COMMANDS: &str = "after_merge.commands";
/// Written when a merge's release was made.
const RELEASE_CREATED: &str = "release.created";
/// Written when a merge's release was refused.
const RELEASE_REFUSED: &str = "release.refused";

/// The runs, as (state root, run id), a round saw going in a project that declared `[after_merge]`.
static SEEN: LazyLock<Mutex<HashSet<(PathBuf, String)>>> = LazyLock::new(Mutex::default);
/// The state roots whose interrupted goes were looked for.
static SCANNED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);
/// Each project's go in progress, by main checkout.
static GOES: LazyLock<Mutex<HashMap<PathBuf, Go>>> = LazyLock::new(Mutex::default);
/// The runs, as (state root, run id), a go holds: waiting for it or in it, until it recorded them.
static IN_FLIGHT: LazyLock<Mutex<HashSet<(PathBuf, String)>>> = LazyLock::new(Mutex::default);

#[derive(Default)]
struct Go {
    running: bool,
    pending: Vec<Job>,
    /// The merges whose release has not landed or been refused yet.
    releasing: usize,
}

/// One merge waiting for its release and its commands.
struct Job {
    state: PathBuf,
    run: String,
    project: String,
    feature: String,
    branch: String,
    base: String,
    merged: String,
    /// Empty when the project declares none, or this merge already had its go.
    commands: Vec<Vec<String>>,
    release: Option<(ReleaseSettings, Env)>,
}

/// The release of a project at `root` is not done: the queue starts nothing until it is.
pub(super) fn release_pending(root: &Path) -> bool {
    locked(&GOES).get(root).is_some_and(|go| go.releasing > 0)
}

/// The `[release]` of the project at `root`: `None` when its `.orq/run.toml` has none.
fn declared_release(root: &Path) -> Option<Result<ReleaseSettings, String>> {
    let text = std::fs::read_to_string(root.join(".orq/run.toml")).ok()?;
    match ReleaseSettings::parse(&text) {
        Ok(settings) => settings.map(Ok),
        Err(why) => Some(Err(why)),
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `[after_merge]` of the project at `root`: `None` when its `.orq/run.toml` has none.
fn declared(root: &Path) -> Option<Result<AfterMergeCommands, String>> {
    let text = std::fs::read_to_string(root.join(".orq/run.toml")).ok()?;
    match AfterMergeCommands::parse(&text) {
        Ok(commands) => commands.map(Ok),
        Err(why) => Some(Err(why)),
    }
}

/// A round saw `run` going in the project at `root`: if the project declares `[after_merge]`, its merge
/// may get a go.
pub(super) fn watch(state: &Path, root: &Path, run: &str) {
    let key = (state.to_path_buf(), run.to_string());
    if locked(&SEEN).contains(&key) || (declared(root).is_none() && declared_release(root).is_none()) {
        return;
    }
    locked(&SEEN).insert(key);
}

/// A round settled `run` in the project at `root`: when its merge landed after it was watched, start its go.
///
/// `env` is called only when the project releases on merge.
pub(super) fn settled(state: &Path, runs: &RunStore, root: &Path, run: &str, env: &dyn Fn() -> Env) {
    if !locked(&SEEN).remove(&(state.to_path_buf(), run.to_string())) {
        return;
    }
    if let Some(job) = job_for(state, runs, root, run, true, env) {
        enqueue(root, vec![job]);
    }
}

/// The go `run`'s merge gets in the project at `root`, recording what is wrong or already done on the run;
/// `None` when it needs none. Its release is made only `with_release`.
fn job_for(state: &Path, runs: &RunStore, root: &Path, run: &str, with_release: bool, env: &dyn Fn() -> Env) -> Option<Job> {
    let (Ok(record), Ok(events)) = (runs.load(run), runs.events(run)) else {
        return None;
    };
    if record.delivery != Delivery::MergeLocal {
        return None;
    }
    let had = |kinds: &[&str]| events.iter().any(|event| kinds.contains(&event.kind.as_str()));
    let merged = events.iter().rev().filter(|event| event.kind == "merge.completed").find_map(|event| event.payload["head_sha"].as_str().map(str::to_string))?;
    let release = if !with_release || had(&[RELEASE_CREATED, RELEASE_REFUSED]) {
        None
    } else {
        match declared_release(root) {
            Some(Ok(settings)) if settings.release_on_merge == ReleaseOnMerge::Auto => Some((settings, env())),
            Some(Err(why)) => {
                let why = format!("the [release] settings are wrong: {}", redact(&why));
                refused(runs, &record.project_id, run, &merged, &why, &env().roots);
                None
            }
            _ => None,
        }
    };
    let commands = if had(&[STARTED, COMMANDS]) {
        Vec::new()
    } else {
        match declared(root) {
            None => Vec::new(),
            Some(Err(why)) => {
                let payload = json!({"ran": false, "ok": false, "merged": merged, "error": redact(&why), "commands": []});
                append(runs, run, COMMANDS, payload);
                Vec::new()
            }
            Some(Ok(declared)) => declared.commands,
        }
    };
    if commands.is_empty() && release.is_none() {
        return None;
    }
    // Held from before the run shows its go started until the go recorded it, so a restart never sees
    // the merge neither unsettled nor held.
    locked(&IN_FLIGHT).insert((state.to_path_buf(), run.to_string()));
    if !commands.is_empty() {
        append(
            runs,
            run,
            STARTED,
            json!({"merged": merged, "commands": commands.len()}),
        );
    }
    Some(Job {
        state: state.to_path_buf(),
        run: run.to_string(),
        project: record.project_id.clone(),
        feature: record.feature_id.clone(),
        branch: record.target_branch.clone(),
        base: record.base_sha.clone(),
        merged,
        commands,
        release,
    })
}

/// Puts `jobs` in the project at `root`'s next go together, starting the project's goes if none runs.
fn enqueue(root: &Path, jobs: Vec<Job>) {
    if jobs.is_empty() {
        return;
    }
    let mut goes = locked(&GOES);
    let go = goes.entry(root.to_path_buf()).or_default();
    for job in jobs {
        go.releasing += usize::from(job.release.is_some());
        go.pending.push(job);
    }
    if !go.running {
        go.running = true;
        let root = root.to_path_buf();
        std::thread::spawn(move || drain(&root));
    }
}

/// A merge-local run that ended merged and whose merge no go has looked at yet.
struct Unsettled {
    run: String,
    branch: String,
    merged: String,
    /// When its merge landed, to put several in the order they landed.
    at: String,
}

/// The merges under `state` that ended merged with nothing recorded of their release or `[after_merge]`
/// and that no go holds now, by project id.
fn unsettled_runs(state: &Path) -> HashMap<String, Vec<Unsettled>> {
    let runs = RunStore::new(state);
    let mut found: HashMap<String, Vec<Unsettled>> = HashMap::new();
    let Ok(ids) = runs.run_ids() else { return found };
    for run in ids {
        if locked(&IN_FLIGHT).contains(&(state.to_path_buf(), run.clone())) {
            continue;
        }
        let (Ok(record), Ok(events)) = (runs.load(&run), runs.events(&run)) else {
            continue;
        };
        if record.delivery != Delivery::MergeLocal {
            continue;
        }
        let ended_merged = events.iter().rev().find(|event| event.kind == "run.completed").is_some_and(|event| event.payload["ending"] == "merged");
        if !ended_merged || events.iter().any(|event| [STARTED, COMMANDS, RELEASE_CREATED, RELEASE_REFUSED].contains(&event.kind.as_str())) {
            continue;
        }
        let Some((merged, at)) = events
            .iter()
            .rev()
            .filter(|event| event.kind == "merge.completed")
            .find_map(|event| Some((event.payload["head_sha"].as_str()?.to_string(), event.occurred_at.clone())))
        else {
            continue;
        };
        found.entry(record.project_id).or_default().push(Unsettled { run, branch: record.target_branch, merged, at });
    }
    found
}

/// Whether the project at `root` gives a merge a go: it declares `[after_merge]` commands, or a `[release]`
/// that releases on merge (or either is wrong, which the go reports). An `[after_merge]` with no commands
/// needs none.
fn gives_a_go(root: &Path) -> bool {
    let commands = match declared(root) {
        Some(Ok(declared)) => !declared.commands.is_empty(),
        Some(Err(_)) => true,
        None => false,
    };
    commands
        || match declared_release(root) {
            Some(Ok(settings)) => settings.release_on_merge == ReleaseOnMerge::Auto,
            Some(Err(_)) => true,
            None => false,
        }
}

/// The main checkout of each project under the server's scan root, by project id.
fn project_roots(projects: &super::super::StoreProjects) -> HashMap<String, PathBuf> {
    let mut roots = HashMap::new();
    for project in super::super::discover(&projects.scan_root) {
        let Ok(git) = orq_git::resolve_project(&project.path) else {
            continue;
        };
        let root = orq_git::main_checkout_root(&git).unwrap_or_else(|| git.root.clone());
        roots.insert(orq_git::project_id(&git), root);
    }
    roots
}

/// The version of the first `chore(release)` commit after `merged` on `branch` in `root`, if there is one.
fn released_by(root: &Path, branch: &str, merged: &str) -> Option<String> {
    let range = format!("{merged}..refs/heads/{branch}");
    let subjects = git(root, &["log", "--reverse", "--format=%s", &range])?;
    let subject = subjects.lines().find(|subject| subject.starts_with("chore(release)"))?;
    subject.split_whitespace().last().map(str::to_string)
}

impl super::super::StoreProjects {
    /// How many merges an idle restart still waits for: the ones a go holds, and the ones that ended merged
    /// in a project that gives a merge a go and that no go has looked at yet.
    pub fn merges_unsettled(&self) -> usize {
        let state = &self.roots.state;
        // The runs first, then what is held: a go is held before its run shows it started and let go of
        // only after it recorded the run, so a merge that moves on in between is counted once or twice,
        // never missed.
        let unsettled = unsettled_runs(state);
        let waiting: usize = if unsettled.is_empty() {
            0
        } else {
            let roots = project_roots(self);
            unsettled.iter().filter(|(project, _)| roots.get(*project).is_some_and(|root| gives_a_go(root))).map(|(_, merges)| merges.len()).sum()
        };
        let held = locked(&IN_FLIGHT).iter().filter(|(root, _)| root == state).count();
        held + waiting
    }

    /// Gives each merge no round settled (the server restarted as its run ended) the go it missed: one go
    /// per project, the newest merge's release with it; a merge no longer on its target, or a release
    /// commit already follows, is recorded as not ran with the reason.
    pub(super) fn settle_unsettled(&self) {
        let state = &self.roots.state;
        let unsettled = unsettled_runs(state);
        if unsettled.is_empty() {
            return;
        }
        let roots = project_roots(self);
        let runs = RunStore::new(state);
        for (project, mut merges) in unsettled {
            let Some(root) = roots.get(&project) else {
                continue;
            };
            if !gives_a_go(root) {
                continue;
            }
            merges.sort_by(|a, b| a.at.cmp(&b.at));
            let mut kept = Vec::new();
            for merge in merges {
                // A merge this server watched land gets its go as the round would have given it.
                if locked(&SEEN).remove(&(state.clone(), merge.run.clone())) {
                    kept.push(merge.run);
                    continue;
                }
                let on_target = git(root, &["merge-base", "--is-ancestor", &merge.merged, &format!("refs/heads/{}", merge.branch)]).is_some();
                let reason = if !on_target {
                    Some(format!("not on {}", merge.branch))
                } else {
                    released_by(root, &merge.branch, &merge.merged).map(|version| format!("released by {version}"))
                };
                match reason {
                    Some(reason) => append(&runs, &merge.run, COMMANDS, json!({"ran": false, "ok": false, "merged": merge.merged, "reason": redact(&reason), "commands": []})),
                    None => kept.push(merge.run),
                }
            }
            let settings = match orq_git::resolve_project(root) {
                Ok(git) => self.settings_of_git(&git).unwrap_or_default(),
                Err(_) => Default::default(),
            };
            let env = || self.release_env(root, &settings);
            let newest = kept.len().saturating_sub(1);
            let jobs = kept.iter().enumerate().filter_map(|(at, run)| job_for(state, &runs, root, run, at == newest, &env)).collect();
            enqueue(root, jobs);
        }
    }
}

/// Once per state root, closes each go the server stopped in the middle of as interrupted, without
/// running it again.
pub(super) fn close_interrupted(runs: &RunStore, state: &Path) {
    if !locked(&SCANNED).insert(state.to_path_buf()) {
        return;
    }
    let Ok(ids) = runs.run_ids() else { return };
    for run in ids {
        let Ok(events) = runs.events(&run) else {
            continue;
        };
        let Some(started) = events.iter().find(|event| event.kind == STARTED) else {
            continue;
        };
        if events.iter().any(|event| event.kind == COMMANDS) {
            continue;
        }
        let payload = json!({"ran": false, "ok": false, "interrupted": true, "merged": started.payload["merged"], "reason": "interrupted: the server stopped before the commands finished", "commands": []});
        append(runs, &run, COMMANDS, payload);
    }
}

/// Runs the project's waiting merges as one go at a time, until none waits.
fn drain(root: &Path) {
    loop {
        let batch = {
            let mut goes = locked(&GOES);
            let Some(go) = goes.get_mut(root) else { return };
            if go.pending.is_empty() {
                go.running = false;
                return;
            }
            std::mem::take(&mut go.pending)
        };
        go_once(root, &batch);
        let mut in_flight = locked(&IN_FLIGHT);
        for job in &batch {
            in_flight.remove(&(job.state.clone(), job.run.clone()));
        }
    }
}

/// One go for `batch`, the merges that landed meanwhile: the checkout is checked against the newest, and
/// each run records the outcome.
fn go_once(root: &Path, batch: &[Job]) {
    for job in batch {
        if let Some((settings, env)) = &job.release {
            let runs = RunStore::new(&job.state);
            let merge = release::Merge { root, branch: &job.branch, base: &job.base, merged: &job.merged, feature: &job.feature, run: &job.run };
            match release::release(env, settings, &merge) {
                Ok(made) => created(&runs, job, &made, &env.roots),
                Err(why) => refused(&runs, &job.project, &job.run, &job.merged, &redact(&why), &env.roots),
            }
            if let Some(go) = locked(&GOES).get_mut(root) {
                go.releasing = go.releasing.saturating_sub(1);
            }
        }
    }
    let with_commands: Vec<&Job> = batch.iter().filter(|job| !job.commands.is_empty()).collect();
    let Some(newest) = with_commands.last().copied() else { return };
    let skipped = checkout_problem(root, &newest.branch, &newest.merged);
    let (results, ok) = match &skipped {
        Some(_) => (
            newest
                .commands
                .iter()
                .map(|argv| json!({"argv": shown(argv), "skipped": true}))
                .collect(),
            false,
        ),
        None => run_commands(root, &newest.commands),
    };
    for job in &with_commands {
        let mut payload =
            json!({"ran": skipped.is_none(), "ok": ok, "merged": job.merged, "commands": results});
        if let Some(why) = &skipped {
            payload["skipped_reason"] = json!(redact(why));
        }
        if with_commands.len() > 1 {
            payload["folded"] = json!(with_commands.len());
        }
        append(&RunStore::new(&job.state), &job.run, COMMANDS, payload);
    }
}

/// Records that `job`'s release was made, on the run and in the audit log.
fn created(runs: &RunStore, job: &Job, made: &release::Created, roots: &orq_core::storage::StorageRoots) {
    let payload = json!({
        "version": made.version, "from": made.from, "level": made.level, "proposed": made.proposed,
        "reason": made.reason, "decided_by": made.decided_by, "merged": job.merged, "commit": made.commit,
    });
    append(runs, &job.run, RELEASE_CREATED, payload);
    audit(roots, &job.project, &job.run, orq_core::remote_audit::Outcome::Allowed, &format!("released {}", made.version));
}

/// Records that the release of `run`'s merge was refused for `why` (already redacted).
fn refused(runs: &RunStore, project: &str, run: &str, merged: &str, why: &str, roots: &orq_core::storage::StorageRoots) {
    append(runs, run, RELEASE_REFUSED, json!({"why": why, "merged": merged}));
    audit(roots, project, run, orq_core::remote_audit::Outcome::Refused, why);
}

fn audit(roots: &orq_core::storage::StorageRoots, project: &str, run: &str, outcome: orq_core::remote_audit::Outcome, reason: &str) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let attempt = orq_core::remote_audit::Attempt { identity: "orq serve", action: "project.release", project, run: Some(run), outcome, reason: Some(reason) };
    if let Err(error) = orq_core::remote_audit::record(roots, &attempt, now) {
        eprintln!("orq: cannot audit the release of {run}: {error}");
    }
}

/// Why the checkout at `root` is not where the commands belong: on another branch than `branch`, or at
/// neither the merged commit nor one after it.
fn checkout_problem(root: &Path, branch: &str, merged: &str) -> Option<String> {
    match git(root, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        Some(on) if on == branch => {}
        Some(on) => return Some(format!("checkout on {on}")),
        None => return Some("checkout on a detached HEAD".to_string()),
    }
    let Some(head) = git(root, &["rev-parse", "HEAD"]) else {
        return Some("HEAD cannot be read".to_string());
    };
    if head == merged || git(root, &["merge-base", "--is-ancestor", merged, "HEAD"]).is_some() {
        return None;
    }
    Some(format!("HEAD is {head}, not {merged}"))
}

/// What `git args` printed in `root`, trimmed; `None` when it failed.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether `word` is a flag (`--name`, not `--name=value`) that names a credential, so the word after it is one.
fn credential_flag(word: &str) -> bool {
    let name = word.to_ascii_lowercase();
    name.starts_with('-') && !name.contains('=') && ["password", "passwd", "token", "secret", "api-key", "api_key", "apikey", "credential", "auth", "private-key", "private_key", "privatekey"].iter().any(|part| name.contains(part))
}

/// `argv` as an event may hold it: the value after a credential flag is replaced whatever it looks like,
/// and every other word is redacted on its own.
fn shown(argv: &[String]) -> Vec<String> {
    let mut previous_names_credential = false;
    argv.iter()
        .map(|word| {
            let masked = previous_names_credential;
            previous_names_credential = credential_flag(word);
            if masked {
                "[redacted]".to_string()
            } else {
                redact(word)
            }
        })
        .collect()
}

/// Runs each argv in `root` in order, stopping at the first that fails or cannot start; what each did
/// and whether all succeeded.
pub(super) fn run_commands(root: &Path, commands: &[Vec<String>]) -> (Vec<Value>, bool) {
    let mut results = Vec::new();
    let mut failed = false;
    for argv in commands {
        if failed {
            results.push(json!({"argv": shown(argv), "skipped": true}));
            continue;
        }
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(status) => {
                failed = !status.success();
                results.push(json!({"argv": shown(argv), "exit_code": status.code()}));
            }
            Err(error) => {
                failed = true;
                results.push(json!({"argv": shown(argv), "error": redact(&error.to_string())}));
            }
        }
    }
    (results, !failed)
}

/// Records `kind` on `run`; a run that cannot be written to is logged and goes on.
fn append(runs: &RunStore, run: &str, kind: &str, payload: Value) {
    let Ok(record) = runs.load(run) else { return };
    let event = Event {
        schema_version: 1,
        event_id: format!("{run}-{}", kind.replace('.', "-")),
        run_id: run.to_string(),
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: kind.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: super::utc_now(),
        payload,
    };
    if let Err(error) = runs.append_event(&event) {
        eprintln!("orq: cannot record {kind} on {run}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orq_orchestrator::RunRecord;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn event(runs: &RunStore, run: &str, kind: &str, payload: Value) {
        let record = runs.load(run).unwrap();
        let n = runs.events(run).unwrap().len();
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: record.project_id, feature_id: record.feature_id, kind: kind.into(), stage: None, attempt: 1, occurred_at: format!("2026-11-25T15:42:0{n}Z"), payload };
        runs.append_event(&event).unwrap();
    }

    /// A server over project `alpha` with one `[after_merge]` command, and its run `run-1` merged at HEAD.
    fn merged_project(tag: &str) -> (super::super::super::StoreProjects, PathBuf, PathBuf) {
        let base = orq_test_support::process_temp_dir().join(format!("orq-after-merge-unit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let roots = orq_core::storage::StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
        roots.create_all().unwrap();
        let scan = base.join("code");
        std::fs::create_dir_all(scan.join("alpha")).unwrap();
        let repo = std::fs::canonicalize(scan.join("alpha")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), ".orq/\n").unwrap();
        git(&repo, &["add", ".gitignore"]);
        git(&repo, &["commit", "-q", "-m", "start"]);
        std::fs::create_dir_all(repo.join(".orq")).unwrap();
        std::fs::write(repo.join(".orq/run.toml"), "[after_merge]\ncommands = [[\"true\"]]\n").unwrap();
        let runs = RunStore::new(&roots.state);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let record = RunRecord {
            delivery: Delivery::MergeLocal,
            run_id: "run-1".into(),
            project_id: orq_git::project_id(&orq_git::resolve_project(&repo).unwrap()),
            feature_id: "F-1".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: head.clone(),
            branch: "orq/run-1".into(),
            worktree: repo.join(".claude/worktrees/run-1"),
            repository: String::new(),
            profiles: Default::default(),
        };
        runs.create(&record).unwrap();
        event(&runs, "run-1", "merge.completed", json!({"head_sha": head, "target_branch": "main"}));
        event(&runs, "run-1", "run.completed", json!({"ending": "merged"}));
        let state = roots.state.clone();
        (super::super::super::StoreProjects::new(roots, scan), state, repo)
    }

    #[test]
    fn a_merge_whose_go_is_made_but_not_yet_queued_still_counts_as_unsettled() {
        let (projects, state, repo) = merged_project("made");
        assert_eq!(projects.merges_unsettled(), 1);
        let job = job_for(&state, &RunStore::new(&state), &repo, "run-1", false, &|| unreachable!("no release"));
        assert!(job.is_some());
        assert!(RunStore::new(&state).events("run-1").unwrap().iter().any(|event| event.kind == STARTED));
        assert_eq!(projects.merges_unsettled(), 1, "the go is made and the merge started, but nothing holds it yet");
    }
}
