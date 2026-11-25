//! A project's run queue, as the server works it: each round settles the entries whose runs ended (a
//! completed run's entry leaves, a failed or blocked one stays marked failed) and then starts the
//! first entries whose dependencies are completed, through the same launch as the web's Start, while
//! the queue is not paused, the time is inside its hours, no restart is pending and both the project
//! and the machine have a free run slot. Under a machine run limit each start goes to the project
//! whose waiting entry was added first. A run limit is waited out, even when the start itself fails on
//! it because another run took the slot meanwhile. A full disk or a start that fails otherwise pauses
//! the queue with the reason, and nothing is retried until someone resumes it.

use std::path::Path;

use orq_core::remote_inbox::{LaunchProfiles, QueueChange, RunControlOutcome, RunStart};
use orq_core::store::run_queue::{QueueAction, QueueAddition, QueueKind, QueueProfiles, QueuedFeature, RunQueue};
use orq_core::store::Store;
use orq_features::{FeatureEntry, Ineligible};
use orq_orchestrator::{RunPhase, RunStore};
use serde_json::{json, Value};

use super::{carried_on, discover, read_ledger, start_blocked, Launch, ResumeRequest, StoreProjects};

// Declared here, not in projects.rs, so the commands a merge-local run's merge sets off live in their own file.
#[path = "after_merge.rs"]
mod after_merge;
#[path = "release.rs"]
pub(super) mod release;

/// What a round of the queue needs to know about now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueRound {
    /// The local time, in minutes after midnight, checked against a project's queue hours.
    pub minute_of_day: u16,
    /// Whether the server is waiting to restart: no run is started meanwhile.
    pub restart_pending: bool,
}

/// A run a round of the queues started, which holds its slot for the rest of that round.
struct RoundStart {
    /// The id of the run's project.
    project: String,
    run: String,
    /// The queue entry it was started for.
    feature: String,
}

/// What became of a queued feature's run.
enum Settled {
    /// It is still going (or waits for a review that does not hold the queue): the entry stays as it is.
    Going,
    /// It waits for a person's review, of its open MR at this address or of the run itself (an
    /// escalated review): the entry stays and the entries after it wait.
    InReview(Option<String>),
    /// It completed or was delivered: the entry leaves the queue.
    Done,
    /// It was continued as another run, which the entry now follows.
    ContinuedAs(String),
    /// It failed or was blocked, for this reason.
    Failed(Option<String>),
    /// Its MR was closed without merging: it counts as a failed run.
    MrClosed,
    /// Its runner refused to start it because a run limit was reached: the entry waits to start again.
    LimitRefused,
}

