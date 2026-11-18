//! Discarding a failed, blocked or paused run: remove its worktree and branch when they hold no
//! uncommitted work, release its feature locks, and forget its record and logs.

use std::path::{Path, PathBuf};

use crate::outcome::git_raw;
use crate::resume::members_of;
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Claim, RunStore, StoreError};

/// What a discard did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardOutcome {
    /// The run's features.
    pub features: Vec<String>,
    /// The worktree left in place, with its branch, because it held uncommitted or untracked work.
    pub kept_worktree: Option<PathBuf>,
    /// Why the worktree was kept, naming the first file that holds work, when it was.
    pub kept_reason: Option<String>,
    /// The branch deleted with the worktree, and the commits that went with it.
    pub deleted_branch: Option<DeletedBranch>,
    /// The run's work was on its target branch (see [`crate::work_is_on_target`]), so its features
    /// were not handed back to be reopened.
    pub merged: bool,
}

/// A run's branch deleted by a discard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedBranch {
    /// The branch's name, without `refs/heads/`.
    pub name: String,
    /// How many of its commits the target branch does not have; `None` when that could not be told.
    pub commits_ahead: Option<usize>,
    /// The SHA the branch pointed at, from which its commits can still be recovered for a while.
    pub last_commit: String,
}

/// Remove the run `run_id` for good: its feature locks, its logs and its record, and its worktree
/// and local branch when the worktree is clean. A worktree with uncommitted or untracked work is
/// kept, with its branch, and reported. A run whose worktree is already gone keeps its branch: it
/// cannot be shown to belong to `repo`. Only a failed, blocked or paused run, whose process has
/// ended, can be discarded. Every check happens before anything is removed, and the record goes
/// last, so a failure part-way can be retried. `repo` is the repository the run's worktree belongs to.
pub fn discard_run(store: &RunStore, run_id: &str, repo: &Path) -> Result<DiscardOutcome, String> {
    discard_run_then(store, run_id, repo, &mut |_, _| {})
}

/// [`discard_run`], calling `orphaned(project_id, features)` once the worktree is gone and before any
/// lock is released, with the run's features that no other run holds: nothing continues those, and
/// no new run can take them while the call runs. A failed, blocked or review-waiting run whose work
/// is already on its target branch was merged by hand: its features are done, and `orphaned` is not
/// called.
pub fn discard_run_then(store: &RunStore, run_id: &str, repo: &Path, orphaned: &mut dyn FnMut(&str, &[String])) -> Result<DiscardOutcome, String> {
    discard_run_before_remove(store, run_id, repo, orphaned, &mut || {})
}

