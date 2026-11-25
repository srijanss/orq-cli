//! Whether a ledger feature may start a pipeline run.

use std::collections::{HashMap, HashSet};

use orq_core::FeatureStatus;

use crate::{FeatureEntry, LedgerStatus};

/// A dependency that is not completed yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmetDependency {
    pub name: String,
    /// Its ledger status, or `None` when the ledger has no such feature.
    pub status: Option<String>,
}

/// Why a feature may not start a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ineligible {
    NotFound,
    /// Only a `pending` feature can start; carries the actual status text.
    NotPending { status: String },
    /// Every `dependsOn` entry that is not `completed`, in ledger order.
    UnmetDependencies(Vec<UnmetDependency>),
    /// A resume takes a feature only as its agent may have left it: `pending`, `in_progress` or
    /// `completed`; carries the actual status text.
    NotResumable { status: String },
    /// A later run took the feature: that run's work, not this one's, is what its status says.
    TakenByLaterRun,
}

impl std::fmt::Display for Ineligible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ineligible::NotFound => write!(f, "feature not found in the ledger"),
            Ineligible::NotPending { status } => {
                write!(f, "feature is {status}, only a pending feature can start a run")
            }
            Ineligible::UnmetDependencies(unmet) => {
                write!(f, "unmet dependencies: ")?;
                for (i, dep) in unmet.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match &dep.status {
                        Some(status) => write!(f, "{} ({status})", dep.name)?,
                        None => write!(f, "{} (not in the ledger)", dep.name)?,
                    }
                }
                Ok(())
            }
            Ineligible::NotResumable { status } => {
                write!(f, "feature is {status}, only a pending, in_progress, awaiting_delivery or completed feature can be resumed")
            }
            Ineligible::TakenByLaterRun => {
                write!(f, "a later run took the feature, so this run cannot pick it up again")
            }
        }
    }
}

impl std::error::Error for Ineligible {}

/// The named feature, if it is `pending` and all its dependencies are
/// `completed`; otherwise the reason it may not start a run.
pub fn check_run_eligibility<'a>(
    ledger: &'a [FeatureEntry],
    feature_name: &str,
) -> Result<&'a FeatureEntry, Ineligible> {
    check_eligibility(ledger, feature_name, &fresh, &HashSet::new())
}

/// As [`check_run_eligibility`], counting a dependency in `archived` (completed features moved
/// out of the ledger) as completed.
pub fn check_run_eligibility_with_archive<'a>(
    ledger: &'a [FeatureEntry],
    feature_name: &str,
    archived: &HashSet<String>,
) -> Result<&'a FeatureEntry, Ineligible> {
    check_eligibility(ledger, feature_name, &fresh, archived)
}

/// As [`check_resume_eligibility`], counting archived dependencies as completed.
pub fn check_resume_eligibility_with_archive<'a>(
    ledger: &'a [FeatureEntry],
    feature_name: &str,
    free: &[String],
    archived: &HashSet<String>,
) -> Result<&'a FeatureEntry, Ineligible> {
    check_eligibility(ledger, feature_name, &|entry| resumable(entry, free), archived)
}

/// As [`check_run_eligibility`] for resuming a run: the feature may be as the run's agent left it,
/// `pending`, `in_progress` or `completed`, when it is in `free` (no later run took it).
pub fn check_resume_eligibility<'a>(
    ledger: &'a [FeatureEntry],
    feature_name: &str,
    free: &[String],
) -> Result<&'a FeatureEntry, Ineligible> {
    check_eligibility(ledger, feature_name, &|entry| resumable(entry, free), &HashSet::new())
}

/// Whether `entry`'s status lets a fresh run start it.
fn fresh(entry: &FeatureEntry) -> Result<(), Ineligible> {
    match entry.status.known() {
        Some(FeatureStatus::Pending) => Ok(()),
        _ => Err(Ineligible::NotPending { status: entry.status.as_str().to_string() }),
    }
}

/// Whether a resumed run may take `entry`: its status is one a run leaves behind, and no later run
/// took it (`free`), since that run, not this one, is what its status speaks for.
fn resumable(entry: &FeatureEntry, free: &[String]) -> Result<(), Ineligible> {
    match entry.status.known() {
        Some(FeatureStatus::Pending | FeatureStatus::InProgress | FeatureStatus::AwaitingDelivery | FeatureStatus::Completed) => {
            free.contains(&entry.feature_name).then_some(()).ok_or(Ineligible::TakenByLaterRun)
        }
        _ => Err(Ineligible::NotResumable { status: entry.status.as_str().to_string() }),
    }
}