impl StoreProjects {
    /// One round of every project's queue. Each project's failure is logged and the others go on.
    pub fn queue_round(&self, round: &QueueRound) {
        let database = self.roots.database();
        if !database.exists() {
            return;
        }
        after_merge::close_interrupted(&RunStore::new(&self.roots.state), &self.roots.state);
        let store = match Store::open(&database) {
            Ok(store) => store,
            Err(error) => return eprintln!("orq: the run queues cannot be read: {error}"),
        };
        let queued = match store.queued_projects() {
            Ok(queued) if !queued.is_empty() => queued,
            Ok(_) => return,
            Err(error) => return eprintln!("orq: the run queues cannot be read: {error}"),
        };
        let machine = match self.machine_runs_now() {
            Ok(machine) => machine,
            Err(error) => return eprintln!("orq: the run queues cannot be worked: {error}"),
        };
        let mut projects = Vec::new();
        for project in discover(&self.scan_root) {
            let Ok(git) = orq_git::resolve_project(&project.path) else { continue };
            let root = orq_git::main_checkout_root(&git).unwrap_or_else(|| git.root.clone());
            if queued.iter().any(|queued| Path::new(queued) == root) {
                projects.push((project, git, root));
            }
        }
        // Under a machine run limit the slots are scarce: one start at a time, each to the project
        // whose entry that may start now was added first, until a pass starts nothing.
        let one_at_a_time = machine.limits.max_active.is_some();
        let runs = RunStore::new(&self.roots.state);
        // Every run this round started: each holds its project's and the machine's slot for the rest
        // of the round, though its runner may not have recorded it yet.
        let mut round_runs = Vec::new();
        loop {
            if one_at_a_time {
                projects.sort_by_cached_key(|(_, git, root)| oldest_waiting(&store, &runs, git, root));
            }
            let mut started = false;
            for (project, git, root) in &projects {
                match self.project_round(&store, &project.id, git, root, round, &machine, one_at_a_time, &mut round_runs) {
                    Ok(true) if one_at_a_time => {
                        started = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => eprintln!("orq: the run queue of {} stopped: {error}", project.name),
                }
            }
            if !started {
                return;
            }
        }
    }

    /// Whether a run the queue started has ended since the last round, so the server need not wait
    /// for the next one to start what comes after it.
    pub fn queued_run_ended(&self) -> bool {
        let database = self.roots.database();
        let Ok(store) = (if database.exists() { Store::open(&database) } else { return false }) else { return false };
        let runs = RunStore::new(&self.roots.state);
        let Ok(roots) = store.queued_projects() else { return false };
        roots.iter().any(|root| {
            let root = Path::new(root);
            let Ok(queue) = store.queue(root) else { return false };
            let wait = store.project_settings(root).map_or(true, |stored| stored.settings.queue_wait_for_review);
            queue.entries.iter().any(|entry| match (&entry.run, entry.failed) {
                (Some(run), false) => !matches!(settle(&store, &runs, run, None, wait), Settled::Going | Settled::InReview(_)),
                // A failed entry whose run recovered since is settled again at once.
                (Some(run), true) => !matches!(settle(&store, &runs, run, None, wait), Settled::Failed(_) | Settled::MrClosed),
                _ => false,
            })
        })
    }

    /// One round of project `id`'s queue under the `machine`'s limits; with `one`, it starts at most
    /// one run. Every run it starts that takes a slot is added to `round_runs`, the runs the whole
    /// round started. Whether it started any.
    #[allow(clippy::too_many_arguments)]
    fn project_round(&self, store: &Store, id: &str, git: &orq_git::Project, root: &Path, round: &QueueRound, machine: &super::MachineRuns, one: bool, round_runs: &mut Vec<RoundStart>) -> Result<bool, String> {
        let project_id = orq_git::project_id(git);
        let settings = self.settings_of_git(git)?;
        let runs = RunStore::new(&self.roots.state);
        let failed = |e: std::io::Error| e.to_string();
        let mut in_review = false;
        // The first run seen waiting for review, which then holds the starts after it.
        let mut waits_on: Option<String> = None;
        // The queued Accepts this round read as going, as (run, entry): each holds its slot for the whole
        // round, though its tests end partway through, and what they ended with is acted on next round.
        let mut accepts_going = Vec::new();
        for entry in store.queue(root).map_err(failed)?.entries {
            let Some(run) = &entry.run else { continue };
            let mut run = run.clone();
            // A start's run is named by its feature, as before the queue held other actions.
            let what = entry.action.as_ref().map_or_else(|| entry.feature.clone(), action_label);
            // A run this round started is going: its runner may not have recorded it yet.
            let mut settled = match &entry.action {
                _ if round_runs.iter().any(|start| start.run == run) => Settled::Going,
                Some(QueueAction { kind: QueueKind::Accept, .. }) => self.settle_accept(store, &runs, &run, Some(root), settings.queue_wait_for_review),
                _ => settle(store, &runs, &run, Some(root), settings.queue_wait_for_review),
            };
            // A failure already marked is looked at again every round: the run may have recovered since,
            // and then the entry follows it again. The pause its failure caused holds until the run
            // ends well or waits for review, which lifts it below, in this round or a later one.
            if entry.failed {
                match &settled {
                    Settled::Failed(_) | Settled::MrClosed => continue,
                    Settled::ContinuedAs(_) | Settled::LimitRefused => {}
                    Settled::Going | Settled::InReview(_) | Settled::Done => store.queue_started(root, &entry.feature, &run).map_err(failed)?,
                }
            }
            // The entry follows the run it was continued as, as far as it goes, so that a run that
            // already waits for review holds the starts below in this same round.
            for _ in 0..CONTINUATIONS_FOLLOWED {
                let Settled::ContinuedAs(next) = settled else { break };
                store.queue_started(root, &entry.feature, &next).map_err(failed)?;
                settled = settle(store, &runs, &next, Some(root), settings.queue_wait_for_review);
                run = next;
            }
            match settled {
                Settled::Going if matches!(entry.action, Some(QueueAction { kind: QueueKind::Accept, .. })) => {
                    after_merge::watch(&self.roots.state, root, &run);
                    accepts_going.push((run, entry.feature.clone()));
                }
                Settled::Going => after_merge::watch(&self.roots.state, root, &run),
                // A run that ends well or waits for review lifts the pause its own failure caused.
                Settled::InReview(_) => {
                    in_review = true;
                    waits_on.get_or_insert_with(|| run.clone());
                    self.lift_failure_pause(store, id, root, &run, &what, "recovered and waits for review").map_err(failed)?;
                }
                Settled::Done => {
                    after_merge::settled(&self.roots.state, &runs, root, &run, &|| self.release_env(root, &settings));
                    store.queue_remove(root, &entry.feature).map_err(failed)?;
                    self.lift_failure_pause(store, id, root, &run, &what, "recovered and merged").map_err(failed)?;
                    if store.queue(root).map_err(failed)?.entries.is_empty() {
                        self.push_queue_news(id, "queue_finished", None);
                    }
                }
                // A chain longer than one round follows: the run at its end may already wait for
                // review, so a queue that waits for review starts nothing until a later round knows.
                Settled::ContinuedAs(next) => {
                    store.queue_started(root, &entry.feature, &next).map_err(failed)?;
                    in_review |= settings.queue_wait_for_review;
                }
                Settled::Failed(reason) => {
                    store.queue_failed(root, &entry.feature).map_err(failed)?;
                    if settings.queue_stop_on_failure {
                        let why = reason.map_or(String::new(), |reason| format!(": {reason}"));
                        let reason = orq_core::redact::redact(&format!("the run {run} of {what} failed{why}"));
                        store.queue_pause(root, &reason).map_err(failed)?;
                        self.push_queue_news(id, "queue_paused", None);
                    }
                }
                // Nothing failed: the entry waits for a free slot like one never started.
                Settled::LimitRefused => {
                    store.queue_unstarted(root, &entry.feature, &run).map_err(failed)?;
                    LimitWaits::of(&self.roots, git).add(&entry.feature);
                }
                Settled::MrClosed => {
                    store.queue_failed(root, &entry.feature).map_err(failed)?;
                    if settings.queue_stop_on_failure {
                        let reason = orq_core::redact::redact(&format!("{what}'s MR was closed without merging"));
                        store.queue_pause(root, &reason).map_err(failed)?;
                        self.push_queue_news(id, "queue_paused", None);
                    }
                }
            }
        }
        let queue = store.queue(root).map_err(failed)?;
        LimitWaits::of(&self.roots, git).keep_waiting(&queue);
        // A review the queue waits for holds what is left to start: told once for each run it waits on.
        if let Some(run) = waits_on.filter(|_| queue.paused.is_none() && queue.entries.iter().any(|entry| entry.run.is_none())) {
            self.push_queue_news(id, "queue_paused", Some(&run));
        }
        // A merge's release lands, or is refused, before the next run starts: its worktree is cut from the target.
        if queue.paused.is_some() || round.restart_pending || in_review || after_merge::release_pending(root) {
            return Ok(false);
        }
        if let Some(hours) = &settings.queue_hours {
            if !orq_core::project_settings::within_queue_hours(hours, round.minute_of_day) {
                return Ok(false);
            }
        }
        let ledger = read_ledger(&orq_features::ledger_path(git))?;
        let unlanded = unlanded_dependencies(Some(store), &runs, git, &queue, &ledger)?;
        let mut started = false;
        for entry in ordered(&queue.entries, &ledger) {
            if entry.run.is_some() || entry.skipped.is_some() {
                continue;
            }
            match &entry.action {
                None if start_wait(&ledger, &unlanded, &entry.feature).is_err() => continue,
                None => {}
                // An action its run no longer needs is marked so and passed over, never started.
                Some(action) => {
                    if let Err(why) = action_applies(&runs, git, action) {
                        store.queue_skip(root, &entry.feature, &orq_core::redact::redact(&why)).map_err(failed)?;
                        continue;
                    }
                }
            }
            // A start takes a while: what was paused or removed meanwhile holds the starts after it.
            let now = store.queue(root).map_err(failed)?;
            if now.paused.is_some() {
                return Ok(started);
            }
            if !now.entries.iter().any(|current| current.feature == entry.feature && current.run.is_none() && current.skipped.is_none()) {
                continue;
            }
            // A resume of a paused run carries on in the slot that run still holds, so no run limit
            // holds it back; anything else is a start.
            let keeps_slot = match &entry.action {
                Some(action) if action.kind != QueueKind::Accept => !orq_orchestrator::resume_takes_a_slot(&runs, &action.run)?,
                _ => false,
            };
            if let Some(hook) = &self.before_queue_slot_count {
                hook(self);
            }
            // A queued Accept holds a slot while its tests run, though an accepting run holds none; one
            // this round read as going or started holds it until the round ends.
            // A run this round started holds its slot too, though its runner may not have recorded it yet.
            let mut accepting = accepts_going.clone();
            let started_here = round_runs.iter().filter(|start| start.project == project_id).map(|start| (start.run.clone(), start.feature.clone()));
            for going in started_here.chain(self.queued_accepts_going(&now, &runs)) {
                if !accepting.iter().any(|(run, _)| *run == going.0) {
                    accepting.push(going);
                }
            }
            let active = match keeps_slot {
                true => Vec::new(),
                false => {
                    let mut active = runs.active_project_runs(&project_id).map_err(|error| format!("{error:?}"))?;
                    for going in accepting {
                        if !active.iter().any(|(run, _)| *run == going.0) {
                            active.push(going);
                        }
                    }
                    active
                }
            };
            if orq_orchestrator::limit_refusal(&git.name, settings.max_active_runs, &active).is_some() {
                return Ok(started);
            }
            let machine_active = match machine.limits.max_active {
                Some(_) if keeps_slot => Vec::new(),
                Some(_) => {
                    // Every project's queued Accepts that are running their tests hold a machine slot.
                    let mut all = runs.active_machine_runs().map_err(|error| format!("{error:?}"))?;
                    let held = accepts_going.iter().map(|(run, _)| (project_id.clone(), run.clone()));
                    let this_round = round_runs.iter().map(|start| (start.project.clone(), start.run.clone()));
                    for going in held.chain(this_round).chain(self.machine_queued_accepts_going(store, &runs)?) {
                        if !all.iter().any(|(_, run)| *run == going.1) {
                            all.push(going);
                        }
                    }
                    all
                }
                None => Vec::new(),
            };
            if orq_orchestrator::machine_limit_refusal(machine.limits.max_active, &machine_active).is_some() {
                return Ok(started);
            }
            let machine = super::MachineRuns { limits: machine.limits, active: machine_active, malformed: machine.malformed.clone() };
            // A run limit is waited out, even when the launch itself finds it reached by a run started
            // meanwhile and fails with it; anything else that refuses or fails the start pauses the queue.
            let refused = match start_blocked(git, &settings, &active, &machine)? {
                Some(why) => Some(why),
                None if entry.action.is_some() => {
                    let (action, what) = (entry.action.as_ref().expect("an action entry"), entry_label(entry));
                    match self.start_queued_action(id, action) {
                        Ok(RunControlOutcome::Done(run)) => {
                            store.queue_started(root, &entry.feature, &run).map_err(failed)?;
                            LimitWaits::of(&self.roots, git).remove(&entry.feature);
                            after_merge::watch(&self.roots.state, root, &run);
                            if !keeps_slot {
                                round_runs.push(RoundStart { project: project_id.clone(), run: run.clone(), feature: entry.feature.clone() });
                            }
                            if action.kind == QueueKind::Accept {
                                accepts_going.push((run, entry.feature.clone()));
                            }
                            started = true;
                            None
                        }
                        Ok(RunControlOutcome::Refused(_)) if !keeps_slot && self.limit_reached(&runs, git, &settings, &machine)? => return Ok(started),
                        Ok(RunControlOutcome::Refused(why)) => Some(format!("{what} could not start: {why}")),
                        Ok(RunControlOutcome::Unknown) => Some(format!("{what}: {} is not one of the project's runs", action.run)),
                        Err(_) if !keeps_slot && self.limit_reached(&runs, git, &settings, &machine)? => return Ok(started),
                        Err(why) => {
                            eprintln!("{}", launch_error_line(&what, &why));
                            Some(format!("{what} could not start; see the server's log"))
                        }
                    }
                }
                None => 'start: {
                    // A project whose runs end in an MR has its target pulled first, so the run starts
                    // from what was merged on the forge; a target that cannot be brought level holds it.
                    if pulls_target(&settings) {
                        let target = project_target(root);
                        let outcome = orq_git::pull_target(root, PULL_REMOTE, &target);
                        // A forge out of reach for a moment is tried again next round; only a few
                        // rounds in a row of it pause the queue.
                        let failures = FetchFailures::of(&self.roots, git);
                        if matches!(outcome, orq_git::PullOutcome::FetchFailed { .. }) && failures.add() < FETCH_FAILURES_BEFORE_PAUSE {
                            return Ok(started);
                        }
                        failures.clear();
                        if !outcome.may_start() {
                            break 'start Some(format!("{}: {}: pull by hand, then resume", git.name, outcome.describe(PULL_REMOTE, &target)));
                        }
                        // The target is level with origin now, so a merge's warning about it no longer holds.
                        PullWarning::of(&self.roots, git).pulled(PULL_REMOTE, &target);
                    }
                    let profiles = entry.profiles.as_ref().map(|p| LaunchProfiles { implementer: p.implementer.clone(), reviewer: p.reviewer.clone() });
                    match self.launch(id, Launch::Queued(&entry.feature), profiles.as_ref()) {
                        Ok(RunStart::Started(run) | RunStart::StartedWithWarning { run, .. }) => {
                            // Removed while it started, the queue no longer follows the run, which
                            // goes on; removed and added again, the new entry follows this run.
                            store.queue_started(root, &entry.feature, &run).map_err(failed)?;
                            LimitWaits::of(&self.roots, git).remove(&entry.feature);
                            // Watched from its launch, so a run that merges before the next round still gets its go.
                            after_merge::watch(&self.roots.state, root, &run);
                            round_runs.push(RoundStart { project: project_id.clone(), run, feature: entry.feature.clone() });
                            started = true;
                            None
                        }
                        Ok(RunStart::Refused(_)) if self.limit_reached(&runs, git, &settings, &machine)? => return Ok(started),
                        Ok(RunStart::Refused(why)) => Some(format!("{} could not start: {why}", entry.feature)),
                        Ok(RunStart::Unknown) => Some(format!("{} is not in the project's features", entry.feature)),
                        Err(_) if self.limit_reached(&runs, git, &settings, &machine)? => return Ok(started),
                        Err(why) => {
                            eprintln!("{}", launch_error_line(&entry.feature, &why.to_string()));
                            Some(format!("{} could not start; see the server's log", entry.feature))
                        }
                    }
                }
            };
            if let Some(why) = refused {
                store.queue_pause(root, &orq_core::redact::redact(&why)).map_err(failed)?;
                self.push_queue_news(id, "queue_paused", None);
                return Ok(started);
            }
            if started && one {
                return Ok(true);
            }
        }
        Ok(started)
    }

