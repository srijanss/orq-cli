//! What is left to tidy once a run's MR merged, offered as the Clean up card: mark the feature
//! completed, delete the kept worktree, delete the local branch and fast-forward the main checkout's
//! target. Each step is recorded as a `cleanup.<step>` run event, which is what the card reads.

use std::path::Path;
use std::process::{Command, Output};

use orq_core::remote_inbox::RunCleanup;
use orq_core::review_lifecycle::{ProviderState, ReviewLifecycle, LOCAL_PROVIDER};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use serde_json::{json, Value};

use super::queue::{project_target, review_ref, utc_now, PULL_REMOTE, TARGET_PULLED_EVENT};
use super::StoreProjects;

/// The steps of a clean-up, in the order they run.
pub const STEPS: [&str; 4] = [
    "complete_feature",
    "delete_worktree",
    "delete_branch",
    "pull_target",
];

/// The run event a step records.
fn event_kind(step: &str) -> String {
    format!("cleanup.{step}")
}

/// What became of one step: `done`, `skipped` (it was not safe, or an earlier step did not finish) or
/// `failed` (git or the ledger refused it), with why when it is not done.
struct Step {
    state: &'static str,
    reason: Option<String>,
    /// More the event keeps, such as the ledger snapshot's file.
    extra: Value,
}

impl Step {
    fn done(reason: Option<String>) -> Self {
        Step {
            state: "done",
            reason,
            extra: Value::Null,
        }
    }

    fn skipped(reason: String) -> Self {
        Step {
            state: "skipped",
            reason: Some(reason),
            extra: Value::Null,
        }
    }

    fn failed(reason: String) -> Self {
        Step {
            state: "failed",
            reason: Some(reason),
            extra: Value::Null,
        }
    }
}

/// The forge review of run `run` once it is merged; `None` while it is not, or has none.
pub(super) fn merged_review(roots: &StorageRoots, run: &str) -> Option<ReviewLifecycle> {
    if !roots.database().exists() {
        return None;
    }
    let store = orq_core::store::Store::open_read_only(&roots.database()).ok()?;
    let latest = store
        .lifecycles_for_run(run)
        .ok()?
        .into_iter()
        .filter(|review| review.provider != LOCAL_PROVIDER)
        .last();
    latest.filter(|review| review.state == ProviderState::Merged)
}

/// The runs whose MR merged and whose clean-up has not finished, per project id, as the project card
/// lists them: `{run, feature, review}`. A run with neither its worktree nor a clean-up step left is
/// one tidied before Clean up existed, and is not listed.
pub(super) fn waiting_by_project(
    store: Option<&orq_core::store::Store>,
    runs: &RunStore,
) -> std::collections::HashMap<String, Vec<Value>> {
    let mut waiting: std::collections::HashMap<String, Vec<Value>> =
        std::collections::HashMap::new();
    let Some(store) = store else { return waiting };
    for run in runs.run_ids().unwrap_or_default() {
        let Ok(record) = runs.load(&run) else {
            continue;
        };
        let Ok(events) = runs.events(&run) else {
            continue;
        };
        if !record.worktree.exists()
            && !events
                .iter()
                .any(|event| event.kind.starts_with("cleanup."))
        {
            continue;
        }
        let latest = store.lifecycles_for_run(&run).ok().and_then(|all| {
            all.into_iter()
                .filter(|review| review.provider != LOCAL_PROVIDER)
                .last()
        });
        let Some(review) = latest.filter(|review| review.state == ProviderState::Merged) else {
            continue;
        };
        if card(&events)["steps"]
            .as_array()
            .is_some_and(|steps| steps.iter().all(|step| step["state"] == "done"))
        {
            continue;
        }
        waiting
            .entry(record.project_id.clone())
            .or_default()
            .push(json!({"run": run, "feature": record.feature_id, "review": review_ref(&review)}));
    }
    waiting
}

/// The commits on the target of the main checkout at `root` that its remote does not have, as the
/// project's card and page show them: `{target, count, message, subjects}` with the newest ten
/// subjects, redacted. `None` when there are none, or the remote's branch is not known here.
pub(super) fn unpushed(root: &Path) -> Option<Value> {
    let target = project_target(root);
    let range = format!("refs/remotes/{PULL_REMOTE}/{target}..refs/heads/{target}");
    let out = git(root, &["rev-list", "--count", &range])
        .ok()
        .filter(|out| out.status.success())?;
    let count: u64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|count| *count > 0)?;
    let log = git(root, &["log", "-n", "10", "--format=%s", &range])
        .ok()
        .filter(|out| out.status.success())?;
    let subjects: Vec<String> = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(orq_core::redact::redact)
        .collect();
    let noun = if count == 1 { "commit" } else { "commits" };
    Some(
        json!({"target": target, "count": count, "message": format!("{count} {noun} on {target} not pushed"), "subjects": subjects}),
    )
}

