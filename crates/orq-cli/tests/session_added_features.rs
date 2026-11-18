//! orq tells which ledger features a run's sessions added by comparing the ledger before and after
//! each session. The ledger is shared, so a person may add or approve features while a session runs;
//! only a new entry the session itself started (`in_progress` or `completed`, as the TDD workflow's
//! `init_feature` and `complete_feature` leave it) is the session's. The features credited in the
//! ending note are further limited to those the run's own session log names (see
//! run_credits_own_features.rs).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

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

/// A repository with a pushed `main`, a pending COM-01, and `run_toml` as its run settings.
fn world(tag: &str, run_toml: &str) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-session-added-{}-{tag}", std::process::id()));
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
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
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

/// A ledger file in the world's scratch space holding `entries`, to be copied over the main ledger.
fn ledger_file(w: &World, tag: &str, entries: &[(&str, &str)]) -> PathBuf {
    let dir = w.base.join(format!("ledger-{tag}"));
    std::fs::create_dir_all(&dir).unwrap();
    let entries: Vec<LedgerEntry> = entries.iter().map(|(name, status)| LedgerEntry::new(name, status)).collect();
    write_ledger(&dir, &entries)
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
    orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"))
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

/// The status the repository's ledger records for `name`.
fn status_of(w: &World, name: &str) -> String {
    let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(w.repo.root().join(".tdd-features.json")).unwrap()).unwrap();
    let entry = ledger.as_array().unwrap().iter().find(|e| e["featureName"] == name).unwrap();
    entry["status"].as_str().unwrap().to_string()
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

fn finishing_agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build()
}

/// A run whose implement session starts and finishes COM-02 while drafts, a pending entry and an
/// abandoned one appear in the ledger beside it.
fn run_with_others_added(tag: &str) -> (World, Output) {
    let w = world(tag, "explain_failures = false\n");
    let after = ledger_file(
        &w,
        "implement",
        &[
            ("COM-01", "in_progress"),
            ("COM-02", "completed"),
            ("DRAFT-01", "draft"),
            ("PEND-01", "pending"),
            ("GONE-01", "abandoned"),
        ],
    );
    let agent = finishing_agent();
    let ledger = w.repo.root().join(".tdd-features.json");
    // The session's TDD server logs the feature it starts in the worktree, as the run reads it.
    let log = r#"printf '%s\n' '{"event": "init_feature", "featureName": "COM-02"}' >> .tdd-session.log"#;
    let path = path_with(&w, &agent, &format!("{log}\ncp '{}' '{}'", after.display(), ledger.display()));
    let output = run(&w, path);
    (w, output)
}

#[test]
fn the_implement_session_is_credited_only_with_the_feature_it_started() {
    let (w, output) = run_with_others_added("implement");

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(payloads_of(&w, "ledger.extra_features"), vec![serde_json::json!({"features": ["COM-02"]})]);
    assert!(stderr.contains("the session also added: COM-02"), "{stderr}");
    assert!(!stderr.contains("DRAFT-01") && !stderr.contains("PEND-01") && !stderr.contains("GONE-01"), "{stderr}");
}

#[test]
fn entries_someone_else_added_during_the_implement_session_keep_their_status() {
    let (w, output) = run_with_others_added("implement-statuses");

    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(&w, "DRAFT-01"), "draft");
    assert_eq!(status_of(&w, "PEND-01"), "pending");
    assert_eq!(status_of(&w, "GONE-01"), "abandoned");
}

/// A run whose gate fails until its gate fix session commits `fixed.txt`; that session starts FIX-01
/// while a draft, DRAFT-02, is added beside it.
fn run_whose_fix_session_runs_beside_a_draft(tag: &str) -> (World, Output) {
    let w = world(tag, "gate = [\"sh\", \"-c\", \"test -e fixed.txt\"]\nexplain_failures = false\n");
    let after = ledger_file(&w, "fix", &[("COM-01", "completed"), ("FIX-01", "in_progress"), ("DRAFT-02", "draft")]);
    let agent = finishing_agent();
    let ledger = w.repo.root().join(".tdd-features.json");
    let script = format!(
        "case \"$*\" in *'local gate failed'*)\n  cp '{}' '{}'\n  echo fixed > fixed.txt\n  git add fixed.txt\n  exec git -c user.name=Fixer -c user.email=fixer@example.invalid commit -q -m fix\nesac",
        after.display(),
        ledger.display()
    );
    let path = path_with(&w, &agent, &script);
    let output = run(&w, path);
    (w, output)
}

#[test]
fn a_gate_fix_session_is_credited_only_with_the_feature_it_started() {
    let (w, output) = run_whose_fix_session_runs_beside_a_draft("gate-fix");

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let added: Vec<serde_json::Value> = payloads_of(&w, "fix.features_added").into_iter().map(|p| p["features"].clone()).collect();
    assert_eq!(added, vec![serde_json::json!(["FIX-01"])]);
    assert_eq!(status_of(&w, "FIX-01"), "completed", "the run closed the feature its fix session started");
    assert_eq!(status_of(&w, "DRAFT-02"), "draft", "a draft added during the session is not the run's");
}