/// [`discard_run_then`], calling `before_remove()` between the worktree's cleanliness check and its
/// removal, so tests can write to it in that window.
#[doc(hidden)]
pub fn discard_run_before_remove(
    store: &RunStore,
    run_id: &str,
    repo: &Path,
    orphaned: &mut dyn FnMut(&str, &[String]),
    before_remove: &mut dyn FnMut(),
) -> Result<DiscardOutcome, String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    refuse_taken_over(store, run_id)?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let summary = summarize_run(&events);
    let phase = summary.phase;
    // A run whose pause is only requested is still running until it reaches a safe point.
    if !matches!(phase, RunPhase::Failed | RunPhase::Blocked | RunPhase::NeedsReview | RunPhase::Paused) {
        let continued = summary.resumed_as.map(|later| format!(", continued as {later}")).unwrap_or_default();
        return Err(format!("{run_id} is {}{continued}; only a failed, blocked, paused or review-waiting run can be discarded", phase.as_str()));
    }
    let branch = record.branch.as_str();
    if branch.is_empty() || branch == record.target_branch {
        return Err(format!("{run_id} records the branch {branch:?}, which must not be deleted"));
    }
    let git = |dir: &Path, args: &[&str]| git_raw(dir, args).map_err(|e: StoreError| e.to_string());
    let worktree = record.worktree.to_string_lossy().into_owned();
    // Every check happens before anything is removed.
    let has_worktree = record.worktree.exists();
    let mut kept_worktree = None;
    let mut kept_reason = None;
    if has_worktree {
        let same_as_repo = match (record.worktree.canonicalize(), repo.canonicalize()) {
            (Ok(worktree), Ok(repo)) => worktree == repo,
            _ => true,
        };
        if same_as_repo {
            return Err(format!("the worktree of {run_id} is the repository itself; refusing to remove it"));
        }
        let head = git(&record.worktree, &["symbolic-ref", "--quiet", "HEAD"])?;
        if !head.success || head.stdout.trim() != format!("refs/heads/{branch}") {
            return Err(format!("the worktree of {run_id} is not on its branch {branch}; refusing to remove it"));
        }
        let common = |dir: &Path| -> Result<Option<std::path::PathBuf>, String> {
            let out = git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
            Ok(out.success.then(|| std::path::PathBuf::from(out.stdout.trim())))
        };
        let (theirs, ours) = (common(&record.worktree)?, common(repo)?);
        if theirs.is_none() || ours.is_none() || theirs.and_then(|p| p.canonicalize().ok()) != ours.and_then(|p| p.canonicalize().ok()) {
            return Err(format!("{} is not the repository of {run_id}", repo.display()));
        }
        // Uncommitted, untracked or ignored work is never deleted: the worktree and its branch stay.
        // Ignored files count unless they are installed packages a committed lockfile brings back or
        // an ignored cache, as an unforced `worktree remove` would delete them.
        kept_reason = uncommitted_work(&record.worktree)
            .map_err(|reason| format!("cannot tell whether the worktree of {run_id} has uncommitted work: {reason}"))?
            .map(|work| work.reason());
        if kept_reason.is_some() {
            kept_worktree = Some(record.worktree.clone());
        }
    }
    if has_worktree && kept_worktree.is_none() {
        // `worktree add --force` lets several worktrees share a branch; deleting it would leave the
        // others on an unborn HEAD, so a branch checked out anywhere else is never deleted.
        let listing = git(repo, &["worktree", "list", "--porcelain", "-z"])?;
        if !listing.success {
            return Err(format!("cannot list the worktrees of {}: {}", repo.display(), listing.stderr.trim()));
        }
        let ours = record.worktree.canonicalize().map_err(|e| e.to_string())?;
        let wanted = format!("branch refs/heads/{branch}");
        let mut path: Option<String> = None;
        for field in listing.stdout.split('\0') {
            if let Some(p) = field.strip_prefix("worktree ") {
                path = Some(p.to_string());
            } else if field == wanted {
                let other = path.as_deref().unwrap_or_default();
                if Path::new(other).canonicalize().ok().as_ref() != Some(&ours) {
                    return Err(format!("the branch {branch} is checked out in {other}; refusing to delete it"));
                }
            }
        }
    }
    // What the branch holds is learned while the worktree is still there, so a git that cannot tell
    // stops the discard before anything is removed.
    let commits = if has_worktree && kept_worktree.is_none() { Some(branch_commits(repo, &record)?) } else { None };
    // Told while the branch is still there.
    let merged = matches!(phase, RunPhase::Failed | RunPhase::Blocked | RunPhase::NeedsReview) && crate::run_list::work_on_target(repo, &record)?;
    if has_worktree && kept_worktree.is_none() {
        before_remove();
        // Without --force git checks again as it removes, so work written since the check above is
        // never deleted. A worktree git will not remove (new work, or an initialized submodule,
        // which git never removes unforced) is kept with its branch like any other with work in it.
        let removed = git(repo, &["worktree", "remove", "--", &worktree])?;
        if !removed.success {
            if record.worktree.exists() {
                kept_worktree = Some(record.worktree.clone());
                kept_reason = Some(format!("git would not remove the worktree: {}", removed.stderr.trim()));
            } else {
                return Err(format!("git would not remove the worktree: {}", removed.stderr.trim()));
            }
        }
    }
    let mut deleted_branch = None;
    if has_worktree && kept_worktree.is_none() {
        let deleted = git(repo, &["update-ref", "-d", &format!("refs/heads/{branch}")])?;
        if !deleted.success {
            return Err(format!("the branch {branch} could not be deleted: {}", deleted.stderr.trim()));
        }
        deleted_branch = commits;
    }
    // Without its worktree nothing ties the run to `repo` (another clone may share the base commit
    // and a branch name), so no branch is deleted; only the run's own records go.
    // Release each member's lock on its own, so one benign outcome cannot hide another's failure.
    let members = members_of(&record, &events);
    // A member whose lock is free is claimed first, so no new run can start it while it is reopened.
    // If one cannot be claimed, the locks taken here are given back (the run's own stay, and so
    // does the record), so no feature is left blocked by a run that is not doing it.
    let mut ours = Vec::new();
    let mut taken = Vec::new();
    for member in &members {
        match store.claim_lock(&record.project_id, member, run_id) {
            Ok(Claim::Taken) => taken.push(member.clone()),
            Ok(Claim::AlreadyHeld) => {}
            Ok(Claim::HeldByAnother) => continue,
            Err(err) => {
                // Best effort: the error being returned is the one that matters.
                let _ = store.finish_group_run(&record.project_id, &taken, run_id);
                return Err(err.to_string());
            }
        }
        ours.push(member.clone());
    }
    if !ours.is_empty() && !merged {
        orphaned(&record.project_id, &ours);
    }
    for member in &members {
        match store.finish_group_run(&record.project_id, std::slice::from_ref(member), run_id) {
            // A lock that is gone, or already held by another run, is not this run's to release.
            Ok(()) | Err(StoreError::NotFound(_)) | Err(StoreError::FeatureBusy { .. }) => {}
            Err(err) => return Err(err.to_string()),
        }
    }
    store.remove_run(run_id).map_err(|e| e.to_string())?;
    Ok(DiscardOutcome { features: members, kept_worktree, kept_reason, deleted_branch, merged })
}