/// The Clean up card of a run whose MR merged, from its `events`: each step, in order, with its state
/// and, when it is not done, why. A step not run yet is pending; the target's step shows the pull the
/// merge itself made, until a clean-up pulls it.
pub(super) fn card(events: &[Event]) -> Value {
    let latest = |kind: &str| events.iter().rev().find(|event| event.kind == kind);
    let steps: Vec<Value> = STEPS
        .iter()
        .map(|step| match latest(&event_kind(step)) {
            Some(event) => json!({"step": step, "state": event.payload["state"], "reason": event.payload["reason"]}),
            None if *step == "pull_target" => match latest(TARGET_PULLED_EVENT) {
                Some(pulled) => json!({"step": step, "state": if pull_may_start(&pulled.payload) { "done" } else { "failed" }, "reason": pulled.payload["message"]}),
                None => json!({"step": step, "state": "pending"}),
            },
            None => json!({"step": step, "state": "pending"}),
        })
        .collect();
    json!({ "steps": steps })
}

/// Whether a `target.pulled` payload says the target holds the remote's commits.
fn pull_may_start(payload: &Value) -> bool {
    matches!(
        payload["outcome"].as_str(),
        Some("fast_forwarded" | "up_to_date" | "ahead" | "no_remote")
    )
}

impl StoreProjects {
    /// Cleans up run `run` of project `id`, whose MR merged, as Clean up asks.
    pub(super) fn clean_up(&self, id: &str, run: &str) -> Result<RunCleanup, String> {
        let Some((_, store)) = self.own_run(id, run)? else {
            return Ok(RunCleanup::Unknown);
        };
        if merged_review(&self.roots, run).is_none() {
            return Ok(RunCleanup::Refused(
                "the run's MR is not merged".to_string(),
            ));
        }
        let Some((_, root)) = self.settings_key(id)? else {
            return Ok(RunCleanup::Unknown);
        };
        let record = store
            .load(run)
            .map_err(|error| format!("run {run} is unreadable: {error:?}"))?;
        self.run_steps(id, &root, &store, &record);
        let events = store
            .events(run)
            .map_err(|error| format!("run {run} is unreadable: {error:?}"))?;
        Ok(RunCleanup::Done(card(&events)))
    }

