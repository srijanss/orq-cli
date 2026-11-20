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
            delivery: Default::default(),
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

    let outcome = orq_orchestrator::run_gate_stage_with_limit(
        &w.store,
        "run-1",
        &gate,
        true,
        &w.log,
        &[],
        Duration::from_secs(1),
    )
    .unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the gate waited for the step"
    );
    assert!(
        matches!(
            outcome,
            GateOutcome::Failed {
                step: 0,
                exit_code: None
            }
        ),
        "{outcome:?}"
    );
    let failed = w
        .store
        .events("run-1")
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "stage.failed")
        .expect("a failure was recorded");
    assert!(
        failed.payload["reason"]
            .as_str()
            .unwrap()
            .contains("timed out"),
        "{:?}",
        failed.payload
    );
    std::thread::sleep(Duration::from_secs(4));
    assert!(!late.exists(), "a process the step started kept running");
}

#[test]
fn a_step_that_exits_early_takes_what_it_left_running_with_it_and_keeps_its_exit_code() {
    use std::time::Duration;
    let w = world("leftover");
    let late = w.root.join("late");
    let gate = steps(vec![sh(&format!(
        "(sleep 2; touch '{}') & exit 3",
        late.display()
    ))]);

    let outcome = run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap();

    assert_eq!(
        outcome,
        GateOutcome::Failed {
            step: 0,
            exit_code: Some(3)
        }
    );
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !late.exists(),
        "a process the step left behind kept running"
    );
}

/// A `cargo` on the PATH given to the gate that notes its arguments and then runs `script`.
fn fake_cargo(w: &World, script: &str) -> Vec<(String, String)> {
    use std::os::unix::fs::PermissionsExt;
    let bin = w.root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = w.root.join("cargo-calls");
    let cargo = bin.join("cargo");
    fs::write(&cargo, format!("#!/bin/sh\ncase \"$1\" in metadata) echo '{{\"packages\":[]}}'; exit 0 ;; esac\necho \"$*\" >> '{}'\n{script}\n", calls.display())).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    vec![("PATH".to_string(), path)]
}

fn cargo_step(args: &[&str]) -> Vec<String> {
    std::iter::once("cargo")
        .chain(args.iter().copied())
        .map(str::to_string)
        .collect()
}

