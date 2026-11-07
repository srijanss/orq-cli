use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use orq_orchestrator::{run_gate_stage, Gate, GateOutcome, RunRecord, RunStore};

struct World {
    store: RunStore,
    root: PathBuf,
    worktree: PathBuf,
    log: PathBuf,
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-gate-stage-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let log = root.join("gate.log");
    World {
        store,
        root,
        worktree,
        log,
    }
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

fn steps(list: Vec<Vec<String>>) -> Gate {
    Gate::Steps {
        steps: list,
        notes: Vec::new(),
    }
}

fn kinds(w: &World) -> Vec<(String, Option<String>)> {
    w.store
        .events("run-1")
        .unwrap()
        .into_iter()
        .map(|e| (e.kind, e.stage))
        .collect()
}

fn verify(kind: &str) -> (String, Option<String>) {
    (kind.to_string(), Some("verify".to_string()))
}

#[test]
fn a_passing_gate_runs_every_step_in_the_worktree_and_logs_their_output() {
    let w = world("pass");
    // The output words are spelled so they never appear in the argv echoed to the log.
    let second_ran = w.root.join("second-ran");
    let gate = steps(vec![
        sh("echo fir\"\"st; pwd"),
        sh(&format!("echo sec\"\"ond; touch {}", second_ran.display())),
    ]);

    let outcome = run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap();

    assert_eq!(outcome, GateOutcome::Passed);
    assert!(second_ran.exists(), "the second step ran");
    let log = fs::read_to_string(&w.log).unwrap();
    assert!(log.contains("first") && log.contains("second"), "{log}");
    let worktree = fs::canonicalize(&w.worktree).unwrap();
    assert!(
        log.contains(worktree.to_str().unwrap()),
        "steps run in the worktree: {log}"
    );
    assert_eq!(
        kinds(&w),
        [verify("stage.started"), verify("stage.completed")]
    );
    let done = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(done.payload["exit_code"], 0);
    assert_eq!(done.payload["log"], w.log.to_str().unwrap());
}

#[test]
fn a_failing_step_stops_the_gate_and_records_its_exit_code() {
    let w = world("fail");
    let marker = w.root.join("third-ran");
    let gate = steps(vec![
        sh("echo ok"),
        sh("echo bro\"\"ken >&2; exit 3"),
        sh(&format!("touch {}", marker.display())),
    ]);

    let outcome = run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap();

    assert_eq!(
        outcome,
        GateOutcome::Failed {
            step: 1,
            exit_code: Some(3)
        }
    );
    assert!(!marker.exists(), "later steps do not run");
    let log = fs::read_to_string(&w.log).unwrap();
    assert!(log.contains("\nbroken\n"), "stderr is logged: {log}");
    assert!(!log.contains("first"), "{log}");
    assert_eq!(kinds(&w), [verify("stage.started"), verify("stage.failed")]);
    let failed = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(failed.payload["exit_code"], 3);
    assert!(
        failed.payload["reason"]
            .as_str()
            .unwrap()
            .contains("step 2"),
        "{}",
        failed.payload
    );
}

#[test]
fn a_step_that_cannot_start_fails_the_gate_without_an_exit_code() {
    let w = world("nostart");
    let gate = steps(vec![vec!["/nonexistent/orq-gate-tool".to_string()]]);

    let outcome = run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap();

    assert_eq!(
        outcome,
        GateOutcome::Failed {
            step: 0,
            exit_code: None
        }
    );
    assert_eq!(kinds(&w), [verify("stage.started"), verify("stage.failed")]);
}

#[test]
fn a_skipped_gate_records_why_and_runs_nothing() {
    let w = world("skipped");
    let gate = Gate::Skipped {
        reason: "no local gate detected".to_string(),
    };

    let outcome = run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap();

    assert_eq!(
        outcome,
        GateOutcome::Skipped {
            reason: "no local gate detected".to_string()
        }
    );
    assert_eq!(kinds(&w), [verify("stage.skipped")]);
    let event = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(event.payload["reason"], "no local gate detected");
    assert!(!w.log.exists());
}

#[test]
fn a_disabled_gate_is_skipped_and_its_steps_never_run() {
    let w = world("disabled");
    let marker = w.root.join("ran");
    let gate = steps(vec![sh(&format!("touch {}", marker.display()))]);

    let outcome = run_gate_stage(&w.store, "run-1", &gate, false, &w.log).unwrap();

    let GateOutcome::Skipped { reason } = outcome else {
        panic!("expected skipped, got {outcome:?}")
    };
    assert!(reason.contains("disabled"), "{reason}");
    assert!(!marker.exists());
    assert_eq!(kinds(&w), [verify("stage.skipped")]);
}

#[cfg(unix)]
#[test]
fn the_gate_log_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("mode");
    fs::write(&w.log, "old\n").unwrap();
    fs::set_permissions(&w.log, fs::Permissions::from_mode(0o644)).unwrap();

