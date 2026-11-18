//! A run is credited only with the features its own sessions started. The ledger is shared, so
//! another run's session may add and complete features in the same window; the run's worktree keeps
//! its own TDD session log (`.tdd-session.log`), whose `init_feature` events name what its sessions
//! started. The ledger is read only to keep a started feature that was then abandoned out.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// A repository with a pushed `main`, pending COM-01 and COM-02, and `run_toml` as its run settings.
fn world(tag: &str, run_toml: &str) -> World {
    world_with_log(tag, run_toml, None)
}

/// As `world`, with `tracked_log` committed on `main` as a session log the project keeps in git.
fn world_with_log(tag: &str, run_toml: &str, tracked_log: Option<&str>) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-credits-own-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let repo = GitFixture::new();
    // As in a real project, the TDD server's session log is ignored, not committed.
    std::fs::write(repo.root().join(".gitignore"), ".tdd-session.log\n").unwrap();
    git(repo.root(), &["add", ".gitignore"]);
    git(repo.root(), &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-q", "-m", "ignore the session log"]);
    if let Some(log) = tracked_log {
        std::fs::write(repo.root().join(".tdd-session.log"), log).unwrap();
        git(repo.root(), &["add", "-f", ".tdd-session.log"]);
        git(repo.root(), &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-q", "-m", "keep the session log"]);
    }
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending")]);
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

/// A shell line appending an `init_feature` event for `name` to the session log of the worktree the
/// session runs in, the way the outside-in-tdd server writes it.
fn log_init(name: &str) -> String {
    format!("printf '%s\\n' '{{\"ts\": \"2026-11-18T00:00:00+00:00\", \"event\": \"init_feature\", \"featureName\": \"{name}\", \"depth\": 1}}' >> .tdd-session.log")
}

/// A PATH (its own `bin-<tag>`) whose `claude` runs `script` (a shell fragment) before running `agent`.
fn path_with(w: &World, tag: &str, agent: &FakeAgent, script: &str) -> std::ffi::OsString {
    let bin = w.base.join(format!("bin-{tag}"));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("claude"), format!("#!/bin/sh\n{script}\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, w.github.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

/// `orq` with `args` on the world's repository and `path`.
fn orq(w: &World, path: std::ffi::OsString, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command
        .args(args)
        .arg("--repo")
        .arg(w.repo.root())
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH");
    command
}

/// `orq run` of `feature` with `path`, started and not waited for.
fn start_run(w: &World, feature: &str, path: std::ffi::OsString) -> std::process::Child {
    orq(w, path, &["run", "--feature", feature, "--remote", "origin", "--target", "main", "--no-ci-check"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

/// The payloads of each run's `kind` events, keyed by the run's feature.
fn payloads_by_feature(w: &World, kind: &str) -> std::collections::BTreeMap<String, Vec<serde_json::Value>> {
    let store = orq_orchestrator::RunStore::new(&w.state.join("orq"));
    let mut by_feature = std::collections::BTreeMap::new();
    for run in store.run_ids().unwrap() {
        let feature = store.load(&run).unwrap().feature_id;
        let payloads: Vec<serde_json::Value> = store.events(&run).unwrap().into_iter().filter(|e| e.kind == kind).map(|e| e.payload).collect();
        by_feature.entry(feature).or_insert_with(Vec::new).extend(payloads);
    }
    by_feature
}

fn finishing_agent(file: &str) -> FakeAgent {
    FakeAgent::builder().write_file(file, "done\n").commit("feat: add the feature").build()
}

/// A session script that starts `own` in its worktree's log, puts `ledger` (where every session's
/// extra feature is already completed) over the shared ledger, says it is in its implement stage
/// with `ready`, and waits until `other` says the same, so both sessions run at once.
fn side_by_side(w: &World, own: &str, ledger: &Path, ready: &str, other: &str) -> String {
    let shared = w.repo.root().join(".tdd-features.json");
    let ready = w.base.join(ready);
    let other = w.base.join(other);
    format!(
        "{log}\ncp '{ledger}' '{shared}'\ntouch '{ready}'\ni=0\nwhile [ ! -e '{other}' ] && [ $i -lt 300 ]; do sleep 0.1; i=$((i+1)); done",
        log = log_init(own),
        ledger = ledger.display(),
        shared = shared.display(),
        ready = ready.display(),
        other = other.display()
    )
}

#[test]
fn two_runs_side_by_side_are_each_credited_only_with_the_feature_their_own_session_started() {
    let w = world("side-by-side", "explain_failures = false\n");
    let ledger = ledger_file(
        &w,
        "both",
        &[("COM-01", "in_progress"), ("COM-02", "in_progress"), ("a-extra", "completed"), ("b-extra", "completed")],
    );
    let agent_a = finishing_agent("a.txt");
    let agent_b = finishing_agent("b.txt");
    let path_a = path_with(&w, "a", &agent_a, &side_by_side(&w, "a-extra", &ledger, "a.ready", "b.ready"));
    let path_b = path_with(&w, "b", &agent_b, &side_by_side(&w, "b-extra", &ledger, "b.ready", "a.ready"));

    let run_a = start_run(&w, "COM-01", path_a);
    let run_b = start_run(&w, "COM-02", path_b);
    let output_a = run_a.wait_with_output().unwrap();
    let output_b = run_b.wait_with_output().unwrap();

    let (stderr_a, stderr_b) = (text(&output_a.stderr), text(&output_b.stderr));
    assert!(text(&output_a.stdout).contains("state: review_ready"), "{}\n{stderr_a}", text(&output_a.stdout));
    assert!(text(&output_b.stdout).contains("state: review_ready"), "{}\n{stderr_b}", text(&output_b.stdout));
    let credited = payloads_by_feature(&w, "ledger.extra_features");
    assert_eq!(credited["COM-01"], vec![serde_json::json!({"features": ["a-extra"]})]);
    assert_eq!(credited["COM-02"], vec![serde_json::json!({"features": ["b-extra"]})]);
    assert!(stderr_a.contains("the session also added: a-extra") && !stderr_a.contains("b-extra"), "{stderr_a}");
    assert!(stderr_b.contains("the session also added: b-extra") && !stderr_b.contains("a-extra"), "{stderr_b}");
}

#[test]
fn a_worktree_without_a_session_log_credits_nothing_and_says_so_once() {
    let w = world("no-log", "explain_failures = false\n");
    let ledger = ledger_file(&w, "no-log", &[("COM-01", "in_progress"), ("COM-02", "pending"), ("c-extra", "completed")]);
    let agent = finishing_agent("c.txt");
    let shared = w.repo.root().join(".tdd-features.json");
    let path = path_with(&w, "no-log", &agent, &format!("cp '{}' '{}'", ledger.display(), shared.display()));

    let output = start_run(&w, "COM-01", path).wait_with_output().unwrap();

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(payloads_by_feature(&w, "ledger.extra_features")["COM-01"], Vec::<serde_json::Value>::new());
    assert!(!stderr.contains("the session also added"), "{stderr}");
    let said: Vec<&str> = stderr.lines().filter(|l| l.contains("cannot tell which features") && l.contains(".tdd-session.log")).collect();
    assert_eq!(said.len(), 1, "{stderr}");
}

/// A run of COM-01 whose gate fails until its gate fix session commits `fixed.txt`; that session
/// starts `gfix-extra` in the worktree's log and leaves it `in_progress` in the ledger.
#[test]
fn a_gate_fix_session_is_credited_with_the_feature_it_started() {
    let w = world("gate-fix", "gate = [\"sh\", \"-c\", \"test -e fixed.txt\"]\nexplain_failures = false\n");
    let ledger = ledger_file(&w, "gate-fix", &[("COM-01", "completed"), ("COM-02", "pending"), ("gfix-extra", "in_progress")]);
    let shared = w.repo.root().join(".tdd-features.json");
    let script = format!(
        "case \"$*\" in *'local gate failed'*)\n  {log}\n  cp '{ledger}' '{shared}'\n  echo fixed > fixed.txt\n  git add fixed.txt\n  exec git -c user.name=Fixer -c user.email=fixer@example.invalid commit -q -m fix\nesac\ntouch .tdd-session.log",
        log = log_init("gfix-extra"),
        ledger = ledger.display(),
        shared = shared.display()
    );
    let agent = finishing_agent("g.txt");

    let output = start_run(&w, "COM-01", path_with(&w, "gate-fix", &agent, &script)).wait_with_output().unwrap();

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(payloads_by_feature(&w, "ledger.extra_features")["COM-01"], vec![serde_json::json!({"features": ["gfix-extra"]})]);
    assert!(stderr.contains("the session also added: gfix-extra"), "{stderr}");
}

#[test]
fn a_feature_the_session_started_and_abandoned_is_not_credited() {
    let w = world("abandoned", "explain_failures = false\n");
    let ledger = ledger_file(&w, "abandoned", &[("COM-01", "in_progress"), ("COM-02", "pending"), ("gone-extra", "abandoned")]);
    let shared = w.repo.root().join(".tdd-features.json");
    let script = format!("{}\ncp '{}' '{}'", log_init("gone-extra"), ledger.display(), shared.display());
    let agent = finishing_agent("d.txt");

    let output = start_run(&w, "COM-01", path_with(&w, "abandoned", &agent, &script)).wait_with_output().unwrap();

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(payloads_by_feature(&w, "ledger.extra_features")["COM-01"], Vec::<serde_json::Value>::new());
    assert!(!stderr.contains("the session also added"), "{stderr}");
}

#[test]
fn a_feature_a_person_adds_as_completed_during_the_session_is_not_credited() {
    let w = world("person", "explain_failures = false\n");
    let ledger = ledger_file(&w, "person", &[("COM-01", "in_progress"), ("COM-02", "pending"), ("own-extra", "completed"), ("by-hand", "completed")]);
    let shared = w.repo.root().join(".tdd-features.json");
    let script = format!("{}\ncp '{}' '{}'", log_init("own-extra"), ledger.display(), shared.display());
    let agent = finishing_agent("e.txt");

    let output = start_run(&w, "COM-01", path_with(&w, "person", &agent, &script)).wait_with_output().unwrap();

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(payloads_by_feature(&w, "ledger.extra_features")["COM-01"], vec![serde_json::json!({"features": ["own-extra"]})]);
    assert!(!stderr.contains("by-hand"), "{stderr}");
}

/// The id of the only run recorded.
fn only_run(w: &World) -> String {
    let mut runs = orq_orchestrator::RunStore::new(&w.state.join("orq")).run_ids().unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.remove(0)
}

#[test]
fn a_resumed_run_credits_what_its_first_session_started_once() {
    let w = world("resumed", "explain_failures = false\n");
    let ledger = ledger_file(&w, "resumed", &[("COM-01", "in_progress"), ("COM-02", "pending"), ("r-extra", "completed")]);
    let shared = w.repo.root().join(".tdd-features.json");
    let script = format!("{}\ncp '{}' '{}'", log_init("r-extra"), ledger.display(), shared.display());
    let failing = FakeAgent::builder().write_file("r.txt", "half\n").commit("wip").exit_code(3).build();
    let first = start_run(&w, "COM-01", path_with(&w, "first", &failing, &script)).wait_with_output().unwrap();
    assert!(text(&first.stdout).contains("failed"), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let first_run = only_run(&w);

    let agent = finishing_agent("r2.txt");
    let output = orq(&w, path_with(&w, "second", &agent, &script), &["run", "resume", &first_run, "--yes", "--no-ci-check"])
        .output()
        .unwrap();

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    let store = orq_orchestrator::RunStore::new(&w.state.join("orq"));
    let resumed = store.run_ids().unwrap().into_iter().find(|run| *run != first_run).unwrap();
    let credited: Vec<serde_json::Value> =
        store.events(&resumed).unwrap().into_iter().filter(|e| e.kind == "ledger.extra_features").map(|e| e.payload).collect();
    assert_eq!(credited, vec![serde_json::json!({"features": ["r-extra"]})]);
    assert!(stderr.contains("the session also added: r-extra") && !stderr.contains("r-extra, r-extra"), "{stderr}");
}

#[test]
fn features_a_session_log_committed_at_the_base_names_are_not_credited_to_the_run() {
    let history = "{\"event\": \"init_feature\", \"featureName\": \"old-extra\"}\n";
    let w = world_with_log("tracked-log", "explain_failures = false\n", Some(history));
    let ledger = ledger_file(&w, "tracked-log", &[("COM-01", "in_progress"), ("COM-02", "pending"), ("old-extra", "completed"), ("new-extra", "completed")]);
    let shared = w.repo.root().join(".tdd-features.json");
    let script = format!("{}\ncp '{}' '{}'", log_init("new-extra"), ledger.display(), shared.display());
    let agent = finishing_agent("t.txt");

    let output = start_run(&w, "COM-01", path_with(&w, "tracked-log", &agent, &script)).wait_with_output().unwrap();

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(payloads_by_feature(&w, "ledger.extra_features")["COM-01"], vec![serde_json::json!({"features": ["new-extra"]})]);
}

#[test]
fn a_session_log_git_cannot_read_at_the_base_credits_nothing_and_says_so() {
    let history = "{\"event\": \"init_feature\", \"featureName\": \"old-extra\"}\n";
    let w = world_with_log("unreadable-log", "explain_failures = false\n", Some(history));
    let ledger = ledger_file(&w, "unreadable-log", &[("COM-01", "in_progress"), ("COM-02", "pending"), ("old-extra", "completed"), ("new-extra", "completed")]);
    let shared = w.repo.root().join(".tdd-features.json");
    // The base's copy of the log is lost from the object store, so git cannot say what it held.
    let lose_base_log = "b=$(git rev-parse HEAD:.tdd-session.log) && rm -f \"$(git rev-parse --git-common-dir)/objects/$(printf %s \"$b\" | cut -c1-2)/$(printf %s \"$b\" | cut -c3-)\"";
    let script = format!("{lose_base_log}\n{}\ncp '{}' '{}'", log_init("new-extra"), ledger.display(), shared.display());
    let agent = finishing_agent("t.txt");

    let output = start_run(&w, "COM-01", path_with(&w, "unreadable-log", &agent, &script)).wait_with_output().unwrap();

    let stderr = text(&output.stderr);
    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{stderr}", text(&output.stdout));
    assert_eq!(payloads_by_feature(&w, "ledger.extra_features")["COM-01"], Vec::<serde_json::Value>::new(), "{stderr}");
    assert!(stderr.contains("cannot tell which features the implement session started"), "{stderr}");
}
