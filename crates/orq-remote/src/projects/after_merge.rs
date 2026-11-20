//! The commands a project's `.orq/run.toml` `[after_merge] commands` runs once the queue settles a
//! merge-local run's merge, in the project's main checkout, one after the other, stopping at the first
//! failure. A merge gets at most one go: none for a merge the server did not watch land (it started
//! after the merge, or before the project had `[after_merge]`), none for a run that already recorded
//! `after_merge.started` or `after_merge.commands`. The commands run on a thread of their own per
//! project, so a round only starts a go and returns; merges landing during one go fold into one more.
//! Every argv, error and reason the run records is redacted.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex, MutexGuard};

use orq_core::project_settings::{AfterMergeCommands, Delivery};
use orq_core::redact::redact;
use orq_orchestrator::{Event, RunStore};
use serde_json::{json, Value};

/// Written before the first command of a go.
const STARTED: &str = "after_merge.started";
/// Written when a go ended, ran or not, or was found interrupted.
const COMMANDS: &str = "after_merge.commands";

/// The runs, as (state root, run id), a round saw going in a project that declared `[after_merge]`.
static SEEN: LazyLock<Mutex<HashSet<(PathBuf, String)>>> = LazyLock::new(Mutex::default);
/// The state roots whose interrupted goes were looked for.
static SCANNED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);
/// Each project's go in progress, by main checkout.
static GOES: LazyLock<Mutex<HashMap<PathBuf, Go>>> = LazyLock::new(Mutex::default);

#[derive(Default)]
struct Go {
    running: bool,
    pending: Vec<Job>,
}

