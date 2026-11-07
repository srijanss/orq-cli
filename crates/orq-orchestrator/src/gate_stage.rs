//! The verify stage: run the chosen gate steps in the run's worktree, one
//! after the other, stopping at the first one that fails. The push only
//! happens after a pass.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::gate::Gate;
use crate::implement::{next_event_id, now_rfc3339, open_log};
use crate::run_activity::test_total_line;
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "verify";
/// What a tool needs to be found; the rest of the caller's environment
/// (tokens, CI variables) is not passed on.
const REQUIRED_ENV: [&str; 3] = ["PATH", "HOME", "TERM"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    Passed,
    /// `step` is the zero-based index; `exit_code` is `None` when the step
    /// could not be started or was killed by a signal.
    Failed {
        step: usize,
        exit_code: Option<i32>,
    },
    Skipped {
        reason: String,
    },
}

pub fn run_gate_stage(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    log_path: &Path,
) -> Result<GateOutcome, StoreError> {
    run_gate_stage_with_env(store, run_id, gate, enabled, log_path, &[])
}

/// `run_gate_stage`, with `env` added to what each step sees (the worktree's own database names).
pub fn run_gate_stage_with_env(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    log_path: &Path,
    env: &[(String, String)],
) -> Result<GateOutcome, StoreError> {
    run_gate_attempt(store, run_id, gate, enabled, log_path, 1, env)
}

/// `run_gate_stage_with_env`, with each step stopped, and the gate failed, once it has run for `limit`.
pub fn run_gate_stage_with_limit(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    log_path: &Path,
    env: &[(String, String)],
    limit: std::time::Duration,
) -> Result<GateOutcome, StoreError> {
    run_gate_attempt_limited(store, run_id, gate, enabled, log_path, 1, env, limit)
}

/// No gate step is left running longer than this: a step that hangs fails the gate instead of the run.
pub const STEP_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// One run of the gate, its events numbered `attempt`.
pub(crate) fn run_gate_attempt(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    log_path: &Path,
    attempt: u32,
    env: &[(String, String)],
) -> Result<GateOutcome, StoreError> {
    run_gate_attempt_limited(store, run_id, gate, enabled, log_path, attempt, env, STEP_LIMIT)
}

#[allow(clippy::too_many_arguments)]
fn run_gate_attempt_limited(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    log_path: &Path,
    attempt: u32,
    env: &[(String, String)],
    limit: std::time::Duration,
) -> Result<GateOutcome, StoreError> {
    let record = store.load(run_id)?;
    let skip_reason = match gate {
        _ if !enabled => Some("the local gate is disabled".to_string()),
        Gate::Skipped { reason } => Some(reason.clone()),
        Gate::Steps { .. } => None,
    };
    if let Some(reason) = skip_reason {
        record_event(
            store,
            &record,
            "stage.skipped",
            attempt,
            serde_json::json!({ "reason": reason }),
        )?;
        return Ok(GateOutcome::Skipped { reason });
    }
    let Gate::Steps { steps, .. } = gate else {
        return Ok(GateOutcome::Passed);
    };

    let mut log = open_log(log_path)?;
    let log_name = log_path.to_string_lossy().into_owned();
    record_event(
        store,
        &record,
        "stage.started",
        attempt,
        serde_json::json!({ "log": log_name, "steps": steps.len() }),
    )?;

    let counts_tests = steps.iter().any(|argv| listing_argv(argv).is_some());
    for (index, argv) in steps.iter().enumerate() {
        writeln!(log, "$ {}", argv.join(" "))?;
        if counts_tests {
            // The log is kept across rounds: an earlier step's total says nothing about this one.
            writeln!(log, "{}", test_total_line(None))?;
        }
        if let Some(total) = listing_argv(argv).and_then(|listing| count_tests(&record, &listing, log_path, &log, env, limit)) {
            writeln!(log, "{}", test_total_line(Some(total)))?;
        }
        let (exit_code, failure) = run_step(&record, argv, &log, &log, env, limit)?;
        if let Some(failure) = failure {
            let reason = format!(
                "gate step {} ({}) {failure}",
                index + 1,
                argv.first().map_or("", String::as_str)
            );
            record_event(
                store,
                &record,
                "stage.failed",
                attempt,
                serde_json::json!({ "exit_code": exit_code, "reason": reason, "log": log_name }),
            )?;
            return Ok(GateOutcome::Failed {
                step: index,
                exit_code,
            });
        }
    }
    record_event(
        store,
        &record,
        "stage.completed",
        attempt,
        serde_json::json!({ "exit_code": 0, "log": log_name }),
    )?;
    Ok(GateOutcome::Passed)
}

/// What lists the tests of a `cargo test` step, with the same selection; `None` for any other step.
fn listing_argv(argv: &[String]) -> Option<Vec<String>> {
    let [program, subcommand, ..] = argv else { return None };
    // Aimed at another manifest, the workspace looked at below would not be the one that runs.
    let unsuited = |arg: &String| arg == "--list" || arg.starts_with("--manifest-path");
    if program != "cargo" || subcommand != "test" || argv.iter().any(unsuited) {
        return None;
    }
    let mut listing = argv.to_vec();
    if !argv.iter().any(|arg| arg == "--") {
        listing.push("--".to_string());
    }
    listing.push("--list".to_string());
    Some(listing)
}

