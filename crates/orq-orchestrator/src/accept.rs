//! Accepting a failed run: the work it left committed is taken as the run's result when the
//! tests pass in its worktree, so the run ends review ready like any other.

use std::path::Path;

use crate::gate::{choose_gate, Gate};
use crate::outcome::git_raw;
use crate::run_status::{record_run_completed, summarize_run, RunPhase};
use crate::store::RunStore;

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_raw(dir, args).map_err(|e| e.to_string())?;
    if !out.success {
        return Err(format!("git {} failed", args.join(" ")));
    }
    Ok(out.stdout)
}

/// The recorded path must be the root of a git worktree with the run's own branch checked out;
/// a subdirectory, a symlink to elsewhere or a switched branch would test work that is not the run's.
fn verify_own_worktree(run_id: &str, worktree: &Path, branch: &str) -> Result<(), String> {
    let top = git(worktree, &["rev-parse", "--show-toplevel"])?;
    let same_root = match (std::fs::canonicalize(top.trim()), std::fs::canonicalize(worktree)) {
        (Ok(top), Ok(recorded)) => top == recorded,
        _ => false,
    };
    if !same_root {
        return Err(format!("{} is not the root of the worktree of {run_id}", worktree.display()));
    }
    let current = git(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if current.trim() != branch {
        return Err(format!("the worktree of {run_id} has {} checked out, not its branch {branch}", current.trim()));
    }
    Ok(())
}

/// Mark the failed run `run_id` review ready when its worktree holds committed work, nothing is
/// left uncommitted and `tests(worktree)` passes. The tests run only once the run is known to be
/// acceptable; a failure leaves the run failed and is returned as the reason.
pub fn accept_run(store: &RunStore, run_id: &str, tests: &dyn Fn(&Path) -> Result<(), String>) -> Result<String, String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let phase = summarize_run(&events).phase;
    if phase != RunPhase::Failed {
        return Err(format!("{run_id} is {}; only a failed run can be accepted", phase.as_str()));
    }
    if !record.worktree.is_dir() {
        return Err(format!("the worktree of {run_id} is gone; there is no work to accept"));
    }
    verify_own_worktree(run_id, &record.worktree, &record.branch)?;
    let status = git(&record.worktree, &["status", "--porcelain", "--untracked-files=all"])?;
    if !status.trim().is_empty() {
        return Err(format!("{run_id} has uncommitted changes in its worktree; commit or discard them first"));
    }
    let range = format!("{}..HEAD", record.base_sha);
    let commits = git(&record.worktree, &["rev-list", "--count", &range])?;
    if commits.trim() == "0" {
        return Err(format!("{run_id} left no committed work to accept"));
    }
    let head_before = git(&record.worktree, &["rev-parse", "HEAD"])?;
    tests(&record.worktree).map_err(|why| format!("the tests failed in {run_id}: {why}"))?;
    let head_after = git(&record.worktree, &["rev-parse", "HEAD"])?;
    let status_after = git(&record.worktree, &["status", "--porcelain", "--untracked-files=all"])?;
    if head_after != head_before || !status_after.trim().is_empty() {
        return Err(format!("the tests changed the worktree of {run_id}; what they left is not the work that was tested"));
    }
    record_run_completed(store, &record, "review_ready", None).map_err(|e| format!("{e:?}"))?;
    Ok(format!("Accepted {run_id}: its tests pass and it is review ready"))
}

const OUTPUT_TAIL: usize = 1500;

fn tail(text: &str) -> &str {
    let text = text.trim();
    if text.len() <= OUTPUT_TAIL {
        return text;
    }
    let mut from = text.len() - OUTPUT_TAIL;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    &text[from..]
}

/// Run the tests of `worktree`: the `configured` command if given, otherwise what its stacks
/// call for. Nothing to run is a failure, not a pass. A failing step returns the end of what it
/// printed. There is no time limit.
pub fn run_worktree_tests(worktree: &Path, configured: Option<&[String]>) -> Result<(), String> {
    let steps = match choose_gate(worktree, configured).map_err(|e| e.to_string())? {
        Gate::Skipped { reason } => return Err(reason),
        Gate::Steps { steps, .. } => steps,
    };
    for argv in steps {
        let Some((program, args)) = argv.split_first() else { continue };
        let cannot_run = |e: std::io::Error| format!("could not run `{}`: {e}", argv.join(" "));
        let (mut reader, writer) = std::io::pipe().map_err(cannot_run)?;
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .current_dir(worktree)
            .stdin(std::process::Stdio::null())
            .stdout(writer.try_clone().map_err(cannot_run)?)
            .stderr(writer);
        let mut child = command.spawn().map_err(cannot_run)?;
        drop(command);
        let printed = read_tail(&mut reader);
        let status = child.wait().map_err(cannot_run)?;
        if !status.success() {
            return Err(format!("`{}` failed: {}", argv.join(" "), tail(&String::from_utf8_lossy(&printed))));
        }
    }
    Ok(())
}

/// Read `source` to its end, keeping only the last bytes of it so a noisy command costs bounded memory.
fn read_tail(source: &mut impl std::io::Read) -> Vec<u8> {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    while let Ok(n) = source.read(&mut chunk) {
        if n == 0 {
            break;
        }
        kept.extend_from_slice(&chunk[..n]);
        if kept.len() > 2 * OUTPUT_TAIL {
            kept.drain(..kept.len() - OUTPUT_TAIL);
        }
    }
    kept
}
