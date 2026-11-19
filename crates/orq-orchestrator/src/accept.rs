//! Accepting a failed run, or one whose review escalated: the work it left committed is taken as the run's result when the
//! tests pass in its worktree, so the run ends review ready like any other.

use std::io::Write;
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
    accept_run_delivering(store, run_id, by, tests, &|_, _, _| Ok(Vec::new()))
}

/// Sets each of the features (second argument) of a run of the project (first argument) completed in the
/// project's ledger, recorded at the time given (third argument, RFC 3339), and says which it set.
pub type CompleteFeatures<'a> = dyn Fn(&str, &[String], &str) ->Result<Vec<String>, String> + 'a;

/// What an accept whose tests passed went on to do.
enum Delivered {
    /// Nothing more: the run is review ready.
    ReviewReady,
    /// The run is merge-local and its branch landed on the target at `head_sha`.
    Merged { head_sha: String, note: Option<String> },
    /// The run is merge-local and its target could not take it; it waits on a person.
    Blocked { reason: String },
}

/// [`accept_run_by`], and a merge-local run whose tests pass is carried on to the merge as the pipeline
/// carries a run whose gate passes ([`crate::land_on_target`]): a target that moved is caught up with by
/// a rebase, and the tests run again on the rebased head; the target is fast-forwarded; the worktree and
/// branch are removed ([`crate::remove_merged_run`]); and `complete` sets the run's features completed.
/// A rebase conflict is undone and fails the accept, naming the files; a target that cannot take the run
/// (a dirty checkout) ends it blocked.
pub fn accept_run_delivering(
    store: &RunStore,
    run_id: &str,
    by: &str,
    tests: &dyn Fn(&Path) -> Result<(), String>,
    complete: &CompleteFeatures<'_>,
) -> Result<String, String> {
    accept_or_merge(store, run_id, by, tests, complete, false)
}

/// Whether `record`'s run is merge-local and was accepted, from `events`, before an accept merged such a run:
/// it ended review ready with its worktree kept and nothing merged, so [`merge_accepted_run`] can land it.
pub fn merge_after_accept_pending(record: &crate::store::RunRecord, events: &[crate::store::Event]) -> bool {
    if record.delivery != orq_core::project_settings::Delivery::MergeLocal || summarize_run(events).phase != RunPhase::Completed {
        return false;
    }
    let Some(ended) = events.iter().rposition(|event| event.kind == "run.completed") else { return false };
    let payload = &events[ended].payload;
    let accepted = payload["ending"] == "review_ready" && payload["note"] == "accepted: tests passed";
    accepted && !events[ended..].iter().any(|event| matches!(event.kind.as_str(), "merge.completed" | "worktree.removed"))
}

/// Carry the merge-local run `run_id`, accepted before an accept merged such a run, on to the merge as
/// [`accept_run_delivering`] does once the tests pass: the tests run again only when catching up with the
/// target changed the head. A run whose branch the target already holds is refused.
pub fn merge_accepted_run(
    store: &RunStore,
    run_id: &str,
    by: &str,
    tests: &dyn Fn(&Path) -> Result<(), String>,
    complete: &CompleteFeatures<'_>,
) -> Result<String, String> {
    accept_or_merge(store, run_id, by, tests, complete, true)
}

/// Whether [`merge_accepted_run`] may merge `run_id`, checked as it checks before landing anything: the reason if not.
pub fn merge_accepted_check(store: &RunStore, run_id: &str) -> Result<(), String> {
    check_accepted(store, run_id).map(drop)
}

