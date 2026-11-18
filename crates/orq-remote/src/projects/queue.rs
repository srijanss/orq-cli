//! A project's run queue, as the server works it: each round settles the entries whose runs ended (a
//! completed run's entry leaves, a failed or blocked one stays marked failed) and then starts the
//! first entries whose dependencies are completed, through the same launch as the web's Start, while
//! the queue is not paused, the time is inside its hours, no restart is pending and the project has a
//! free run slot. A full disk or a start that fails pauses the queue with the reason, and nothing is
//! retried until someone resumes it.

use std::path::Path;

use orq_core::remote_inbox::{LaunchProfiles, QueueChange, RunControlOutcome, RunStart};
use orq_core::store::run_queue::{QueueAddition, QueueProfiles, QueuedFeature, RunQueue};
use orq_core::store::Store;
use orq_features::{FeatureEntry, Ineligible};
use orq_orchestrator::{RunPhase, RunStore};
use serde_json::{json, Value};

use super::{discover, read_ledger, start_blocked, Launch, StoreProjects};

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
    /// It is still going (or waits for a review): the entry stays as it is.
    Going,
    /// It completed or was delivered: the entry leaves the queue.
    Done,
    /// It was continued as another run, which the entry now follows.
    ContinuedAs(String),
    /// It failed or was blocked, for this reason.
    Failed(Option<String>),
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
        for project in discover(&self.scan_root) {
            let Ok(git) = orq_git::resolve_project(&project.path) else { continue };
            let root = orq_git::main_checkout_root(&git).unwrap_or_else(|| git.root.clone());
            if !queued.iter().any(|queued| Path::new(queued) == root) {
                continue;
            }
            if let Err(error) = self.project_round(&store, &project.id, &git, &root, round) {
                eprintln!("orq: the run queue of {} stopped: {error}", project.name);
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
        roots.iter().filter_map(|root| store.queue(Path::new(root)).ok()).flat_map(|queue| queue.entries).any(|entry| match (&entry.run, entry.failed) {
            (Some(run), false) => !matches!(settle(&runs, run), Settled::Going),
            _ => false,
        })
    }

    fn project_round(&self, store: &Store, id: &str, git: &orq_git::Project, root: &Path, round: &QueueRound) -> Result<(), String> {
        let settings = self.settings_of_git(git)?;
        let runs = RunStore::new(&self.roots.state);
        let failed = |e: std::io::Error| e.to_string();
        for entry in store.queue(root).map_err(failed)?.entries {
            let (Some(run), false) = (&entry.run, entry.failed) else { continue };
            match settle(&runs, run) {
                Settled::Going => {}
                Settled::Done => drop(store.queue_remove(root, &entry.feature).map_err(failed)?),
                Settled::ContinuedAs(next) => store.queue_started(root, &entry.feature, &next).map_err(failed)?,
                Settled::Failed(reason) => {
                    store.queue_failed(root, &entry.feature).map_err(failed)?;
                    if settings.queue_stop_on_failure {
                        let why = reason.map_or(String::new(), |reason| format!(": {reason}"));
                        let reason = orq_core::redact::redact(&format!("the run {run} of {} failed{why}", entry.feature));
                        store.queue_pause(root, &reason).map_err(failed)?;
                    }
                }
            }
        }
        let queue = store.queue(root).map_err(failed)?;
        if queue.paused.is_some() || round.restart_pending {
            return Ok(());
        }
        if let Some(hours) = &settings.queue_hours {
            if !orq_core::project_settings::within_queue_hours(hours, round.minute_of_day) {
                return Ok(());
            }
        }
        let ledger = read_ledger(&orq_features::ledger_path(git))?;
        for entry in ordered(&queue.entries, &ledger) {
            if entry.run.is_some() || orq_features::check_run_eligibility(&ledger, &entry.feature).is_err() {
                continue;
            }
            let active = runs.active_project_runs(&orq_git::project_id(git)).map_err(|error| format!("{error:?}"))?;
            if orq_orchestrator::limit_refusal(&git.name, settings.max_active_runs, &active).is_some() {
                return Ok(());
            }
            // The run limit is waited out; anything else that refuses the start pauses the queue.
            let refused = match start_blocked(git, &settings, &active)? {
                Some(why) => Some(why),
                None => {
                    let profiles = entry.profiles.as_ref().map(|p| LaunchProfiles { implementer: p.implementer.clone(), reviewer: p.reviewer.clone() });
                    match self.launch(id, Launch::One(&entry.feature), profiles.as_ref()) {
                        Ok(RunStart::Started(run) | RunStart::StartedWithWarning { run, .. }) => {
                            store.queue_started(root, &entry.feature, &run).map_err(failed)?;
                            None
                        }
                        Ok(RunStart::Refused(why)) => Some(format!("{} could not start: {why}", entry.feature)),
                        Ok(RunStart::Unknown) => Some(format!("{} is not in the project's features", entry.feature)),
                        Err(why) => {
                            eprintln!("orq: the queue could not start {}: {why}", entry.feature);
                            Some(format!("{} could not start; see the server's log", entry.feature))
                        }
                    }
                }
            };
            if let Some(why) = refused {
                store.queue_pause(root, &orq_core::redact::redact(&why)).map_err(failed)?;
                return Ok(());
            }
        }
        Ok(())
    }

    /// Project `id`'s queue as the web shows it; `None` when there is no such project.
    pub(super) fn queue_answer(&self, id: &str) -> Result<Option<Value>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        let queue = self.read_queue(&root)?;
        let ledger = read_ledger(&orq_features::ledger_path(&git))?;
        Ok(Some(queue_json(&queue, &ledger)))
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
        Ok(match change_queue(&store, &git, &root, change, "web")? {
            Ok(done) => RunControlOutcome::Done(done),
            Err(why) => RunControlOutcome::Refused(orq_core::redact::redact(&why)),
        })
    }
}