/// The branch a discard of `run_id` would delete now, with its commits, without changing anything:
/// `None` when the branch would stay, because the worktree is gone or holds uncommitted work. A run a
/// later run took over gets the reason a discard would be refused.
pub fn discard_preview(store: &RunStore, run_id: &str, repo: &Path) -> Result<Option<DeletedBranch>, String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    refuse_taken_over(store, run_id)?;
    if !record.worktree.exists() || uncommitted_work(&record.worktree)?.is_some() {
        return Ok(None);
    }
    branch_commits(repo, &record).map(Some)
}

/// The run's branch in `repo` with its last commit and how many of its commits the target lacks:
/// counted against the local target branch, else the remote's copy of it, else the run's base.
fn branch_commits(repo: &Path, record: &crate::store::RunRecord) -> Result<DeletedBranch, String> {
    let resolve = |rev: &str| -> Result<Option<String>, String> {
        let out = git_raw(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &format!("{rev}^{{commit}}")]).map_err(|e| e.to_string())?;
        Ok(out.success.then(|| out.stdout.trim().to_string()))
    };
    let name = record.branch.clone();
    let last_commit = resolve(&format!("refs/heads/{name}"))?.ok_or_else(|| format!("the branch {name} does not resolve to a commit"))?;
    let mut target = None;
    for rev in [format!("refs/heads/{}", record.target_branch), format!("refs/remotes/{}/{}", record.remote, record.target_branch), record.base_sha.clone()] {
        if !rev.is_empty() {
            if let Some(sha) = resolve(&rev)? {
                target = Some(sha);
                break;
            }
        }
    }
    let commits_ahead = match target {
        Some(target) => {
            let out = git_raw(repo, &["rev-list", "--count", &last_commit, "--not", &target]).map_err(|e| e.to_string())?;
            out.success.then(|| out.stdout.trim().parse().ok()).flatten()
        }
        None => None,
    };
    Ok(DeletedBranch { name, commits_ahead, last_commit })
}