/// [`merge_accepted_run`]'s check: the run was accepted, and its own clean worktree holds work its target lacks.
fn check_accepted(store: &RunStore, run_id: &str) -> Result<crate::store::RunRecord, String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    crate::discard::refuse_taken_over(store, run_id)?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    if !merge_after_accept_pending(&record, &events) {
        return Err(format!("{run_id} is not a merge-local run accepted and waiting to be merged"));
    }
    if !record.worktree.is_dir() {
        return Err(format!("the worktree of {run_id} is gone; there is no work to merge"));
    }
    verify_own_worktree(run_id, &record.worktree, &record.branch)?;
    let status = git(&record.worktree, &["status", "--porcelain", "--untracked-files=all"])?;
    if !status.trim().is_empty() {
        return Err(format!("{run_id} has uncommitted changes in its worktree; commit or discard them first"));
    }
    let target = format!("refs/heads/{}", record.target_branch);
    if git_raw(&record.worktree, &["merge-base", "--is-ancestor", "HEAD", &target]).map_err(|e| e.to_string())?.success {
        return Err(format!("{} already holds the work of {run_id}", record.target_branch));
    }
    Ok(record)
}

/// The head the accept that ended run `run_id` tested: the one its `run.accept_started` recorded, if any.
fn accepted_head(store: &RunStore, run_id: &str) -> Option<String> {
    let events = store.events(run_id).ok()?;
    let ended = events.iter().rposition(|event| event.kind == "run.completed")?;
    let started = events[..ended].iter().rev().find(|event| event.kind == "run.accept_started")?;
    started.payload.get("head").and_then(|head| head.as_str()).map(str::to_string)
}

/// An accept, or with `merge_only` the merge of a run accepted before: the tests of an accept run first, and
/// those of a merge when the head is not the one its accept tested.
fn accept_or_merge(
    store: &RunStore,
    run_id: &str,
    by: &str,
    tests: &dyn Fn(&Path) -> Result<(), String>,
    complete: &CompleteFeatures<'_>,
    merge_only: bool,
) -> Result<String, String> {
    store.load(run_id).map_err(|e| e.to_string())?;
    // Held until the result is recorded, so only one accept of a run runs at a time, whoever starts it.
    let Some(_lock) = lock_accept(store, run_id)? else {
        return Err(format!("{run_id}: an accept is running; wait for its tests to finish"));
    };
    // With the lock ours, an accept still shown running has nothing running its tests.
    record_interrupted(store, run_id)?;
    let record = if merge_only { check_accepted(store, run_id)? } else { check(store, run_id)? };
    let head_before = git(&record.worktree, &["rev-parse", "HEAD"])?;
    append(store, &record, "run.accept_started", serde_json::json!({ "by": by, "head": head_before.trim() }))?;
    let log = open_accept_log(store, run_id);
    let outcome = {
        let _teeing = log.as_ref().map(|(_, file)| StepLog::tee_into(file));
        // A run accepted before is merged untested only at the head its accept tested.
        let tested_before = merge_only && accepted_head(store, run_id).as_deref() == Some(head_before.trim());
        let tested = if tested_before { Ok(()) } else { test_and_confirm(run_id, &record, &head_before, tests) };
        tested.and_then(|()| match record.delivery {
            orq_core::project_settings::Delivery::MergeLocal => land(store, run_id, &record, tests),
            _ => Ok(Delivered::ReviewReady),
        })
    };
    let log = log.map(|(path, _)| path);
    if let Err(why) = &outcome {
        record_accept_failed(store, &record, why, log.as_deref())?;
    }
    let (ending, note, said) = match outcome? {
        Delivered::ReviewReady => ("review_ready", "accepted: tests passed".to_string(), format!("Accepted {run_id}: its tests pass and it is review ready")),
        Delivered::Merged { head_sha, note } => {
            let target = &record.target_branch;
            let mut said = format!("Accepted {run_id}: its tests pass and it is merged into {target} at {head_sha}");
            if let Some(note) = note {
                said.push_str(&format!("; {note}"));
            }
            if let Some(kept) = finish_merged(store, &record, complete) {
                said.push_str(&format!("; {kept}"));
            }
            ("merged", format!("accepted: tests passed, merged into {target}"), said)
        }
        Delivered::Blocked { reason } => {
            let mut blocked = serde_json::json!({ "ending": "blocked", "reason": reason, "by": by });
            if let Some(log) = &log {
                blocked["log"] = log.to_string_lossy().into_owned().into();
            }
            append(store, &record, "run.completed", blocked)?;
            return Err(format!("{run_id} passed its tests but is blocked: {reason}"));
        }
    };
    let mut completed = serde_json::json!({ "ending": ending, "note": note });
    completed["by"] = serde_json::Value::String(by.to_string());
    if let Some(log) = &log {
        completed["log"] = log.to_string_lossy().into_owned().into();
    }
    append(store, &record, "run.completed", completed)?;
    Ok(said)
}