fn check_eligibility<'a>(
    ledger: &'a [FeatureEntry],
    feature_name: &str,
    startable: &dyn Fn(&FeatureEntry) -> Result<(), Ineligible>,
    archived: &HashSet<String>,
) -> Result<&'a FeatureEntry, Ineligible> {
    let entry = ledger
        .iter()
        .find(|e| e.feature_name == feature_name)
        .ok_or(Ineligible::NotFound)?;
    startable(entry)?;
    if entry.depends_on.is_empty() {
        return Ok(entry);
    }
    let mut by_name: HashMap<&str, &LedgerStatus> = HashMap::with_capacity(ledger.len());
    for e in ledger {
        by_name.entry(e.feature_name.as_str()).or_insert(&e.status);
    }
    let unmet: Vec<UnmetDependency> = entry
        .depends_on
        .iter()
        .filter_map(|dep| match by_name.get(dep.as_str()) {
            Some(status) if status.known() == Some(FeatureStatus::Completed) => None,
            Some(status) => Some(UnmetDependency {
                name: dep.clone(),
                status: Some(status.as_str().to_string()),
            }),
            None if archived.contains(dep.as_str()) => None,
            None => Some(UnmetDependency {
                name: dep.clone(),
                status: None,
            }),
        })
        .collect();
    if unmet.is_empty() {
        Ok(entry)
    } else {
        Err(Ineligible::UnmetDependencies(unmet))
    }
}

/// Why a group of features may not start one run together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupRefusal {
    /// One member may not start, or has a dependency outside the group that is not completed.
    Feature { id: String, reason: Ineligible },
    /// The same feature is named more than once.
    Duplicate(String),
    /// These members wait on each other (or on a feature that does), so none can go first.
    Cycle(Vec<String>),
}

impl std::fmt::Display for GroupRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupRefusal::Feature { id, reason } => write!(f, "feature {id} may not start a run: {reason}"),
            GroupRefusal::Duplicate(id) => write!(f, "feature {id} is named more than once"),
            GroupRefusal::Cycle(members) => write!(f, "these features depend on each other: {}", members.join(", ")),
        }
    }
}

impl std::error::Error for GroupRefusal {}

/// The group's features in the order to implement them: each after the group members it depends
/// on, and otherwise in the order given. Every member must be `pending`, and every dependency
/// outside the group must be `completed`.
pub fn order_run_group<'a>(ledger: &'a [FeatureEntry], ids: &[String]) -> Result<Vec<&'a FeatureEntry>, GroupRefusal> {
    order_group(ledger, ids, &fresh, &HashSet::new())
}

/// As [`order_run_group`], counting archived dependencies as completed.
pub fn order_run_group_with_archive<'a>(ledger: &'a [FeatureEntry], ids: &[String], archived: &HashSet<String>) -> Result<Vec<&'a FeatureEntry>, GroupRefusal> {
    order_group(ledger, ids, &fresh, archived)
}

/// As [`order_resume_group`], counting archived dependencies as completed.
pub fn order_resume_group_with_archive<'a>(
    ledger: &'a [FeatureEntry],
    ids: &[String],
    free: &[String],
    archived: &HashSet<String>,
) -> Result<Vec<&'a FeatureEntry>, GroupRefusal> {
    order_group(ledger, ids, &|entry| resumable(entry, free), archived)
}

/// As [`order_run_group`] for resuming a group's run: each member may be as the run's agent left it,
/// as [`check_resume_eligibility`] says.
pub fn order_resume_group<'a>(ledger: &'a [FeatureEntry], ids: &[String], free: &[String]) -> Result<Vec<&'a FeatureEntry>, GroupRefusal> {
    order_group(ledger, ids, &|entry| resumable(entry, free), &HashSet::new())
}

fn order_group<'a>(
    ledger: &'a [FeatureEntry],
    ids: &[String],
    startable: &dyn Fn(&FeatureEntry) -> Result<(), Ineligible>,
    archived: &HashSet<String>,
) -> Result<Vec<&'a FeatureEntry>, GroupRefusal> {
    if let Some(index) = (1..ids.len()).find(|&i| ids[..i].contains(&ids[i])) {
        return Err(GroupRefusal::Duplicate(ids[index].clone()));
    }
    let mut members: Vec<&FeatureEntry> = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = ledger.iter().find(|e| &e.feature_name == id);
        let refuse = |reason| GroupRefusal::Feature { id: id.clone(), reason };
        let entry = entry.ok_or_else(|| refuse(Ineligible::NotFound))?;
        startable(entry).map_err(refuse)?;
        let unmet: Vec<UnmetDependency> = entry
            .depends_on
            .iter()
            .filter(|dep| !ids.contains(dep))
            .filter_map(|dep| match ledger.iter().find(|e| &e.feature_name == dep) {
                Some(e) if e.status.known() == Some(FeatureStatus::Completed) => None,
                Some(e) => Some(UnmetDependency { name: dep.clone(), status: Some(e.status.as_str().to_string()) }),
                None if archived.contains(dep.as_str()) => None,
                None => Some(UnmetDependency { name: dep.clone(), status: None }),
            })
            .collect();
        if !unmet.is_empty() {
            return Err(refuse(Ineligible::UnmetDependencies(unmet)));
        }
        members.push(entry);
    }
    let mut ordered: Vec<&FeatureEntry> = Vec::with_capacity(members.len());
    while !members.is_empty() {
        let next = members
            .iter()
            .position(|m| m.depends_on.iter().all(|dep| !ids.contains(dep) || ordered.iter().any(|o| &o.feature_name == dep)));
        match next {
            Some(index) => ordered.push(members.remove(index)),
            None => return Err(GroupRefusal::Cycle(members.iter().map(|m| m.feature_name.clone()).collect())),
        }
    }
    Ok(ordered)
}
