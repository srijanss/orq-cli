use std::collections::{HashMap, HashSet};

use crate::{Candidate, FeatureEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateProblem {
    /// `dependsOn` names a feature that is neither in the ledger nor a new candidate.
    UnknownDependency(String),
    /// The candidate is in, or depends on, a dependency cycle (including on itself).
    Cycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// May be added; nothing in the ledger changes.
    New,
    /// The name already exists; the existing entry and its status are kept.
    DuplicateOfExisting,
    /// An earlier candidate in the same output already has this name.
    DuplicateInBatch,
    Invalid(Vec<CandidateProblem>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedCandidate {
    pub candidate: Candidate,
    pub verdict: Verdict,
    /// The agent proposed `parallelizable: true`. Only a proposal: scheduling
    /// safety is decided later from `dependsOn`.
    pub suggests_parallel: bool,
}

/// Checks agent candidates against the current ledger without changing it.
/// Candidates keep their order; ledger-owned fields were already dropped by
/// extraction and are listed in `candidate.ignored_fields`.
pub fn review_candidates(existing: &[FeatureEntry], candidates: Vec<Candidate>) -> Vec<ReviewedCandidate> {
    let existing_names: HashSet<&str> = existing.iter().map(|e| e.feature_name.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut verdicts: Vec<Verdict> = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let name = candidate.draft.feature_name.as_str();
        verdicts.push(if existing_names.contains(name) {
            Verdict::DuplicateOfExisting
        } else if !seen.insert(name.to_string()) {
            Verdict::DuplicateInBatch
        } else {
            Verdict::New
        });
    }

    let new_names: HashSet<&str> = candidates
        .iter()
        .zip(&verdicts)
        .filter(|(_, v)| **v == Verdict::New)
        .map(|(c, _)| c.draft.feature_name.as_str())
        .collect();

    // Unknown dependencies.
    let mut problems: Vec<Vec<CandidateProblem>> = vec![Vec::new(); candidates.len()];
    // Dependency names already reported per candidate, so repeats stay O(1).
    let mut reported: Vec<HashSet<&str>> = vec![HashSet::new(); candidates.len()];
    for (i, candidate) in candidates.iter().enumerate() {
        if verdicts[i] != Verdict::New {
            continue;
        }
        for dep in candidate.draft.depends_on.iter().flatten() {
            if !existing_names.contains(dep.as_str()) && !new_names.contains(dep.as_str()) {
                if reported[i].insert(dep.as_str()) {
                    problems[i].push(CandidateProblem::UnknownDependency(dep.clone()));
                }
            }
        }
    }

    // A candidate that depends on one that is itself invalid cannot be added
    // either: propagate until nothing changes.
    let by_name: HashMap<&str, usize> = candidates
        .iter()
        .enumerate()
        .filter(|(i, _)| verdicts[*i] == Verdict::New)
        .map(|(i, c)| (c.draft.feature_name.as_str(), i))
        .collect();
    // Reverse edges, so each node and edge is visited once.
    let mut reverse: Vec<Vec<(usize, &str)>> = vec![Vec::new(); candidates.len()];
    for (i, candidate) in candidates.iter().enumerate() {
        if verdicts[i] != Verdict::New {
            continue;
        }
        for dep in candidate.draft.depends_on.iter().flatten() {
            if let Some(&j) = by_name.get(dep.as_str()) {
                if j != i {
                    reverse[j].push((i, dep.as_str()));
                }
            }
        }
    }
    let mut queued: Vec<bool> = problems.iter().map(|p| !p.is_empty()).collect();
    let mut work: Vec<usize> = (0..candidates.len()).filter(|&i| queued[i]).collect();
    while let Some(j) = work.pop() {
        for &(i, dep) in &reverse[j] {
            if reported[i].insert(dep) {
                problems[i].push(CandidateProblem::UnknownDependency(dep.to_string()));
                if !queued[i] {
                    queued[i] = true;
                    work.push(i);
                }
            }
        }
    }

    // Cycles among new candidates (existing entries are already acyclic and
    // never depend on candidates): peel off nodes whose new-candidate
    // dependencies are all resolved; what remains is in or behind a cycle.
    let index: HashMap<&str, usize> = candidates
        .iter()
        .enumerate()
        .filter(|(i, _)| verdicts[*i] == Verdict::New)
        .map(|(i, c)| (c.draft.feature_name.as_str(), i))
        .collect();
    let mut remaining: Vec<usize> = vec![0; candidates.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); candidates.len()];
    for (&_name, &i) in &index {
        for dep in candidates[i].draft.depends_on.iter().flatten() {
            if let Some(&j) = index.get(dep.as_str()) {
                remaining[i] += 1;
                dependents[j].push(i);
            }
        }
    }
    let mut queue: Vec<usize> = index.values().copied().filter(|&i| remaining[i] == 0).collect();
    let mut peeled = 0;
    while let Some(i) = queue.pop() {
        peeled += 1;
        for &d in &dependents[i] {
            remaining[d] -= 1;
            if remaining[d] == 0 {
                queue.push(d);
            }
        }
    }
    if peeled < index.len() {
        for &i in index.values() {
            if remaining[i] > 0 {
                problems[i].push(CandidateProblem::Cycle);
            }
        }
    }

    candidates
        .into_iter()
        .zip(verdicts)
        .zip(problems)
        .map(|((candidate, verdict), problems)| {
            let verdict = if verdict == Verdict::New && !problems.is_empty() {
                Verdict::Invalid(problems)
            } else {
                verdict
            };
            let suggests_parallel = candidate.draft.parallelizable == Some(true);
            ReviewedCandidate { candidate, verdict, suggests_parallel }
        })
        .collect()
}
