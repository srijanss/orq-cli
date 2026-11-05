use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{plan_setup, run_setup, SetupError};

fn worktree(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-setup-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn touch(dir: &Path, name: &str, text: &str) {
    fs::write(dir.join(name), text).unwrap();
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

#[test]
fn the_install_step_is_chosen_by_lockfile_after_the_configured_steps() {
    let cases: [(&[&str], &[&str]); 7] = [
        (&["uv.lock"], &["uv", "sync", "--locked"]),
        (
            &["package.json", "pnpm-lock.yaml"],
            &["pnpm", "install", "--frozen-lockfile"],
        ),
        (
            &["package.json", "yarn.lock"],
            &["yarn", "install", "--frozen-lockfile"],
        ),
        (
            &["package.json", "bun.lock"],
            &["bun", "install", "--frozen-lockfile"],
        ),
        (
            &["package.json", "bun.lockb"],
            &["bun", "install", "--frozen-lockfile"],
        ),
        (&["package.json", "package-lock.json"], &["npm", "ci"]),
        // pnpm wins over a stale npm lockfile, as in the gate.
        (
            &["package.json", "pnpm-lock.yaml", "package-lock.json"],
            &["pnpm", "install", "--frozen-lockfile"],
        ),
    ];
    for (n, (files, install)) in cases.iter().enumerate() {
        let dir = worktree(&format!("lock-{n}"));
        for file in *files {
            touch(&dir, file, "");
        }
        let configured = vec![argv(&["mcpctl", "setup", "--mcp", "docs"])];
        let plan = plan_setup(&dir, &configured).unwrap();
        assert_eq!(
            plan.steps,
            vec![configured[0].clone(), argv(install)],
            "{files:?}"
        );
    }
}

#[test]
fn a_lockfile_without_its_manifest_or_no_lockfile_installs_nothing_and_says_why() {
    let dir = worktree("none");
    assert!(plan_setup(&dir, &[]).unwrap().steps.is_empty());

    // A node lockfile alone is not a project; a manifest alone has nothing to restore.
    touch(&dir, "yarn.lock", "");
    assert!(plan_setup(&dir, &[]).unwrap().steps.is_empty());
    fs::remove_file(dir.join("yarn.lock")).unwrap();
    touch(&dir, "package.json", "{}");
    let plan = plan_setup(&dir, &[]).unwrap();
    assert!(plan.steps.is_empty());
    assert!(
        plan.notes.iter().any(|note| note.contains("lockfile")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn a_configured_step_without_a_program_is_refused_before_anything_runs() {
    let dir = worktree("blank");
    for bad in [vec![], argv(&[""]), argv(&["  ", "x"])] {
        assert_eq!(
            plan_setup(&dir, &[bad]).unwrap_err(),
            SetupError::EmptyProgram
        );
    }
}

#[test]
fn steps_run_in_order_in_the_worktree_and_their_output_is_logged() {
    let dir = worktree("run");
    let log = dir.join("setup.log");
    let steps = vec![
        argv(&["sh", "-c", r#"pwd; echo first-""out; echo first-""err >&2"#]),
        argv(&["sh", "-c", r#"echo second-""out"#]),
    ];

    run_setup(&dir, &steps, &log).unwrap();

    let text = fs::read_to_string(&log).unwrap();
    let canonical = fs::canonicalize(&dir).unwrap();
    assert!(
        text.contains(canonical.to_str().unwrap()),
        "cwd is the worktree: {text}"
    );
    let positions: Vec<usize> = ["first-out\n", "first-err\n", "second-out\n"]
        .iter()
        .map(|needle| text.find(needle).expect(needle))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
}

#[test]
fn a_failing_step_stops_setup_with_its_exit_code_and_log_and_keeps_the_worktree() {
    let dir = worktree("fail");
    let log = dir.join("setup.log");
    let steps = vec![
        argv(&["sh", "-c", "echo ok"]),
        argv(&[
            "sh",
            "-c",
            r#"echo broke-""out; echo broke-""err >&2; exit 7"#,
        ]),
        argv(&["sh", "-c", "touch must-not-run"]),
    ];

    let failure = run_setup(&dir, &steps, &log).unwrap_err();

    assert_eq!(failure.step, steps[1]);
    assert_eq!(failure.exit_code, Some(7));
    assert_eq!(failure.log, log);
    let text = fs::read_to_string(&log).unwrap();
    assert!(
        text.contains("broke-out\n") && text.contains("broke-err\n"),
        "{text}"
    );
    assert!(
        !dir.join("must-not-run").exists(),
        "later steps are skipped"
    );
    assert!(dir.is_dir(), "the worktree is kept");
}

#[test]
fn a_program_that_cannot_be_started_is_a_failed_step_without_an_exit_code() {
    let dir = worktree("missing");
    let log = dir.join("setup.log");
    let steps = vec![argv(&["orq-no-such-program-anywhere", "sync"])];

    let failure = run_setup(&dir, &steps, &log).unwrap_err();

    assert_eq!(failure.exit_code, None);
    assert!(
        failure.reason.contains("orq-no-such-program-anywhere"),
        "{}",
        failure.reason
    );
    assert!(fs::read_to_string(&log)
        .unwrap()
        .contains("orq-no-such-program-anywhere"));
}

#[test]
fn arguments_are_never_written_to_the_log_or_the_failure_message() {
    let dir = worktree("secrets");
    let log = dir.join("setup.log");
    let steps = vec![argv(&["sh", "-c", "exit 3", "--token=hunter2-s3cret"])];

    let failure = run_setup(&dir, &steps, &log).unwrap_err();

    // The full argv stays available to the caller, not to logs and messages.
    assert_eq!(failure.step, steps[0]);
    assert!(!fs::read_to_string(&log).unwrap().contains("hunter2-s3cret"));
    assert!(!failure.to_string().contains("hunter2-s3cret"), "{failure}");
    assert!(!failure.reason.contains("hunter2-s3cret"));
    assert!(
        failure.to_string().contains("sh"),
        "the program is still named: {failure}"
    );
}
