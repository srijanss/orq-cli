//! Carrying out a prune plan. Nothing is trusted from the plan: every file is
//! checked to be inside its run's directory and its run is assessed again
//! just before removal, because the world may have changed since the preview.

use std::path::{Component, Path};

use crate::ownership::delete_in_root;
use crate::retention::{assess, ArtifactClass, Assessment, PrunePlan, RetentionPolicy, RunFacts};
use crate::storage::check_run_id;
use crate::store::Store;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub removed: Vec<String>,
    pub freed: u64,
    /// Already gone when the prune got to them.
    pub missing: Vec<String>,
    /// Files left alone, with the reason.
    pub skipped: Vec<(String, String)>,
    pub metadata_removed: Vec<String>,
}

/// True when `rel` is `runs/<run_id>/<something>` with nothing that climbs or restarts the path.
fn inside_run(rel: &Path, run_id: &str) -> bool {
    let mut parts = rel.components();
    matches!(parts.next(), Some(Component::Normal(p)) if p == "runs")
        && matches!(parts.next(), Some(Component::Normal(p)) if p == run_id)
        && rel.components().count() >= 3
        && rel.components().all(|c| matches!(c, Component::Normal(_)))
}

fn prunable(policy: &RetentionPolicy, facts: Option<RunFacts>, now: u64) -> Result<Vec<ArtifactClass>, String> {
    let Some(facts) = facts else { return Err("the run's state can no longer be read".into()) };
    match assess(policy, &facts, now) {
        Assessment::Prunable(classes) => Ok(classes),
        Assessment::Retained(why) => Err(format!("run is now retained: {}", why.describe())),
    }
}

pub fn prune(
    data_root: &Path,
    store: &Store,
    policy: &RetentionPolicy,
    plan: &PrunePlan,
    now: u64,
    current: &dyn Fn(&str) -> Option<RunFacts>,
) -> std::io::Result<PruneReport> {
    let mut report = PruneReport::default();
    for candidate in &plan.candidates {
        let artifact = &candidate.artifact;
        let rel = artifact.rel.to_string_lossy().into_owned();
        if check_run_id(&artifact.run_id).is_err() || !inside_run(&artifact.rel, &artifact.run_id) {
            report.skipped.push((rel, "path is not inside its run".into()));
            continue;
        }
        match prunable(policy, current(&artifact.run_id), now) {
            Ok(classes) if classes.contains(&artifact.class) => {}
            Ok(_) => {
                report.skipped.push((rel, "not yet past retention".into()));
                continue;
            }
            Err(why) => {
                report.skipped.push((rel, why));
                continue;
            }
        }
        store.begin_removal(&artifact.run_id, &rel)?;
        match delete_in_root(data_root, &artifact.rel) {
            Ok(()) => {
                store.finish_removal(&rel)?;
                report.freed += artifact.bytes;
                report.removed.push(rel);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                store.finish_removal(&rel)?;
                report.missing.push(rel);
            }
            Err(e) => {
                store.abandon_removal(&rel)?;
                report.skipped.push((rel, e.to_string()));
            }
        }
    }
    for run_id in &plan.metadata {
        if check_run_id(run_id).is_err() {
            continue;
        }
        if prunable(policy, current(run_id), now).is_ok_and(|classes| classes.contains(&ArtifactClass::Metadata)) {
            store.remove_run_metadata(run_id)?;
            report.metadata_removed.push(run_id.clone());
        }
    }
    Ok(report)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Journaled removals whose file is gone; the bookkeeping now agrees.
    pub completed: Vec<String>,
    /// Journaled removals whose file is still there; it stays, and so does its index row.
    pub abandoned: Vec<String>,
}

/// Settles every removal the journal still lists, after an interruption. A file
/// that is gone completes the removal; one that is still there is simply kept
/// (the next plan decides about it afresh), as is anything that is not a plain
/// path inside a run. Nothing is deleted here.
pub fn reconcile(data_root: &Path, store: &Store) -> std::io::Result<Reconciled> {
    let mut done = Reconciled::default();
    for (run_id, rel) in store.pending_removals()? {
        let path = Path::new(&rel);
        let gone = inside_run(path, &run_id)
            && matches!(std::fs::symlink_metadata(data_root.join(path)), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
        if gone {
            store.finish_removal(&rel)?;
            done.completed.push(rel);
        } else {
            store.abandon_removal(&rel)?;
            done.abandoned.push(rel);
        }
    }
    Ok(done)
}