fn cargo_calls(w: &World) -> Vec<String> {
    fs::read_to_string(w.root.join("cargo-calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
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
    assert_eq!(
        cargo_calls(&w),
        ["test --workspace -- --list", "test --workspace"]
    );
    let log = fs::read_to_string(&w.log).unwrap();
    let total = orq_orchestrator::test_total_line(Some(4));
    let at = log.find(&total).unwrap_or_else(|| panic!("{log}"));
    assert!(at < log.find("the-tests-ran").unwrap(), "{log}");
    assert!(
        !log.contains("a::one"),
        "the listing is not test output: {log}"
    );
    let left: Vec<_> = fs::read_dir(&w.root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(
        left.iter()
            .all(|name| !name.to_string_lossy().contains("list")),
        "{left:?}"
    );
}

#[test]
fn the_count_goes_to_the_test_binaries_alongside_their_own_arguments() {
    let w = world("count-args");
    let env = fake_cargo(&w, LISTING);
    run_with(
        &w,
        &steps(vec![cargo_step(&["test", "-p", "x", "--", "--nocapture"])]),
        &env,
    );
    assert_eq!(
        cargo_calls(&w),
        [
            "test -p x -- --nocapture --list",
            "test -p x -- --nocapture"
        ]
    );
}

#[test]
fn a_count_that_fails_leaves_the_total_unknown_and_the_tests_still_decide() {
    for (tag, script) in [
        (
            "count-fails",
            "case \"$*\" in *--list*) echo partial; echo '3 tests, 0 benchmarks'; exit 101 ;; esac",
        ),
        (
            "count-empty",
            "case \"$*\" in *--list*) echo nothing-countable ;; esac",
        ),
        (
            "count-garbled",
            "case \"$*\" in *--list*) echo 'many tests, 0 benchmarks' ;; esac",
        ),
        (
            "count-overflow",
            "case \"$*\" in *--list*) echo '18446744073709551615 tests, 1 benchmark' ;; esac",
        ),
    ] {
        let w = world(tag);
        let env = fake_cargo(&w, script);
        assert_eq!(
            run_with(&w, &steps(vec![cargo_step(&["test"])]), &env),
            GateOutcome::Passed,
            "{tag}"
        );
        assert_eq!(cargo_calls(&w).len(), 2, "{tag}");
        let log = fs::read_to_string(&w.log).unwrap();
        assert!(
            log.contains(&orq_orchestrator::test_total_line(None)),
            "{tag}: {log}"
        );
        assert_eq!(orq_orchestrator::test_progress(&log), None, "{tag}: {log}");
    }
}

#[test]
fn a_count_that_hangs_is_stopped_and_the_tests_still_run() {
    let w = world("count-hangs");
    let env = fake_cargo(
        &w,
        "case \"$*\" in *--list*) sleep 30 ;; *) echo the-tests-ran ;; esac",
    );
    let gate = steps(vec![cargo_step(&["test"])]);
    let limit = std::time::Duration::from_secs(1);
    let outcome = orq_orchestrator::run_gate_stage_with_limit(
        &w.store, "run-1", &gate, true, &w.log, &env, limit,
    )
    .unwrap();
    assert_eq!(outcome, GateOutcome::Passed);
    let log = fs::read_to_string(&w.log).unwrap();
    assert!(
        log.contains(&orq_orchestrator::test_total_line(None)) && log.contains("the-tests-ran"),
        "{log}"
    );
}

#[test]
fn only_a_plain_cargo_test_step_is_counted() {
    let w = world("not-counted");
    let env = fake_cargo(&w, "exit 0");
    let gate = steps(vec![
        sh("echo cargo test"),
        cargo_step(&["clippy"]),
        cargo_step(&["test", "--", "--list"]),
        cargo_step(&["+nightly", "test"]),
    ]);
    assert_eq!(run_with(&w, &gate, &env), GateOutcome::Passed);
    assert_eq!(
        cargo_calls(&w),
        ["clippy", "test -- --list", "+nightly test"]
    );
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
    assert_eq!(
        orq_orchestrator::test_progress(&finished).map(|t| (t.done, t.total)),
        Some((1, 1))
    );
}

/// A real crate in the worktree, built by the real cargo: `files` are (path, content) pairs.
fn real_crate(w: &World, files: &[(&str, &str)]) {
    for (path, content) in files {
        let path = w.worktree.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

#[test]
fn a_test_target_with_its_own_harness_runs_once_and_leaves_the_total_unknown() {
    let w = world("own-harness");
    real_crate(&w, &[
        (
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[test]]\nname = \"custom\"\npath = \"tests/custom.rs\"\nharness = false\n",
        ),
        ("src/lib.rs", "#[test]\nfn unit() {}\n"),
        (
            "tests/custom.rs",
            "use std::io::Write;\nfn main() {\n    let mut ran = std::fs::OpenOptions::new().create(true).append(true).open(\"ran\").unwrap();\n    writeln!(ran, \"ran\").unwrap();\n}\n",
        ),
    ]);
    let outcome = run_with(&w, &steps(vec![cargo_step(&["test", "--workspace"])]), &[]);
    let log = fs::read_to_string(&w.log).unwrap();
    assert_eq!(outcome, GateOutcome::Passed, "{log}");
    assert_eq!(
        fs::read_to_string(w.worktree.join("ran"))
            .unwrap()
            .lines()
            .count(),
        1,
        "{log}"
    );
    assert_eq!(orq_orchestrator::test_progress(&log), None, "{log}");
}

#[test]
fn every_way_toml_can_spell_a_custom_harness_runs_the_target_once() {
    const PACKAGE: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\nautotests = false\n\n";
    const TARGET: &str = "name = \"custom\"\npath = \"tests/custom.rs\"\n";
    let spellings = [
        ("double-quoted", format!("{PACKAGE}[[test]]\n{TARGET}\"harness\" = false\n")),
        ("single-quoted", format!("{PACKAGE}[[test]]\n{TARGET}'harness' = false\n")),
        ("inline", format!("test = [{{ name = \"custom\", path = \"tests/custom.rs\", harness = false }}]\n\n{PACKAGE}")),
    ];
    for (tag, manifest) in spellings {
        let w = world(&format!("spelling-{tag}"));
        real_crate(&w, &[
            ("Cargo.toml", manifest.as_str()),
            ("src/lib.rs", ""),
            (
                "tests/custom.rs",
                "use std::io::Write;\nfn main() {\n    let mut ran = std::fs::OpenOptions::new().create(true).append(true).open(\"ran\").unwrap();\n    writeln!(ran, \"ran\").unwrap();\n}\n",
            ),
        ]);
        let outcome = run_with(&w, &steps(vec![cargo_step(&["test", "--workspace"])]), &[]);
        let log = fs::read_to_string(&w.log).unwrap();
        assert_eq!(outcome, GateOutcome::Passed, "{tag}: {log}");
        assert_eq!(
            fs::read_to_string(w.worktree.join("ran"))
                .unwrap()
                .lines()
                .count(),
            1,
            "{tag}: {log}"
        );
    }
}

#[test]
fn a_comment_that_mentions_a_custom_harness_does_not_cost_the_percentage() {
    let w = world("harness-comment");
    real_crate(&w, &[
        ("Cargo.toml", "# harness = false\n[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("src/lib.rs", "#[test]\nfn unit() {}\n"),
    ]);
    let outcome = run_with(&w, &steps(vec![cargo_step(&["test", "--lib"])]), &[]);
    let log = fs::read_to_string(&w.log).unwrap();
    assert_eq!(outcome, GateOutcome::Passed, "{log}");
    assert_eq!(
        orq_orchestrator::test_progress(&log).map(|t| (t.done, t.total)),
        Some((1, 1)),
        "{log}"
    );
}

#[test]
fn a_manifest_that_cannot_be_parsed_is_not_listed_and_the_tests_still_run() {
    let w = world("bad-manifest");
    let env = fake_cargo(&w, "");
    let manifest = w.root.join("bad.toml");
    fs::write(&manifest, "not [ toml").unwrap();
    let bin = w.root.join("bin").join("cargo");
    let script = fs::read_to_string(&bin).unwrap();
    let packages = format!(
        "{{\"packages\":[{{\"manifest_path\":\"{}\"}}]}}",
        manifest.display()
    );
    fs::write(&bin, script.replace("{\"packages\":[]}", &packages)).unwrap();
    assert_eq!(
        run_with(&w, &steps(vec![cargo_step(&["test"])]), &env),
        GateOutcome::Passed
    );
    assert_eq!(cargo_calls(&w), ["test"], "only the step itself ran");
}

#[test]
fn a_workspace_that_cannot_be_inspected_is_not_listed_and_the_tests_still_run() {
    let w = world("no-metadata");
    let env = fake_cargo(&w, "");
    let bin = w.root.join("bin").join("cargo");
    let script = fs::read_to_string(&bin).unwrap();
    fs::write(
        &bin,
        script.replacen(
            "#!/bin/sh\n",
            "#!/bin/sh\ncase \"$1\" in metadata) exit 1 ;; esac\n",
            1,
        ),
    )
    .unwrap();
    assert_eq!(
        run_with(&w, &steps(vec![cargo_step(&["test"])]), &env),
        GateOutcome::Passed
    );
    assert_eq!(cargo_calls(&w), ["test"], "only the step itself ran");
}

#[test]
fn real_cargo_reports_as_many_results_as_it_listed_whatever_the_selection() {
    let selections: [(&str, &[&str], u64); 6] = [
        ("all", &["test", "--workspace"], 5),
        ("filter", &["test", "one"], 2),
        ("lib", &["test", "--lib"], 3),
        ("ignored", &["test", "--", "--ignored"], 1),
        ("include-ignored", &["test", "--", "--include-ignored"], 5),
        ("skip", &["test", "--", "--skip", "one"], 3),
    ];
    for (tag, args, expected) in selections {
        let w = world(&format!("real-{tag}"));
        real_crate(&w, &[
            ("Cargo.toml", "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            (
                "src/lib.rs",
                "/// ```\n/// assert_eq!(fixture::two(), 2);\n/// ```\npub fn two() -> u32 {\n    2\n}\n\n#[test]\nfn one() {}\n#[test]\nfn another() {}\n#[test]\n#[ignore]\nfn slow() {}\n",
            ),
            ("tests/it.rs", "#[test]\nfn one_more() {}\n"),
        ]);
        let outcome = run_with(&w, &steps(vec![cargo_step(args)]), &[]);
        let log = fs::read_to_string(&w.log).unwrap();
        assert_eq!(outcome, GateOutcome::Passed, "{tag}: {log}");
        let progress = orq_orchestrator::test_progress(&log).map(|t| (t.done, t.total));
        assert_eq!(progress, Some((expected, expected)), "{tag}: {log}");
    }
}

#[test]
fn a_file_already_at_the_scratch_path_is_left_alone_and_only_costs_the_percentage() {
    let w = world("scratch-taken");
    let env = fake_cargo(&w, LISTING);
    // The names the scratch file has had or has: neither may be emptied or removed.
    let taken = [
        w.log.with_extension("test-list"),
        w.log
            .with_extension(format!("test-list-{}", std::process::id())),
    ];
    for path in &taken {
        fs::write(path, "someone else's").unwrap();
    }
    assert_eq!(
        run_with(&w, &steps(vec![cargo_step(&["test"])]), &env),
        GateOutcome::Passed
    );
    for path in &taken {
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "someone else's",
            "{}",
            path.display()
        );
    }
    let log = fs::read_to_string(&w.log).unwrap();
    assert!(log.contains("the-tests-ran"), "{log}");
    assert_eq!(orq_orchestrator::test_progress(&log), None, "{log}");
}

#[test]
fn a_step_aimed_at_another_manifest_is_not_counted() {
    let w = world("manifest-path");
    let env = fake_cargo(&w, "exit 0");
    let gate = steps(vec![
        cargo_step(&["test", "--manifest-path", "other/Cargo.toml"]),
        cargo_step(&["test", "--manifest-path=other/Cargo.toml"]),
    ]);
    assert_eq!(run_with(&w, &gate, &env), GateOutcome::Passed);
    assert_eq!(
        cargo_calls(&w),
        [
            "test --manifest-path other/Cargo.toml",
            "test --manifest-path=other/Cargo.toml"
        ]
    );
}

/// A `name` on the PATH given to the gate that notes its arguments in `<name>-calls` and then runs `script`.
fn fake_tool(w: &World, name: &str, script: &str) -> Vec<(String, String)> {
    use std::os::unix::fs::PermissionsExt;
    let bin = w.root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = w.root.join(format!("{name}-calls"));
    let tool = bin.join(name);
    fs::write(
        &tool,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n{script}\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    vec![("PATH".to_string(), path)]
}

fn tool_calls(w: &World, name: &str) -> Vec<String> {
    fs::read_to_string(w.root.join(format!("{name}-calls")))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

/// A vitest that lists 3 tests as JSON when asked, and otherwise runs them, one file line each.
const VITEST: &str = r#"case "$*" in
*list*) echo '[{"name":"total > adds","file":"/w/src/a.test.ts"},{"name":"total > rounds","file":"/w/src/a.test.ts"},{"name":"coupon > applies","file":"/w/src/b.test.ts"}]' ;;
*) echo ' ✓ src/a.test.ts (2 tests) 3ms'; echo ' ✓ src/b.test.ts (1 test) 2ms' ;;
esac"#;

#[test]
fn a_js_run_step_is_counted_by_listing_its_tests_before_it_runs() {
    let w = world("js-run");
    let env = fake_tool(&w, "vitest", VITEST);
    let outcome = run_with(&w, &steps(vec![argv(&["vitest", "run", "src/"])]), &env);
    assert_eq!(outcome, GateOutcome::Passed);
    assert_eq!(tool_calls(&w, "vitest"), ["list --json src/", "run src/"]);
    let log = fs::read_to_string(&w.log).unwrap();
    let total = orq_orchestrator::test_total_line(Some(3));
    let at = log.find(&total).unwrap_or_else(|| panic!("{log}"));
    assert!(at < log.find("src/a.test.ts").unwrap(), "{log}");
    assert!(
        !log.contains("coupon > applies"),
        "the listing is not test output: {log}"
    );
    let progress = orq_orchestrator::test_progress(&log).map(|t| (t.done, t.total, t.current));
    assert_eq!(
        progress,
        Some((3, 3, Some("src/b.test.ts".to_string()))),
        "{log}"
    );
}

#[test]
fn a_vitest_run_step_writes_its_listed_total_before_it_runs() {
    let w = world("vitest-total");
    let env = fake_tool(&w, "vitest", VITEST);
    let outcome = run_with(
        &w,
        &steps(vec![argv(&["vitest", "run", "--reporter=default", "src/"])]),
        &env,
    );
    assert_eq!(outcome, GateOutcome::Passed);
    assert_eq!(
        tool_calls(&w, "vitest"),
        [
            "list --json --reporter=default src/",
            "run --reporter=default src/"
        ]
    );
    let log = fs::read_to_string(&w.log).unwrap();
    let at = log
        .find(&orq_orchestrator::test_total_line(Some(3)))
        .unwrap_or_else(|| panic!("{log}"));
    assert!(at < log.find("src/a.test.ts").unwrap(), "{log}");
    assert!(
        !log.contains("coupon > applies"),
        "the listing is not test output: {log}"
    );
}

/// An npm, pnpm or yarn that runs a package's `vitest` for `exec` and bin forms, and the `vitest run`
/// a script would be for `run <script>` or `test`.
const RUNNER: &str = r#"case "$1" in
exec) shift; [ "$1" = --no ] && shift; [ "$1" = -- ] && shift; exec "$@" ;;
--no) shift; exec "$@" ;;
vitest) exec "$@" ;;
run|test) exec vitest run ;;
esac"#;