    run_gate_stage(&w.store, "run-1", &steps(vec![sh("echo x")]), true, &w.log).unwrap();

    assert_eq!(
        fs::metadata(&w.log).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn a_log_path_that_is_a_symlink_is_refused_and_its_target_untouched() {
    let w = world("symlink");
    let target = w.root.join("victim");
    fs::write(&target, "precious\n").unwrap();
    std::os::unix::fs::symlink(&target, &w.log).unwrap();
    let marker = w.root.join("ran");

    let result = run_gate_stage(
        &w.store,
        "run-1",
        &steps(vec![sh(&format!("touch {}", marker.display()))]),
        true,
        &w.log,
    );

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "precious\n");
    assert!(!marker.exists(), "no step runs without a safe log");
}

#[test]
fn an_unknown_run_is_an_error_and_nothing_runs() {
    let w = world("unknown");
    let marker = w.root.join("ran");
    let gate = steps(vec![sh(&format!("touch {}", marker.display()))]);

    assert!(run_gate_stage(&w.store, "ghost", &gate, true, &w.log).is_err());
    assert!(!marker.exists());
}

#[test]
fn a_step_that_never_ends_fails_the_gate_at_the_limit_and_leaves_nothing_running() {
    use std::time::{Duration, Instant};
    let w = world("limit");
    let late = w.root.join("late");
    let script = format!("(sleep 3; touch '{}') & sleep 60", late.display());
    let gate = steps(vec![sh(&script)]);
    let started = Instant::now();

    let outcome = orq_orchestrator::run_gate_stage_with_limit(&w.store, "run-1", &gate, true, &w.log, &[], Duration::from_secs(1)).unwrap();

    assert!(started.elapsed() < Duration::from_secs(20), "the gate waited for the step");
    assert!(matches!(outcome, GateOutcome::Failed { step: 0, exit_code: None }), "{outcome:?}");
    let failed = w.store.events("run-1").unwrap().into_iter().find(|e| e.kind == "stage.failed").expect("a failure was recorded");
    assert!(failed.payload["reason"].as_str().unwrap().contains("timed out"), "{:?}", failed.payload);
    std::thread::sleep(Duration::from_secs(4));
    assert!(!late.exists(), "a process the step started kept running");
}

#[test]
fn a_step_that_exits_early_takes_what_it_left_running_with_it_and_keeps_its_exit_code() {
    use std::time::Duration;
    let w = world("leftover");
    let late = w.root.join("late");
    let gate = steps(vec![sh(&format!("(sleep 2; touch '{}') & exit 3", late.display()))]);

    let outcome = run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap();

    assert_eq!(outcome, GateOutcome::Failed { step: 0, exit_code: Some(3) });
    std::thread::sleep(Duration::from_secs(3));
    assert!(!late.exists(), "a process the step left behind kept running");
}

/// A `cargo` on the PATH given to the gate that notes its arguments and then runs `script`.
fn fake_cargo(w: &World, script: &str) -> Vec<(String, String)> {
    use std::os::unix::fs::PermissionsExt;
    let bin = w.root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = w.root.join("cargo-calls");
    let cargo = bin.join("cargo");
    fs::write(&cargo, format!("#!/bin/sh\necho \"$*\" >> '{}'\n{script}\n", calls.display())).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    vec![("PATH".to_string(), path)]
}

fn cargo_step(args: &[&str]) -> Vec<String> {
    std::iter::once("cargo").chain(args.iter().copied()).map(str::to_string).collect()
}

fn cargo_calls(w: &World) -> Vec<String> {
    fs::read_to_string(w.root.join("cargo-calls")).unwrap_or_default().lines().map(str::to_string).collect()
}

fn run_with(w: &World, gate: &Gate, env: &[(String, String)]) -> GateOutcome {
    orq_orchestrator::run_gate_stage_with_env(&w.store, "run-1", gate, true, &w.log, env).unwrap()
}

const LISTING: &str = r#"case "$*" in
*--list*) printf 'a::one: test\n1 test, 0 benchmarks\nb::two: test\nb::three: test\nb::bench: benchmark\n2 tests, 1 benchmark\n' ;;
*) echo the-tests-ran ;;
esac"#;

#[test]
fn a_cargo_test_step_is_counted_across_its_binaries_before_it_runs() {
    let w = world("count");
    let env = fake_cargo(&w, LISTING);
    let outcome = run_with(&w, &steps(vec![cargo_step(&["test", "--workspace"])]), &env);
    assert_eq!(outcome, GateOutcome::Passed);
    assert_eq!(cargo_calls(&w), ["test --workspace -- --list", "test --workspace"]);
    let log = fs::read_to_string(&w.log).unwrap();
    let total = orq_orchestrator::test_total_line(Some(4));
    let at = log.find(&total).unwrap_or_else(|| panic!("{log}"));
    assert!(at < log.find("the-tests-ran").unwrap(), "{log}");
    assert!(!log.contains("a::one"), "the listing is not test output: {log}");
    let left: Vec<_> = fs::read_dir(&w.root).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(left.iter().all(|name| !name.to_string_lossy().contains("list")), "{left:?}");
}

