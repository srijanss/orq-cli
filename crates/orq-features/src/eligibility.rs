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
