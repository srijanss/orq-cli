//! Accepting a failed run, or one whose review escalated: the work it left committed is taken as the run's result when the
//! tests pass in its worktree, so the run ends review ready like any other.

use std::path::Path;

use crate::gate::{choose_gate, Gate};
use crate::outcome::git_raw;
use crate::run_status::{summarize_run, RunPhase};
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

/// Say whether the run `run_id` could be accepted, without running its tests or changing it: it must be failed or
/// waiting on a review decision, with its own worktree holding committed work and nothing left uncommitted.
pub fn accept_check(store: &RunStore, run_id: &str) -> Result<(), String> {
    check(store, run_id).map(|_| ())
}

fn check(store: &RunStore, run_id: &str) -> Result<crate::store::RunRecord, String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    // A later run works in the same worktree; its tests and its state are that run's.
    crate::discard::refuse_taken_over(store, run_id)?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let phase = summarize_run(&events).phase;
    if phase == RunPhase::Accepting {
        return Err(format!("{run_id}: an accept is running; wait for its tests to finish"));
    }
    if !matches!(phase, RunPhase::Failed | RunPhase::NeedsReview) {
        return Err(format!(
            "{run_id} is {}; only a failed run can be accepted, or one waiting on a review decision",
            phase.as_str()
        ));
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
    Ok(record)
}

/// [`accept_run_by`] from the command line.
pub fn accept_run(store: &RunStore, run_id: &str, tests: &dyn Fn(&Path) -> Result<(), String>) -> Result<String, String> {
    accept_run_by(store, run_id, "cli", tests)
}

/// Mark the failed or needs-review run `run_id` review ready when [`accept_check`] passes and `tests(worktree)`
/// passes. The tests run only once the run is known to be acceptable. Once they start, `run.accept_started`
/// (with `by` and the head being tested) is recorded; a failure after that is recorded as `run.accept_failed`,
/// which gives the run back the phase it had, and is returned as the reason.
pub fn accept_run_by(store: &RunStore, run_id: &str, by: &str, tests: &dyn Fn(&Path) -> Result<(), String>) -> Result<String, String> {
    let record = check(store, run_id)?;
    let head_before = git(&record.worktree, &["rev-parse", "HEAD"])?;
    append(store, &record, "run.accept_started", serde_json::json!({ "by": by, "head": head_before.trim() }))?;
    let outcome = test_and_confirm(run_id, &record, &head_before, tests);
    if let Err(why) = &outcome {
        record_accept_failed(store, &record, why)?;
    }
    outcome?;
    let mut completed = serde_json::json!({ "ending": "review_ready", "note": "accepted: tests passed" });
    completed["by"] = serde_json::Value::String(by.to_string());
    append(store, &record, "run.completed", completed)?;
    Ok(format!("Accepted {run_id}: its tests pass and it is review ready"))
}

fn test_and_confirm(run_id: &str, record: &crate::store::RunRecord, head_before: &str, tests: &dyn Fn(&Path) -> Result<(), String>) -> Result<(), String> {
    tests(&record.worktree).map_err(|why| format!("the tests failed in {run_id}: {why}"))?;
    verify_own_worktree(run_id, &record.worktree, &record.branch)?;
    let head_after = git(&record.worktree, &["rev-parse", "HEAD"])?;
    let status_after = git(&record.worktree, &["status", "--porcelain", "--untracked-files=all"])?;
    if head_after != head_before || !status_after.trim().is_empty() {
        return Err(format!("the tests changed the worktree of {run_id}; what they left is not the work that was tested"));
    }
    Ok(())
}

/// What an accept the server stopped during is recorded as.
pub const ACCEPT_INTERRUPTED: &str = "the server stopped while the tests were running; accept again";

/// When `run_id` is still accepting (started, no result) and nothing is running its tests anymore, record the
/// accept as failed because the server stopped. Says whether it recorded anything.
pub fn fail_interrupted_accept(store: &RunStore, run_id: &str) -> Result<bool, String> {
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    if summarize_run(&events).phase != RunPhase::Accepting {
        return Ok(false);
    }
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    append(store, &record, "run.accept_failed", serde_json::json!({ "reason": ACCEPT_INTERRUPTED, "output": "" }))?;
    Ok(true)
}