#[test]
fn a_vitest_run_through_a_package_runner_or_its_script_is_listed_through_the_same_runner() {
    let cases: [(&str, &[&str], &str); 11] = [
        ("npx", &["npx", "vitest", "run", "src/"], "list --json src/"),
        (
            "pnpm-exec",
            &["pnpm", "exec", "vitest", "run"],
            "list --json",
        ),
        ("pnpm-bin", &["pnpm", "vitest", "run"], "list --json"),
        ("yarn-bin", &["yarn", "vitest"], "list --json"),
        ("npm-run", &["npm", "run", "test"], "list --json"),
        (
            "npm-run-args",
            &["npm", "run", "test", "--", "src/"],
            "list --json src/",
        ),
        ("npm-test", &["npm", "test"], "list --json"),
        ("pnpm-test", &["pnpm", "test"], "list --json"),
        (
            "pnpm-run",
            &["pnpm", "run", "unit"],
            "list --json --project unit",
        ),
        ("yarn-test", &["yarn", "test"], "list --json"),
        (
            "yarn-run",
            &["yarn", "run", "unit", "src/"],
            "list --json --project unit src/",
        ),
    ];
    for (tag, step, listed) in cases {
        let w = world(&format!("runner-{tag}"));
        fs::write(
            w.worktree.join("package.json"),
            r#"{"scripts":{"test":"vitest run","unit":"vitest --project unit"}}"#,
        )
        .unwrap();
        let env = fake_tool(&w, "vitest", VITEST);
        fake_tool(&w, step[0], RUNNER);
        assert_eq!(
            run_with(&w, &steps(vec![argv(step)]), &env),
            GateOutcome::Passed,
            "{tag}"
        );
        let calls = tool_calls(&w, "vitest");
        assert_eq!(
            calls.first().map(String::as_str),
            Some(listed),
            "{tag}: {calls:?}"
        );
        assert_eq!(calls.len(), 2, "{tag}: {calls:?}");
        let log = fs::read_to_string(&w.log).unwrap();
        assert!(
            log.contains(&orq_orchestrator::test_total_line(Some(3))),
            "{tag}: {log}"
        );
    }
}