    /// Lifts the pause of project `id`'s queue at `root` when it is the one the failure of `run`, the
    /// run of `what`, caused, and records the lift like a resume, saying how the run `recovered`. A
    /// pause a person set, or one another run's failure caused, is kept.
    /// What a release needs: the project's implementer picks the level, through the injected agent when
    /// there is one.
    fn release_env(&self, root: &Path, settings: &orq_core::project_settings::ProjectSettings) -> release::Env {
        let (harness, model, thinking) = match &settings.implementer {
            Some(agent) => (agent.harness.clone(), agent.model.clone(), agent.effort.clone()),
            None => {
                let fallback = self.model_profiles(root).ok().and_then(|profiles| orq_features::role_fallback(&profiles, &orq_features::TddReviewerConfig::default(), orq_features::AgentRole::Implementer).ok());
                match fallback {
                    Some((model, _)) => (model.harness, model.model, model.thinking),
                    None => ("claude".to_string(), "sonnet".to_string(), None),
                }
            }
        };
        let agent = self.release_agent.clone().unwrap_or_else(|| std::sync::Arc::new(release::HarnessAgent::default()));
        release::Env { roots: self.roots.clone(), agent, harness, model, thinking }
    }

    fn lift_failure_pause(&self, store: &Store, id: &str, root: &Path, run: &str, what: &str, recovered: &str) -> std::io::Result<()> {
        let caused = orq_core::redact::redact(&format!("the run {run} of {what} failed"));
        let Some(paused) = store.queue(root)?.paused else { return Ok(()) };
        if paused != caused && !paused.starts_with(&format!("{caused}: ")) {
            return Ok(());
        }
        store.queue_resume(root)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        let reason = format!("{run} {recovered}");
        let attempt = orq_core::remote_audit::Attempt { identity: "queue", action: "queue.resume", project: id, run: Some(run), outcome: orq_core::remote_audit::Outcome::Allowed, reason: Some(&reason) };
        if let Err(error) = orq_core::remote_audit::record(&self.roots, &attempt, now) {
            eprintln!("orq: the lifted pause of {what}'s queue could not be recorded: {error}");
        }
        Ok(())
    }

    /// Whether project `git` or the machine is now at its run limit: a start refused once another
    /// took the last slot after the queue looked is waited out, not a pause.
    fn limit_reached(&self, runs: &RunStore, git: &orq_git::Project, settings: &orq_core::project_settings::ProjectSettings, machine: &super::MachineRuns) -> Result<bool, String> {
        let active = runs.active_project_runs(&orq_git::project_id(git)).map_err(|error| format!("{error:?}"))?;
        if orq_orchestrator::limit_refusal(&git.name, settings.max_active_runs, &active).is_some() {
            return Ok(true);
        }
        if machine.limits.max_active.is_none() {
            return Ok(false);
        }
        let active = runs.active_machine_runs().map_err(|error| format!("{error:?}"))?;
        Ok(orq_orchestrator::machine_limit_refusal(machine.limits.max_active, &active).is_some())
    }

    /// What became of run `run`, which a queued Accept accepts: going while its tests run, failed
    /// when the accept failed, and otherwise as [`settle`] says.
    fn settle_accept(&self, store: &Store, runs: &RunStore, run: &str, repo: Option<&Path>, wait: bool) -> Settled {
        if self.being_accepted(run).is_some() {
            return Settled::Going;
        }
        if runs.events(run).is_ok_and(|events| orq_orchestrator::summarize_run(&events).phase == RunPhase::Accepting) {
            return Settled::Going;
        }
        match accept_failed(runs, run) {
            Some(reason) => Settled::Failed(Some(format!("its accept failed: {reason}"))),
            None => settle(store, runs, run, repo, wait),
        }
    }

    /// The runs of `queue`'s Accepts whose tests are running, as (run, entry) like the active runs.
    fn queued_accepts_going(&self, queue: &RunQueue, runs: &RunStore) -> Vec<(String, String)> {
        queue
            .entries
            .iter()
            .filter(|entry| matches!(entry.action, Some(QueueAction { kind: QueueKind::Accept, .. })) && !entry.failed)
            .filter_map(|entry| {
                let run = entry.run.as_ref()?;
                let accepting = self.being_accepted(run).is_some() || runs.events(run).is_ok_and(|events| orq_orchestrator::summarize_run(&events).phase == RunPhase::Accepting);
                accepting.then(|| (run.clone(), entry.feature.clone()))
            })
            .collect()
    }

    /// The queued Accepts of every project's queue in `store` whose tests are running, as (project id,
    /// run id) like the machine's active runs.
    fn machine_queued_accepts_going(&self, store: &Store, runs: &RunStore) -> Result<Vec<(String, String)>, String> {
        let mut going = Vec::new();
        for root in store.queued_projects().map_err(|error| error.to_string())? {
            let queue = store.queue(Path::new(&root)).map_err(|error| error.to_string())?;
            for (run, _) in self.queued_accepts_going(&queue, runs) {
                let project = runs.load(&run).map_or_else(|_| String::new(), |record| record.project_id);
                going.push((project, run));
            }
        }
        Ok(going)
    }

    /// Starts queued `action` in project `id` the way the web's Resume, Review again and Accept do:
    /// the run that carries on (the accepted run itself, for an Accept), or why it may not.
    fn start_queued_action(&self, id: &str, action: &QueueAction) -> Result<RunControlOutcome, String> {
        let Some((project, store)) = self.own_run(id, &action.run)? else { return Ok(RunControlOutcome::Unknown) };
        match action.kind {
            QueueKind::Accept => self.accept(&project.path, store, &action.run),
            kind => {
                if let Some(busy) = self.being_accepted(&action.run) {
                    return Ok(RunControlOutcome::Refused(busy));
                }
                let Some(launcher) = &self.launcher else { return Ok(RunControlOutcome::Refused("this server cannot resume runs".to_string())) };
                let request = ResumeRequest { answer: action.answer.as_deref(), review_again: kind == QueueKind::ReviewAgain };
                carried_on(launcher.resume_with(&project.path, &action.run, &request), &project.path)
            }
        }
    }

