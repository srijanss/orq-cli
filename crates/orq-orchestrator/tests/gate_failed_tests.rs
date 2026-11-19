//! A failed verify gate records the tests its failing step failed, by name with the end of each one's output, the
//! same way a failed Accept does, so the run page can list them.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use orq_orchestrator::{run_gate_stage, Event, Gate, RunRecord, RunStore};

fn world(tag: &str) -> (RunStore, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-gate-failed-tests-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".into(),
            project_id: "repo-1".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "0".repeat(40),
            branch: "fix--F".into(),
            worktree,
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    (store, root.join("gate.log"))
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), script.into()]
}

fn failed(store: &RunStore) -> Event {
    store.events("run-1").unwrap().into_iter().rev().find(|event| event.kind == "stage.failed").expect("stage.failed")
}

#[test]
fn a_failed_gate_step_records_its_failed_tests_by_name_with_their_output() {
    let (store, log) = world("named");
    // An earlier attempt's failure in the same log is not this step's.
    fs::write(&log, "        FAIL [   0.010s] (1/1) old::suite gone\n").unwrap();
    let script = "echo '        FAIL [   0.010s] (1/2) orq-core::store saves'; \
                  echo '──── STDERR:             orq-core::store saves'; \
                  echo \"thread 'saves' panicked at src/store.rs:4:5:\"; \
                  echo '  left: 1'; echo ' right: 2'; \
                  echo '        PASS [   0.004s] (2/2) orq-core::store loads'; exit 100";
    let gate = Gate::Steps { steps: vec![sh(script)], notes: Vec::new() };

    run_gate_stage(&store, "run-1", &gate, true, &log).unwrap();

    let event = failed(&store);
    let tests = event.payload["failed_tests"].as_array().expect("failed_tests");
    let names: Vec<&str> = tests.iter().map(|test| test["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["orq-core::store saves"]);
    let output = tests[0]["output"].as_str().unwrap();
    assert!(output.contains("left: 1") && output.contains("right: 2"), "{output}");
    assert_eq!(event.payload["failed_tests_more"], 0);
}

#[test]
fn a_gate_step_that_fails_without_naming_a_test_records_an_empty_list() {
    let (store, log) = world("build");
    let gate = Gate::Steps { steps: vec![sh("echo 'error: could not compile'; exit 101")], notes: Vec::new() };

    run_gate_stage(&store, "run-1", &gate, true, &log).unwrap();

    let event = failed(&store);
    assert_eq!(event.payload["failed_tests"], serde_json::json!([]));
}
