//! After a rebase stopped on a conflict: let a harness session resolve it. If
//! the rebase is still not finished afterwards, nothing is aborted: the
//! worktree stays mid-rebase and the run stays blocked with a recovery hint.

use crate::implement::{run_session_stage, ImplementSession};
use crate::outcome::git_raw;
use crate::rebase::record_event;
use crate::store::{RunRecord, RunStore, StoreError};

const FIX_STAGE: &str = "rebase_fix";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictFixOutcome {
    /// The rebase is complete; the run can go on.
    Resolved,
    /// The rebase is still in progress; the run stays blocked.
    StillBlocked { reason: String },
}

pub fn fix_rebase_conflicts(
    store: &RunStore,
    run_id: &str,
    session: &ImplementSession<'_>,
) -> Result<ConflictFixOutcome, StoreError> {
    let record = store.load(run_id)?;
    if !rebase_in_progress(&record)? {
        return Err(StoreError::Corrupt(
            "the worktree is not in the middle of a rebase".to_string(),
        ));
    }
    // The commit the rebase is replaying onto, read before the session can touch it.
    let onto = rebase_onto(&record)?;
    // The session's own success is not trusted: only the repository state counts.
    run_session_stage(store, run_id, session, FIX_STAGE, 1)?;
    let still_rebasing = rebase_in_progress(&record)?;
    if !still_rebasing && completed_onto(&record, &onto)? {
        record_event(
            store,
            &record,
            "rebase.completed",
            serde_json::json!({ "resolved_by": "session", "target_sha": onto }),
        )?;
        return Ok(ConflictFixOutcome::Resolved);
    }
    let reason = if still_rebasing {
        format!(
            "the rebase onto {} is still in progress after the fix session; resolve the conflicts in {}, \
             then run `git rebase --continue` there",
            record.target_branch,
            record.worktree.display()
        )
    } else {
        format!(
            "the fix session ended the rebase onto {} without completing it; check the branch {} in {} \
             and rebase it again",
            record.target_branch,
            record.branch,
            record.worktree.display()
        )
    };
    record_event(
        store,
        &record,
        "run.blocked",
        serde_json::json!({ "reason": reason }),
    )?;
    Ok(ConflictFixOutcome::StillBlocked { reason })
}

/// The most commits of each side the conflict session's task lists, and the longest line of one.
const MAX_COMMITS: usize = 20;
const MAX_SUBJECT: usize = 120;