    /// What the review refresh does once it has looked at `run_id`'s review: the first time it sees the
    /// MR merged, in a project whose runs end in an MR and that pulls its target, the target is pulled,
    /// fast-forward only. The run records `target.pulled`, the audit log `project.pull`, and a pull that
    /// could not bring the target level is shown on the project until one does.
    pub fn review_looked_at(&self, run_id: &str) {
        let runs = RunStore::new(&self.roots.state);
        let Ok(record) = runs.load(run_id) else { return };
        let Ok(store) = Store::open_read_only(&self.roots.database()) else { return };
        let latest = store.lifecycles_for_run(run_id).ok().and_then(|all| all.into_iter().filter(|l| l.provider != orq_core::review_lifecycle::LOCAL_PROVIDER).last());
        if let Some(review) = &latest {
            self.push_review_news(run_id, &record, review);
        }
        let Some(review) = latest.filter(|review| review.state == orq_core::review_lifecycle::ProviderState::Merged) else { return };
        // Only a merged review reads the run's events, to pull its merge once and clean up once.
        let Ok(events) = runs.events(run_id) else { return };
        let pulled = events.iter().any(|event| event.kind == TARGET_PULLED_EVENT);
        let cleaned = events.iter().any(|event| event.kind.starts_with("cleanup."));
        if pulled && cleaned {
            return;
        }
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| {
            orq_git::resolve_project(&project.path).is_ok_and(|git| orq_git::project_id(&git) == record.project_id)
        }) else {
            return;
        };
        let Ok(Some((git, root))) = self.settings_key(&project.id) else { return };
        let Ok(settings) = self.settings_of_git(&git) else { return };
        if !pulled && pulls_target(&settings) {
            self.pull_merged_target(run_id, &record, &review, &project.id, &git, &root);
        }
        if !cleaned && settings.after_merge == orq_core::project_settings::AfterMerge::Auto {
            self.auto_clean_up(&project.id, &root, &runs, &record, &review_ref(&review));
        }
    }

    /// Tells the subscribed devices that project `id`'s queue paused or ran out, in the background. A pause for
    /// the review of `waiting_on` is told once for that run; any other is told when it happens.
    fn push_queue_news(&self, id: &str, kind: &'static str, waiting_on: Option<&str>) {
        let event_id = waiting_on.map_or_else(|| kind.to_string(), |run| format!("{kind}:{run}"));
        let once = waiting_on.map(|_| event_id.clone());
        let push = crate::web_push::Push { kind, project_id: id.to_string(), run_id: None, event_id: Some(event_id), once };
        drop(crate::web_push::notify_later(&self.roots, push));
    }

    /// Tells the subscribed devices, once each, that `run_id`'s MR merged or went stale after a push to it, in
    /// the background.
    fn push_review_news(&self, run_id: &str, record: &orq_orchestrator::RunRecord, review: &orq_core::review_lifecycle::ReviewLifecycle) {
        use orq_core::review_lifecycle::ProviderState;
        let (kind, what) = match review.state {
            ProviderState::Merged => ("merged", "merged".to_string()),
            ProviderState::Open if review.stale => ("review", format!("stale:{}", review.observed_head_sha.as_deref().unwrap_or_default())),
            _ => return,
        };
        let Some(project_id) = self.web_project_of(&record.project_id) else { return };
        let push = crate::web_push::Push { kind, project_id, run_id: Some(run_id.to_string()), event_id: Some(what.clone()), once: Some(what) };
        drop(crate::web_push::notify_later(&self.roots, push));
    }

    /// Pulls the target `review` merged into, fast-forward only, and records how it went.
    fn pull_merged_target(&self, run_id: &str, record: &orq_orchestrator::RunRecord, review: &orq_core::review_lifecycle::ReviewLifecycle, project_id: &str, git: &orq_git::Project, root: &std::path::Path) {
        let runs = RunStore::new(&self.roots.state);
        // The MR merged into the run's own target, on its own remote, which a run started with a
        // different target than the project's names in its record.
        let target = if record.target_branch.is_empty() { project_target(&root) } else { record.target_branch.clone() };
        let remote = if record.remote.is_empty() { PULL_REMOTE } else { record.remote.as_str() };
        let outcome = orq_git::pull_target(&root, remote, &target);
        let message = orq_core::redact::redact(&outcome.describe(remote, &target));
        let mut payload = json!({"outcome": outcome.kind(), "target": target, "message": message, "review": review_ref(&review)});
        if let orq_git::PullOutcome::FastForwarded { from, to } = &outcome {
            payload["from"] = json!(from);
            payload["to"] = json!(to);
        }
        let event = orq_orchestrator::Event {
            schema_version: 1,
            event_id: format!("{run_id}-target-pulled"),
            run_id: run_id.to_string(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: TARGET_PULLED_EVENT.to_string(),
            stage: None,
            attempt: 1,
            occurred_at: utc_now(),
            payload,
        };
        if let Err(error) = runs.append_event(&event) {
            eprintln!("orq: cannot record that {run_id}'s target was pulled: {error:?}");
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        let attempt = orq_core::remote_audit::Attempt { identity: "orq serve", action: "project.pull", project: project_id, run: Some(run_id), outcome: orq_core::remote_audit::Outcome::Allowed, reason: Some(&message) };
        if let Err(error) = orq_core::remote_audit::record(&self.roots, &attempt, now) {
            eprintln!("orq: cannot audit the pull of {project_id}'s target: {error}");
        }
        let warning = PullWarning::of(&self.roots, git);
        match outcome {
            orq_git::PullOutcome::FastForwarded { .. } | orq_git::PullOutcome::UpToDate | orq_git::PullOutcome::Ahead { .. } | orq_git::PullOutcome::NoRemote => warning.pulled(remote, &target),
            _ => warning.set(remote, &target, &format!("{target} not updated after {} merged: {message}", review_ref(&review))),
        }
    }

    /// Pulls project `id`'s target again, as a person asked: the target a merge's warning is about, or
    /// else the project's; what came of it, and whether the warning is gone. `None` for an unknown project.
    pub(super) fn pull_target_of(&self, id: &str) -> Result<Option<Value>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        let warning = PullWarning::of(&self.roots, &git);
        let (remote, target) = warning.about().unwrap_or_else(|| (PULL_REMOTE.to_string(), project_target(&root)));
        let outcome = orq_git::pull_target(&root, &remote, &target);
        if outcome.may_start() {
            warning.pulled(&remote, &target);
        }
        let message = orq_core::redact::redact(&outcome.describe(&remote, &target));
        Ok(Some(json!({"outcome": outcome.kind(), "message": message, "target_pull_warning": warning.read()})))
    }

    /// Clears project `id`'s target pull warning once `target` was pulled level from `remote`, when that is what it is about.
    pub(super) fn target_pulled(&self, id: &str, remote: &str, target: &str) -> Result<(), String> {
        if let Some((git, _)) = self.settings_key(id)? {
            PullWarning::of(&self.roots, &git).pulled(remote, target);
        }
        Ok(())
    }

    /// Project `id`'s queue as the web shows it; `None` when there is no such project.
    pub(super) fn queue_answer(&self, id: &str) -> Result<Option<Value>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        let queue = self.read_queue(&root)?;
        let wait = self.settings_of_git(&git)?.queue_wait_for_review;
        let ledger = read_ledger(&orq_features::ledger_path(&git))?;
        let mut review = if self.roots.database().exists() {
            let store = Store::open(&self.roots.database()).map_err(|error| error.to_string())?;
            in_review(&store, &RunStore::new(&self.roots.state), &queue, &git, wait)
        } else {
            std::collections::HashMap::new()
        };
        // An Accept whose tests run here is running, though its run may not have recorded it yet.
        review.retain(|feature, _| {
            !queue.entries.iter().any(|entry| {
                entry.feature == *feature && matches!(entry.action, Some(QueueAction { kind: QueueKind::Accept, .. })) && entry.run.as_deref().is_some_and(|run| self.being_accepted(run).is_some())
            })
        });
        let store = if self.roots.database().exists() { Some(Store::open(&self.roots.database()).map_err(|error| error.to_string())?) } else { None };
        let unlanded = unlanded_dependencies(store.as_ref(), &RunStore::new(&self.roots.state), &git, &queue, &ledger)?;
        let paused = paused_runs(&RunStore::new(&self.roots.state), &queue);
        let mut answer = queue_json_with(&queue, &ledger, &review, &paused, &unlanded, &LimitWaits::of(&self.roots, &git).entries());
        if let Some(warning) = PullWarning::of(&self.roots, &git).read() {
            answer["target_pull_warning"] = json!(warning);
        }
        if let Some(unpushed) = super::cleanup::unpushed(&root) {
            answer["unpushed"] = unpushed;
        }
        Ok(Some(answer))
    }

    /// Why a start of `feature` in project `git`, whose ledger is `ledger`, would wait in the queue
    /// for a dependency whose work has not landed yet, as the queue page says it; `None` when none.
    pub(super) fn dependency_wait(&self, git: &orq_git::Project, ledger: &[FeatureEntry], feature: &str) -> Result<Option<String>, String> {
        let store = if self.roots.database().exists() { Some(Store::open(&self.roots.database()).map_err(|error| error.to_string())?) } else { None };
        let unlanded = unlanded_of(store.as_ref(), &RunStore::new(&self.roots.state), git, ledger, std::iter::once(feature))?;
        Ok(start_wait(ledger, &unlanded, feature).err())
    }

    fn read_queue(&self, root: &Path) -> Result<RunQueue, String> {
        let database = self.roots.database();
        if !database.exists() {
            return Ok(RunQueue::default());
        }
        Store::open(&database).and_then(|store| store.queue(root)).map_err(|error| error.to_string())
    }

    /// Changes project `id`'s queue from the web.
    /// Whether `feature` of project `id` is to be queued once approved; a wish whose feature left the ledger is
    /// dropped. orq.db is not created for a read.
    pub(super) fn queue_wish_of(&self, id: &str, feature: &str) -> Result<Option<bool>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        if !self.roots.database().exists() {
            return Ok(Some(false));
        }
        let store = Store::open(&self.roots.database()).map_err(|error| error.to_string())?;
        if !store.queue_wish(&root, feature).map_err(|error| error.to_string())? {
            return Ok(Some(false));
        }
        let listed = read_ledger(&orq_features::ledger_path(&git))?.iter().any(|entry| entry.feature_name == feature);
        if !listed {
            store.queue_wish_drop(&root, feature).map_err(|error| error.to_string())?;
        }
        Ok(Some(listed))
    }

    /// Asks (`wanted`) or no longer asks for `feature` of project `id` to be queued once approved; only a draft in
    /// the ledger can be asked for (`Some(Err(why))` otherwise).
    pub(super) fn set_queue_wish_of(&self, id: &str, feature: &str, wanted: bool) -> Result<Option<Result<(), String>>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        if wanted {
            let ledger = read_ledger(&orq_features::ledger_path(&git))?;
            if !ledger.iter().any(|entry| entry.feature_name == feature && entry.status.known() == Some(orq_core::FeatureStatus::Draft)) {
                return Ok(Some(Err(format!("{feature} is not a draft in this project"))));
            }
        }
        let store = Store::open(&self.roots.database()).map_err(|error| error.to_string())?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64);
        if wanted {
            store.queue_wish_set(&root, feature, now).map_err(|error| error.to_string())?;
        } else {
            store.queue_wish_drop(&root, feature).map_err(|error| error.to_string())?;
        }
        Ok(Some(Ok(())))
    }

    pub(super) fn change_queue_of(&self, id: &str, change: &QueueChange) -> Result<RunControlOutcome, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(RunControlOutcome::Unknown) };
        if let QueueChange::Add { profiles: Some(chosen), .. } = change {
            let listed = self.model_profiles(&git.root)?;
            for name in [&chosen.implementer, &chosen.reviewer] {
                if !listed.profiles().iter().any(|profile| &profile.name == name) {
                    return Ok(RunControlOutcome::Refused(orq_core::redact::redact(&format!("the profile {name} is not defined in the model profiles"))));
                }
            }
        }
        let store = Store::open(&self.roots.database()).map_err(|error| error.to_string())?;
        Ok(match change_queue(&store, &RunStore::new(&self.roots.state), &git, &root, change, "web")? {
            Ok(done) => RunControlOutcome::Done(done),
            Err(why) => RunControlOutcome::Refused(orq_core::redact::redact(&why)),
        })
    }
}