#[test]
fn a_vitest_listing_that_fails_leaves_the_total_unknown_and_the_tests_still_decide() {
    for (tag, listing) in [
        (
            "vitest-list-fails",
            r#"echo '[{"name":"a","file":"a.test.ts"}]'; exit 1"#,
        ),
        ("vitest-list-garbled", "echo 'no tests here'"),
        ("vitest-list-not-tests", "echo '[1, 2, 3]'"),
        (
            "vitest-list-object",
            r#"echo '{"name":"a","file":"a.test.ts"}'"#,
        ),
        ("vitest-list-empty", "echo '[]'"),
    ] {
        let w = world(tag);
        let env = fake_tool(
            &w,
            "vitest",
            &format!("case \"$1\" in list) {listing} ;; *) echo the-tests-ran; exit 4 ;; esac"),
        );
        let outcome = run_with(&w, &steps(vec![argv(&["vitest", "run"])]), &env);
        assert_eq!(
            outcome,
            GateOutcome::Failed {
                step: 0,
                exit_code: Some(4)
            },
            "{tag}"
        );
        assert_eq!(tool_calls(&w, "vitest").len(), 2, "{tag}");
        let log = fs::read_to_string(&w.log).unwrap();
        assert!(
            log.contains(&orq_orchestrator::test_total_line(None)) && log.contains("the-tests-ran"),
            "{tag}: {log}"
        );
        assert_eq!(orq_orchestrator::test_progress(&log), None, "{tag}: {log}");
    }
}

