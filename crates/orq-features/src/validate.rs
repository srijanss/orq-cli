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

/// What `name` would need changed to be a feature name, or `None` when it is one: only ASCII letters, digits,
/// '-', '_' and '.', and no leading '.', so a name can never be a path. The refusal names the first character
/// it refused, its code point and its position (in characters, from 1), so a look-alike dash or a hidden
/// space can be found; a space is written as "a space" and an invisible character only by its code point,
/// so nothing invisible or controlling reaches the reader raw. The web page holds the same rule.
pub fn feature_name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("a feature name is required".to_string());
    }
    if name.starts_with('.') {
        return Some("a feature name may not start with '.'".to_string());
    }
    let (index, bad) = name.chars().enumerate().find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))?;
    let code = format!("U+{:04X}", bad as u32);
    let shown = match bad {
        ' ' => "a space".to_string(),
        c if invisible(c) => code,
        c => format!("\"{c}\" ({code})"),
    };
    let hint = match bad {
        '\u{2014}' | '\u{2013}' | '\u{2010}' | '\u{2011}' | '\u{2212}' => " (use '-')",
        c if c.is_whitespace() => " (use '-' between words)",
        _ => "",
    };
    Some(format!("{shown} at position {} is not allowed in a feature name; use letters, digits, '-', '_' or '.'{hint}", index + 1))
}

/// A character that shows nothing, or changes how the text around it shows: whitespace, a control character,
/// or a format character (zero-width spaces and joiners, bidirectional marks, the byte-order mark).
fn invisible(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '\u{ad}' | '\u{61c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}')
}

/// Validates a whole candidate array: unique names, every dependency present,
/// no self-dependency, no cycle.
pub fn validate_entries(entries: &[FeatureEntry]) -> Result<(), ValidationError> {
    validate_entries_with_archive(entries, &HashSet::new())
}

/// As [`validate_entries`], but a dependency naming a completed feature in `archived` is accepted
/// (it is dropped on save, see [`drop_archived_dependencies`]).
pub fn validate_entries_with_archive(entries: &[FeatureEntry], archived: &HashSet<String>) -> Result<(), ValidationError> {
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
            if !seen.contains(dep.as_str()) && !archived.contains(dep.as_str()) {
                return Err(ValidationError::MissingDependency {
                    feature: entry.feature_name.clone(),
                    missing: dep.clone(),
                });
            }
        }
    }
    find_cycle(entries).map_or(Ok(()), |features| Err(ValidationError::Cycle { features }))
}

/// A problem in an entry that was already in the project's ledger, which a save of other entries goes past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingProblem(pub ValidationError);

impl std::fmt::Display for ExistingProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the project's ledger has a problem in an existing entry: {}", self.0)
    }
}

/// As [`validate_entries_with_archive`], but only a problem that involves a changed entry (`changed(index)`)
/// refuses: a repeated name with a changed entry among its holders, a changed entry's blank name, missing
/// dependency or self-dependency, or a cycle through a changed entry. Every other problem, in an entry that
/// was already there, is returned as an [`ExistingProblem`], in the order [`validate_entries`] would meet it.
pub fn validate_changed_entries(
    entries: &[FeatureEntry],
    archived: &HashSet<String>,
    changed: &dyn Fn(usize) -> bool,
) -> Result<Vec<ExistingProblem>, ValidationError> {
    let mut existing = Vec::new();
    let mut first_at: HashMap<&str, usize> = HashMap::new();
    let mut repeated: HashMap<&str, bool> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.feature_name.trim().is_empty() {
            if changed(index) {
                return Err(ValidationError::BlankName);
            }
            existing.push(ExistingProblem(ValidationError::BlankName));
            continue;
        }
        match first_at.get(entry.feature_name.as_str()) {
            None => {
                first_at.insert(entry.feature_name.as_str(), index);
            }
            Some(&first) => {
                let duplicate = ValidationError::DuplicateName { name: entry.feature_name.clone() };
                if changed(index) || changed(first) {
                    return Err(duplicate);
                }
                if repeated.insert(entry.feature_name.as_str(), true).is_none() {
                    existing.push(ExistingProblem(duplicate));
                }
            }
        }
    }
    for (index, entry) in entries.iter().enumerate() {
        for dep in &entry.depends_on {
            let problem = if dep == &entry.feature_name {
                ValidationError::SelfDependency { feature: dep.clone() }
            } else if !first_at.contains_key(dep.as_str()) && !archived.contains(dep.as_str()) {
                ValidationError::MissingDependency { feature: entry.feature_name.clone(), missing: dep.clone() }
            } else {
                continue;
            };
            if changed(index) {
                return Err(problem);
            }
            existing.push(ExistingProblem(problem));
        }
    }
    for members in find_cycles(entries) {
        let cycle = ValidationError::Cycle { features: cycle_names(entries, &members) };
        if members.iter().any(|&member| changed(member)) {
            return Err(cycle);
        }
        existing.push(ExistingProblem(cycle));
    }
    Ok(existing)
}

/// For each entry, a note per dependency that names an archived feature (and no live one):
/// "<name> is archived and completed: not needed".
pub fn archived_dependency_notes(entries: &[FeatureEntry], archived: &HashSet<String>) -> Vec<(String, String)> {
    let live: HashSet<&str> = entries.iter().map(|e| e.feature_name.as_str()).collect();
    entries
        .iter()
        .flat_map(|e| {
            e.depends_on
                .iter()
                .filter(|d| archived.contains(d.as_str()) && !live.contains(d.as_str()))
                .map(|d| (e.feature_name.clone(), format!("{d} is archived and completed: not needed")))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Removes from every `depends_on` the names that are archived and not live.
pub fn drop_archived_dependencies(entries: &mut [FeatureEntry], archived: &HashSet<String>) {
    let live: HashSet<String> = entries.iter().map(|e| e.feature_name.clone()).collect();
    for entry in entries {
        entry.depends_on.retain(|d| live.contains(d) || !archived.contains(d));
    }
}

/// The features of the first dependency cycle in file order, sorted by name.
fn find_cycle(entries: &[FeatureEntry]) -> Option<Vec<String>> {
    find_cycles(entries).first().map(|members| cycle_names(entries, members))
}

fn cycle_names(entries: &[FeatureEntry], members: &[usize]) -> Vec<String> {
    let mut names: Vec<String> = members.iter().map(|&i| entries[i].feature_name.clone()).collect();
    names.sort();
    names
}

/// The indices of every dependency cycle's members, cycles in file order of their earliest member: each
/// strongly connected component with more than one member, found by an iterative Tarjan pass (linear, no
/// recursion depth to blow). Features that merely lead into or out of a cycle are not members of it.
fn find_cycles(entries: &[FeatureEntry]) -> Vec<Vec<usize>> {
    // A dependency leads to every entry with that name, so a repeated name cannot hide a cycle.
    let mut holders: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        holders.entry(e.feature_name.as_str()).or_default().push(i);
    }
    let edges: Vec<Vec<usize>> = entries
        .iter()
        .map(|e| e.depends_on.iter().flat_map(|d| holders.get(d.as_str()).into_iter().flatten().copied()).collect())
        .collect();
    let n = entries.len();
    let unvisited = usize::MAX;
    let mut order = vec![unvisited; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_order = 0;
    let mut cycles: Vec<(usize, Vec<usize>)> = Vec::new();
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
                    cycles.push((*component.iter().min().unwrap(), component));
                }
            }
        }
    }
    cycles.sort_by_key(|(earliest, _)| *earliest);
    cycles.into_iter().map(|(_, component)| component).collect()
}