/// How many features project `git`'s queue in `store` holds and whether it is paused, for the
/// dashboard card; without orq.db, none.
/// With `wait` (the project waits for each run's review), it names the feature whose review the
/// queue waits for, read from `runs` without asking git.
pub(super) fn queue_summary(roots: &orq_core::storage::StorageRoots, store: Option<&Store>, runs: &RunStore, git: &orq_git::Project, wait: bool) -> Result<Value, String> {
    let mut summary = queue_counts(store, runs, git, wait)?;
    if let Some(warning) = PullWarning::of(roots, git).read() {
        summary["target_pull_warning"] = json!(warning);
    }
    Ok(summary)
}

fn queue_counts(store: Option<&Store>, runs: &RunStore, git: &orq_git::Project, wait: bool) -> Result<Value, String> {
    let root = orq_git::main_checkout_root(git).unwrap_or_else(|| git.root.clone());
    let queue = store.map(|store| store.queue(&root)).transpose().map_err(|error| error.to_string())?.unwrap_or_default();
    let waiting_for_review = store.filter(|_| wait).and_then(|store| {
        queue.entries.iter().filter(|entry| !entry.failed).find_map(|entry| {
            let run = entry.run.as_ref()?;
            matches!(settle(store, runs, run, None, true), Settled::InReview(_)).then(|| entry.feature.clone())
        })
    });
    Ok(json!({"count": queue.entries.len(), "paused": queue.paused.is_some(), "waiting_for_review": waiting_for_review}))
}

/// Applies `change` to the queue of project `git`, whose main checkout is `root`, as `by` (`web` or
/// `cli`) asked: what was done, or why it may not be.
/// A run action is checked against the run in `runs`: it must be one of the project's runs that the
/// action applies to now.
pub fn change_queue(store: &Store, runs: &RunStore, git: &orq_git::Project, root: &Path, change: &QueueChange, by: &str) -> Result<Result<String, String>, String> {
    let failed = |e: std::io::Error| e.to_string();
    Ok(match change {
        QueueChange::Add { features, profiles } => {
            let ledger = read_ledger(&orq_features::ledger_path(git))?;
            for feature in features {
                match ledger.iter().find(|entry| &entry.feature_name == feature) {
                    None => return Ok(Err(format!("{feature} is not one of the project's features"))),
                    Some(entry) if entry.status.as_str() != "pending" => {
                        return Ok(Err(format!("{feature} is {}: only an approved (pending) feature can be queued", entry.status.as_str())));
                    }
                    Some(_) => {}
                }
            }
            let profiles = profiles.as_ref().map(|p| QueueProfiles { implementer: p.implementer.clone(), reviewer: p.reviewer.clone() });
            let additions: Vec<QueueAddition> = features.iter().map(|feature| QueueAddition { feature: feature.clone(), profiles: profiles.clone() }).collect();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64);
            let added = match store.queue_add(root, &additions, by, now) {
                Ok(added) => added,
                Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => return Ok(Err(error.to_string())),
                Err(error) => return Err(error.to_string()),
            };
            if added.is_empty() {
                Err("those features are queued already".to_string())
            } else {
                Ok(added.join(", "))
            }
        }
        QueueChange::AddAction(action) => {
            let label = action_label(action);
            match runs.load(&action.run) {
                Ok(record) if record.project_id == orq_git::project_id(git) => {}
                _ => return Ok(Err(format!("{} is not one of the project's runs", action.run))),
            }
            if let Err(why) = action_applies(runs, git, action) {
                return Ok(Err(format!("{label} cannot be queued: {why}")));
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64);
            match store.queue_add_action(root, action, by, now) {
                Ok(Some(_)) => Ok(label),
                Ok(None) => Err(format!("{label} is queued already")),
                Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Err(error.to_string()),
                Err(error) => return Err(error.to_string()),
            }
        }
        QueueChange::Remove(feature) => {
            if store.queue_remove(root, feature).map_err(failed)? {
                Ok(feature.clone())
            } else {
                Err(format!("{feature} is not queued"))
            }
        }
        QueueChange::Reorder(order) => match store.queue_reorder(root, order) {
            Ok(()) => Ok(order.join(", ")),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Err(error.to_string()),
            Err(error) => return Err(error.to_string()),
        },
        QueueChange::Pause => {
            store.queue_pause(root, &format!("paused from the {by}")).map_err(failed)?;
            Ok("paused".to_string())
        }
        QueueChange::Resume => {
            store.queue_resume(root).map_err(failed)?;
            Ok("resumed".to_string())
        }
    })
}

/// Project `git`'s queue in `store`, whose main checkout is `root`, as `orq queue list` shows it: as
/// the page does, with the runs in `runs` that wait for review shown so when the project waits for them.
pub fn project_queue_json(store: &Store, runs: &RunStore, git: &orq_git::Project, root: &Path) -> Result<Value, String> {
    let queue = store.queue(root).map_err(|error| format!("cannot read the queue: {error}"))?;
    let ledger = read_ledger(&orq_features::ledger_path(git))?;
    let wait = store.project_settings(root).map_or(true, |stored| stored.settings.queue_wait_for_review);
    let unlanded = unlanded_dependencies(Some(store), runs, git, &queue, &ledger)?;
    Ok(queue_json_with(&queue, &ledger, &in_review(store, runs, &queue, git, wait), &paused_runs(runs, &queue), &unlanded, &[]))
}

/// For the features queued to start, why each dependency that has a run in `runs` has not landed yet
/// where the next run starts from: the dependency's name to the reason, read from its newest run. A
/// dependency whose work landed, or that never ran, is not named.
fn unlanded_dependencies(store: Option<&Store>, runs: &RunStore, git: &orq_git::Project, queue: &RunQueue, ledger: &[FeatureEntry]) -> Result<std::collections::HashMap<String, String>, String> {
    let starts = queue.entries.iter().filter(|entry| entry.run.is_none() && entry.action.is_none() && entry.skipped.is_none());
    unlanded_of(store, runs, git, ledger, starts.map(|entry| entry.feature.as_str()))
}

/// Why each dependency of `features` that has a run in `runs` has not landed yet, as
/// [`unlanded_dependencies`] gives it.
fn unlanded_of<'f>(store: Option<&Store>, runs: &RunStore, git: &orq_git::Project, ledger: &[FeatureEntry], features: impl Iterator<Item = &'f str>) -> Result<std::collections::HashMap<String, String>, String> {
    let root = orq_git::main_checkout_root(git).unwrap_or_else(|| git.root.clone());
    let mut deps: Vec<&str> = Vec::new();
    for feature in features {
        let Some(listed) = ledger.iter().find(|listed| listed.feature_name == feature) else { continue };
        for dep in &listed.depends_on {
            if !deps.contains(&dep.as_str()) {
                deps.push(dep);
            }
        }
    }
    let newest = orq_orchestrator::newest_runs_of(runs, &orq_git::project_id(git), &deps)?;
    let mut unlanded = std::collections::HashMap::new();
    for (dep, run) in newest {
        if let Some(why) = landing(store, runs, &root, &run) {
            unlanded.insert(dep, why);
        }
    }
    Ok(unlanded)
}

/// Why the work of run `run`, followed through the runs it was continued as, has not landed yet in
/// the project whose main checkout is `root`; `None` once it has. A run delivered for review has
/// landed once the newest MR `store` holds for it merged and the local target holds that MR's head.
fn landing(store: Option<&Store>, runs: &RunStore, root: &Path, run: &str) -> Option<String> {
    let mut run = run.to_string();
    for _ in 0..CONTINUATIONS_FOLLOWED {
        // A run that cannot be read gives no sign its work landed.
        let events = match runs.events(&run) {
            Ok(events) => events,
            Err(orq_orchestrator::StoreError::NotFound(_)) => return Some("discarded".to_string()),
            Err(_) => return Some("its run cannot be read".to_string()),
        };
        let summary = orq_orchestrator::summarize_live_run(&events);
        if let Some(next) = summary.resumed_as {
            run = next;
            continue;
        }
        return match summary.phase {
            RunPhase::Running | RunPhase::PauseRequested | RunPhase::Paused | RunPhase::Accepting => Some("running".to_string()),
            RunPhase::Failed | RunPhase::Blocked => Some("failed".to_string()),
            RunPhase::NeedsReview => Some("in review".to_string()),
            RunPhase::DesignFilesNeeded => Some("waiting for design files".to_string()),
            RunPhase::Completed if matches!(ending(&events), Some("review_ready" | "pushed")) => mr_landing(store, runs, root, &run, &events),
            RunPhase::Completed | RunPhase::Merged => None,
        };
    }
    Some("running".to_string())
}