#[test]
fn a_step_that_is_not_a_plain_vitest_run_gets_no_vitest_listing() {
    let scripts = r#"{"scripts":{"test":"vitest run","jest":"jest","chained":"vitest run && eslint .","env":"CI=1 vitest run","quoted":"vitest run 'src/a b'","list":"vitest list"}}"#;
    let cases: [(&str, &[&str]); 10] = [
        ("other-script", &["npm", "run", "jest"]),
        ("chained-script", &["npm", "run", "chained"]),
        ("env-script", &["pnpm", "env"]),
        ("quoted-script", &["yarn", "quoted"]),
        ("list-script", &["npm", "run", "list"]),
        ("missing-script", &["npm", "run", "nothing"]),
        ("npm-args-without-dashes", &["npm", "run", "test", "src/"]),
        ("pnpm-args-after-dashes", &["pnpm", "test", "--", "src/"]),
        ("compound-shell", &["bash", "-c", "vitest run; echo done"]),
        ("vitest-watch", &["vitest", "watch"]),
    ];
    for (tag, step) in cases {
        let w = world(&format!("not-vitest-{tag}"));
        fs::write(w.worktree.join("package.json"), scripts).unwrap();
        let env = fake_tool(&w, "vitest", VITEST);
        fake_tool(&w, step[0], "exit 0");
        assert_eq!(
            run_with(&w, &steps(vec![argv(step)]), &env),
            GateOutcome::Passed,
            "{tag}"
        );
        assert!(
            tool_calls(&w, "vitest")
                .iter()
                .all(|call| !call.starts_with("list")),
            "{tag}"
        );
        assert!(
            !fs::read_to_string(&w.log).unwrap().contains("tests to run"),
            "{tag}"
        );
    }
}

