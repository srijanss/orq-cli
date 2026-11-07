use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{
    run_gate_with_fixes, run_gate_with_fixes_env, FixSessionSpec, Gate, GateFixOutcome, RunRecord,
    RunStore,
};

struct World {
    store: RunStore,
    root: PathBuf,
    worktree: PathBuf,
    gate_log: PathBuf,
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-gate-fix-{}-{tag}", std::process::id()));
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
    let gate_log = root.join("gate.log");
    World {
        store,
        root,
        worktree,
        gate_log,
    }
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

/// A gate that passes once `fixed` exists in the worktree.
fn gate_needing_fixed() -> Gate {
    Gate::Steps {
        steps: vec![sh("test -f fixed")],
        notes: Vec::new(),
    }
}

fn spec(w: &World, script: &str) -> FixSessionSpec {
    FixSessionSpec {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        env: Vec::new(),
        cwd: w.worktree.clone(),
        session_id: Some("session-1".to_string()),
        log: w.root.join("fix.log"),
        terminal: false,
        exit_marker: None,
    }
}

fn kinds(w: &World) -> Vec<(String, String)> {
    w.store
        .events("run-1")
        .unwrap()
        .into_iter()
        .map(|e| (e.stage.unwrap_or_default(), e.kind))
        .collect()
}

fn pair(stage: &str, kind: &str) -> (String, String) {
    (stage.to_string(), kind.to_string())
}

#[test]
fn a_passing_gate_starts_no_fix_session() {
    let w = world("pass");
    fs::write(w.worktree.join("fixed"), "").unwrap();
    let calls = RefCell::new(0);

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        2,
        |_, _| {
            *calls.borrow_mut() += 1;
            spec(&w, "exit 0")
        },
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 0 });
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(
        kinds(&w),
        [
            pair("verify", "stage.started"),
            pair("verify", "stage.completed")
        ]
    );
}

#[test]
fn a_fix_session_that_repairs_the_failure_lets_the_gate_pass_on_the_rerun() {
    let w = world("repair");

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        2,
        |_, _| spec(&w, "touch fixed"),
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
    assert_eq!(
        kinds(&w),
        [
            pair("verify", "stage.started"),
            pair("verify", "stage.failed"),
            pair("gate_fix", "stage.started"),
            pair("gate_fix", "stage.completed"),
            pair("verify", "stage.started"),
            pair("verify", "stage.completed"),
        ]
    );
}

#[test]
fn reruns_and_fix_sessions_carry_their_attempt_number() {
    let w = world("numbered");

    run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        2,
        |attempt, _| {
            spec(
                &w,
                if attempt == 2 {
                    "touch fixed"
                } else {
                    "exit 0"
                },
            )
        },
    )
    .unwrap();

    let seen: Vec<(String, String, u32)> = w
        .store
        .events("run-1")
        .unwrap()
        .into_iter()
        .map(|e| (e.stage.unwrap(), e.kind, e.attempt))
        .collect();
    let started: Vec<_> = seen
        .iter()
        .filter(|(_, kind, _)| kind == "stage.started")
        .collect();
    assert_eq!(
        started,
        [
            &("verify".to_string(), "stage.started".to_string(), 1),
            &("gate_fix".to_string(), "stage.started".to_string(), 1),
            &("verify".to_string(), "stage.started".to_string(), 2),
            &("gate_fix".to_string(), "stage.started".to_string(), 2),
            &("verify".to_string(), "stage.started".to_string(), 3),
        ]
    );
    assert!(seen.iter().all(|(_, _, attempt)| *attempt >= 1));
}

#[test]
fn a_fixer_that_repairs_the_worktree_but_exits_nonzero_still_passes_on_the_rerun() {
    let w = world("nonzero");

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        2,
        |_, _| spec(&w, "touch fixed; exit 1"),
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
    let fix_failed = w
        .store
        .events("run-1")
        .unwrap()
        .into_iter()
        .find(|e| e.stage.as_deref() == Some("gate_fix") && e.kind == "stage.failed")
        .unwrap();
    assert_eq!(
        fix_failed.payload["exit_code"], 1,
        "the harness status is recorded but does not decide"
    );
    assert_eq!(
        kinds(&w),
        [
            pair("verify", "stage.started"),
            pair("verify", "stage.failed"),
            pair("gate_fix", "stage.started"),
            pair("gate_fix", "stage.failed"),
            pair("verify", "stage.started"),
            pair("verify", "stage.completed"),
        ]
    );
}

#[test]
fn a_gate_still_red_after_the_allowed_attempts_fails_the_run_and_keeps_the_worktree() {
    let w = world("red");
    let attempts = RefCell::new(Vec::new());

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        2,
        |attempt, _| {
            attempts.borrow_mut().push(attempt);
            spec(&w, "exit 0")
        },
    )
    .unwrap();

    assert_eq!(
        outcome,
        GateFixOutcome::Failed {
            fix_attempts: 2,
            step: 0,
            exit_code: Some(1)
        }
    );
    assert_eq!(*attempts.borrow(), [1, 2]);
    assert!(w.worktree.exists(), "the worktree is kept for inspection");
    let last = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(last.kind, "run.failed");
    assert!(
        last.payload["reason"]
            .as_str()
            .unwrap()
            .contains("2 fix attempt"),
        "{}",
        last.payload
    );
}