#[test]
fn the_count_goes_to_the_test_binaries_alongside_their_own_arguments() {
    let w = world("count-args");
    let env = fake_cargo(&w, LISTING);
    run_with(&w, &steps(vec![cargo_step(&["test", "-p", "x", "--", "--nocapture"])]), &env);
    assert_eq!(cargo_calls(&w), ["test -p x -- --nocapture --list", "test -p x -- --nocapture"]);
}

#[test]
fn a_count_that_fails_leaves_the_total_unknown_and_the_tests_still_decide() {
    for (tag, script) in [
        ("count-fails", "case \"$*\" in *--list*) echo partial; echo '3 tests, 0 benchmarks'; exit 101 ;; esac"),
        ("count-empty", "case \"$*\" in *--list*) echo nothing-countable ;; esac"),
        ("count-garbled", "case \"$*\" in *--list*) echo 'many tests, 0 benchmarks' ;; esac"),
        ("count-overflow", "case \"$*\" in *--list*) echo '18446744073709551615 tests, 1 benchmark' ;; esac"),
    ] {
        let w = world(tag);
        let env = fake_cargo(&w, script);
        assert_eq!(run_with(&w, &steps(vec![cargo_step(&["test"])]), &env), GateOutcome::Passed, "{tag}");
        assert_eq!(cargo_calls(&w).len(), 2, "{tag}");
        let log = fs::read_to_string(&w.log).unwrap();
        assert!(log.contains(&orq_orchestrator::test_total_line(None)), "{tag}: {log}");
        assert_eq!(orq_orchestrator::test_progress(&log), None, "{tag}: {log}");
    }
}

#[test]
fn a_count_that_hangs_is_stopped_and_the_tests_still_run() {
    let w = world("count-hangs");
    let env = fake_cargo(&w, "case \"$*\" in *--list*) sleep 30 ;; *) echo the-tests-ran ;; esac");
    let gate = steps(vec![cargo_step(&["test"])]);
    let limit = std::time::Duration::from_secs(1);
    let outcome = orq_orchestrator::run_gate_stage_with_limit(&w.store, "run-1", &gate, true, &w.log, &env, limit).unwrap();
    assert_eq!(outcome, GateOutcome::Passed);
    let log = fs::read_to_string(&w.log).unwrap();
    assert!(log.contains(&orq_orchestrator::test_total_line(None)) && log.contains("the-tests-ran"), "{log}");
}

#[test]
fn only_a_plain_cargo_test_step_is_counted() {
    let w = world("not-counted");
    let env = fake_cargo(&w, "exit 0");
    let gate = steps(vec![sh("echo cargo test"), cargo_step(&["clippy"]), cargo_step(&["test", "--", "--list"]), cargo_step(&["+nightly", "test"])]);
    assert_eq!(run_with(&w, &gate, &env), GateOutcome::Passed);
    assert_eq!(cargo_calls(&w), ["clippy", "test -- --list", "+nightly test"]);
    assert!(!fs::read_to_string(&w.log).unwrap().contains("tests to run"));
}

#[test]
fn a_later_round_shows_no_percentage_from_the_last_until_its_own_count_is_in() {
    let w = world("rounds");
    // While each step of the second round runs, the log it has so far is what the watcher sees.
    let seen = w.root.join("seen");
    let script = format!(
        r#"case "$*" in
clippy*) cat '{log}' >> '{seen}' ; echo ==== >> '{seen}' ;;
*--list*) cat '{log}' >> '{seen}' ; echo ==== >> '{seen}' ; echo '1 test, 0 benchmarks' ;;
*) echo 'running 1 test'; echo 'test a ... ok' ;;
esac"#,
        log = w.log.display(),
        seen = seen.display(),
    );
    let env = fake_cargo(&w, &script);
    let gate = steps(vec![cargo_step(&["clippy"]), cargo_step(&["test"])]);
    run_with(&w, &gate, &env);
    let _ = fs::remove_file(&seen);
    run_with(&w, &gate, &env);
    let seen = fs::read_to_string(&seen).unwrap();
    let views: Vec<&str> = seen.split("====\n").filter(|v| !v.is_empty()).collect();
    assert_eq!(views.len(), 2, "{seen}");
    for view in views {
        assert_eq!(orq_orchestrator::test_progress(view), None, "{view}");
    }
    let finished = fs::read_to_string(&w.log).unwrap();
    assert_eq!(orq_orchestrator::test_progress(&finished).map(|t| (t.done, t.total)), Some((1, 1)));
}
