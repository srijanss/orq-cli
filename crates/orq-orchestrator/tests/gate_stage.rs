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