    /// Cleans up run `record` of project `id` on its own, as a project set to `after_merge = auto` has
    /// the server do once it sees the MR `review` merged: once only, audited, and told through the
    /// notify command. A run that has any clean-up step recorded is left as it is.
    pub(super) fn auto_clean_up(
        &self,
        id: &str,
        root: &Path,
        store: &RunStore,
        record: &RunRecord,
        review: &str,
    ) {
        let tried = store.events(&record.run_id).map_or(true, |events| {
            events
                .iter()
                .any(|event| event.kind.starts_with("cleanup."))
        });
        if tried {
            return;
        }
        self.run_steps(id, root, store, record);
        let events = store.events(&record.run_id).unwrap_or_default();
        let stopped = STEPS.iter().find_map(|step| {
            let event = events
                .iter()
                .rev()
                .find(|event| event.kind == event_kind(step))?;
            (event.payload["state"] != "done").then(|| {
                event.payload["reason"]
                    .as_str()
                    .unwrap_or("a step did not finish")
                    .to_string()
            })
        });
        use orq_core::remote_audit::{record as audit, Attempt, Outcome};
        let (outcome, ending, detail) = match &stopped {
            None => (
                Outcome::Allowed,
                "cleaned up",
                format!("after {review} merged"),
            ),
            Some(why) => (
                Outcome::Refused,
                "clean-up stopped",
                orq_core::redact::redact(why),
            ),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let reason = format!("{ending}: {detail}");
        let attempt = Attempt {
            identity: "orq serve",
            action: "run.cleanup",
            project: id,
            run: Some(&record.run_id),
            outcome,
            reason: Some(&reason),
        };
        if let Err(error) = audit(&self.roots, &attempt, now) {
            eprintln!(
                "orq: cannot audit the clean-up of {}: {error}",
                record.run_id
            );
        }
        let Ok(settings) =
            orq_orchestrator::load_run_settings(root, &|name| std::env::var(name).ok())
        else {
            return;
        };
        let Some(argv) = settings.notify_cmd.filter(|argv| !argv.is_empty()) else {
            return;
        };
        if stopped.is_none() && settings.notify_on.as_deref() == Some("failures") {
            return;
        }
        let message =
            orq_orchestrator::notification_message(&record.feature_id, ending, Some(&detail), None);
        let _ = orq_orchestrator::send_notification_with(
            &argv,
            root,
            &message,
            std::time::Duration::from_secs(30),
            settings.notify_env.as_deref().unwrap_or(&[]),
        );
    }

    /// Runs the clean-up steps of `record` in project `id`, whose main checkout is `root`, in order,
    /// recording each; once one is not done, those after it are skipped.
    fn run_steps(&self, id: &str, root: &Path, store: &RunStore, record: &RunRecord) {
        let target = if record.target_branch.is_empty() {
            project_target(root)
        } else {
            record.target_branch.clone()
        };
        let remote = if record.remote.is_empty() {
            PULL_REMOTE
        } else {
            record.remote.as_str()
        };
        let mut stopped: Option<&str> = None;
        for step in STEPS {
            let outcome = match stopped {
                Some(earlier) => Step::skipped(format!("{} did not finish", step_name(earlier))),
                None => match step {
                    "complete_feature" => self.complete_feature(root, record),
                    "delete_worktree" => delete_worktree(root, record, remote, &target),
                    "delete_branch" => delete_branch(root, record, remote, &target),
                    _ => self.pull_target(id, root, store, record, remote, &target),
                },
            };
            if outcome.state != "done" && stopped.is_none() {
                stopped = Some(step);
            }
            record_step(store, record, step, &outcome);
        }
    }

    /// Marks the run's feature completed in the project's ledger, after snapshotting the ledger.
    fn complete_feature(&self, root: &Path, record: &RunRecord) -> Step {
        let ledger = match orq_git::resolve_project(root) {
            Ok(git) => orq_features::ledger_path(&git),
            Err(error) => return Step::failed(format!("the project could not be read: {error:?}")),
        };
        let snapshot = match self.snapshot_ledger(&ledger, record) {
            Ok(snapshot) => snapshot,
            Err(why) => return Step::failed(format!("the ledger could not be snapshotted: {why}")),
        };
        match orq_features::complete_run_features(
            &ledger,
            std::slice::from_ref(&record.feature_id),
            &utc_now(),
        ) {
            Ok(set) => {
                let reason = set
                    .is_empty()
                    .then(|| format!("{} was already completed", record.feature_id));
                Step {
                    extra: json!({"snapshot": snapshot}),
                    ..Step::done(reason)
                }
            }
            Err(why) => Step::failed(orq_core::redact::redact(&why)),
        }
    }

    /// Copies the ledger at `ledger` under the state root before a clean-up changes it; the copy's file name.
    fn snapshot_ledger(&self, ledger: &Path, record: &RunRecord) -> Result<String, String> {
        let dir = self
            .roots
            .state
            .join("ledger-snapshots")
            .join(&record.project_id);
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let name = format!("{}-{secs}.json", record.run_id);
        std::fs::copy(ledger, dir.join(&name)).map_err(|error| error.to_string())?;
        Ok(name)
    }

    /// Fast-forwards the main checkout's target, only when the checkout is on it; a pull the merge itself
    /// made that brought the target level is the step done, and one that did not is retried.
    fn pull_target(
        &self,
        id: &str,
        root: &Path,
        store: &RunStore,
        record: &RunRecord,
        remote: &str,
        target: &str,
    ) -> Step {
        let pulled = store.events(&record.run_id).ok().and_then(|events| {
            events
                .into_iter()
                .rev()
                .find(|event| event.kind == TARGET_PULLED_EVENT)
        });
        if let Some(pulled) = pulled
            .as_ref()
            .filter(|pulled| pull_may_start(&pulled.payload))
        {
            return Step::done(pulled.payload["message"].as_str().map(str::to_string));
        }
        match current_branch(root) {
            Some(branch) if branch == target => {}
            Some(branch) => {
                return Step::skipped(format!("the main checkout is on {branch}, not {target}"))
            }
            None => return Step::skipped(format!("the main checkout is not on {target}")),
        }
        if pulled.is_some() {
            return match self.pull_target_of(id) {
                Ok(Some(answer)) if pull_may_start(&answer) => {
                    Step::done(answer["message"].as_str().map(str::to_string))
                }
                Ok(Some(answer)) => Step::skipped(
                    answer["message"]
                        .as_str()
                        .unwrap_or("the target could not be pulled")
                        .to_string(),
                ),
                Ok(None) => Step::failed("the project is gone".to_string()),
                Err(why) => Step::failed(orq_core::redact::redact(&why)),
            };
        }
        let outcome = orq_git::pull_target(root, remote, target);
        let message = orq_core::redact::redact(&outcome.describe(remote, target));
        if outcome.may_start() {
            Step::done(Some(message))
        } else {
            Step::skipped(message)
        }
    }
}

/// How a step is named to a person.
fn step_name(step: &str) -> &'static str {
    match step {
        "complete_feature" => "marking the feature completed",
        "delete_worktree" => "deleting the worktree",
        "delete_branch" => "deleting the branch",
        _ => "pulling the target",
    }
}