/// Land the accepted merge-local run on its target ([`crate::land_on_target`]). A rebase that stops on a
/// conflict is undone, so the worktree is left as it was, and fails the accept; a rebased head is tested again.
fn land(store: &RunStore, run_id: &str, record: &crate::store::RunRecord, tests: &dyn Fn(&Path) -> Result<(), String>) -> Result<Delivered, String> {
    let mut conflict = |reason: String| -> Result<Option<String>, crate::store::StoreError> {
        let files: Vec<String> = crate::rebase::unmerged_files(&record.worktree)?.iter().map(|file| crate::cleanup::printable(file)).collect();
        let aborted = git_raw(&record.worktree, &["rebase", "--abort"])?;
        let undone = if aborted.success { "the rebase was undone" } else { "the rebase could not be undone" };
        Ok(Some(format!("{reason} in {}; {undone} and {} was left as it was", files.join(", "), record.target_branch)))
    };
    let mut check = || -> Result<Option<String>, crate::store::StoreError> {
        let head = git(&record.worktree, &["rev-parse", "HEAD"]).map_err(crate::store::StoreError::Corrupt)?;
        Ok(test_and_confirm(run_id, record, &head, tests).err())
    };
    let landed = crate::merge_local::land_on_target(store, run_id, false, true, &mut conflict, &mut check).map_err(|e| format!("{e:?}"))?;
    match landed {
        crate::merge_local::Landing::Merged { head_sha, note } => Ok(Delivered::Merged { head_sha, note }),
        crate::merge_local::Landing::Blocked { reason } => Ok(Delivered::Blocked { reason }),
        crate::merge_local::Landing::Stopped(why) => Err(why),
    }
}

/// After an accepted run merged: its worktree and branch go, and its features no later run took are set
/// completed. Best effort, as the run is merged whatever happens here: what went wrong is returned to be said.
fn finish_merged(store: &RunStore, record: &crate::store::RunRecord, complete: &CompleteFeatures<'_>) -> Option<String> {
    let mut notes = Vec::new();
    match crate::merge_local::remove_merged_run(store, &record.run_id) {
        Ok(crate::cleanup::CleanupOutcome::Removed) => {}
        Ok(crate::cleanup::CleanupOutcome::Kept { reason }) => notes.push(format!("its worktree was kept: {reason}")),
        Err(err) => notes.push(format!("its worktree could not be removed: {err:?}")),
    }
    let members = store.events(&record.run_id).map(|events| crate::resume::members_of(record, &events)).unwrap_or_else(|_| vec![record.feature_id.clone()]);
    let free = crate::discard::features_no_later_run_took(store, &record.run_id, &members);
    match free.and_then(|free| complete(&record.project_id, &free, &crate::implement::now_rfc3339())) {
        Ok(completed) if completed.is_empty() => {}
        Ok(completed) => {
            if let Err(err) = crate::ledger_events::record_ledger_features(store, record, "ledger.completed_by_run", &completed) {
                notes.push(format!("cannot record its completed features: {err:?}"));
            }
        }
        Err(err) => notes.push(format!("its features could not be set completed: {err}")),
    }
    (!notes.is_empty()).then(|| notes.join("; "))
}

