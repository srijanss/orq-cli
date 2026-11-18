//! A group run whose session completed every feature and then reported a problem must be finishable by
//! resuming it: the run remembers what its sessions completed, so a failed ending does not reopen that
//! work, a resume does not hand it back, and the group check counts it done.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
    github: FakeProvider,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A repository with a pushed `main`, pending COM-01 and COM-02, and runs that do not explain their failures.
fn world(tag: &str) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-group-keeps-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending")]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "explain_failures = false\n").unwrap();
    let github = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    World { repo, state: base.join("state"), config: base.join("config"), ssh, base, github }
}

/// A PATH whose `claude` sets every feature of the main ledger to `mark` (none when `mark` is empty),
/// as the agent's TDD server does, and then runs `agent`.
fn path_with(w: &World, agent: &FakeAgent, mark: &str, tag: &str) -> std::ffi::OsString {
    let bin = w.base.join(format!("bin-{tag}"));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let ledger = w.repo.root().join(".tdd-features.json");
    let marking = if mark.is_empty() {
        String::new()
    } else {
        format!("sed -i -E 's/\"status\": \"[a-z_]+\"/\"status\": \"{mark}\"/' '{}'\n", ledger.display())
    };
    std::fs::write(bin.join("claude"), format!("#!/bin/sh\n{marking}exec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, w.github.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

fn orq(w: &World, path: std::ffi::OsString, args: &[&str]) -> Output {
    orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .arg("--repo")
        .arg(w.repo.root())
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

/// `orq run` of the group COM-01,COM-02 with a `claude` that marks both `mark` and then runs `agent`.
fn run_group(w: &World, agent: &FakeAgent, mark: &str, tag: &str) -> Output {
    orq(w, path_with(w, agent, mark, tag), &["run", "--group", "COM-01,COM-02", "--remote", "origin", "--target", "main", "--no-ci-check"])
}

/// The status the repository's ledger records for `name`.
fn status_of(repo: &Path, name: &str) -> String {
    let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(repo.join(".tdd-features.json")).unwrap()).unwrap();
    let entry = ledger.as_array().unwrap().iter().find(|e| e["featureName"] == name).unwrap();
    entry["status"].as_str().unwrap().to_string()
}

/// An agent that commits its work and then reports a problem.
fn reporting_agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "both\n").commit("feat: both").exit_code(3).build()
}

#[test]
fn a_group_whose_session_completed_both_and_reported_a_problem_ends_failed_with_both_still_completed() {
    let w = world("failed-keeps");

    let output = run_group(&w, &reporting_agent(), "completed", "first");

    assert!(text(&output.stdout).contains("failed"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "completed", "{}", text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-02"), "completed", "{}", text(&output.stderr));
}

/// The payloads of every `kind` event any run recorded.
fn payloads_of(w: &World, kind: &str) -> Vec<serde_json::Value> {
    let store = orq_orchestrator::RunStore::new(&w.state.join("orq"));
    let mut payloads = Vec::new();
    for run in store.run_ids().unwrap() {
        payloads.extend(store.events(&run).unwrap().into_iter().filter(|e| e.kind == kind).map(|e| e.payload));
    }
    payloads
}

/// The id of the only run recorded so far.
fn only_run(w: &World) -> String {
    let mut runs: Vec<String> = std::fs::read_dir(w.state.join("orq/runs")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.remove(0)
}

/// The commit the run's worktree is on.
fn worktree_head(w: &World, run_id: &str) -> String {
    let record = orq_orchestrator::RunStore::new(&w.state.join("orq")).load(run_id).unwrap();
    git(&record.worktree, &["rev-parse", "HEAD"]).trim().to_string()
}

#[test]
fn the_run_records_the_features_its_session_completed_with_the_head_it_left() {
    let w = world("records");

    run_group(&w, &reporting_agent(), "completed", "first");

    let run_id = only_run(&w);
    let head = worktree_head(&w, &run_id);
    assert_eq!(payloads_of(&w, "ledger.session_completed"), vec![serde_json::json!({"features": ["COM-01", "COM-02"], "head": head})]);
}

/// Sets every feature of the repository's ledger to `status`, as a person or an earlier orq might have.
fn set_all(repo: &Path, status: &str) {
    let path = repo.join(".tdd-features.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let re_marked: String = text.split("\"status\": \"").enumerate().map(|(i, part)| if i == 0 { part.to_string() } else { format!("\"status\": \"{status}{}", &part[part.find('"').unwrap()..]) }).collect();
    std::fs::write(&path, re_marked).unwrap();
}

/// `orq run resume <run_id>` with a `claude` that marks the features `mark` (none when empty) and runs `agent`.
fn resume(w: &World, run_id: &str, agent: &FakeAgent, mark: &str, tag: &str) -> Output {
    orq(w, path_with(w, agent, mark, tag), &["run", "resume", run_id, "--yes", "--no-ci-check"])
}

#[test]
fn resuming_a_group_its_session_completed_hands_nothing_back_and_a_session_that_does_nothing_finishes_it() {
    let w = world("resume-finishes");
    run_group(&w, &reporting_agent(), "completed", "first");
    let run_id = only_run(&w);
    // As the run in the incident left them: reopened at its failed ending.
    set_all(w.repo.root(), "in_progress");

    let output = resume(&w, &run_id, &FakeAgent::builder().build(), "", "second");

    let stdout = text(&output.stdout);
    assert!(stdout.contains("state: review_ready"), "{stdout}\n{}", text(&output.stderr));
    assert_eq!(payloads_of(&w, "ledger.handed_back"), Vec::<serde_json::Value>::new());
    assert_eq!(status_of(w.repo.root(), "COM-01"), "completed");
    assert_eq!(status_of(w.repo.root(), "COM-02"), "completed");
}

#[test]
fn a_feature_completed_and_then_changed_by_a_later_commit_is_reopened() {
    let w = world("changed-later");
    run_group(&w, &reporting_agent(), "completed", "first");
    let run_id = only_run(&w);
    assert_eq!(status_of(w.repo.root(), "COM-01"), "completed");

    let changing = FakeAgent::builder().write_file("more.txt", "more\n").commit("fix: more").exit_code(3).build();
    let output = resume(&w, &run_id, &changing, "", "second");

    assert!(text(&output.stdout).contains("failed"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "in_progress", "{}", text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-02"), "in_progress");
}

#[test]
fn a_group_feature_its_session_set_abandoned_is_not_done_and_the_ending_says_so() {
    let w = world("abandoned");
    let agent = FakeAgent::builder().write_file("feature.txt", "both\n").commit("feat: both").build();

    let output = run_group(&w, &agent, "abandoned", "first");

    let stdout = text(&output.stdout);
    assert!(stdout.contains("failed"), "{stdout}\n{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("the implement session set COM-01 abandoned, which does not finish it"), "{stderr}");
}

/// `orq feature set-status --repo <repo> --id <id> --status <status>` with the world's state.
fn set_status(w: &World, id: &str, status: &str) -> Output {
    orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"))
        .args(["feature", "set-status", "--repo"])
        .arg(w.repo.root())
        .args(["--id", id, "--status", status])
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

#[test]
fn set_status_completed_is_allowed_for_a_feature_a_session_of_a_run_completed() {
    let w = world("set-completed");
    run_group(&w, &reporting_agent(), "completed", "first");
    // As the session in the incident left it.
    set_all(w.repo.root(), "abandoned");

    let output = set_status(&w, "COM-01", "completed");

    assert!(output.status.success(), "{}", text(&output.stderr));
    let entry: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(entry["name"], "COM-01");
    assert_eq!(entry["status"], "completed");
    assert_eq!(status_of(w.repo.root(), "COM-01"), "completed");
    assert_eq!(status_of(w.repo.root(), "COM-02"), "abandoned");
}

#[test]
fn a_group_session_is_told_never_to_set_its_features_abandoned() {
    let w = world("prompt");
    let agent = FakeAgent::builder().write_file("feature.txt", "both\n").commit("feat: both").build();

    run_group(&w, &agent, "completed", "first");

    let prompt = agent.first_run().unwrap().args.join(" ");
    assert!(prompt.contains("Never set any of these features to abandoned"), "{prompt}");
}

#[test]
fn set_status_completed_is_refused_with_the_reason_for_a_feature_no_run_completed() {
    let w = world("set-refused");
    let before = std::fs::read(w.repo.root().join(".tdd-features.json")).unwrap();

    let output = set_status(&w, "COM-01", "completed");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(stderr.contains("no run of this project recorded a session completing it"), "{stderr}");
    assert_eq!(std::fs::read(w.repo.root().join(".tdd-features.json")).unwrap(), before);
}
