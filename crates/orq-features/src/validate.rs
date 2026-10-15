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

/// Names come from the ledger file, so control characters are escaped before
/// a message can reach a terminal.
fn shown(name: &str) -> String {
    name.chars()
        .flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] })
        .collect()
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::BlankName => write!(f, "featureName must not be blank"),
            ValidationError::DuplicateName { name } => write!(f, "featureName \"{}\" is repeated", shown(name)),
            ValidationError::MissingDependency { feature, missing } => {
                write!(f, "feature \"{}\" depends on \"{}\", which does not exist", shown(feature), shown(missing))
            }
            ValidationError::SelfDependency { feature } => write!(f, "feature \"{}\" depends on itself", shown(feature)),
            ValidationError::Cycle { features } => {
                write!(f, "dependency cycle among: {}", features.iter().map(|n| shown(n)).collect::<Vec<_>>().join(", "))
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

/// The features of the first dependency cycle in file order: the smallest
/// strongly connected component with more than one member, found by an
/// iterative Tarjan pass (linear, no recursion depth to blow). Features that
/// merely lead into or out of a cycle are not members of it.
fn find_cycle(entries: &[FeatureEntry]) -> Option<Vec<String>> {
    let index_of: HashMap<&str, usize> =
        entries.iter().enumerate().map(|(i, e)| (e.feature_name.as_str(), i)).collect();
    let edges: Vec<Vec<usize>> = entries
        .iter()
        .map(|e| e.depends_on.iter().filter_map(|d| index_of.get(d.as_str()).copied()).collect())
        .collect();
    let n = entries.len();
    let unvisited = usize::MAX;
    let mut order = vec![unvisited; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_order = 0;
    let mut first: Option<(usize, Vec<usize>)> = None;
    for root in 0..n {
        if order[root] != unvisited {
            continue;
        }
        // (node, next edge to look at)
        let mut work = vec![(root, 0usize)];
        order[root] = next_order;
        low[root] = next_order;
        next_order += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&(node, edge)) = work.last() {
            if let Some(&to) = edges[node].get(edge) {
                work.last_mut().unwrap().1 += 1;
                if order[to] == unvisited {
                    order[to] = next_order;
                    low[to] = next_order;
                    next_order += 1;
                    stack.push(to);
                    on_stack[to] = true;
                    work.push((to, 0));
                } else if on_stack[to] {
                    low[node] = low[node].min(order[to]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == order[node] {
                let mut component = Vec::new();
                loop {
                    let member = stack.pop().unwrap();
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                if component.len() > 1 {
                    let earliest = *component.iter().min().unwrap();
                    if first.as_ref().is_none_or(|(best, _)| earliest < *best) {
                        first = Some((earliest, component));
                    }
                }
            }
        }
    }
    first.map(|(_, component)| {
        let mut names: Vec<String> = component.into_iter().map(|i| entries[i].feature_name.clone()).collect();
        names.sort();
        names
    })
}