#[test]
fn a_vitest_script_package_json_that_is_not_a_plain_file_is_not_read_and_the_step_still_runs() {
    use std::sync::mpsc;
    let w = world("package-fifo");
    let fifo = w.worktree.join("package.json");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let env = fake_tool(&w, "npm", "echo the-tests-ran");
    let (sent, received) = mpsc::channel();
    let (store, log) = (RunStore::new(&w.root.join("state")), w.log.clone());
    std::thread::spawn(move || {
        let outcome = orq_orchestrator::run_gate_stage_with_env(
            &store,
            "run-1",
            &steps(vec![argv(&["npm", "test"])]),
            true,
            &log,
            &env,
        );
        let _ = sent.send(outcome.unwrap());
    });
    let outcome = received.recv_timeout(std::time::Duration::from_secs(20));
    if outcome.is_err() {
        // Let the stuck reader go, so the test process can end.
        let _ = fs::OpenOptions::new().write(true).open(&fifo);
    }
    assert_eq!(
        outcome,
        Ok(GateOutcome::Passed),
        "the gate waited on the package.json"
    );
    assert!(fs::read_to_string(&w.log)
        .unwrap()
        .contains("the-tests-ran"));
}

/// An npm or npx with no vitest installed, that would fetch one from the registry for a listing
/// unless told not to install.
const FETCHING_RUNNER: &str = r#"case " $* " in
*" --no "*) echo 'npm error could not determine executable to run' >&2; exit 1 ;;
*" list "*) touch fetched; echo '[{"name":"a","file":"a.test.ts"}]' ;;
*) echo the-tests-ran ;;
esac"#;