fn test_and_confirm(run_id: &str, record: &crate::store::RunRecord, head_before: &str, tests: &dyn Fn(&Path) -> Result<(), String>) -> Result<(), String> {
    // Tests that panic fail the accept like any other failure, so the run is never left accepting.
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tests(&record.worktree)));
    let ran = ran.unwrap_or_else(|panic| Err(format!("they panicked: {}", panic_message(&*panic))));
    ran.map_err(|why| format!("the tests failed in {run_id}: {why}"))?;
    verify_own_worktree(run_id, &record.worktree, &record.branch)?;
    let head_after = git(&record.worktree, &["rev-parse", "HEAD"])?;
    let status_after = git(&record.worktree, &["status", "--porcelain", "--untracked-files=all"])?;
    if head_after != head_before || !status_after.trim().is_empty() {
        return Err(format!("the tests changed the worktree of {run_id}; what they left is not the work that was tested"));
    }
    Ok(())
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    match (panic.downcast_ref::<&str>(), panic.downcast_ref::<String>()) {
        (Some(text), _) => text.to_string(),
        (_, Some(text)) => text.clone(),
        _ => "no message".to_string(),
    }
}

/// What an accept the server stopped during is recorded as.
pub const ACCEPT_INTERRUPTED: &str = "the server stopped while the tests were running; accept again";

/// When `run_id` is still accepting (started, no result) and nothing is running its tests anymore, record the
/// accept as failed because the server stopped. Says whether it recorded anything.
pub fn fail_interrupted_accept(store: &RunStore, run_id: &str) -> Result<bool, String> {
    if summarize_run(&store.events(run_id).map_err(|e| e.to_string())?).phase != RunPhase::Accepting {
        return Ok(false);
    }
    // A held lock means an accept, in this process or another, is still running the tests: leave it alone.
    let Some(_lock) = lock_accept(store, run_id)? else { return Ok(false) };
    record_interrupted(store, run_id)
}

/// Take the accept lock of `run_id` (`accept.lock` in its run folder) if no one holds it. The lock goes with the
/// returned file, and the system lets go of it when the process holding it ends, however it ends.
fn lock_accept(store: &RunStore, run_id: &str) -> Result<Option<std::fs::File>, String> {
    let path = store.run_dir(run_id).map_err(|e| format!("{e:?}"))?.join("accept.lock");
    let opened = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path);
    let file = opened.map_err(|e| format!("could not open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("could not lock {}: {e}", path.display())),
    }
}

/// Record the accept of `run_id` as interrupted when the run is still shown accepting. Call it holding the lock.
fn record_interrupted(store: &RunStore, run_id: &str) -> Result<bool, String> {
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    if summarize_run(&events).phase != RunPhase::Accepting {
        return Ok(false);
    }
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    append(store, &record, "run.accept_failed", serde_json::json!({ "reason": ACCEPT_INTERRUPTED, "output": "" }))?;
    Ok(true)
}

/// Record why the accept failed: the tests that failed, found in the accept's `log` (or in `why` when the log holds
/// nothing), with a reason naming them, or the first line of `why` when none was named; the end of `why` as the
/// output; and the log. All of it is redacted before anything is cut, so a cut never leaves part of a secret too
/// short to recognise.
fn record_accept_failed(store: &RunStore, record: &crate::store::RunRecord, why: &str, log: Option<&Path>) -> Result<(), String> {
    let why = orq_core::redact::redact(why);
    let logged = log.and_then(read_log).filter(|text| !text.trim().is_empty());
    if let (None, Some(path)) = (&logged, log) {
        // Tests that ran no step still leave their failure in the log.
        let _ = std::fs::write(path, format!("{why}\n"));
    }
    let failures = crate::test_failures::find_test_failures(logged.as_deref().unwrap_or(&why));
    let reason = failures.headline().unwrap_or_else(|| why.lines().next().unwrap_or_default().to_string());
    let reason: String = reason.chars().take(300).collect();
    let mut payload = serde_json::json!({ "reason": reason, "output": tail(&why) });
    if let Some(log) = log {
        payload["log"] = log.to_string_lossy().into_owned().into();
    }
    failures.add_to(&mut payload);
    append(store, record, "run.accept_failed", payload)
}

