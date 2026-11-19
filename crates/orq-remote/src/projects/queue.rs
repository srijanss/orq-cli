//! A project's run queue, as the server works it: each round settles the entries whose runs ended (a
//! completed run's entry leaves, a failed or blocked one stays marked failed) and then starts the
//! first entries whose dependencies are completed, through the same launch as the web's Start, while
//! the queue is not paused, the time is inside its hours, no restart is pending and both the project
//! and the machine have a free run slot. Under a machine run limit each start goes to the project
//! whose waiting entry was added first. A full disk or a start that fails pauses the queue with the reason, and nothing is
//! retried until someone resumes it.

use std::path::Path;

use orq_core::remote_inbox::{LaunchProfiles, QueueChange, RunControlOutcome, RunStart};
use orq_core::store::run_queue::{QueueAction, QueueAddition, QueueKind, QueueProfiles, QueuedFeature, RunQueue};
use orq_core::store::Store;
use orq_features::{FeatureEntry, Ineligible};
use orq_orchestrator::{RunPhase, RunStore};
use serde_json::{json, Value};

use super::{carried_on, discover, read_ledger, start_blocked, Launch, ResumeRequest, StoreProjects};

/// What a round of the queue needs to know about now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueRound {
    /// The local time, in minutes after midnight, checked against a project's queue hours.
    pub minute_of_day: u16,
    /// Whether the server is waiting to restart: no run is started meanwhile.
    pub restart_pending: bool,
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
}

