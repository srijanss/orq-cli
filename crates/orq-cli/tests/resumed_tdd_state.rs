//! A resumed run's session is told when its worktree's TDD state still has the run's feature open, and
//! after a session orq puts back the declared files a TDD tool dropped from the ledger entry.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{FakeAgent, FakeProvider, GitFixture};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    _remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
    github: FakeProvider,
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ledger_text() -> String {
    let entries = vec![serde_json::json!({
        "featureName": "COM-01",
        "status": "pending",
        "dependsOn": [],
        "testFile": "tests/feature_test.rs",
        "targetFiles": ["src/feature.rs"],
    })];
    serde_json::to_string_pretty(&entries).unwrap() + "\n"
}

fn world(tag: &str) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-tdd-state-{}-{tag}", std::process::id()));
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
    std::fs::write(repo.root().join(".tdd-features.json"), ledger_text()).unwrap();
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "explain_failures = false\n").unwrap();
    let github = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    World { repo, _remote: remote, state: base.join("state"), config: base.join("config"), ssh, base, github }
}

fn path_with(w: &World, agent: &FakeAgent, script: &str) -> std::ffi::OsString {
    let bin = w.base.join("bin");
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("claude"), format!("#!/bin/sh\n{script}\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, w.github.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

fn orq(w: &World, path: &std::ffi::OsStr, args: &[&str]) -> Output {
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

fn run(w: &World, path: std::ffi::OsString) -> Output {
    orq(w, &path, &["run", "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
}

fn payloads_of(w: &World, kind: &str) -> Vec<serde_json::Value> {
    let store = orq_orchestrator::RunStore::new(&w.state.join("orq"));
    let mut payloads = Vec::new();
    for run in store.run_ids().unwrap() {
        payloads.extend(store.events(&run).unwrap().into_iter().filter(|e| e.kind == kind).map(|e| e.payload));
    }
    payloads
}

/// The resumed session's prompt names the phase and depth the first session left open.
#[test]
fn a_resumed_session_is_told_the_feature_is_still_open_in_the_tdd_state() {
    let w = world("open");
    let ledger_path = w.repo.root().join(".tdd-features.json").display().to_string();
    let prompt_file = w.base.join("prompt.txt");
    let open_state = r#"{"featureName":"fix-the-finding","stack":[{"testFile":"tests/feature_test.rs","targetFiles":[],"phase":"implement"},{"testFile":"tests/inner.rs","targetFiles":["src/feature.rs"],"phase":"verify_red"}],"phase":"verify_red"}"#;
    let script = format!(
        "case \"$*\" in\n*'This run continues'*) printf '%s' \"$*\" > '{}';;\n*) printf '%s' '{open_state}' > .tdd-state.json;;\nesac\nsed -i -E 's/\"status\": \"[a-z_]+\"/\"status\": \"in_progress\"/' '{ledger_path}'",
        prompt_file.display()
    );
    let failing = FakeAgent::builder().write_file("src/feature.rs", "half\n").commit("wip").exit_code(3).build();
    let failed = run(&w, path_with(&w, &failing, &script));
    assert!(text(&failed.stdout).contains("failed"), "{}\n{}", text(&failed.stdout), text(&failed.stderr));
    let run_id = orq_orchestrator::RunStore::new(&w.state.join("orq")).run_ids().unwrap().remove(0);

    let finishing = FakeAgent::builder().write_file("src/feature.rs", "done\n").commit("feat: add the feature").build();
    let resumed = orq(&w, &path_with(&w, &finishing, &script), &["run", "resume", &run_id, "--yes", "--no-ci-check"]);

    assert!(text(&resumed.stdout).contains("state: review_ready"), "{}\n{}", text(&resumed.stdout), text(&resumed.stderr));
    let prompt = std::fs::read_to_string(&prompt_file).unwrap();
    assert!(prompt.contains("The feature fix-the-finding is still open in the TDD state: phase verify_red, depth 2."), "the feature the state names, not the run's: {prompt}");
}

/// A session whose TDD tool leaves the entry without the files it declared gets them back, and adds
/// the files the session declared.
#[test]
fn a_session_that_drops_the_declared_target_files_gets_them_put_back_with_its_own_kept() {
    let w = world("restore");
    let after = w.base.join("ledger-after.json");
    let mut entries: serde_json::Value = serde_json::from_str(&ledger_text()).unwrap();
    entries[0]["status"] = "completed".into();
    entries[0]["targetFiles"] = serde_json::json!(["src/added.rs"]);
    std::fs::write(&after, entries.to_string()).unwrap();
    let once = w.base.join("copied");
    let script = format!("[ -e '{once}' ] || {{ cp '{}' '{}'; touch '{once}'; }}", after.display(), w.repo.root().join(".tdd-features.json").display(), once = once.display());
    let agent = FakeAgent::builder().write_file("src/feature.rs", "done\n").write_file("src/added.rs", "done\n").commit("feat: add the feature").build();

    let output = run(&w, path_with(&w, &agent, &script));

    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    let ledger: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(w.repo.root().join(".tdd-features.json")).unwrap()).unwrap();
    assert_eq!(ledger[0]["targetFiles"], serde_json::json!(["src/feature.rs", "src/added.rs"]));
    assert_eq!(payloads_of(&w, "ledger.target_files_restored"), vec![serde_json::json!({"feature": "COM-01", "restored": ["src/feature.rs"]})]);
}

/// A ledger a session leaves unreadable cannot get its files back: that is said, and no restoration is recorded.
#[test]
fn a_ledger_that_cannot_be_read_after_the_session_says_the_declared_files_were_not_put_back() {
    let w = world("unreadable");
    let script = format!("printf 'not json' > '{}'", w.repo.root().join(".tdd-features.json").display());
    let agent = FakeAgent::builder().write_file("src/feature.rs", "done\n").commit("feat: add the feature").build();

    let output = run(&w, path_with(&w, &agent, &script));

    assert!(text(&output.stderr).contains("cannot read the feature ledger to put back the files its features declared"), "{}", text(&output.stderr));
    assert_eq!(payloads_of(&w, "ledger.target_files_restored"), Vec::<serde_json::Value>::new());
}

/// A ledger a session deleted cannot get its files back: that is said, and nothing is recreated.
#[test]
fn a_ledger_deleted_by_the_session_says_the_declared_files_were_not_put_back() {
    let w = world("deleted");
    let script = format!("rm -f '{}'", w.repo.root().join(".tdd-features.json").display());
    let agent = FakeAgent::builder().write_file("src/feature.rs", "done\n").commit("feat: add the feature").build();

    let output = run(&w, path_with(&w, &agent, &script));

    assert!(text(&output.stderr).contains("the feature ledger is gone, so the files its features declared are not put back"), "{}", text(&output.stderr));
    assert_eq!(payloads_of(&w, "ledger.target_files_restored"), Vec::<serde_json::Value>::new());
}

/// A feature a session removed from the ledger is said, and the others still get their files back.
#[test]
fn a_feature_missing_from_the_ledger_after_the_session_is_said() {
    let w = world("missing-entry");
    let script = format!("printf '[]' > '{}'", w.repo.root().join(".tdd-features.json").display());
    let agent = FakeAgent::builder().write_file("src/feature.rs", "done\n").commit("feat: add the feature").build();

    let output = run(&w, path_with(&w, &agent, &script));

    assert!(text(&output.stderr).contains("COM-01 is no longer in the feature ledger, so the files it declared are not put back"), "{}", text(&output.stderr));
}

/// A session that keeps every declared file changes nothing and records nothing.
#[test]
fn a_session_that_keeps_the_declared_files_records_nothing() {
    let w = world("kept");
    let agent = FakeAgent::builder().write_file("src/feature.rs", "done\n").commit("feat: add the feature").build();

    let output = run(&w, path_with(&w, &agent, ""));

    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(payloads_of(&w, "ledger.target_files_restored"), Vec::<serde_json::Value>::new());
}
