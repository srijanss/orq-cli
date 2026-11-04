//! Whether a ledger feature may start a pipeline run.

use std::collections::HashMap;

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
    let entry = ledger
        .iter()
        .find(|e| e.feature_name == feature_name)
        .ok_or(Ineligible::NotFound)?;
    if entry.status.known() != Some(FeatureStatus::Pending) {
        return Err(Ineligible::NotPending {
            status: entry.status.as_str().to_string(),
        });
    }
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
    if let Some(index) = (1..ids.len()).find(|&i| ids[..i].contains(&ids[i])) {
        return Err(GroupRefusal::Duplicate(ids[index].clone()));
    }
    let mut members: Vec<&FeatureEntry> = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = ledger.iter().find(|e| &e.feature_name == id);
        let refuse = |reason| GroupRefusal::Feature { id: id.clone(), reason };
        let entry = entry.ok_or_else(|| refuse(Ineligible::NotFound))?;
        if entry.status.known() != Some(FeatureStatus::Pending) {
            return Err(refuse(Ineligible::NotPending { status: entry.status.as_str().to_string() }));
        }
        let unmet: Vec<UnmetDependency> = entry
            .depends_on
            .iter()
            .filter(|dep| !ids.contains(dep))
            .filter_map(|dep| match ledger.iter().find(|e| &e.feature_name == dep) {
                Some(e) if e.status.known() == Some(FeatureStatus::Completed) => None,
                Some(e) => Some(UnmetDependency { name: dep.clone(), status: Some(e.status.as_str().to_string()) }),
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
