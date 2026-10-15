use std::collections::{HashMap, HashSet};

use crate::FeatureEntry;

/// Why a candidate ledger array is not acceptable. Each variant names the
/// features involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// A featureName that is empty or only whitespace.
    BlankName,
    DuplicateName { name: String },
    MissingDependency { feature: String, missing: String },
    SelfDependency { feature: String },
    /// The features on one dependency cycle.
    Cycle { features: Vec<String> },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::BlankName => write!(f, "featureName must not be blank"),
            ValidationError::DuplicateName { name } => write!(f, "featureName \"{name}\" is repeated"),
            ValidationError::MissingDependency { feature, missing } => {
                write!(f, "feature \"{feature}\" depends on \"{missing}\", which does not exist")
            }
            ValidationError::SelfDependency { feature } => write!(f, "feature \"{feature}\" depends on itself"),
            ValidationError::Cycle { features } => {
                write!(f, "dependency cycle among: {}", features.join(", "))
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validates a whole candidate array: unique names, every dependency present,
/// no self-dependency, no cycle.
pub fn validate_entries(entries: &[FeatureEntry]) -> Result<(), ValidationError> {
    let mut seen = HashSet::new();
    for entry in entries {
        if entry.feature_name.trim().is_empty() {
            return Err(ValidationError::BlankName);
        }
        if !seen.insert(entry.feature_name.as_str()) {
            return Err(ValidationError::DuplicateName { name: entry.feature_name.clone() });
        }
    }
    for entry in entries {
        for dep in &entry.depends_on {
            if dep == &entry.feature_name {
                return Err(ValidationError::SelfDependency { feature: dep.clone() });
            }
            if !seen.contains(dep.as_str()) {
                return Err(ValidationError::MissingDependency {
                    feature: entry.feature_name.clone(),
                    missing: dep.clone(),
                });
            }
        }
    }
    find_cycle(entries).map_or(Ok(()), |features| Err(ValidationError::Cycle { features }))
}

/// Kahn's algorithm: whatever cannot be ordered lies on or behind a cycle;
/// peeling features nothing remaining depends on leaves only cycle members.
fn find_cycle(entries: &[FeatureEntry]) -> Option<Vec<String>> {
    let mut deps: HashMap<&str, HashSet<&str>> = entries
        .iter()
        .map(|e| (e.feature_name.as_str(), e.depends_on.iter().map(String::as_str).collect()))
        .collect();
    loop {
        let ready: Vec<&str> = deps.iter().filter(|(_, d)| d.is_empty()).map(|(n, _)| *n).collect();
        if ready.is_empty() {
            break;
        }
        for name in ready {
            deps.remove(name);
            for d in deps.values_mut() {
                d.remove(name);
            }
        }
    }
    if deps.is_empty() {
        return None;
    }
    // Remaining features may merely depend on a cycle: peel those nothing
    // remaining depends on.
    loop {
        let needed: HashSet<&str> = deps.values().flatten().copied().collect();
        let leaves: Vec<&str> = deps.keys().filter(|n| !needed.contains(*n)).copied().collect();
        if leaves.is_empty() {
            break;
        }
        for leaf in leaves {
            deps.remove(leaf);
        }
    }
    let mut names: Vec<String> = deps.keys().map(|n| n.to_string()).collect();
    names.sort();
    Some(names)
}