/// How many features project `git`'s queue in `store` holds and whether it is paused, for the
/// dashboard card; without orq.db, none.
pub(super) fn queue_summary(store: Option<&Store>, git: &orq_git::Project) -> Result<Value, String> {
    let root = orq_git::main_checkout_root(git).unwrap_or_else(|| git.root.clone());
    let queue = store.map(|store| store.queue(&root)).transpose().map_err(|error| error.to_string())?.unwrap_or_default();
    Ok(json!({"count": queue.entries.len(), "paused": queue.paused.is_some()}))
}

/// Applies `change` to the queue of project `git`, whose main checkout is `root`, as `by` (`web` or
/// `cli`) asked: what was done, or why it may not be.
pub fn change_queue(store: &Store, git: &orq_git::Project, root: &Path, change: &QueueChange, by: &str) -> Result<Result<String, String>, String> {
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
            let added = store.queue_add(root, &additions, by, now).map_err(failed)?;
            if added.is_empty() {
                Err("those features are queued already".to_string())
            } else {
                Ok(added.join(", "))
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

/// Project `git`'s queue in `store`, whose main checkout is `root`, as `orq queue list` shows it.
pub fn project_queue_json(store: &Store, git: &orq_git::Project, root: &Path) -> Result<Value, String> {
    let queue = store.queue(root).map_err(|error| format!("cannot read the queue: {error}"))?;
    let ledger = read_ledger(&orq_features::ledger_path(git))?;
    Ok(queue_json(&queue, &ledger))
}

/// The queue as the page and `orq queue list` show it: entries in the order they will start, each
/// with its state (`running`, `failed`, `next`, `queued` or `waiting`) and why it waits.
pub fn queue_json(queue: &RunQueue, ledger: &[FeatureEntry]) -> Value {
    let mut next_given = false;
    let entries: Vec<Value> = ordered(&queue.entries, ledger)
        .into_iter()
        .map(|entry| {
            let (state, reason) = match (&entry.run, entry.failed) {
                (Some(_), true) => ("failed", None),
                (Some(_), false) => ("running", None),
                (None, _) => match orq_features::check_run_eligibility(ledger, &entry.feature) {
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
            json!({
                "feature": entry.feature,
                "state": state,
                "reason": reason.map(|reason| orq_core::redact::redact(&reason)),
                "run": entry.run,
                "added_at": entry.added_at,
                "added_by": entry.added_by,
                "implementer": profiles.map(|p| &p.implementer),
                "reviewer": profiles.map(|p| &p.reviewer),
            })
        })
        .collect();
    json!({"entries": entries, "count": entries.len(), "paused": queue.paused})
}

/// The entries in the order they may start: one never comes before a queued feature it depends on,
/// and otherwise the queue's own order holds.
fn ordered<'a>(entries: &'a [QueuedFeature], ledger: &[FeatureEntry]) -> Vec<&'a QueuedFeature> {
    let depends_on = |feature: &str| -> &[String] { ledger.iter().find(|entry| entry.feature_name == feature).map_or(&[][..], |entry| &entry.depends_on[..]) };
    let mut remaining: Vec<&QueuedFeature> = entries.iter().collect();
    let mut placed = Vec::with_capacity(entries.len());
    while !remaining.is_empty() {
        let free = remaining.iter().position(|entry| depends_on(&entry.feature).iter().all(|dep| !remaining.iter().any(|other| &other.feature == dep)));
        // A cycle among queued features keeps the queue's own order for what is left.
        let at = free.unwrap_or(0);
        placed.push(remaining.remove(at));
    }
    placed
}

/// What became of run `run`, from its events. A run that cannot be read anymore counts as failed.
fn settle(runs: &RunStore, run: &str) -> Settled {
    let Ok(events) = runs.events(run) else { return Settled::Failed(Some(format!("the run {run} cannot be read"))) };
    let summary = orq_orchestrator::summarize_run(&events);
    if let Some(next) = summary.resumed_as {
        return Settled::ContinuedAs(next);
    }
    match summary.phase {
        RunPhase::Completed | RunPhase::Merged => Settled::Done,
        RunPhase::Failed | RunPhase::Blocked => Settled::Failed(summary.reason),
        RunPhase::Running | RunPhase::NeedsReview | RunPhase::PauseRequested | RunPhase::Paused => Settled::Going,
    }
}

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