/// Appends `step`'s outcome to the run's events.
fn record_step(store: &RunStore, record: &RunRecord, step: &str, outcome: &Step) {
    let n = store
        .events(&record.run_id)
        .map_or(0, |events| events.len());
    let mut payload = json!({"state": outcome.state, "reason": outcome.reason.as_deref().map(orq_core::redact::redact)});
    if let Value::Object(extra) = &outcome.extra {
        for (key, value) in extra {
            payload[key] = value.clone();
        }
    }
    let event = Event {
        schema_version: 1,
        event_id: format!("{}-cleanup-{n}", record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: event_kind(step),
        stage: None,
        attempt: 1,
        occurred_at: utc_now(),
        payload,
    };
    if let Err(error) = store.append_event(&event) {
        eprintln!(
            "orq: cannot record {}'s clean-up step {step}: {error:?}",
            record.run_id
        );
    }
}

/// Deletes the run's kept worktree, only when it has no changes and its HEAD is on `remote`'s `target`.
fn delete_worktree(root: &Path, record: &RunRecord, remote: &str, target: &str) -> Step {
    let worktree = &record.worktree;
    if !worktree.exists() {
        return Step::done(Some("the worktree was already deleted".to_string()));
    }
    if let Err(why) = orq_git::fetch_target_base(root, remote, target) {
        return Step::failed(format!(
            "{remote}/{target} could not be fetched: {}",
            orq_core::redact::redact(&why.to_string())
        ));
    }
    match git(
        worktree,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    ) {
        Ok(out) if out.status.success() => {
            let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.get(3..))
                .take(10)
                .map(str::to_string)
                .collect();
            if !files.is_empty() {
                return Step::skipped(format!(
                    "the worktree has uncommitted changes: {}",
                    files.join(", ")
                ));
            }
        }
        Ok(out) => return Step::failed(stderr(&out)),
        Err(why) => return Step::failed(why),
    }
    let tracking = format!("refs/remotes/{remote}/{target}");
    match git(
        worktree,
        &["merge-base", "--is-ancestor", "HEAD", &tracking],
    ) {
        Ok(out) if out.status.success() => {}
        Ok(out) if out.status.code() == Some(1) => {
            return Step::skipped(format!("the worktree has commits not on {remote}/{target}"))
        }
        Ok(out) => return Step::failed(stderr(&out)),
        Err(why) => return Step::failed(why),
    }
    match git(
        root,
        &["worktree", "remove", "--", &worktree.to_string_lossy()],
    ) {
        Ok(out) if out.status.success() => Step::done(None),
        Ok(out) => Step::failed(stderr(&out)),
        Err(why) => Step::failed(why),
    }
}

/// Deletes the run's local branch, only when it is fully merged into `remote`'s `target`.
fn delete_branch(root: &Path, record: &RunRecord, remote: &str, target: &str) -> Step {
    let branch = format!("refs/heads/{}", record.branch);
    let tip = match git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{branch}^{{commit}}"),
        ],
    ) {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Ok(_) => return Step::done(Some(format!("{} was already deleted", record.branch))),
        Err(why) => return Step::failed(why),
    };
    let tracking = format!("refs/remotes/{remote}/{target}");
    match git(root, &["merge-base", "--is-ancestor", &tip, &tracking]) {
        Ok(out) if out.status.success() => {}
        Ok(out) if out.status.code() == Some(1) => {
            return Step::skipped(format!(
                "{} has commits not on {remote}/{target}",
                record.branch
            ))
        }
        Ok(out) => return Step::failed(stderr(&out)),
        Err(why) => return Step::failed(why),
    }
    // Compare-and-swap on the tip that was checked: a branch moved meanwhile is left alone.
    match git(root, &["update-ref", "-d", &branch, &tip]) {
        Ok(out) if out.status.success() => Step::done(None),
        Ok(out) => Step::failed(stderr(&out)),
        Err(why) => Step::failed(why),
    }
}

/// The branch the checkout at `root` is on; `None` when it is detached or git fails.
fn current_branch(root: &Path) -> Option<String> {
    let out = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|error| format!("git could not run: {error}"))
}

fn stderr(out: &Output) -> String {
    orq_core::redact::redact(&orq_git::redact_urls(
        String::from_utf8_lossy(&out.stderr).trim(),
    ))
}