#[test]
fn a_vitest_listing_never_fetches_vitest_when_it_is_not_installed() {
    for (tag, step) in [
        ("npm", &["npm", "test"][..]),
        ("npx", &["npx", "vitest", "run"][..]),
    ] {
        let w = world(&format!("vitest-no-fetch-{tag}"));
        fs::write(
            w.worktree.join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .unwrap();
        let env = fake_tool(&w, step[0], FETCHING_RUNNER);
        assert_eq!(
            run_with(&w, &steps(vec![argv(step)]), &env),
            GateOutcome::Passed,
            "{tag}"
        );
        assert!(
            !w.worktree.join("fetched").exists(),
            "{tag}: the listing fetched a package: {:?}",
            tool_calls(&w, step[0])
        );
        let log = fs::read_to_string(&w.log).unwrap();
        assert!(
            log.contains(&orq_orchestrator::test_total_line(None)) && log.contains("the-tests-ran"),
            "{tag}: {log}"
        );
    }
}

#[test]
fn a_vitest_script_with_a_bracket_glob_is_not_listed_since_its_shell_would_expand_it() {
    for (tag, script) in [
        ("class", "vitest run src/[ab].test.ts"),
        ("closing", "vitest run src/a].test.ts"),
    ] {
        let w = world(&format!("vitest-bracket-{tag}"));
        for file in ["a.test.ts", "b.test.ts"] {
            fs::create_dir_all(w.worktree.join("src")).unwrap();
            fs::write(w.worktree.join("src").join(file), "").unwrap();
        }
        fs::write(
            w.worktree.join("package.json"),
            format!(r#"{{"scripts":{{"test":"{script}"}}}}"#),
        )
        .unwrap();
        let env = fake_tool(&w, "vitest", VITEST);
        fake_tool(&w, "npm", RUNNER);
        assert_eq!(
            run_with(&w, &steps(vec![argv(&["npm", "test"])]), &env),
            GateOutcome::Passed,
            "{tag}"
        );
        assert!(
            tool_calls(&w, "vitest")
                .iter()
                .all(|call| !call.starts_with("list")),
            "{tag}"
        );
        assert!(
            !fs::read_to_string(&w.log).unwrap().contains("tests to run"),
            "{tag}"
        );
    }
}

#[test]
fn the_gates_notes_open_its_log_so_a_fallback_to_cargo_test_is_said_first() {
    let w = world("notes");
    let gate = Gate::Steps {
        steps: vec![sh("echo ran")],
        notes: vec!["cargo-nextest not found: running cargo test".to_string()],
    };

    assert_eq!(
        run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap(),
        GateOutcome::Passed
    );

    let log = fs::read_to_string(&w.log).unwrap();
    assert_eq!(
        log.lines().next(),
        Some("orq: cargo-nextest not found: running cargo test"),
        "{log}"
    );
    assert!(log.contains("ran"), "{log}");
}

/// What the fake `cargo nextest run` prints: a test that passed on its second try (shown twice, as
/// with and without the counter column), one that passed on its third, after a long unbroken line.
const NEXTEST_WITH_FLAKY: &str = r#"case "$1" in nextest)
printf '    Starting 2 tests across 1 binary\n        PASS [   0.002s] (1/2) orq-core a_steady_one\n'
head -c 300000 /dev/zero | tr '\0' x; echo
printf '       FLAKY 2/3 [   0.004s] (2/2) orq-cli::pipeline two_runs_in_one_second\n'
printf '       FLAKY 2/3 [   0.004s] orq-cli::pipeline two_runs_in_one_second\n'
printf '       FLAKY 3/3 [   0.009s] orq-core::store a_slow_lock\n' ;;
esac"#;

#[test]
fn a_test_nextest_passed_only_on_a_retry_is_recorded_as_flaky_before_the_stage_ends() {
    let w = world("flaky");
    let env = fake_cargo(&w, NEXTEST_WITH_FLAKY);
    let gate = steps(vec![cargo_step(&["nextest", "run", "--workspace"])]);

    assert_eq!(run_with(&w, &gate, &env), GateOutcome::Passed);

    assert_eq!(
        kinds(&w),
        [
            verify("stage.started"),
            verify("gate.flaky_tests"),
            verify("stage.completed")
        ]
    );
    let flaky = &w.store.events("run-1").unwrap()[1];
    assert_eq!(
        flaky.payload["tests"],
        serde_json::json!([
            { "name": "orq-cli::pipeline two_runs_in_one_second", "retry": 1 },
            { "name": "orq-core::store a_slow_lock", "retry": 2 },
        ])
    );
}

#[test]
fn flaky_lines_from_a_step_that_is_not_nextest_are_not_looked_for() {
    let w = world("flaky-elsewhere");
    let env = fake_cargo(&w, NEXTEST_WITH_FLAKY);
    // The same output from a step that is not itself `cargo nextest ...`: only nextest's steps are scanned.
    let gate = steps(vec![sh("cargo nextest run")]);

    assert_eq!(run_with(&w, &gate, &env), GateOutcome::Passed);

    assert_eq!(
        kinds(&w),
        [verify("stage.started"), verify("stage.completed")]
    );
    assert!(
        fs::read_to_string(&w.log).unwrap().contains("FLAKY 3/3"),
        "the step did print them"
    );
}

#[test]
fn a_gate_with_no_flaky_line_records_no_flaky_event_even_when_a_step_fails() {
    let w = world("steady");
    let gate = steps(vec![sh(
        "echo '        PASS [   0.002s] orq-core a_steady_one'; exit 4",
    )]);

    run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap();

    assert_eq!(kinds(&w), [verify("stage.started"), verify("stage.failed")]);
}

#[test]
fn a_gate_start_records_how_many_lines_its_log_had_before_the_attempt_wrote_to_it() {
    let w = world("log-line");
    fs::write(&w.log, "earlier attempt\nits failure\n").unwrap();
    let gate = Gate::Steps {
        steps: vec![sh("echo checked")],
        notes: vec!["nextest is not installed".to_string()],
    };

    run_gate_stage(&w.store, "run-1", &gate, true, &w.log).unwrap();

    let start = w
        .store
        .events("run-1")
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "stage.started")
        .unwrap();
    // The attempt's part starts with its notes, so they belong to it.
    assert_eq!(start.payload["log_line"], 2, "{}", start.payload);
    // And the byte the part starts at, so it is read from there.
    assert_eq!(
        start.payload["log_offset"],
        "earlier attempt\nits failure\n".len(),
        "{}",
        start.payload
    );
    let log = fs::read_to_string(&w.log).unwrap();
    assert_eq!(
        log.lines().nth(2),
        Some("orq: nextest is not installed"),
        "{log}"
    );
}