/// Why run `run`, delivered for review, has not landed yet, from the newest MR `store` holds for it
/// and the run's `events`.
fn mr_landing(store: Option<&Store>, runs: &RunStore, root: &Path, run: &str, events: &[orq_orchestrator::Event]) -> Option<String> {
    use orq_core::review_lifecycle::{ProviderState, LOCAL_PROVIDER};
    let Some(Ok(all)) = store.map(|store| store.lifecycles_for_run(run)) else { return Some("in review".to_string()) };
    // No MR recorded yet is no sign the work landed; a review kept here has landed once it was merged
    // into the target locally.
    let Some(mr) = all.iter().rev().find(|lifecycle| lifecycle.provider != LOCAL_PROVIDER).cloned() else {
        let merged_here = all.iter().any(|lifecycle| lifecycle.provider == LOCAL_PROVIDER && lifecycle.state == ProviderState::Merged);
        return (!merged_here).then(|| "in review".to_string());
    };
    match mr.state {
        ProviderState::Open => Some("in review".to_string()),
        ProviderState::Closed => Some("MR closed without merging".to_string()),
        ProviderState::Merged => {
            let head = mr.observed_head_sha.as_deref().unwrap_or(&mr.reported_head_sha);
            let target = runs.load(run).ok().map(|record| record.target_branch).filter(|target| !target.is_empty()).unwrap_or_else(|| project_target(root));
            // A squash or rebase merge never holds the head itself; the pull the refresh made after
            // the merge, when it brought the target level with its remote, says the merge is in. A pull
            // that found no remote brought nothing in, and one made after an earlier MR merged says
            // nothing of this one.
            let this_mr = review_ref(&mr);
            let pulled = || {
                events.iter().any(|event| {
                    event.kind == TARGET_PULLED_EVENT && event.payload["review"].as_str() == Some(this_mr.as_str()) && matches!(event.payload["outcome"].as_str(), Some("fast_forwarded" | "up_to_date" | "ahead"))
                })
            };
            let landed = target_holds(root, &target, head) || pulled();
            (!landed).then(|| "merged, not pulled".to_string())
        }
    }
}

/// Whether the local branch `target` of the repository at `root` holds commit `sha`.
fn target_holds(root: &Path, target: &str, sha: &str) -> bool {
    if sha.is_empty() || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--is-ancestor", sha, &format!("refs/heads/{target}")])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Whether a start of `feature` may go now: the ledger's rule, and every dependency's work landed
/// (none named in `unlanded`). Why not, as the queue shows it: `waiting for A (running), B`.
fn start_wait(ledger: &[FeatureEntry], unlanded: &std::collections::HashMap<String, String>, feature: &str) -> Result<(), String> {
    let mut waiting: Vec<String> = match orq_features::check_run_eligibility(ledger, feature) {
        Ok(_) => Vec::new(),
        Err(Ineligible::UnmetDependencies(unmet)) => unmet.iter().map(|dep| dep.name.clone()).collect(),
        Err(why) => return Err(why.to_string()),
    };
    let depends_on = ledger.iter().find(|entry| entry.feature_name == feature).map_or(&[][..], |entry| &entry.depends_on[..]);
    for dep in depends_on {
        if unlanded.contains_key(dep) && !waiting.contains(dep) {
            waiting.push(dep.clone());
        }
    }
    if waiting.is_empty() {
        return Ok(());
    }
    let named: Vec<String> = waiting.iter().map(|dep| unlanded.get(dep).map_or_else(|| dep.clone(), |why| format!("{dep} ({why})"))).collect();
    Err(format!("waiting for {}", named.join(", ")))
}

/// Why the last accept of run `run` failed, when the run is still where that failure left it.
fn accept_failed(runs: &RunStore, run: &str) -> Option<String> {
    let summary = orq_orchestrator::summarize_run(&runs.events(run).ok()?);
    let stopped = matches!(summary.phase, RunPhase::NeedsReview | RunPhase::Failed | RunPhase::Blocked) && summary.resumed_as.is_none();
    summary.accept_failure.filter(|_| stopped).map(|failure| failure.reason)
}

/// The entries of `queue` whose run waits for a person's review, each to its open MR's address, if
/// any; none unless the project is to `wait` for review.
fn in_review(store: &Store, runs: &RunStore, queue: &RunQueue, git: &orq_git::Project, wait: bool) -> std::collections::HashMap<String, Option<String>> {
    let mut review = std::collections::HashMap::new();
    for entry in queue.entries.iter().filter(|entry| !entry.failed) {
        // A queued Accept whose tests failed is a failure, not a run waiting for review.
        let accepting = matches!(entry.action, Some(QueueAction { kind: QueueKind::Accept, .. }));
        if accepting && entry.run.as_deref().is_some_and(|run| accept_failed(runs, run).is_some()) {
            continue;
        }
        if let Some(Settled::InReview(url)) = entry.run.as_ref().map(|run| settle(store, runs, run, Some(git.root.as_path()), wait)) {
            review.insert(entry.feature.clone(), url);
        }
    }
    review
}

/// The queue as the page and `orq queue list` show it: entries in the order they will start, each
/// with its state (`running`, `paused`, `review`, `failed`, `next`, `queued`, `waiting` or `skipped`) and
/// why it waits or is paused.
pub fn queue_json(queue: &RunQueue, ledger: &[FeatureEntry]) -> Value {
    queue_json_with(queue, ledger, &std::collections::HashMap::new(), &std::collections::HashMap::new(), &std::collections::HashMap::new(), &[])
}

/// The entries of `queue` with a run that has not failed whose run, followed through the runs it was
/// resumed as, is paused: each feature to why, as the queue shows it. A run that is running, being
/// accepted, waiting for review or ended is not named.
fn paused_runs(runs: &RunStore, queue: &RunQueue) -> std::collections::HashMap<String, &'static str> {
    queue
        .entries
        .iter()
        .filter(|entry| !entry.failed && entry.skipped.is_none())
        .filter_map(|entry| Some((entry.feature.clone(), paused_reason(runs, entry.run.as_deref()?)?)))
        .collect()
}

/// Why run `run`, followed through the runs it was resumed as, is paused; `None` when it is not.
fn paused_reason(runs: &RunStore, run: &str) -> Option<&'static str> {
    let mut run = run.to_string();
    for _ in 0..CONTINUATIONS_FOLLOWED {
        let summary = orq_orchestrator::summarize_run(&runs.events(&run).ok()?);
        if let Some(next) = summary.resumed_as {
            run = next;
            continue;
        }
        return match summary.phase {
            RunPhase::Paused => Some("Paused: resume it from the run page"),
            RunPhase::PauseRequested => Some("Pausing"),
            RunPhase::DesignFilesNeeded => Some("Waiting for design files: add the files the run page lists, then resume"),
            _ => None,
        };
    }
    None
}

/// [`queue_json`], with the entries whose run waits for a person's review (`review`: feature to its
/// open MR's address, if any) shown waiting for review, those whose dependency's work has not
/// landed (`unlanded`: dependency to why) shown waiting for it, and those whose start was refused
/// for a run limit (`limit_waits`) shown waiting for a slot, and those whose run is paused
/// (`paused`: feature to why, see [`paused_runs`]) shown paused.
fn queue_json_with(queue: &RunQueue, ledger: &[FeatureEntry], review: &std::collections::HashMap<String, Option<String>>, paused: &std::collections::HashMap<String, &'static str>, unlanded: &std::collections::HashMap<String, String>, limit_waits: &[String]) -> Value {
    let mut next_given = false;
    let mut waiting_for_review = None;
    let entries: Vec<Value> = ordered(&queue.entries, ledger)
        .into_iter()
        .map(|entry| {
            let eligible = || match &entry.action {
                Some(_) => Ok(()),
                None => start_wait(ledger, unlanded, &entry.feature),
            };
            let (state, reason) = match (&entry.run, entry.failed) {
                _ if entry.skipped.is_some() => ("skipped", entry.skipped.as_ref().map(|why| format!("No longer needed: {why}"))),
                (Some(_), true) => ("failed", None),
                (Some(_), false) if review.contains_key(&entry.feature) => ("review", None),
                (Some(_), false) if paused.contains_key(&entry.feature) => ("paused", paused.get(&entry.feature).map(|why| why.to_string())),
                (Some(_), false) => ("running", None),
                (None, _) => match eligible() {
                    // Still the one to start next, once a slot frees.
                    Ok(_) if limit_waits.contains(&entry.feature) => {
                        next_given = true;
                        ("waiting", Some(LIMIT_WAIT_REASON.to_string()))
                    }
                    Ok(_) if !next_given => {
                        next_given = true;
                        ("next", None)
                    }
                    Ok(_) => ("queued", None),
                    Err(why) => ("waiting", Some(why)),
                }
            };
            let profiles = entry.profiles.as_ref();
            let mr_url = review.get(&entry.feature).cloned().flatten().map(|url| orq_core::redact::redact(&url));
            if state == "review" && waiting_for_review.is_none() {
                waiting_for_review = Some(json!({"feature": entry.feature, "label": entry_label(entry), "mr_url": mr_url}));
            }
            let action = entry.action.as_ref();
            json!({
                "feature": entry.feature,
                "kind": action.map_or("start", |action| action.kind.as_str()),
                "label": entry_label(entry),
                "target_run": action.map(|action| &action.run),
                "answer": action.and_then(|action| action.answer.as_ref()).map(|answer| orq_core::redact::redact(answer)),
                "state": state,
                "reason": reason.map(|reason| orq_core::redact::redact(&reason)),
                "run": entry.run,
                "added_at": entry.added_at,
                "added_by": entry.added_by,
                "implementer": profiles.map(|p| &p.implementer),
                "reviewer": profiles.map(|p| &p.reviewer),
                "mr_url": mr_url,
            })
        })
        .collect();
    json!({"entries": entries, "count": entries.len(), "paused": queue.paused, "waiting_for_review": waiting_for_review})
}

