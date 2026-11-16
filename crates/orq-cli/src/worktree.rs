//! `orq worktree list|remove`: the worktrees orq made that runs left behind, and deleting one with its branch.

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use orq_core::retention::human_bytes;
use orq_orchestrator::{delete_kept_worktree, list_kept_worktrees, ChangeKind, KeptWorktree, RunStore};

use crate::{fail, pipeline, printable, usage_error};

struct Args {
    repo: PathBuf,
    branch: Option<String>,
    yes: bool,
    force: bool,
}

fn parse(args: &[OsString]) -> Option<Args> {
    let mut repo = None;
    let mut branch = None;
    let (mut yes, mut force) = (false, false);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str()? {
            "--repo" => repo = Some(PathBuf::from(iter.next()?)),
            "--yes" => yes = true,
            "--force" => force = true,
            flag if flag.starts_with('-') => return None,
            word if branch.is_none() => branch = Some(word.to_string()),
            _ => return None,
        }
    }
    Some(Args { repo: repo?, branch, yes, force })
}

pub(crate) fn worktree(args: &[OsString]) -> ExitCode {
    let command = args.first().and_then(|a| a.to_str());
    let parsed = parse(args.get(1..).unwrap_or_default());
    let result = match (command, parsed) {
        (Some("list"), Some(a)) if a.branch.is_none() && !a.yes && !a.force => list(&a.repo),
        (Some("remove"), Some(Args { repo, branch: Some(branch), yes, force })) => remove(&repo, &branch, yes, force),
        _ => return usage_error(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => fail(message),
    }
}

fn run_store() -> Result<RunStore, String> {
    let state = pipeline::state_root().ok_or_else(|| "no state location: set XDG_STATE_HOME or HOME".to_string())?;
    Ok(RunStore::new(&state))
}

fn kept(store: &RunStore, repo: &Path) -> Result<Vec<KeptWorktree>, String> {
    list_kept_worktrees(store, repo).map_err(|err| format!("cannot list the worktrees of {}: {err}", repo.display()))
}

fn plural(n: u64, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

fn commits_text(entry: &KeptWorktree) -> String {
    match entry.commits {
        Some(0) => "merged".to_string(),
        Some(n) => format!("{} not on {}", plural(n, "commit"), entry.target),
        None => format!("commits on {} unknown", entry.target),
    }
}

fn file_text(path: &str, kind: ChangeKind) -> String {
    match kind {
        ChangeKind::Tracked => format!("{path} (changed)"),
        ChangeKind::Untracked => format!("{path} (untracked)"),
        ChangeKind::Ignored => format!("{path} (ignored)"),
    }
}

fn list(repo: &Path) -> Result<(), String> {
    let entries = kept(&run_store()?, repo)?;
    if entries.is_empty() {
        println!("No kept worktrees in {}.", repo.display());
        return Ok(());
    }
    let bytes: u64 = entries.iter().filter_map(|e| e.size).sum();
    println!("Kept worktrees: {}, {}", entries.len(), human_bytes(bytes));
    for entry in &entries {
        let run = entry.run_id.as_deref().map_or("no run recorded".to_string(), |id| format!("run {id}"));
        let ending = entry.ending.as_deref().unwrap_or("unknown");
        let size = entry.size.map_or("size unknown".to_string(), human_bytes);
        let verdict = if entry.safe() { "safe to delete" } else { "needs --force" };
        println!(
            "{}  {run}, ended {ending}, {}, {size}, {verdict}",
            printable(&entry.branch),
            commits_text(entry)
        );
        println!("  {}", printable(&entry.path.display().to_string()));
        if let Some(reason) = &entry.kept_reason {
            println!("  kept because {}", printable(reason));
        }
        if entry.files_unreadable {
            println!("  files: could not be read");
        }
        for file in &entry.files {
            println!("  {}", file_text(&file.path, file.kind));
        }
        if entry.more_files > 0 {
            println!("  and {} more", entry.more_files);
        }
    }
    Ok(())
}

/// What deleting `entry` loses, as a question.
fn question(entry: &KeptWorktree) -> String {
    if entry.safe() {
        let ignored: Vec<&str> = entry.files.iter().map(|f| f.path.as_str()).collect();
        return if ignored.is_empty() {
            "Delete this worktree and its branch?".to_string()
        } else {
            format!(
                "Delete this worktree and its branch? Its ignored files ({}) are deleted with it.",
                printable(&ignored.join(", "))
            )
        };
    }
    let mut lost = Vec::new();
    match entry.commits {
        Some(0) => {}
        Some(n) => lost.push(format!("{} not on {}", plural(n, "commit"), entry.target)),
        None => lost.push(format!("commits that may not be on {}", entry.target)),
    }
    if entry.files_unreadable {
        lost.push("files that could not be read".to_string());
    } else if !entry.files.is_empty() {
        let names: Vec<&str> = entry.files.iter().map(|f| f.path.as_str()).collect();
        let more = if entry.more_files > 0 { format!(" and {} more", entry.more_files) } else { String::new() };
        lost.push(format!("its changed files ({}{more})", printable(&names.join(", "))));
    }
    format!("Delete this worktree and its branch with {}? They are lost for good.", lost.join(" and "))
}

fn answer() -> String {
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

fn remove(repo: &Path, branch: &str, yes: bool, force: bool) -> Result<(), String> {
    let store = run_store()?;
    let entries = kept(&store, repo)?;
    let Some(entry) = entries.iter().find(|e| e.branch == branch) else {
        return Err(format!("no kept worktree is on {}; nothing was deleted", printable(branch)));
    };
    let safe = entry.safe();
    if !safe && !force {
        return Err(format!(
            "{} has commits or files that would be lost ({}); pass --force to delete them",
            printable(branch),
            commits_text(entry)
        ));
    }
    if !yes {
        println!("{}", printable(&entry.path.display().to_string()));
        if safe {
            print!("{} [y/N] ", question(entry));
            let _ = std::io::stdout().flush();
            if !matches!(answer().as_str(), "y" | "Y" | "yes") {
                return Err("nothing was deleted".to_string());
            }
        } else {
            print!("{} Type the branch name to confirm: ", question(entry));
            let _ = std::io::stdout().flush();
            if answer() != branch {
                return Err("the branch name did not match; nothing was deleted".to_string());
            }
        }
    }
    match delete_kept_worktree(&store, repo, branch, &entry.version, !safe)
        .map_err(|err| format!("cannot delete {}: {err}", printable(branch)))?
    {
        Ok(deleted) => {
            println!("Deleted {} (was at {}).", printable(&deleted.branch), deleted.sha);
            Ok(())
        }
        Err(refusal) => Err(refusal.to_string()),
    }
}

/// The repositories whose runs made worktrees: each recorded worktree sits in `<repo>/.claude/worktrees`.
fn recorded_repos(store: &RunStore) -> Vec<PathBuf> {
    let mut repos: Vec<PathBuf> = Vec::new();
    for id in store.run_ids().unwrap_or_default() {
        let Ok(record) = store.load(&id) else { continue };
        let repo = record.worktree.ancestors().find_map(|dir| {
            let parent = dir.parent()?;
            (dir.file_name()? == "worktrees" && parent.file_name()? == ".claude").then(|| parent.parent()).flatten()
        });
        if let Some(repo) = repo.filter(|r| r.is_dir()) {
            if !repos.iter().any(|known| known == repo) {
                repos.push(repo.to_path_buf());
            }
        }
    }
    repos
}

/// The `orq storage status` line for the kept worktrees of every repository runs worked in.
pub(crate) fn status_line(store: &RunStore) -> String {
    let mut found = Vec::new();
    for repo in recorded_repos(store) {
        if let Ok(entries) = list_kept_worktrees(store, &repo) {
            if !entries.is_empty() {
                found.push((repo, entries));
            }
        }
    }
    if found.is_empty() {
        return "kept worktrees: none".to_string();
    }
    let count: usize = found.iter().map(|(_, e)| e.len()).sum();
    let bytes: u64 = found.iter().flat_map(|(_, e)| e.iter().filter_map(|w| w.size)).sum();
    let hints: Vec<String> =
        found.iter().map(|(repo, _)| format!("orq worktree list --repo {}", printable(&repo.display().to_string()))).collect();
    format!("kept worktrees: {count} in {} project(s), {} ({})", found.len(), human_bytes(bytes), hints.join("; "))
}