/// Record why the accept failed: the first line of `why` as the reason, and the end of all of it as the output.
/// All of it is redacted before either is cut, so a cut never leaves part of a secret too short to recognise.
fn record_accept_failed(store: &RunStore, record: &crate::store::RunRecord, why: &str) -> Result<(), String> {
    let why = orq_core::redact::redact(why);
    let first = why.lines().next().unwrap_or_default();
    let reason: String = first.chars().take(300).collect();
    let payload = serde_json::json!({ "reason": reason, "output": tail(&why) });
    append(store, record, "run.accept_failed", payload)
}

fn append(store: &RunStore, record: &crate::store::RunRecord, kind: &str, payload: serde_json::Value) -> Result<(), String> {
    store
        .append_event(&crate::store::Event {
            schema_version: 1,
            event_id: crate::implement::next_event_id(&record.run_id),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: kind.to_string(),
            stage: None,
            attempt: 1,
            occurred_at: crate::implement::now_rfc3339(),
            payload,
        })
        .map_err(|e| format!("{e:?}"))
}

const OUTPUT_TAIL: usize = 1500;

/// The end of `text`, at most [`OUTPUT_TAIL`] bytes. Redact `text` first: what is cut off cannot be recognised.
/// The line the cut falls in goes too, since it may hold the end of a secret; and when the kept text holds the end
/// of a private key whose start was cut off, everything up to that end goes.
fn tail(text: &str) -> &str {
    let text = text.trim();
    if text.len() <= OUTPUT_TAIL {
        return text;
    }
    let mut from = text.len() - OUTPUT_TAIL;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let mut kept = &text[from..];
    if let Some(newline) = kept.find('\n') {
        kept = &kept[newline + 1..];
    }
    const KEY_END: &str = "PRIVATE KEY-----";
    if let Some(end) = kept.rfind(KEY_END) {
        if !kept[..end].contains("-----BEGIN") {
            kept = kept[end + KEY_END.len()..].trim_start();
        }
    }
    kept
}

/// Run the tests of `worktree`: the `configured` command if given, otherwise what its stacks
/// call for. Nothing to run is a failure, not a pass. A failing step returns the end of what it
/// printed. There is no time limit.
pub fn run_worktree_tests(worktree: &Path, configured: Option<&[String]>) -> Result<(), String> {
    let steps = match choose_gate(worktree, configured).map_err(|e| e.to_string())? {
        Gate::Skipped { reason } => return Err(reason),
        Gate::Steps { steps, .. } => steps,
    };
    run_steps(worktree, &steps)
}

/// Run each of `steps` in `worktree`, in order; the first to fail ends the run and the rest do not run.
pub fn run_steps(worktree: &Path, steps: &[Vec<String>]) -> Result<(), String> {
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
        drop(reader); // a child still writing after a failed read gets a broken pipe, not a full one
        let status = child.wait().map_err(cannot_run)?;
        if !status.success() {
            let printed = orq_core::redact::redact(&String::from_utf8_lossy(&printed));
            return Err(format!("`{}` failed: {}", argv.join(" "), tail(&printed)));
        }
    }
    Ok(())
}

/// Read `source` to its end, keeping only the last bytes of it so a noisy command costs bounded memory. What is
/// kept is always more than [`tail`] keeps when anything was dropped, so the cut made here never shows.
fn read_tail(source: &mut impl std::io::Read) -> Vec<u8> {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        kept.extend_from_slice(&chunk[..n]);
        if kept.len() > 4 * OUTPUT_TAIL {
            kept.drain(..kept.len() - 2 * OUTPUT_TAIL);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::read_tail;
    use std::io::{Error, ErrorKind, Read};

    /// Fails with `Interrupted` before every chunk, as a read cut short by a signal does.
    struct Interrupted<'a>(&'a [&'a [u8]], bool);

    impl Read for Interrupted<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1 = !self.1;
            if self.1 {
                return Err(Error::from(ErrorKind::Interrupted));
            }
            let Some((first, rest)) = self.0.split_first() else { return Ok(0) };
            self.0 = rest;
            buf[..first.len()].copy_from_slice(first);
            Ok(first.len())
        }
    }

    #[test]
    fn an_interrupted_read_is_retried_not_taken_for_the_end_of_the_output() {
        let mut source = Interrupted(&[b"first ", b"last"], false);
        assert_eq!(read_tail(&mut source), b"first last");
    }
}