/// The entries in the order they may start: one never comes before a queued feature it depends on,
/// and otherwise the queue's own order holds.
fn ordered<'a>(entries: &'a [QueuedFeature], ledger: &[FeatureEntry]) -> Vec<&'a QueuedFeature> {
    use std::collections::{BTreeSet, HashMap};
    let depends_on: HashMap<&str, &[String]> = ledger.iter().map(|entry| (entry.feature_name.as_str(), &entry.depends_on[..])).collect();
    let position: HashMap<&str, usize> = entries.iter().enumerate().map(|(at, entry)| (entry.feature.as_str(), at)).collect();
    // For each queued feature, how many of its dependencies are still queued before it, and which
    // queued features wait on it.
    let mut waiting_on = vec![0usize; entries.len()];
    let mut dependents = vec![Vec::new(); entries.len()];
    for (at, entry) in entries.iter().enumerate() {
        for dep in depends_on.get(entry.feature.as_str()).copied().unwrap_or_default() {
            if let Some(&of) = position.get(dep.as_str()) {
                waiting_on[at] += 1;
                dependents[of].push(at);
            }
        }
    }
    let mut remaining: BTreeSet<usize> = (0..entries.len()).collect();
    let mut free: BTreeSet<usize> = (0..entries.len()).filter(|&at| waiting_on[at] == 0).collect();
    let mut placed = Vec::with_capacity(entries.len());
    while let Some(&first) = remaining.first() {
        // A cycle among queued features keeps the queue's own order for what is left.
        let at = free.pop_first().unwrap_or(first);
        remaining.remove(&at);
        placed.push(&entries[at]);
        for &next in &dependents[at] {
            waiting_on[next] -= 1;
            if waiting_on[next] == 0 && remaining.contains(&next) {
                free.insert(next);
            }
        }
    }
    placed
}

/// How the page and `orq queue list` name an entry: `Start <feature>`, or the action and its run.
pub fn entry_label(entry: &QueuedFeature) -> String {
    entry.action.as_ref().map_or_else(|| format!("Start {}", entry.feature), action_label)
}

/// `Resume <run>`, `Review again <run>` or `Accept <run>`.
pub fn action_label(action: &QueueAction) -> String {
    let verb = match action.kind {
        QueueKind::Resume => "Resume",
        QueueKind::ReviewAgain => "Review again",
        QueueKind::Accept => "Accept",
    };
    format!("{verb} {}", action.run)
}

/// Whether queued `action` still applies to its run in `runs`, of project `git`: why not, if it
/// does not (the run was resumed, accepted, discarded or merged, or its feature started again).
fn action_applies(runs: &RunStore, git: &orq_git::Project, action: &QueueAction) -> Result<(), String> {
    let run = &action.run;
    let Ok(record) = runs.load(run) else { return Err(format!("{run} was discarded")) };
    if record.project_id != orq_git::project_id(git) {
        return Err(format!("{run} is not one of the project's runs"));
    }
    let Ok(events) = runs.events(run) else { return Err(format!("{run} can no longer be read")) };
    let summary = orq_orchestrator::summarize_run(&events);
    if let Some(next) = summary.resumed_as {
        return Err(format!("{run} was resumed as {next}"));
    }
    if let Ok(Some((later, _))) = orq_orchestrator::later_run_of(runs, run) {
        return Err(format!("its feature {} was started again as {later}", record.feature_id));
    }
    let applies = match action.kind {
        QueueKind::Resume => matches!(summary.phase, RunPhase::Failed | RunPhase::Blocked | RunPhase::Paused | RunPhase::NeedsReview | RunPhase::DesignFilesNeeded),
        QueueKind::ReviewAgain => matches!(summary.phase, RunPhase::Failed | RunPhase::NeedsReview),
        QueueKind::Accept => matches!(summary.phase, RunPhase::NeedsReview | RunPhase::Failed | RunPhase::Blocked),
    };
    match summary.phase {
        _ if applies => Ok(()),
        RunPhase::Completed | RunPhase::Merged => Err(format!("{run} was accepted or merged")),
        phase => Err(format!("{run} is {}", phase.as_str().replace('_', " "))),
    }
}

/// The remote a project's target is pulled from: the one its runs push to.
pub(super) const PULL_REMOTE: &str = "origin";

/// The run event that says the project's target was pulled after the run's MR merged.
pub const TARGET_PULLED_EVENT: &str = "target.pulled";

/// Whether a project with `settings` has its target pulled after a merge and before its runs: its
/// runs end in an MR, and the setting is on.
fn pulls_target(settings: &orq_core::project_settings::ProjectSettings) -> bool {
    settings.pull_target_before_runs && settings.delivery != orq_core::project_settings::Delivery::MergeLocal
}

/// How a review is named on its forge: GitLab's `!12`, anyone else's `#12`.
pub(super) fn review_ref(review: &orq_core::review_lifecycle::ReviewLifecycle) -> String {
    let sign = if review.provider == "gitlab" { '!' } else { '#' };
    format!("{sign}{}", review.review_id)
}

/// The current UTC time as RFC 3339, without a date-time dependency.
pub(super) fn utc_now() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// What a project shows after a merge whose pull could not bring its target level, kept in a file
/// under the state root until a pull of that same target, from that same remote, does.
struct PullWarning(std::path::PathBuf);

impl PullWarning {
    fn of(roots: &orq_core::storage::StorageRoots, git: &orq_git::Project) -> Self {
        Self(roots.state.join("target-pull").join(orq_git::project_id(git)))
    }

    /// The stored warning; a file from before warnings named their target is about no target in particular.
    fn stored(&self) -> Option<Value> {
        let text = std::fs::read_to_string(&self.0).ok()?;
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(serde_json::from_str::<Value>(text).ok().filter(Value::is_object).unwrap_or_else(|| json!({"message": text})))
    }

    fn read(&self) -> Option<String> {
        self.stored()?["message"].as_str().map(|text| orq_core::redact::redact(text.trim())).filter(|text| !text.is_empty())
    }

    /// The remote and target the warning is about, when it names them.
    fn about(&self) -> Option<(String, String)> {
        let stored = self.stored()?;
        Some((stored["remote"].as_str()?.to_string(), stored["target"].as_str()?.to_string()))
    }

    fn set(&self, remote: &str, target: &str, warning: &str) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.0, json!({"remote": remote, "target": target, "message": warning}).to_string());
    }

    /// Clears the warning once `target` was pulled level from `remote`, when that is what it is about.
    fn pulled(&self, remote: &str, target: &str) {
        let Some(stored) = self.stored() else { return };
        let names = |key: &str, value: &str| stored[key].as_str().map_or(true, |named| named == value);
        if names("remote", remote) && names("target", target) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// The entries of a project's queue whose start its runner refused for a run limit, which wait for a
/// slot again, kept in a file under the state root until each starts or leaves the queue.
struct LimitWaits(std::path::PathBuf);

impl LimitWaits {
    fn of(roots: &orq_core::storage::StorageRoots, git: &orq_git::Project) -> Self {
        Self(roots.state.join("queue-limit-wait").join(orq_git::project_id(git)))
    }

    /// The entries that wait so; none when the file is missing or cannot be read.
    fn entries(&self) -> Vec<String> {
        std::fs::read_to_string(&self.0).ok().and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok()).unwrap_or_default()
    }

    fn save(&self, entries: &[String]) {
        if entries.is_empty() {
            let _ = std::fs::remove_file(&self.0);
            return;
        }
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.0, json!(entries).to_string());
    }

    fn add(&self, entry: &str) {
        let mut entries = self.entries();
        if !entries.iter().any(|kept| kept == entry) {
            entries.push(entry.to_string());
            self.save(&entries);
        }
    }

    fn remove(&self, entry: &str) {
        let entries = self.entries();
        if entries.iter().any(|kept| kept == entry) {
            self.save(&entries.into_iter().filter(|kept| kept != entry).collect::<Vec<_>>());
        }
    }

    /// Forgets the entries that are no longer in `queue` waiting to start.
    fn keep_waiting(&self, queue: &RunQueue) {
        let entries = self.entries();
        let waiting = |kept: &String| queue.entries.iter().any(|entry| entry.feature == *kept && entry.run.is_none() && entry.skipped.is_none());
        if !entries.iter().all(waiting) {
            self.save(&entries.into_iter().filter(waiting).collect::<Vec<_>>());
        }
    }
}

/// How the queue page says an entry waits because its runner refused it for a run limit.
const LIMIT_WAIT_REASON: &str = "run limit reached";

/// How many queue rounds in a row the target may fail to fetch before the queue pauses.
const FETCH_FAILURES_BEFORE_PAUSE: u32 = 3;

/// The count of a project's queue rounds in a row whose pull of the target could not fetch, kept in a
/// file under the state root so it outlasts the server's restarts.
struct FetchFailures(std::path::PathBuf);