/// The task of the session that resolves the conflict a run's rebase stopped on: where the rebase
/// stands, the conflicting files with the commits of each side that touched them, and how to
/// resolve it. `generated` and `rebuild` are the project's generated files and the command that
/// rebuilds them, when its run settings name them.
pub fn conflict_fix_task(
    store: &RunStore,
    run_id: &str,
    generated: &[String],
    rebuild: Option<&[String]>,
) -> Result<String, StoreError> {
    let record = store.load(run_id)?;
    let onto = rebase_onto(&record)?;
    let files = crate::rebase::unmerged_files(&record.worktree)?;
    // Each name is a literal pathspec: glob characters in it match only itself.
    let pathspecs: Vec<String> = files
        .iter()
        .map(|file| format!(":(literal){file}"))
        .collect();
    // The run branch's ref still names the head the rebase started from until the rebase ends.
    let branch = format!("refs/heads/{}", record.branch);
    // The latest commits of each side, their subjects cut short, so a long history does not fill the
    // session's context; the rest are a `git log` away.
    let commits = |range: String| -> Result<String, StoreError> {
        let max_count = format!("--max-count={}", MAX_COMMITS + 1);
        let mut args = vec![
            "log",
            "--no-decorate",
            "--format=%h %s",
            &max_count,
            &range,
            "--",
        ];
        args.extend(pathspecs.iter().map(String::as_str));
        let log = git_raw(&record.worktree, &args)?;
        let lines: Vec<&str> = log
            .stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if !log.success || lines.is_empty() {
            return Ok("(none)".to_string());
        }
        let mut text = lines
            .iter()
            .take(MAX_COMMITS)
            .map(|line| match line.char_indices().nth(MAX_SUBJECT) {
                Some((cut, _)) => format!("{}…", &line[..cut]),
                None => line.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        if lines.len() > MAX_COMMITS {
            text += &format!(
                "\n… and more; `git log --oneline {range} -- <the files above>` lists them all."
            );
        }
        Ok(text)
    };
    let theirs = commits(format!("{branch}..{onto}"))?;
    let ours = commits(format!("{onto}..{branch}"))?;
    let target = &record.target_branch;
    let listed = |names: &[String]| {
        names
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let generated_rule = match (generated.is_empty(), rebuild) {
        (false, Some(command)) => format!(
            "This project's generated files are {}; rebuild them with `{}`, then add them.",
            listed(generated),
            shell_words(command)
        ),
        (true, Some(command)) => format!("This project rebuilds its generated files with `{}`; run it, then add them.", shell_words(command)),
        (false, None) => format!("This project's generated files are {}; work out from the repository how they are rebuilt.", listed(generated)),
        (true, None) => "Work out from the repository which of the conflicting files are generated and how they are rebuilt.".to_string(),
    };
    Ok(format!(
        "The rebase of this run's branch onto {target} stopped on a conflict. It replays the branch {branch_name} onto {onto}, \
         where {target} is now.\n\n\
         Conflicting files:\n{files}\n\n\
         Commits on {target} since this run's base that touched them:\n{theirs}\n\n\
         This run's commits that touched them:\n{ours}\n\n\
         Resolve the conflicts in this worktree:\n\
         - Keep both sides' behaviour. When both sides added the same name (an event kind, a function, a field, a CSS class) \
         with different meanings, keep {target}'s and rename this run's, everywhere this run's commits use it, tests included.\n\
         - Generated files are never merged by hand: rebuild them from their sources. {generated_rule}\n\
         - Finish with `git rebase --continue` until the rebase is done; when it stops on a later commit, resolve that one too. \
         Do not skip or drop commits, do not abort the rebase, and do not change commits that did not conflict.\n\
         - You may compile, but do not run the tests: the gate runs them after you.",
        branch_name = record.branch,
        files = files.iter().map(|file| format!("- {file}")).collect::<Vec<_>>().join("\n"),
    ))
}

/// `words` as a shell command line: each word that is not plain is single-quoted, so it stays one
/// argument and nothing in it is read as shell syntax.
fn shell_words(words: &[String]) -> String {
    let plain = |word: &str| {
        !word.is_empty()
            && word
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b))
    };
    words
        .iter()
        .map(|word| {
            if plain(word) {
                word.clone()
            } else {
                format!("'{}'", word.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The rebase really finished: the run branch is checked out, no entry is
/// unmerged, and the commit it was replayed onto is part of its history.
fn completed_onto(record: &RunRecord, onto: &str) -> Result<bool, StoreError> {
    let head = git_raw(&record.worktree, &["symbolic-ref", "-q", "HEAD"])?;
    if head.stdout.trim() != format!("refs/heads/{}", record.branch) {
        return Ok(false);
    }
    let unmerged = git_raw(
        &record.worktree,
        &["diff", "--name-only", "--diff-filter=U"],
    )?;
    if !unmerged.success || !unmerged.stdout.trim().is_empty() {
        return Ok(false);
    }
    if !git_raw(
        &record.worktree,
        &["merge-base", "--is-ancestor", onto, "HEAD"],
    )?
    .success
    {
        return Ok(false);
    }
    // The run only reaches a conflict with work of its own: a branch that sits
    // on the target with nothing past it has lost that work.
    let ahead = git_raw(
        &record.worktree,
        &["rev-list", "--count", &format!("{onto}..HEAD")],
    )?;
    Ok(ahead.success
        && ahead
            .stdout
            .trim()
            .parse::<u32>()
            .is_ok_and(|count| count > 0))
}

fn rebase_onto(record: &RunRecord) -> Result<String, StoreError> {
    for name in ["rebase-merge/onto", "rebase-apply/onto"] {
        let path = git_raw(&record.worktree, &["rev-parse", "--git-path", name])?;
        if !path.success {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(record.worktree.join(path.stdout.trim())) {
            let sha = text.trim();
            if !sha.is_empty() {
                return Ok(sha.to_string());
            }
        }
    }
    Err(StoreError::Corrupt(
        "could not tell which commit the rebase is replaying onto".to_string(),
    ))
}

fn rebase_in_progress(record: &RunRecord) -> Result<bool, StoreError> {
    for name in ["rebase-merge", "rebase-apply"] {
        let path = git_raw(&record.worktree, &["rev-parse", "--git-path", name])?;
        if !path.success {
            return Err(StoreError::Corrupt(format!(
                "git rev-parse failed: {}",
                path.stderr.trim()
            )));
        }
        // `--git-path` is relative to the directory git was started in.
        if record.worktree.join(path.stdout.trim()).exists() {
            return Ok(true);
        }
    }
    Ok(false)
}
