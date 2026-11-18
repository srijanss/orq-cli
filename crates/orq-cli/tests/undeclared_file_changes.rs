//! After each session stage orq compares the files the run's commits changed with the files its
//! features declare (their `targetFiles` and test files, and those of the features its sessions
//! started) and records any others as `session.undeclared_files`. Tests under `tests/`, lockfiles and
//! the web bundle rebuilt from a declared source are not counted. Nothing is blocked: the run goes on
//! and its ending note names the files.

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

/// A ledger whose COM-01 declares `src/feature.rs` and its test file, plus `extra` entries as given.
fn ledger(extra: &[serde_json::Value]) -> String {
    let mut entries = vec![serde_json::json!({
        "featureName": "COM-01",
        "status": "pending",
        "dependsOn": [],
        "testFile": "tests/feature_test.rs",
        "targetFiles": ["src/feature.rs"],
    })];
    entries.extend(extra.iter().cloned());
    serde_json::to_string_pretty(&entries).unwrap() + "\n"
}

/// A repository with a pushed `main` holding an existing test and lockfile, the ledger, and `run_toml`.
fn world(tag: &str, run_toml: &str, ledger_text: &str) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-undeclared-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let repo = GitFixture::new();
    std::fs::create_dir_all(repo.root().join("crates/app/tests")).unwrap();
    std::fs::write(repo.root().join("crates/app/tests/old.rs"), "old\n").unwrap();
    std::fs::write(repo.root().join("Cargo.lock"), "old\n").unwrap();
    git(repo.root(), &["add", "."]);
    git(repo.root(), &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-q", "-m", "an existing test and lockfile"]);
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::write(repo.root().join(".tdd-features.json"), ledger_text).unwrap();
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), run_toml).unwrap();
    World { repo, _remote: remote, state: base.join("state"), config: base.join("config"), ssh, base, github: github() }
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

/// A PATH whose `claude` runs `script` (a shell fragment) before running `agent`.
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

/// `orq run` of COM-01 with `path`.
fn run(w: &World, path: std::ffi::OsString) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--repo"])
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

/// The payloads of every `kind` event any run recorded.
fn payloads_of(w: &World, kind: &str) -> Vec<serde_json::Value> {
    let store = orq_orchestrator::RunStore::new(&w.state.join("orq"));
    let mut payloads = Vec::new();
    for run in store.run_ids().unwrap() {
        payloads.extend(store.events(&run).unwrap().into_iter().filter(|e| e.kind == kind).map(|e| e.payload));
    }
    payloads
}

/// An implementer that writes each of `files` and commits them.
fn agent_writing(files: &[&str]) -> FakeAgent {
    let mut builder = FakeAgent::builder();
    for file in files {
        builder = builder.write_file(file, "changed\n");
    }
    builder.commit("feat: add the feature").build()
}

#[test]
fn a_change_outside_the_features_files_is_recorded_and_named_in_the_ending_note() {
    let w = world("outside", "explain_failures = false\n", &ledger(&[]));
    let agent = agent_writing(&["src/feature.rs", "src/other.rs"]);
    let output = run(&w, path_with(&w, &agent, ""));

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(
        payloads_of(&w, "session.undeclared_files"),
        vec![serde_json::json!({"files": ["src/other.rs"], "stage": "implement_and_review"})]
    );
    assert!(stderr.contains("changed outside its features' files: src/other.rs (implement)"), "{stderr}");
}

#[test]
fn changes_only_to_declared_files_tests_lockfiles_and_the_rebuilt_bundle_record_nothing() {
    let web = serde_json::json!({
        "featureName": "WEB-01",
        "status": "pending",
        "dependsOn": [],
        "testFile": "-p orq-remote --test web_run_steps",
        "targetFiles": ["crates/orq-remote/web-src/src/main.jsx"],
    });
    let w = world("declared", "explain_failures = false\n", &ledger(&[web]));
    // The session starts WEB-01, so its files are the run's too.
    let log = r#"printf '%s\n' '{"event": "init_feature", "featureName": "WEB-01"}' >> .tdd-session.log"#;
    let ledger_after = w.base.join("ledger-after.json");
    let mut after: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(w.repo.root().join(".tdd-features.json")).unwrap()).unwrap();
    after[0]["status"] = "in_progress".into();
    after[1]["status"] = "completed".into();
    std::fs::write(&ledger_after, after.to_string()).unwrap();
    let script = format!("{log}\ncp '{}' '{}'", ledger_after.display(), w.repo.root().join(".tdd-features.json").display());
    let agent = agent_writing(&[
        "src/feature.rs",
        "tests/feature_test.rs",
        "crates/app/tests/old.rs",
        "Cargo.lock",
        "crates/orq-remote/web-src/src/main.jsx",
        "crates/orq-remote/web/app.react.js",
        "crates/orq-remote/web/app.react.js.stamp",
        "crates/orq-remote/web/sw.js",
    ]);
    let output = run(&w, path_with(&w, &agent, &script));

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(payloads_of(&w, "session.undeclared_files"), Vec::<serde_json::Value>::new());
    assert!(!stderr.contains("changed outside its features' files"), "{stderr}");
}

#[test]
fn a_gate_fix_sessions_change_outside_the_features_files_is_recorded_under_its_stage() {
    let w = world("gate-fix", "gate = [\"sh\", \"-c\", \"test -e fixed.txt\"]\nexplain_failures = false\n", &ledger(&[]));
    let agent = agent_writing(&["src/feature.rs"]);
    let script = "case \"$*\" in *'local gate failed'*)\n  echo fixed > fixed.txt\n  git add fixed.txt\n  exec git -c user.name=Fixer -c user.email=fixer@example.invalid commit -q -m fix\nesac";
    let output = run(&w, path_with(&w, &agent, script));

    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(payloads_of(&w, "session.undeclared_files"), vec![serde_json::json!({"files": ["fixed.txt"], "stage": "gate_fix"})]);
    assert!(stderr.contains("changed outside its features' files: fixed.txt (gate fix)"), "{stderr}");
}
