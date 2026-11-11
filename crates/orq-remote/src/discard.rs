//! Discarding a run the way the popup and `orq run discard` do: the run goes, its features go back to
//! pending unless its work is on the target branch, and a note says what went with it. Shared by the CLI
//! and the web so both say the same thing.

use std::path::Path;

/// What a discard did: the note to show after "Discarded {run}", and whether the run's work was on its
/// target branch (the popup calls that Clear).
pub struct Discarded {
    pub note: String,
    pub merged: bool,
}

/// Diagnostics can carry repository-controlled text (paths, ledger names), so control characters are
/// shown escaped.
fn printable(text: &str) -> String {
    text.chars().flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] }).collect()
}

/// Discard `run_id`, putting the features it held back to pending in the project's own ledger before
/// their locks are released, and say which were and where a worktree with uncommitted work was kept. A
/// run whose work is on its target branch leaves its features as they are.
pub fn discard_and_reopen(root: &Path, run_id: &str, repo: &Path) -> Result<Discarded, String> {
    let mut note = String::new();
    let store = orq_orchestrator::RunStore::new(root);
    let outcome = orq_orchestrator::discard_run_then(&store, run_id, repo, &mut |project_id, features| {
        note = reopen_discarded(&store, run_id, repo, project_id, features);
    })?;
    if outcome.merged {
        note.push_str(" Its work is on the target branch; its features stay as the ledger has them.");
    }
    if let Some(kept) = outcome.kept_worktree {
        note.push_str(&format!(" Uncommitted work: kept {} and its branch.", printable(&kept.display().to_string())));
    }
    if let Some(deleted) = outcome.deleted_branch {
        if deleted.commits_ahead != Some(0) {
            note.push_str(&format!(
                " Deleted the branch {} with {}; its last commit was {}.",
                printable(&deleted.name),
                commits_not_on_target(deleted.commits_ahead),
                printable(&deleted.last_commit)
            ));
        }
    }
    Ok(Discarded { note, merged: outcome.merged })
}

/// "2 commits not on the target branch", or "commits the target branch may not have" when the count
/// could not be told.
fn commits_not_on_target(count: Option<usize>) -> String {
    match count {
        Some(1) => "1 commit not on the target branch".to_string(),
        Some(n) => format!("{n} commits not on the target branch"),
        None => "commits the target branch may not have".to_string(),
    }
}

/// What a discard of `run_id` would take with the branch, for a prompt shown before it: empty when no
/// commit would go, or when that cannot be told.
pub fn discard_warning(root: &Path, run_id: &str, repo: &Path) -> String {
    let store = orq_orchestrator::RunStore::new(root);
    match orq_orchestrator::discard_preview(&store, run_id, repo) {
        Ok(Some(branch)) if branch.commits_ahead != Some(0) => {
            format!("its branch {} goes with {}", printable(&branch.name), commits_not_on_target(branch.commits_ahead))
        }
        _ => String::new(),
    }
}

/// Put the features of a discarded run back to `pending`, and say which were. Only the ledger of the
/// run's own project is touched: another repository may have features of the same names. A feature
/// marked `completed` is reopened too, as its work goes with the run, unless a later run took it. A
/// ledger that cannot be updated is only warned about: the run is going regardless.
fn reopen_discarded(store: &orq_orchestrator::RunStore, run_id: &str, repo: &Path, project_id: &str, members: &[String]) -> String {
    let project = match orq_git::resolve_project(repo) {
        Ok(project) => project,
        Err(err) => {
            eprintln!("orq: cannot find the feature ledger of {}: {err:?}", repo.display());
            return String::new();
        }
    };
    if orq_git::project_id(&project) != project_id {
        eprintln!("orq: {} is not the project of the run; its features are left as they are", repo.display());
        return String::new();
    }
    let completed_too = orq_orchestrator::features_no_later_run_took(store, run_id, members).unwrap_or_else(|err| {
        eprintln!("orq: features marked completed stay completed: cannot read the other runs: {}", printable(&err));
        Vec::new()
    });
    match orq_features::reopen_discarded_features(&orq_features::ledger_path(&project), members, &completed_too) {
        Ok(reopened) if reopened.is_empty() => String::new(),
        Ok(reopened) => format!(" {} {} pending again.", printable(&reopened.join(", ")), if reopened.len() == 1 { "is" } else { "are" }),
        Err(err) => {
            eprintln!("orq: cannot put {} back to pending: {}", printable(&members.join(",")), printable(&err));
            String::new()
        }
    }
}