/// How many tests `listing` says there are. Anything going wrong only costs the percentage: the
/// step itself decides the gate. What the build says goes to the log, so a long compile shows life.
fn count_tests(
    record: &RunRecord,
    listing: &[String],
    log_path: &Path,
    log: &fs::File,
    env: &[(String, String)],
    limit: std::time::Duration,
) -> Option<u64> {
    let metadata = ["cargo", "metadata", "--no-deps", "--format-version", "1"].map(String::from);
    let metadata = captured_stdout(record, &metadata, log_path, log, env, limit)?;
    if !all_targets_can_list(&metadata)? {
        return None;
    }
    let listed = captured_stdout(record, listing, log_path, log, env, limit)?;
    listed_total(&String::from_utf8_lossy(&listed))
}

/// Whether every target of the workspace uses the standard test harness. One with its own need not
/// know `--list`: it would just run, and then run again as the step. Cargo does not report the
/// setting, so the manifests are read.
fn all_targets_can_list(metadata: &[u8]) -> Option<bool> {
    let metadata: serde_json::Value = serde_json::from_slice(metadata).ok()?;
    for package in metadata.get("packages")?.as_array()? {
        let manifest = fs::read_to_string(package.get("manifest_path")?.as_str()?).ok()?;
        let manifest: toml_edit::DocumentMut = manifest.parse().ok()?;
        if turns_off_harness(manifest.as_item()) {
            return Some(false);
        }
    }
    Some(true)
}

/// Whether `harness = false` is set anywhere under `item`, in any form TOML allows for a target.
fn turns_off_harness(item: &toml_edit::Item) -> bool {
    if let Some(table) = item.as_table_like() {
        return table
            .iter()
            .any(|(key, value)| (key == "harness" && value.as_bool() == Some(false)) || turns_off_harness(value));
    }
    match item {
        toml_edit::Item::ArrayOfTables(tables) => tables.iter().any(|table| turns_off_harness(&toml_edit::Item::Table(table.clone()))),
        toml_edit::Item::Value(toml_edit::Value::Array(values)) => values.iter().any(|value| turns_off_harness(&toml_edit::Item::Value(value.clone()))),
        _ => false,
    }
}

/// What `argv` printed to its standard output, if it succeeded.
fn captured_stdout(
    record: &RunRecord,
    argv: &[String],
    log_path: &Path,
    log: &fs::File,
    env: &[(String, String)],
    limit: std::time::Duration,
) -> Option<Vec<u8>> {
    let mut captured = open_listing(log_path).ok()?;
    let (_, failure) = run_step(record, argv, &captured, log, env, limit).ok()?;
    if failure.is_some() {
        return None;
    }
    captured.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = Vec::new();
    captured.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// A private scratch file beside the log, gone from the folder as soon as it is open. Only a file
/// this call created is ever written or removed: anything already at that name is an error.
fn open_listing(log_path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = log_path.with_extension(format!("test-list-{}", std::process::id()));
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    fs::remove_file(&path)?;
    Ok(file)
}

/// The sum of the `N tests, M benchmarks` line each test binary ends its listing with.
fn listed_total(text: &str) -> Option<u64> {
    let mut total: Option<u64> = None;
    for count in text.lines().filter_map(listed_count) {
        total = Some(total.unwrap_or(0).checked_add(count?)?);
    }
    total.filter(|total| *total > 0)
}

/// `Some` when `line` is a binary's summary; inside it, `None` when its count does not fit.
fn listed_count(line: &str) -> Option<Option<u64>> {
    let (tests, benchmarks) = line.split_once(", ")?;
    let tests: u64 = tests.strip_suffix(" tests").or_else(|| tests.strip_suffix(" test"))?.parse().ok()?;
    let benchmarks: u64 = benchmarks.strip_suffix(" benchmarks").or_else(|| benchmarks.strip_suffix(" benchmark"))?.parse().ok()?;
    Some(tests.checked_add(benchmarks))
}

/// Run one step; the second value is why it failed, if it did.
fn run_step(
    record: &RunRecord,
    argv: &[String],
    stdout: &fs::File,
    log: &fs::File,
    env: &[(String, String)],
    limit: std::time::Duration,
) -> Result<(Option<i32>, Option<String>), StoreError> {
    let Some((program, args)) = argv.split_first() else {
        return Ok((None, Some("is empty".to_string())));
    };
    let mut command = Command::new(program);
    command.args(args).current_dir(&record.worktree).env_clear();
    for name in REQUIRED_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(env.iter().map(|(name, value)| (name, value)));
    command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(log.try_clone()?);
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let child = match crate::child_groups::spawn(&mut command) {
        Ok(child) => child,
        Err(err) => return Ok((None, Some(format!("could not be started: {err}")))),
    };
    let deadline = std::time::Instant::now() + limit;
    let exited = loop {
        // Looking does not reap, so the leader's id stays reserved until the group has been signalled.
        match child.has_exited() {
            Ok(true) => break true,
            Ok(false) if std::time::Instant::now() >= deadline => break false,
            Ok(false) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(err) => {
                child.end_group();
                let _ = child.reap();
                return Err(err.into());
            }
        }
    };
    // Whatever the step started goes with it.
    child.end_group();
    let status = child.reap()?;
    if !exited {
        return Ok((None, Some(format!("timed out after {}s and was stopped", limit.as_secs()))));
    }
    Ok(match status {
        status if status.success() => (status.code(), None),
        status => match status.code() {
            Some(code) => (Some(code), Some(format!("exited with status {code}"))),
            None => (None, Some("was stopped by a signal".to_string())),
        },
    })
}

pub(crate) fn record_event(
    store: &RunStore,
    record: &RunRecord,
    kind: &str,
    attempt: u32,
    payload: serde_json::Value,
) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: Some(STAGE.to_string()),
        attempt,
        occurred_at: now_rfc3339(),
        payload,
    })
}