impl FetchFailures {
    fn of(roots: &orq_core::storage::StorageRoots, git: &orq_git::Project) -> Self {
        Self(roots.state.join("queue-pull").join(orq_git::project_id(git)))
    }

    /// Counts one more failure; the count now.
    fn add(&self) -> u32 {
        let count = std::fs::read_to_string(&self.0).ok().and_then(|text| text.trim().parse::<u32>().ok()).unwrap_or(0) + 1;
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.0, count.to_string());
        count
    }

    fn clear(&self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The branch project `root`'s runs target: its configured base branch, else the remote's default,
/// else `main`, as `orq run` picks it.
pub(super) fn project_target(root: &Path) -> String {
    let configured = orq_orchestrator::load_run_settings(root, &|name| std::env::var(name).ok()).ok().and_then(|settings| settings.base_branch);
    configured.unwrap_or_else(|| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["symbolic-ref", "--short", &format!("refs/remotes/{PULL_REMOTE}/HEAD")])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|text| text.trim().strip_prefix(&format!("{PULL_REMOTE}/")).map(str::to_string))
            .filter(|branch| !branch.is_empty())
            .unwrap_or_else(|| "main".to_string())
    })
}

/// The server-log line for a start that failed with `why`, with any secret in it redacted.
fn launch_error_line(feature: &str, why: &str) -> String {
    orq_core::redact::redact(&format!("orq: the queue could not start {feature}: {why}"))
}

/// What became of run `run`, from its events and, for a run delivered for review, what the review
/// refresh saved in `store` about its MR. With the project's `repo`, a run that waits for review but
/// whose work is on its target branch, merged by hand, is done; without it (looked at often, so no git
/// is asked) it still waits. Unless the project's queue is to `wait` for review, a run delivered for
/// review is done and one that needs a person's review is still going, as before the queue waited.
/// A run that cannot be read anymore counts as failed.
fn settle(store: &Store, runs: &RunStore, run: &str, repo: Option<&Path>, wait: bool) -> Settled {
    let Ok(events) = runs.events(run) else {
        // A start its runner refused for a run limit has no record, and never began.
        if orq_orchestrator::recorded_limit_refusal(runs, run).is_some() {
            return Settled::LimitRefused;
        }
        return Settled::Failed(Some(format!("the run {run} cannot be read")));
    };
    // A failed stage the run's process goes on to fix is not the run's end.
    let summary = orq_orchestrator::summarize_live_run(&events);
    if let Some(next) = summary.resumed_as {
        return Settled::ContinuedAs(next);
    }
    match summary.phase {
        RunPhase::Completed if wait && ending(&events) == Some("review_ready") => mr_settled(store, run),
        RunPhase::Completed | RunPhase::Merged => Settled::Done,
        RunPhase::Failed | RunPhase::Blocked => Settled::Failed(summary.reason),
        RunPhase::NeedsReview if !wait => Settled::Going,
        RunPhase::NeedsReview if repo.is_some_and(|repo| runs.load(run).is_ok_and(|record| orq_orchestrator::work_is_on_target(repo, &record))) => Settled::Done,
        RunPhase::NeedsReview => Settled::InReview(None),
        // Waiting for design files is a pause like any other, never a failure.
        RunPhase::Running | RunPhase::PauseRequested | RunPhase::Paused | RunPhase::DesignFilesNeeded | RunPhase::Accepting => Settled::Going,
    }
}

/// How run `events` said the run ended, from the last `run.completed`.
fn ending(events: &[orq_orchestrator::Event]) -> Option<&str> {
    events.iter().rev().find(|event| event.kind == "run.completed").and_then(|event| event.payload.get("ending")).and_then(Value::as_str)
}

/// What became of run `run`, delivered for review, from the newest MR `store` holds for it: open, it
/// waits for review; closed, it failed; merged, or with no MR, it is done. When what the store holds
/// cannot be read, the MR may still be open, so it still waits.
fn mr_settled(store: &Store, run: &str) -> Settled {
    use orq_core::review_lifecycle::{ProviderState, LOCAL_PROVIDER};
    let Ok(all) = store.lifecycles_for_run(run) else { return Settled::InReview(None) };
    let newest = all.into_iter().rev().find(|lifecycle| lifecycle.provider != LOCAL_PROVIDER);
    match newest {
        Some(mr) if mr.state == ProviderState::Open => Settled::InReview(Some(mr.url)),
        Some(mr) if mr.state == ProviderState::Closed => Settled::MrClosed,
        _ => Settled::Done,
    }
}

/// How many runs, each continued as the next, one round follows; a longer chain, or a loop, is
/// followed further in the rounds after, and holds the queue's starts meanwhile.
const CONTINUATIONS_FOLLOWED: usize = 16;

/// How often the server works every queue even when no queued run ended.
pub const QUEUE_ROUND_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// The server's queue rounds, stopped and joined when dropped.
pub struct QueueRounds {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Works every queue at once, then every [`QUEUE_ROUND_EVERY`], and sooner when a queued run ended,
/// which is looked for every `tick`. `restart_pending` says whether the server waits to restart.
pub fn start_queue_rounds(projects: std::sync::Arc<StoreProjects>, tick: std::time::Duration, restart_pending: impl Fn() -> bool + Send + 'static) -> QueueRounds {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let mut last: Option<std::time::Instant> = None;
        loop {
            let due = last.is_none_or(|at| at.elapsed() >= QUEUE_ROUND_EVERY) || projects.queued_run_ended();
            if due {
                projects.queue_round(&QueueRound { minute_of_day: local_minute_of_day(), restart_pending: restart_pending() });
                last = Some(std::time::Instant::now());
            }
            match stopped.recv_timeout(tick) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                _ => break,
            }
        }
    });
    QueueRounds { stop: Some(stop), thread: Some(thread) }
}

impl Drop for QueueRounds {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The machine's local time of day, in minutes after midnight, which queue hours are written in.
fn local_minute_of_day() -> u16 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs()) as libc::time_t;
    // SAFETY: `localtime_r` writes only into the `tm` it is given and reads `now`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return ((now / 60) % (24 * 60)) as u16;
    }
    (tm.tm_hour * 60 + tm.tm_min) as u16
}

/// When the first entry of project `git`'s queue at `root` that may start now was added; a queue
/// with none comes last.
fn oldest_waiting(store: &Store, runs: &RunStore, git: &orq_git::Project, root: &Path) -> i64 {
    let queue = store.queue(root).unwrap_or_default();
    // An entry still waiting on its dependencies cannot take a slot, so it does not count.
    let ledger = read_ledger(&orq_features::ledger_path(git)).unwrap_or_default();
    let unlanded = unlanded_dependencies(Some(store), runs, git, &queue, &ledger).unwrap_or_default();
    let may_start = |entry: &&QueuedFeature| entry.run.is_none() && !entry.failed && entry.skipped.is_none() && (entry.action.is_some() || start_wait(&ledger, &unlanded, &entry.feature).is_ok());
    queue.entries.iter().filter(may_start).map(|entry| entry.added_at).min().unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_start_error_is_logged_with_its_secrets_redacted() {
        let line = super::launch_error_line("COM-01", "git push failed: password=hunter2hunter2");
        assert_eq!(line, "orq: the queue could not start COM-01: git push failed: password=[REDACTED]");
    }

    fn queued(feature: String) -> super::QueuedFeature {
        super::QueuedFeature { feature, added_at: 0, added_by: "web".into(), profiles: None, run: None, failed: false, action: None, skipped: None }
    }

    fn entry(feature: String, depends_on: Vec<String>) -> super::FeatureEntry {
        super::FeatureEntry {
            feature_name: feature,
            description: None,
            depends_on,
            status: orq_features::LedgerStatus::Known(orq_core::FeatureStatus::Pending),
            test_file: None,
            target_files: Vec::new(),
            recorded_at: None,
            cycles_completed: None,
            parallelizable: None,
        }
    }

    #[test]
    fn a_long_chain_queued_backwards_is_put_in_dependency_order_quickly() {
        let name = |i: usize| format!("COM-{i:04}");
        let ledger: Vec<_> = (0..3000).map(|i| entry(name(i), if i == 0 { Vec::new() } else { vec![name(i - 1)] })).collect();
        let entries: Vec<_> = (0..3000).rev().map(|i| queued(name(i))).collect();
        let started = std::time::Instant::now();
        let order: Vec<&str> = super::ordered(&entries, &ledger).into_iter().map(|e| e.feature.as_str()).collect();
        assert!(started.elapsed() < std::time::Duration::from_secs(1), "took {:?}", started.elapsed());
        assert_eq!(order, (0..3000).map(name).collect::<Vec<_>>());
    }

    #[test]
    fn the_queue_order_is_kept_where_dependencies_allow_and_for_a_cycle() {
        let s = |v: &str| v.to_string();
        let ledger = vec![entry(s("A"), vec![s("C")]), entry(s("B"), vec![]), entry(s("C"), vec![]), entry(s("X"), vec![s("Y")]), entry(s("Y"), vec![s("X")])];
        let entries: Vec<_> = ["X", "A", "B", "Y", "C"].map(s).into_iter().map(queued).collect();
        let order: Vec<&str> = super::ordered(&entries, &ledger).into_iter().map(|e| e.feature.as_str()).collect();
        assert_eq!(order, ["B", "C", "A", "X", "Y"]);
    }
}