/// The most of an accept log read back to find the failed tests in.
const LOG_READ: u64 = 64 * 1024 * 1024;

/// Where a log too long to read whole was cut: a separator line, so no block runs on across the cut.
const LOG_CUT: &str = "…\n────────────\n";

fn read_log(path: &Path) -> Option<String> {
    read_log_within(path, LOG_READ)
}

/// The log, or when it is longer than `cap` bytes, the whole lines of its first and last halves of that around a
/// cut: a test failing early and one failing late are both found.
fn read_log_within(path: &Path, cap: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len <= cap {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    let half = cap / 2;
    let mut head = Vec::new();
    (&mut file).take(half).read_to_end(&mut head).ok()?;
    file.seek(SeekFrom::Start(len - half)).ok()?;
    let mut tail = Vec::new();
    file.take(half).read_to_end(&mut tail).ok()?;
    let head = head.iter().rposition(|&byte| byte == b'\n').map_or(&head[..0], |at| &head[..=at]);
    let tail = tail.iter().position(|&byte| byte == b'\n').map_or(&tail[..0], |at| &tail[at + 1..]);
    Some(format!("{}{LOG_CUT}{}", String::from_utf8_lossy(head), String::from_utf8_lossy(tail)))
}

/// A new `accept-<n>.log` in the run's log folder, `n` one past the last, for what this accept's test steps print.
/// `None` when it cannot be made: the accept still runs, without a log.
fn open_accept_log(store: &RunStore, run_id: &str) -> Option<(std::path::PathBuf, std::fs::File)> {
    let dir = store.log_dir(run_id).ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    for n in 1..=10_000 {
        let path = dir.join(format!("accept-{n}.log"));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Some((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

std::thread_local! {
    /// Where the test steps run on this thread also write all they print, redacted: the accept log of the accept
    /// running them, while it does.
    static STEP_LOG: std::cell::RefCell<Option<std::fs::File>> = const { std::cell::RefCell::new(None) };
}

/// While held, [`run_steps_with_env`] on this thread writes what each step prints to a log too.
struct StepLog;

impl StepLog {
    fn tee_into(file: &std::fs::File) -> StepLog {
        STEP_LOG.with(|slot| *slot.borrow_mut() = file.try_clone().ok());
        StepLog
    }

    fn current() -> Option<std::fs::File> {
        STEP_LOG.with(|slot| slot.borrow().as_ref().and_then(|file| file.try_clone().ok()))
    }
}

impl Drop for StepLog {
    fn drop(&mut self) {
        STEP_LOG.with(|slot| *slot.borrow_mut() = None);
    }
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
/// Each step sees what a verify gate step sees: `PATH`, `HOME` and `TERM`, and nothing else of this process.
/// A failing step returns "`<step>` failed: exited with status N" on its first line, then the end of what it
/// printed. Run by an accept, all each step prints also goes, redacted, to the accept's log.
pub fn run_steps(worktree: &Path, steps: &[Vec<String>]) -> Result<(), String> {
    run_steps_with_env(worktree, steps, &[])
}

/// `run_steps`, with `env` added to what each step sees, as the verify gate adds its configured env.
pub fn run_steps_with_env(worktree: &Path, steps: &[Vec<String>], env: &[(String, String)]) -> Result<(), String> {
    for argv in steps {
        let Some((program, args)) = argv.split_first() else { continue };
        let cannot_run = |e: std::io::Error| format!("could not run `{}`: {e}", argv.join(" "));
        let mut log = StepLog::current();
        if let Some(log) = &mut log {
            let _ = writeln!(log, "$ {}", orq_core::redact::redact(&argv.join(" ")));
        }
        let (mut reader, writer) = std::io::pipe().map_err(cannot_run)?;
        let mut command = crate::gate_stage::step_command(program, worktree, env);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(writer.try_clone().map_err(cannot_run)?)
            .stderr(writer);
        let mut child = command.spawn().map_err(cannot_run)?;
        drop(command);
        let printed = read_tail(&mut reader, log.as_ref());
        drop(reader); // a child still writing after a failed read gets a broken pipe, not a full one
        let status = child.wait().map_err(cannot_run)?;
        if !status.success() {
            let printed = orq_core::redact::redact(&printed);
            let ended = match status.code() {
                Some(code) => format!("exited with status {code}"),
                None => "was stopped by a signal".to_string(),
            };
            return Err(format!("`{}` failed: {ended}\n{}", argv.join(" "), tail(&printed)));
        }
    }
    Ok(())
}

/// Read `source` to its end, keeping only the end of it, redacted, so a noisy command costs bounded memory.
/// Each line is redacted whole before anything older is let go, so no cut ever splits a secret. Every line kept
/// is also written to `log`, when given.
fn read_tail(source: &mut impl std::io::Read, log: Option<&std::fs::File>) -> String {
    let mut shown = ShownOutput { log, ..ShownOutput::default() };
    let mut chunk = [0u8; 8192];
    loop {
        let n = match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        shown.push(&chunk[..n]);
    }
    shown.finish()
}

/// The longest line shown. A longer one is left out whole: a part of it could be part of a secret, with what
/// would show it is one (its name, its start) in the part that is gone.
const LONGEST_LINE: usize = 1000;
/// How much of a line that is too long is still looked at, enough to find a private key's first line in it.
const LINE_END_LOOKED_AT: usize = 128;
const LEFT_OUT_LINE: &str = "[a line too long to show was left out]";
const LEFT_OUT_KEY: &str = "[REDACTED private key]";

/// Output read so far, line by line: redacted lines, the last ones kept, and the line still being read.
#[derive(Default)]
struct ShownOutput<'a> {
    /// Where each line, once redacted, is written too.
    log: Option<&'a std::fs::File>,
    kept: String,
    line: Vec<u8>,
    too_long: bool,
    in_private_key: bool,
    /// The line being read has a private key's first line in it, seen before any of it was let go.
    line_opens_key: bool,
    /// The last private key marker in the line being read: `Some(true)` a first line, `Some(false)` a last one.
    line_last_marker: Option<bool>,
}

/// Find private key markers (`-----BEGIN … PRIVATE KEY-----`, `-----END … PRIVATE KEY-----`) in `text`:
/// whether one opens a key, and whether the last one opens (true) or closes (false) a key.
fn key_markers(text: &[u8]) -> (bool, Option<bool>) {
    const KEY: &[u8] = b"PRIVATE KEY-----";
    let (mut opens, mut last) = (false, None);
    for at in (0..text.len().saturating_sub(KEY.len() - 1)).filter(|&at| text[at..].starts_with(KEY)) {
        let before = &text[at.saturating_sub(48)..at];
        let found = |marker: &[u8]| before.windows(marker.len()).rposition(|w| w == marker);
        let marker = match (found(b"-----BEGIN"), found(b"-----END")) {
            (Some(begin), Some(end)) => Some(begin > end),
            (Some(_), None) => Some(true),
            (None, Some(_)) => Some(false),
            (None, None) => None,
        };
        if let Some(begins) = marker {
            opens |= begins;
            last = Some(begins);
        }
    }
    (opens, last)
}

impl ShownOutput<'_> {
    fn push(&mut self, mut bytes: &[u8]) {
        while let Some(at) = bytes.iter().position(|&b| b == b'\n') {
            self.add(&bytes[..at]);
            self.end_line();
            bytes = &bytes[at + 1..];
        }
        self.add(bytes);
    }

    fn add(&mut self, part: &[u8]) {
        self.line.extend_from_slice(part);
        if self.line.len() > LONGEST_LINE {
            // Look for key markers before letting go of anything; one still being read is in what stays.
            self.see_key_markers();
            self.too_long = true;
            self.line.drain(..self.line.len() - LINE_END_LOOKED_AT);
        }
    }

    fn see_key_markers(&mut self) {
        let (opens, last) = key_markers(&self.line);
        self.line_opens_key |= opens;
        self.line_last_marker = last.or(self.line_last_marker);
    }

    /// A private key is left out from its first line to its last; one that never ends leaves out all after it.
    fn end_line(&mut self) {
        self.see_key_markers();
        let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
        let too_long = std::mem::take(&mut self.too_long);
        let opens_key = std::mem::take(&mut self.line_opens_key);
        let was_in_key = self.in_private_key;
        if let Some(begins) = self.line_last_marker.take() {
            self.in_private_key = begins;
        }
        if was_in_key {
            // Part of a key, its last line included.
        } else if opens_key && (too_long || self.in_private_key) {
            self.keep(LEFT_OUT_KEY);
        } else if too_long {
            self.keep(LEFT_OUT_LINE);
        } else {
            self.keep(&orq_core::redact::redact(&line));
        }
    }

    fn keep(&mut self, line: &str) {
        if let Some(mut log) = self.log {
            let _ = writeln!(log, "{line}");
        }
        if !self.kept.is_empty() {
            self.kept.push('\n');
        }
        self.kept.push_str(line);
        // Only whole, redacted lines go, and always more stays than [`tail`] keeps.
        if self.kept.len() > 4 * OUTPUT_TAIL {
            let mut from = self.kept.len() - 2 * OUTPUT_TAIL;
            while !self.kept.is_char_boundary(from) {
                from += 1;
            }
            let from = self.kept[from..].find('\n').map_or(from, |at| from + at + 1);
            self.kept.drain(..from);
        }
    }

    fn finish(mut self) -> String {
        if !self.line.is_empty() || self.too_long {
            self.end_line();
        }
        self.kept
    }
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
        assert_eq!(read_tail(&mut source, None), "first last");
    }

    #[test]
    fn a_private_key_marker_split_across_reads_of_a_long_line_still_opens_the_key() {
        let spaces = vec![b' '; 3000];
        let chunks: [&[u8]; 5] = [b"FAILED\n-----BEGIN OPENSSH PRIV", b"ATE KEY-----", &spaces, &spaces, b"\nMIIkeybody\nMIIkeybody\n"];
        let shown = read_tail(&mut Interrupted(&chunks, false), None);
        assert!(shown.contains("FAILED"), "{shown}");
        assert!(!shown.contains("MIIkeybody"), "{shown}");
    }

    #[test]
    fn a_log_longer_than_the_read_keeps_failures_from_its_start_and_its_end() {
        let mut log = String::from("        FAIL [   0.010s] (1/900) suite early\n");
        for n in 2..900 {
            log.push_str(&format!("        PASS [   0.004s] ({n}/900) suite test_{n}\n"));
        }
        log.push_str("        FAIL [   0.010s] (900/900) suite late\n──── STDERR:             suite late\nthread 'late' panicked at src/a.rs:1:1:\nlate broke\n");
        let path = log_file("long");
        std::fs::write(&path, &log).unwrap();
        let read = super::read_log_within(&path, 4096).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(read.len() < 4200, "{}", read.len());
        let failures = crate::test_failures::find_test_failures(&read);
        let names: Vec<&str> = failures.tests.iter().map(|test| test.name.as_str()).collect();
        assert_eq!(names, ["suite early", "suite late"], "{read}");
        assert!(failures.tests[1].output.contains("late broke"), "{:?}", failures.tests[1]);
    }

    #[test]
    fn a_log_within_the_read_is_read_whole() {
        let path = log_file("short");
        std::fs::write(&path, "        FAIL [   0.010s] (1/1) suite only\n").unwrap();
        let read = super::read_log_within(&path, 4096).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(read, "        FAIL [   0.010s] (1/1) suite only\n");
    }

    fn log_file(tag: &str) -> std::path::PathBuf {
        orq_test_support::process_temp_dir().join(format!("orq-accept-log-{tag}-{}.log", std::process::id()))
    }
}