#[test]
fn zero_allowed_attempts_fails_straight_away_without_a_session() {
    let w = world("zero");
    let calls = RefCell::new(0);

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        0,
        |_, _| {
            *calls.borrow_mut() += 1;
            spec(&w, "touch fixed")
        },
    )
    .unwrap();

    assert_eq!(
        outcome,
        GateFixOutcome::Failed {
            fix_attempts: 0,
            step: 0,
            exit_code: Some(1)
        }
    );
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(
        w.store.events("run-1").unwrap().pop().unwrap().kind,
        "run.failed"
    );
}

#[test]
fn a_skipped_or_disabled_gate_is_never_fixed() {
    let w = world("skipped");
    let calls = RefCell::new(0);
    let mut fixer = |_: u32, _: &Path| {
        *calls.borrow_mut() += 1;
        spec(&w, "exit 0")
    };

    let skipped = Gate::Skipped {
        reason: "no local gate detected".to_string(),
    };
    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &skipped,
        true,
        &w.gate_log,
        2,
        &mut fixer,
    )
    .unwrap();
    assert_eq!(
        outcome,
        GateFixOutcome::Skipped {
            reason: "no local gate detected".to_string()
        }
    );

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        false,
        &w.gate_log,
        2,
        &mut fixer,
    )
    .unwrap();
    assert!(matches!(outcome, GateFixOutcome::Skipped { .. }));
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn a_fix_session_that_cannot_start_still_uses_up_an_attempt() {
    let w = world("nostart");
    let mut broken = spec(&w, "");
    broken.program = "/nonexistent/orq-fixer".to_string();

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        1,
        |_, _| broken.clone(),
    )
    .unwrap();

    assert_eq!(
        outcome,
        GateFixOutcome::Failed {
            fix_attempts: 1,
            step: 0,
            exit_code: Some(1)
        }
    );
    assert!(kinds(&w).contains(&pair("gate_fix", "stage.failed")));
}

#[test]
fn the_fixer_is_told_where_the_gate_output_is() {
    let w = world("log");
    let seen = RefCell::new(String::new());

    run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        1,
        |_, log| {
            *seen.borrow_mut() = fs::read_to_string(log).unwrap();
            spec(&w, "touch fixed")
        },
    )
    .unwrap();

    assert!(
        seen.borrow().contains("test -f fixed"),
        "the gate log is already written: {}",
        seen.borrow()
    );
}

#[test]
fn an_unknown_run_is_an_error_and_no_fix_session_is_asked_for() {
    let w = world("unknown");
    let calls = RefCell::new(0);

    let result = run_gate_with_fixes(
        &w.store,
        "ghost",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        2,
        |_, _| {
            *calls.borrow_mut() += 1;
            spec(&w, "exit 0")
        },
    );

    assert!(result.is_err());
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn every_gate_run_in_the_fix_loop_gets_the_run_environment() {
    let w = world("env");
    // The gate notes the variable on every run, so the first (failing) run is checked as well as the rerun.
    let gate = Gate::Steps {
        steps: vec![sh("echo \"[$TDD_DB]\" >> seen_db && test -f fixed")],
        notes: Vec::new(),
    };
    let env = vec![("TDD_DB".to_string(), "test_shop".to_string())];

    let outcome = run_gate_with_fixes_env(
        &w.store,
        "run-1",
        &gate,
        true,
        &w.gate_log,
        2,
        &env,
        |_, _| spec(&w, "touch fixed"),
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
    let seen = std::fs::read_to_string(w.worktree.join("seen_db")).unwrap();
    assert_eq!(
        seen, "[test_shop]\n[test_shop]\n",
        "both gate runs got the variable"
    );
}

#[test]
fn a_fix_session_that_leaves_its_marker_is_closed_and_the_gate_runs_again() {
    let w = world("marker");
    let marker = w.root.join("gate-fix.done");
    let script = format!("touch fixed; touch '{}'; sleep 30", marker.display());
    let started = std::time::Instant::now();

    let outcome = run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 2, |_, _| FixSessionSpec {
        exit_marker: Some(marker.clone()),
        ..spec(&w, &script)
    })
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "the session was left open: {:?}", started.elapsed());
}

#[test]
fn a_marker_left_by_the_first_fix_session_does_not_close_the_second() {
    let w = world("marker-retry");
    let marker = w.root.join("gate-fix.done");
    let gave_up = format!("touch '{}'; sleep 30", marker.display());

    let outcome = run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 2, |attempt, _| FixSessionSpec {
        exit_marker: Some(marker.clone()),
        ..spec(&w, if attempt == 1 { &gave_up } else { "sleep 2; touch fixed" })
    })
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 2 });
}