/// One merge waiting for its commands.
struct Job {
    state: PathBuf,
    run: String,
    branch: String,
    merged: String,
    commands: Vec<Vec<String>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `[after_merge]` of the project at `root`: `None` when its `.orq/run.toml` has none.
fn declared(root: &Path) -> Option<Result<AfterMergeCommands, String>> {
    let text = std::fs::read_to_string(root.join(".orq/run.toml")).ok()?;
    match AfterMergeCommands::parse(&text) {
        Ok(commands) => commands.map(Ok),
        Err(why) => Some(Err(why)),
    }
}

/// A round saw `run` going in the project at `root`: if the project declares `[after_merge]`, its merge
/// may get a go.
pub(super) fn watch(state: &Path, root: &Path, run: &str) {
    let key = (state.to_path_buf(), run.to_string());
    if locked(&SEEN).contains(&key) || declared(root).is_none() {
        return;
    }
    locked(&SEEN).insert(key);
}

/// A round settled `run` in the project at `root`: when its merge landed after it was watched, start its go.
pub(super) fn settled(state: &Path, runs: &RunStore, root: &Path, run: &str) {
    if !locked(&SEEN).remove(&(state.to_path_buf(), run.to_string())) {
        return;
    }
    let (Ok(record), Ok(events)) = (runs.load(run), runs.events(run)) else {
        return;
    };
    if record.delivery != Delivery::MergeLocal
        || events
            .iter()
            .any(|event| event.kind == STARTED || event.kind == COMMANDS)
    {
        return;
    }
    let Some(merged) = events
        .iter()
        .rev()
        .filter(|event| event.kind == "merge.completed")
        .find_map(|event| event.payload["head_sha"].as_str().map(str::to_string))
    else {
        return;
    };
    let commands = match declared(root) {
        None => return,
        Some(Err(why)) => {
            let payload = json!({"ran": false, "ok": false, "merged": merged, "error": redact(&why), "commands": []});
            return append(runs, run, COMMANDS, payload);
        }
        Some(Ok(declared)) if declared.commands.is_empty() => return,
        Some(Ok(declared)) => declared.commands,
    };
    append(
        runs,
        run,
        STARTED,
        json!({"merged": merged, "commands": commands.len()}),
    );
    let job = Job {
        state: state.to_path_buf(),
        run: run.to_string(),
        branch: record.target_branch.clone(),
        merged,
        commands,
    };
    let mut goes = locked(&GOES);
    let go = goes.entry(root.to_path_buf()).or_default();
    go.pending.push(job);
    if !go.running {
        go.running = true;
        let root = root.to_path_buf();
        std::thread::spawn(move || drain(&root));
    }
}

/// Once per state root, closes each go the server stopped in the middle of as interrupted, without
/// running it again.
pub(super) fn close_interrupted(runs: &RunStore, state: &Path) {
    if !locked(&SCANNED).insert(state.to_path_buf()) {
        return;
    }
    let Ok(ids) = runs.run_ids() else { return };
    for run in ids {
        let Ok(events) = runs.events(&run) else {
            continue;
        };
        let Some(started) = events.iter().find(|event| event.kind == STARTED) else {
            continue;
        };
        if events.iter().any(|event| event.kind == COMMANDS) {
            continue;
        }
        let payload = json!({"ran": false, "ok": false, "interrupted": true, "merged": started.payload["merged"], "reason": "interrupted: the server stopped before the commands finished", "commands": []});
        append(runs, &run, COMMANDS, payload);
    }
}

/// Runs the project's waiting merges as one go at a time, until none waits.
fn drain(root: &Path) {
    loop {
        let batch = {
            let mut goes = locked(&GOES);
            let Some(go) = goes.get_mut(root) else { return };
            if go.pending.is_empty() {
                go.running = false;
                return;
            }
            std::mem::take(&mut go.pending)
        };
        go_once(root, &batch);
    }
}

/// One go for `batch`, the merges that landed meanwhile: the checkout is checked against the newest, and
/// each run records the outcome.
fn go_once(root: &Path, batch: &[Job]) {
    let Some(newest) = batch.last() else { return };
    let skipped = checkout_problem(root, &newest.branch, &newest.merged);
    let (results, ok) = match &skipped {
        Some(_) => (
            newest
                .commands
                .iter()
                .map(|argv| json!({"argv": shown(argv), "skipped": true}))
                .collect(),
            false,
        ),
        None => run_commands(root, &newest.commands),
    };
    for job in batch {
        let mut payload =
            json!({"ran": skipped.is_none(), "ok": ok, "merged": job.merged, "commands": results});
        if let Some(why) = &skipped {
            payload["skipped_reason"] = json!(redact(why));
        }
        if batch.len() > 1 {
            payload["folded"] = json!(batch.len());
        }
        append(&RunStore::new(&job.state), &job.run, COMMANDS, payload);
    }
}

/// Why the checkout at `root` is not where the commands belong: on another branch than `branch`, or at
/// neither the merged commit nor one after it.
fn checkout_problem(root: &Path, branch: &str, merged: &str) -> Option<String> {
    match git(root, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        Some(on) if on == branch => {}
        Some(on) => return Some(format!("checkout on {on}")),
        None => return Some("checkout on a detached HEAD".to_string()),
    }
    let Some(head) = git(root, &["rev-parse", "HEAD"]) else {
        return Some("HEAD cannot be read".to_string());
    };
    if head == merged || git(root, &["merge-base", "--is-ancestor", merged, "HEAD"]).is_some() {
        return None;
    }
    Some(format!("HEAD is {head}, not {merged}"))
}

/// What `git args` printed in `root`, trimmed; `None` when it failed.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn shown(argv: &[String]) -> Vec<String> {
    argv.iter().map(|word| redact(word)).collect()
}

/// Runs each argv in `root` in order, stopping at the first that fails or cannot start; what each did
/// and whether all succeeded.
fn run_commands(root: &Path, commands: &[Vec<String>]) -> (Vec<Value>, bool) {
    let mut results = Vec::new();
    let mut failed = false;
    for argv in commands {
        if failed {
            results.push(json!({"argv": shown(argv), "skipped": true}));
            continue;
        }
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(status) => {
                failed = !status.success();
                results.push(json!({"argv": shown(argv), "exit_code": status.code()}));
            }
            Err(error) => {
                failed = true;
                results.push(json!({"argv": shown(argv), "error": redact(&error.to_string())}));
            }
        }
    }
    (results, !failed)
}

/// Records `kind` on `run`; a run that cannot be written to is logged and goes on.
fn append(runs: &RunStore, run: &str, kind: &str, payload: Value) {
    let Ok(record) = runs.load(run) else { return };
    let event = Event {
        schema_version: 1,
        event_id: format!("{run}-{}", kind.replace('.', "-")),
        run_id: run.to_string(),
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: kind.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: super::utc_now(),
        payload,
    };
    if let Err(error) = runs.append_event(&event) {
        eprintln!("orq: cannot record {kind} on {run}: {error}");
    }
}