/// Those of `features` that no other run of `run_id`'s project started later, as one of its features or
/// one its fix sessions added or carried on from the run it resumed. A later run that took a
/// feature is the one its ledger status speaks for, so a `completed` is not this run's to undo. Times
/// are compared at the second they were recorded, and a run started in the same second, or with no
/// time at all, counts as later: wrongly keeping `completed` is the mistake that can be seen and
/// corrected, launching finished work again is not. A run `run_id` resumed, however close in time, is
/// never a later one.
pub fn features_no_later_run_took(store: &RunStore, run_id: &str, features: &[String]) -> Result<Vec<String>, String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    let started = store.events(run_id).map_err(|e| e.to_string())?.first().map(|e| e.occurred_at.clone());
    let resumed = resumed_from(store, run_id).map_err(|e| e.to_string())?;
    let mut free = features.to_vec();
    for other in store.run_ids().map_err(|e| e.to_string())? {
        if other == run_id || resumed.contains(&other) {
            continue;
        }
        let other_record = store.load(&other).map_err(|e| e.to_string())?;
        if other_record.project_id != record.project_id {
            continue;
        }
        let events = store.events(&other).map_err(|e| e.to_string())?;
        let earlier = matches!((events.first(), &started), (Some(first), Some(started)) if first.occurred_at < *started);
        if !earlier {
            let mut taken = members_of(&other_record, &events);
            taken.extend(crate::fix_features::fix_features(store, &other).map_err(|e| e.to_string())?);
            free.retain(|feature| !taken.contains(feature));
        }
    }
    Ok(free)
}

/// Of `completed`, the features a ledger marks `completed` (each with its `recordedAt`, if any), those
/// that were finished outside run `run_id`, so a discard of it must leave them completed, each with why:
/// one recorded at a later second than the run's last event ("finished after this run stopped"), or one
/// a commit on the run's local target branch in `repo` since the run's base names in the house form,
/// `(<feature>)` or `(review finding …, <feature>)` ("its work is on <target> (<commit>)"). A
/// target or base git cannot find names no commit; a git that fails is an error.
pub fn features_finished_elsewhere(
    store: &RunStore,
    run_id: &str,
    repo: &Path,
    completed: &[(String, Option<String>)],
) -> Result<Vec<(String, String)>, String> {
    if completed.is_empty() {
        return Ok(Vec::new());
    }
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let stopped = events.last().and_then(|event| utc_second(&event.occurred_at));
    let commits = target_commits_since_base(repo, &record)?;
    let mut kept = Vec::new();
    for (feature, recorded_at) in completed {
        let after = matches!((recorded_at.as_deref().and_then(utc_second), stopped), (Some(recorded), Some(stopped)) if recorded > stopped);
        if after {
            kept.push((feature.clone(), "finished after this run stopped".to_string()));
        } else if let Some((sha, _)) = commits.iter().find(|(_, subject)| names_feature(subject, feature)) {
            kept.push((feature.clone(), format!("its work is on {} ({})", record.target_branch, &sha[..sha.len().min(8)])));
        }
    }
    Ok(kept)
}

/// The second an RFC 3339 UTC time names, as `YYYY-MM-DDTHH:MM:SS` text that sorts by time; `None`
/// for a time in another zone or not in that form, which is then never later than anything.
fn utc_second(time: &str) -> Option<&str> {
    // Ledger text: only plain ASCII is sliced at fixed byte offsets.
    if !time.is_ascii() {
        return None;
    }
    let (second, rest) = (time.get(..19)?, time.get(19..)?);
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    let well_formed = second.len() == 19
        && [4, 7].iter().all(|&i| &second[i..=i] == "-")
        && &second[10..11] == "T"
        && [13, 16].iter().all(|&i| &second[i..=i] == ":")
        && [&second[..4], &second[5..7], &second[8..10], &second[11..13], &second[14..16], &second[17..19]].iter().all(|part| digits(part));
    let zone = match rest.strip_prefix('.') {
        Some(fraction) => fraction.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => rest,
    };
    (well_formed && matches!(zone, "Z" | "z" | "+00:00")).then_some(second)
}