#[test]
fn a_marker_from_before_the_loop_does_not_close_the_first_fix_session() {
    let w = world("marker-stale");
    let marker = w.root.join("gate-fix.done");
    fs::write(&marker, "").unwrap();

    let outcome = run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 1, |_, _| FixSessionSpec {
        exit_marker: Some(marker.clone()),
        ..spec(&w, "sleep 2; touch fixed")
    })
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
}

#[test]
fn a_fix_session_that_reports_an_unrelated_failure_stops_the_loop_without_fixing_or_failing_the_run() {
    let w = world("unrelated");
    let calls = RefCell::new(0);
    let report = w.root.join("unrelated-failure.txt");

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        3,
        |_, _| {
            *calls.borrow_mut() += 1;
            spec(
                &w,
                &format!("printf 'billing clock test is flaky\\n' > {}", report.display()),
            )
        },
    )
    .unwrap();

    assert_eq!(
        outcome,
        GateFixOutcome::Unrelated {
            summary: "billing clock test is flaky".to_string()
        }
    );
    assert_eq!(*calls.borrow(), 1);
    assert!(!kinds(&w).contains(&pair("gate", "run.failed")), "{:?}", kinds(&w));
}

#[test]
fn a_report_left_by_an_earlier_session_does_not_stop_a_loop_whose_fix_works() {
    let w = world("stale-report");
    fs::write(w.root.join("unrelated-failure.txt"), "billing clock test is flaky\n").unwrap();

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        3,
        |_, _| spec(&w, "touch fixed"),
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
}

#[test]
fn an_empty_or_blank_report_is_no_report_and_the_gate_reruns() {
    let w = world("report-blank");
    let report = w.root.join("unrelated-failure.txt");

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        1,
        |_, _| spec(&w, &format!("printf ' \\n\\n' > '{}'; touch fixed", report.display())),
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
}

#[test]
fn a_report_that_is_not_text_is_still_reported_without_failing() {
    let w = world("report-binary");
    let report = w.root.join("unrelated-failure.txt");

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        1,
        |_, _| spec(&w, &format!("printf 'flaky \\377 clock\\n' > '{}'", report.display())),
    )
    .unwrap();

    match outcome {
        GateFixOutcome::Unrelated { summary } => assert!(summary.starts_with("flaky"), "{summary}"),
        other => panic!("expected Unrelated, got {other:?}"),
    }
}

#[test]
fn a_report_that_cannot_be_read_is_no_report() {
    let w = world("report-directory");
    let report = w.root.join("unrelated-failure.txt");

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        1,
        |_, _| spec(&w, &format!("mkdir '{}'; touch fixed", report.display())),
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
}

#[test]
fn a_directory_left_as_the_report_does_not_stop_the_next_fix_attempt() {
    let w = world("report-directory-twice");
    let report = w.root.join("unrelated-failure.txt");

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        2,
        |attempt, _| {
            if attempt == 1 {
                spec(&w, &format!("mkdir -p '{}/inner'; touch '{}/inner/f'", report.display(), report.display()))
            } else {
                spec(&w, "touch fixed")
            }
        },
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 2 });
}

#[test]
fn a_directory_already_in_the_reports_place_does_not_stop_the_first_fix_attempt() {
    let w = world("report-directory-before");
    fs::create_dir_all(w.root.join("unrelated-failure.txt")).unwrap();

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        1,
        |_, _| spec(&w, "touch fixed"),
    )
    .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
}

#[test]
fn a_huge_report_is_cut_to_one_short_line() {
    let w = world("report-huge");
    let report = w.root.join("unrelated-failure.txt");
    let mut text = "x".repeat(100_000);
    text.push('\n');
    text.push_str(&"second line\n".repeat(100_000));
    fs::write(w.root.join("big.txt"), text).unwrap();

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        1,
        |_, _| spec(&w, &format!("cp '{}' '{}'", w.root.join("big.txt").display(), report.display())),
    )
    .unwrap();

    match outcome {
        GateFixOutcome::Unrelated { summary } => {
            assert!(summary.chars().count() <= 500, "{} chars", summary.len());
            assert!(!summary.contains('\n') && !summary.contains("second line"), "{summary}");
        }
        other => panic!("expected Unrelated, got {other:?}"),
    }
}

#[test]
fn a_report_that_is_a_link_to_another_file_is_not_read() {
    let w = world("report-link");
    let secret = w.root.join("secret.txt");
    fs::write(&secret, "hunter2-private-contents\n").unwrap();
    let report = w.root.join("unrelated-failure.txt");

    let outcome = run_gate_with_fixes(
        &w.store,
        "run-1",
        &gate_needing_fixed(),
        true,
        &w.gate_log,
        1,
        |_, _| spec(&w, &format!("ln -s '{}' '{}'", secret.display(), report.display())),
    )
    .unwrap();

    assert!(
        !matches!(outcome, GateFixOutcome::Unrelated { .. }),
        "a linked report was read: {outcome:?}"
    );
}