impl StoreProjects {
    /// One round of every project's queue. Each project's failure is logged and the others go on.
    pub fn queue_round(&self, round: &QueueRound) {
        let database = self.roots.database();
        if !database.exists() {
            return;
        }
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
        loop {
            if one_at_a_time {
                projects.sort_by_cached_key(|(_, git, root)| oldest_waiting(&store, git, root));
            }
            let mut started = false;
            for (project, git, root) in &projects {
                match self.project_round(&store, &project.id, git, root, round, &machine, one_at_a_time) {
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
                (Some(run), true) => matches!(settle(&store, &runs, run, None, wait), Settled::ContinuedAs(_)),
                _ => false,
            })
        })
    }

    /// One round of project `id`'s queue under the `machine`'s limits; with `one`, it starts at most
    /// one run. Whether it started any.
    #[allow(clippy::too_many_arguments)]
    fn project_round(&self, store: &Store, id: &str, git: &orq_git::Project, root: &Path, round: &QueueRound, machine: &super::MachineRuns, one: bool) -> Result<bool, String> {
        let settings = self.settings_of_git(git)?;
        let runs = RunStore::new(&self.roots.state);
        let failed = |e: std::io::Error| e.to_string();
        let mut in_review = false;
        // The queued Accepts this round read as going, as (run, entry): each holds its slot for the whole
        // round, though its tests end partway through, and what they ended with is acted on next round.
        let mut accepts_going = Vec::new();
        for entry in store.queue(root).map_err(failed)?.entries {
            let Some(run) = &entry.run else { continue };
            let mut run = run.clone();
            // A start's run is named by its feature, as before the queue held other actions.
            let what = entry.action.as_ref().map_or_else(|| entry.feature.clone(), action_label);
            let mut settled = match &entry.action {
                Some(QueueAction { kind: QueueKind::Accept, .. }) => self.settle_accept(store, &runs, &run, Some(root), settings.queue_wait_for_review),
                _ => settle(store, &runs, &run, Some(root), settings.queue_wait_for_review),
            };
            // A failure already marked is only looked at again for the run it was resumed as.
            if entry.failed && !matches!(settled, Settled::ContinuedAs(_)) {
                continue;
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
                Settled::Going if matches!(entry.action, Some(QueueAction { kind: QueueKind::Accept, .. })) => accepts_going.push((run, entry.feature.clone())),
                Settled::Going => {}
                Settled::InReview(_) => in_review = true,
                Settled::Done => drop(store.queue_remove(root, &entry.feature).map_err(failed)?),
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
                    }
                }
                Settled::MrClosed => {
                    store.queue_failed(root, &entry.feature).map_err(failed)?;
                    if settings.queue_stop_on_failure {
                        let reason = orq_core::redact::redact(&format!("{what}'s MR was closed without merging"));
                        store.queue_pause(root, &reason).map_err(failed)?;
                    }
                }
            }
        }
        let queue = store.queue(root).map_err(failed)?;
        if queue.paused.is_some() || round.restart_pending || in_review {
            return Ok(false);
        }
        if let Some(hours) = &settings.queue_hours {
            if !orq_core::project_settings::within_queue_hours(hours, round.minute_of_day) {
                return Ok(false);
            }
        }
        let ledger = read_ledger(&orq_features::ledger_path(git))?;
        let mut started = false;
        for entry in ordered(&queue.entries, &ledger) {
            if entry.run.is_some() || entry.skipped.is_some() {
                continue;
            }
            match &entry.action {
                None if orq_features::check_run_eligibility(&ledger, &entry.feature).is_err() => continue,
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
            let mut accepting = accepts_going.clone();
            for going in self.queued_accepts_going(&now, &runs) {
                if !accepting.iter().any(|(run, _)| *run == going.0) {
                    accepting.push(going);
                }
            }
            let active = match keeps_slot {
                true => Vec::new(),
                false => {
                    let mut active = runs.active_project_runs(&orq_git::project_id(git)).map_err(|error| format!("{error:?}"))?;
                    active.extend(accepting.iter().cloned());
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
                    let held = accepts_going.iter().map(|(run, _)| (orq_git::project_id(git), run.clone()));
                    for going in held.chain(self.machine_queued_accepts_going(store, &runs)?) {
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
            // A run limit is waited out; anything else that refuses the start pauses the queue.
            let refused = match start_blocked(git, &settings, &active, &machine)? {
                Some(why) => Some(why),
                None if entry.action.is_some() => {
                    let (action, what) = (entry.action.as_ref().expect("an action entry"), entry_label(entry));
                    match self.start_queued_action(id, action) {
                        Ok(RunControlOutcome::Done(run)) => {
                            store.queue_started(root, &entry.feature, &run).map_err(failed)?;
                            if action.kind == QueueKind::Accept {
                                accepts_going.push((run, entry.feature.clone()));
                            }
                            started = true;
                            None
                        }
                        Ok(RunControlOutcome::Refused(_)) if self.limit_reached(&runs, git, &settings, &machine)? => return Ok(started),
                        Ok(RunControlOutcome::Refused(why)) => Some(format!("{what} could not start: {why}")),
                        Ok(RunControlOutcome::Unknown) => Some(format!("{what}: {} is not one of the project's runs", action.run)),
                        Err(why) => {
                            eprintln!("{}", launch_error_line(&what, &why));
                            Some(format!("{what} could not start; see the server's log"))
                        }
                    }
                }
                None => {
                    let profiles = entry.profiles.as_ref().map(|p| LaunchProfiles { implementer: p.implementer.clone(), reviewer: p.reviewer.clone() });
                    match self.launch(id, Launch::Queued(&entry.feature), profiles.as_ref()) {
                        Ok(RunStart::Started(run) | RunStart::StartedWithWarning { run, .. }) => {
                            // Removed while it started, the queue no longer follows the run, which
                            // goes on; removed and added again, the new entry follows this run.
                            store.queue_started(root, &entry.feature, &run).map_err(failed)?;
                            started = true;
                            None
                        }
                        Ok(RunStart::Refused(_)) if self.limit_reached(&runs, git, &settings, &machine)? => return Ok(started),
                        Ok(RunStart::Refused(why)) => Some(format!("{} could not start: {why}", entry.feature)),
                        Ok(RunStart::Unknown) => Some(format!("{} is not in the project's features", entry.feature)),
                        Err(why) => {
                            eprintln!("{}", launch_error_line(&entry.feature, &why.to_string()));
                            Some(format!("{} could not start; see the server's log", entry.feature))
                        }
                    }
                }
            };
            if let Some(why) = refused {
                store.queue_pause(root, &orq_core::redact::redact(&why)).map_err(failed)?;
                return Ok(started);
            }
            if started && one {
                return Ok(true);
            }
        }
        Ok(started)
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
            QueueKind::Accept => self.accept(store, &action.run),
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

    /// Project `id`'s queue as the web shows it; `None` when there is no such project.
    pub(super) fn queue_answer(&self, id: &str) -> Result<Option<Value>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        let queue = self.read_queue(&root)?;
        let wait = self.settings_of_git(&git)?.queue_wait_for_review;
        let ledger = read_ledger(&orq_features::ledger_path(&git))?;
        let review = if self.roots.database().exists() {
            let store = Store::open(&self.roots.database()).map_err(|error| error.to_string())?;
            in_review(&store, &RunStore::new(&self.roots.state), &queue, &git, wait)
        } else {
            std::collections::HashMap::new()
        };
        Ok(Some(queue_json_with(&queue, &ledger, &review)))
    }

    fn read_queue(&self, root: &Path) -> Result<RunQueue, String> {
        let database = self.roots.database();
        if !database.exists() {
            return Ok(RunQueue::default());
        }
        Store::open(&database).and_then(|store| store.queue(root)).map_err(|error| error.to_string())
    }

    /// Changes project `id`'s queue from the web.
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
pub(super) fn queue_summary(store: Option<&Store>, runs: &RunStore, git: &orq_git::Project, wait: bool) -> Result<Value, String> {
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
    Ok(queue_json_with(&queue, &ledger, &in_review(store, runs, &queue, git, wait)))
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
/// with its state (`running`, `failed`, `next`, `queued` or `waiting`) and why it waits.
pub fn queue_json(queue: &RunQueue, ledger: &[FeatureEntry]) -> Value {
    queue_json_with(queue, ledger, &std::collections::HashMap::new())
}

/// [`queue_json`], with the entries whose run waits for a person's review (`review`: feature to its
/// open MR's address, if any) shown waiting for review.
fn queue_json_with(queue: &RunQueue, ledger: &[FeatureEntry], review: &std::collections::HashMap<String, Option<String>>) -> Value {
    let mut next_given = false;
    let mut waiting_for_review = None;
    let entries: Vec<Value> = ordered(&queue.entries, ledger)
        .into_iter()
        .map(|entry| {
            let eligible = || match &entry.action {
                Some(_) => Ok(()),
                None => orq_features::check_run_eligibility(ledger, &entry.feature).map(drop),
            };
            let (state, reason) = match (&entry.run, entry.failed) {
                _ if entry.skipped.is_some() => ("skipped", entry.skipped.as_ref().map(|why| format!("No longer needed: {why}"))),
                (Some(_), true) => ("failed", None),
                (Some(_), false) if review.contains_key(&entry.feature) => ("review", None),
                (Some(_), false) => ("running", None),
                (None, _) => match eligible() {
                    Ok(_) if !next_given => {
                        next_given = true;
                        ("next", None)
                    }
                    Ok(_) => ("queued", None),
                    Err(Ineligible::UnmetDependencies(unmet)) => {
                        let names: Vec<String> = unmet.iter().map(|dep| dep.name.clone()).collect();
                        ("waiting", Some(format!("waiting for {}", names.join(", "))))
                    }
                    Err(why) => ("waiting", Some(why.to_string())),
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
        QueueKind::Resume => matches!(summary.phase, RunPhase::Failed | RunPhase::Blocked | RunPhase::Paused | RunPhase::NeedsReview),
        QueueKind::ReviewAgain => matches!(summary.phase, RunPhase::Failed | RunPhase::NeedsReview),
        QueueKind::Accept => matches!(summary.phase, RunPhase::NeedsReview | RunPhase::Failed | RunPhase::Blocked),
    };
    match summary.phase {
        _ if applies => Ok(()),
        RunPhase::Completed | RunPhase::Merged => Err(format!("{run} was accepted or merged")),
        phase => Err(format!("{run} is {}", phase.as_str().replace('_', " "))),
    }
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
    let Ok(events) = runs.events(run) else { return Settled::Failed(Some(format!("the run {run} cannot be read"))) };
    let summary = orq_orchestrator::summarize_run(&events);
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
        RunPhase::Running | RunPhase::PauseRequested | RunPhase::Paused | RunPhase::Accepting => Settled::Going,
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
fn oldest_waiting(store: &Store, git: &orq_git::Project, root: &Path) -> i64 {
    let queue = store.queue(root).unwrap_or_default();
    // An entry still waiting on its dependencies cannot take a slot, so it does not count.
    let ledger = read_ledger(&orq_features::ledger_path(git)).unwrap_or_default();
    let may_start = |entry: &&QueuedFeature| entry.run.is_none() && !entry.failed && entry.skipped.is_none() && (entry.action.is_some() || orq_features::check_run_eligibility(&ledger, &entry.feature).is_ok());
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