/// The commits on the local target branch of `record` in `repo` that its base does not have, newest
/// first, as (SHA, subject); none when the target branch or the base is not there.
fn target_commits_since_base(repo: &Path, record: &crate::store::RunRecord) -> Result<Vec<(String, String)>, String> {
    if record.target_branch.is_empty() || record.base_sha.is_empty() {
        return Ok(Vec::new());
    }
    let target = format!("refs/heads/{}", record.target_branch);
    let failed = |e: String| format!("could not read the commits on {} since the base of {}: {e}", record.target_branch, record.run_id);
    for rev in [&target, &record.base_sha] {
        let out = git_raw(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &format!("{rev}^{{commit}}")]).map_err(|e| failed(e.to_string()))?;
        match out.code {
            Some(0) => {}
            Some(1) => return Ok(Vec::new()),
            _ => return Err(failed(out.stderr.trim().to_string())),
        }
    }
    let out = git_raw(repo, &["log", "--format=%H%x1f%s", "--end-of-options", &target, &format!("^{}", record.base_sha)]).map_err(|e| failed(e.to_string()))?;
    if !out.success {
        return Err(failed(out.stderr.trim().to_string()));
    }
    Ok(out.stdout.lines().filter_map(|line| line.split_once('\u{1f}')).map(|(sha, subject)| (sha.to_string(), subject.to_string())).collect())
}

/// Whether a commit `subject` names `feature` in the house form: `(<feature>)` or
/// `(review finding …, <feature>)`.
fn names_feature(subject: &str, feature: &str) -> bool {
    let finding_of = format!(", {feature}");
    subject.split('(').skip(1).filter_map(|group| group.split_once(')').map(|(inside, _)| inside)).any(|inside| {
        inside == feature || (inside.starts_with("review finding ") && inside.ends_with(&finding_of))
    })
}

/// The runs `run_id` carried on from, as its fix-session features or its sessions record them: the
/// run it resumed, the run that one resumed, and so on, whichever record each run left.
fn resumed_from(store: &RunStore, run_id: &str) -> Result<Vec<String>, StoreError> {
    let carries = |kind: &str| kind == crate::fix_features::ADDED || kind == "run.sessions_carried";
    let mut runs: Vec<String> = Vec::new();
    let mut next = vec![run_id.to_string()];
    while let Some(run) = next.pop() {
        for event in store.events(&run)?.iter().filter(|e| carries(&e.kind)) {
            if let Some(from) = event.payload.get("from").and_then(|f| f.as_str()) {
                if from != run_id && !runs.iter().any(|r| r == from) {
                    runs.push(from.to_string());
                    next.push(from.to_string());
                }
            }
        }
    }
    Ok(runs)
}

/// The reason `run_id` must not be discarded or accepted when a later run took it over: that run
/// works on the same branch or in the same worktree, so it is the one to discard or accept.
pub fn refuse_taken_over(store: &RunStore, run_id: &str) -> Result<(), String> {
    match crate::resume::taken_over_by(store, run_id)? {
        Some(later) => Err(format!("{run_id} was continued as {later}; discard or accept that run instead")),
        None => Ok(()),
    }
}

/// What in `worktree` would be lost with it, from one status scan; git's message when it cannot tell.
fn uncommitted_work(worktree: &Path) -> Result<Option<crate::cleanup::UncommittedWork>, String> {
    crate::cleanup::read_uncommitted_work(worktree).map_err(|e| e.to_string())?
}
