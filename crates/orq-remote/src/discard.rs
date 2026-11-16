//! Discarding a run the way the popup and `orq run discard` do: the run goes, its features go back to
//! pending unless its work is on the target branch, and a note says what went with it. Shared by the CLI
//! and the web so both say the same thing.

use std::path::Path;

/// What a discard did: the note to show after "Discarded {run}", and whether the run's work was on its
/// target branch (the popup calls that Clear).
pub struct Discarded {
    pub note: String,
    /// The note for the web: the same, but a kept worktree is named within the project, never by where it
    /// is on this machine.
    pub web_note: String,
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
    let mut web_note = note.clone();
    if let Some(kept) = outcome.kept_worktree {
        note.push_str(&format!(" Uncommitted work: kept {} and its branch.", printable(&kept.display().to_string())));
        web_note.push_str(&format!(" Uncommitted work: kept {} and its branch.", printable(&within_project(&kept, repo))));
        if let Some(reason) = outcome.kept_reason {
            note.push_str(&format!(" Reason: {}.", printable(&reason)));
            web_note.push_str(&format!(" Reason: {}.", printable(&paths_within_project(&reason, repo))));
        }
    }
    if let Some(deleted) = outcome.deleted_branch {
        if deleted.commits_ahead != Some(0) {
            let line = format!(
                " Deleted the branch {} with {}; its last commit was {}.",
                printable(&deleted.name),
                commits_not_on_target(deleted.commits_ahead),
                printable(&deleted.last_commit)
            );
            note.push_str(&line);
            web_note.push_str(&line);
        }
    }
    Ok(Discarded { note, web_note, merged: outcome.merged })
}

/// `path` relative to the project at `repo` when it is inside it, else only its last component. A path
/// that no longer exists is compared as written as well as resolved.
fn within_project(path: &Path, repo: &Path) -> String {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    for (path, repo) in [(canonical(path), canonical(repo)), (path.to_path_buf(), repo.to_path_buf())] {
        if let Ok(inside) = path.strip_prefix(&repo) {
            return inside.display().to_string();
        }
    }
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `text` with every absolute path in it named as [`within_project`] names it, for a message the web shows.
pub(crate) fn paths_within_project(text: &str, repo: &Path) -> String {
    let word = |word: &str| {
        if !word.starts_with('/') {
            return word.to_string();
        }
        let path = word.trim_end_matches([';', ':', ',', '.', ')', '\'', '"']);
        format!("{}{}", within_project(Path::new(path), repo), &word[path.len()..])
    };
    text.split(' ').map(word).collect::<Vec<_>>().join(" ")
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
    discard_warning_or_refusal(root, run_id, repo).unwrap_or_default()
}

/// [`discard_warning`], or why the run cannot be discarded at all, worded "<run> ...", when a later
/// run took it over.
pub fn discard_warning_or_refusal(root: &Path, run_id: &str, repo: &Path) -> Result<String, String> {
    let store = orq_orchestrator::RunStore::new(root);
    match orq_orchestrator::discard_preview(&store, run_id, repo) {
        Ok(Some(branch)) if branch.commits_ahead != Some(0) => {
            Ok(format!("its branch {} goes with {}", printable(&branch.name), commits_not_on_target(branch.commits_ahead)))
        }
        Err(why) if why.starts_with(&format!("{run_id} ")) => Err(why),
        _ => Ok(String::new()),
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
    let ledger = orq_features::ledger_path(&project);
    let mut note = match orq_features::reopen_discarded_features(&ledger, members, &completed_too) {
        Ok(reopened) => features_are(&reopened, "pending again"),
        Err(err) => {
            eprintln!("orq: cannot put {} back to pending: {}", printable(&members.join(",")), printable(&err));
            String::new()
        }
    };
    note.push_str(&abandon_fix_features(store, run_id, &ledger));
    note
}

/// Set the features the discarded run's fix sessions started `abandoned`, but for those a later run
/// took, and say which were. Their work goes with the run; nobody asked for them as features.
fn abandon_fix_features(store: &orq_orchestrator::RunStore, run_id: &str, ledger: &Path) -> String {
    let started = orq_orchestrator::fix_features(store, run_id).unwrap_or_else(|err| {
        eprintln!("orq: cannot read which features the fix sessions of {} started: {err:?}", printable(run_id));
        Vec::new()
    });
    if started.is_empty() {
        return String::new();
    }
    let free = orq_orchestrator::features_no_later_run_took(store, run_id, &started).unwrap_or_else(|err| {
        eprintln!("orq: the features the fix sessions started are left as they are: cannot read the other runs: {}", printable(&err));
        Vec::new()
    });
    // An abandoned feature is not stamped with when, so no time is given.
    match orq_features::close_features(ledger, &free, orq_core::FeatureStatus::Abandoned, "") {
        Ok(abandoned) => features_are(&abandoned, "abandoned"),
        Err(err) => {
            eprintln!("orq: cannot set {} abandoned: {}", printable(&free.join(",")), printable(&err.to_string()));
            String::new()
        }
    }
}

/// " A, B are {what}." for the features named, or nothing when there are none.
fn features_are(names: &[String], what: &str) -> String {
    match names.len() {
        0 => String::new(),
        1 => format!(" {} is {what}.", printable(&names[0])),
        _ => format!(" {} are {what}.", printable(&names.join(", "))),
    }
}
