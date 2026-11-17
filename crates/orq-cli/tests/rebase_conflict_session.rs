//! A run whose rebase stops on a conflict lets a session of its implementer resolve it, before the
//! gate and at a local merge, instead of stopping blocked for a person.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::RunStore;
use orq_test_support::{write_ledger, FakeAgent, GitFixture, LedgerEntry};

struct World {
    repo: GitFixture,
    remote: PathBuf,
    home: PathBuf,
    ssh: PathBuf,
    path: std::ffi::OsString,
    agent: FakeAgent,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// The words the conflict-fix session's task starts with; the fake harness knows it by them.
const FIX_TASK: &str = "The rebase of this run's branch onto";

/// Resolves the conflict on feature.txt and finishes the rebase.
const RESOLVE: &str = "printf 'merged\\n' > feature.txt && git add feature.txt && GIT_EDITOR=true git rebase --continue";

fn world(tag: &str) -> World {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let home = orq_test_support::process_temp_dir().join(format!("orq-rebase-session-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let ssh = home.join("ssh");
    executable(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n");
    let mut dirs = vec![home.join("bin")];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "gate = [\"true\"]\n").unwrap();
    World { repo, remote, home, ssh, path, agent }
}

/// The fake harness: the implement session commits feature.txt after `on_implement` ran; a
/// conflict-fix session runs `on_fix` (sh) in the worktree and leaves its task in `fix-task.txt`.
fn harness(w: &World, on_implement: &str, on_fix: &str) {
    let task = w.home.join("fix-task.txt");
    executable(
        &w.home.join("bin/claude"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n*'local gate failed'*) exit 0;;\n*\"{FIX_TASK}\"*) printf '%s' \"$*\" > '{task}'; {on_fix}; exit 0;;\nesac\n{on_implement}\nexec '{agent}' \"$@\"\n",
            task = task.display(),
            agent = w.agent.path().display(),
        ),
    );
}

/// Shell that, once, pushes a commit giving feature.txt other contents to the remote's main.
fn remote_main_takes_feature(w: &World) -> String {
    let done = w.home.join("moved");
    let clone = w.home.join("other");
    format!(
        "[ -f '{done}' ] || {{ touch '{done}'; git clone -q -b main '{remote}' '{clone}' && printf 'theirs\\n' > '{clone}/feature.txt' && git -C '{clone}' add feature.txt && git -C '{clone}' -c user.name=Someone -c user.email=someone@example.invalid commit -q -m 'other: feature.txt' && git -C '{clone}' push -q origin HEAD:main; }}",
        done = done.display(),
        remote = w.remote.display(),
        clone = clone.display(),
    )
}

fn orq(w: &World, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_CONFIG_HOME", w.home.join("config"))
        .env("XDG_DATA_HOME", w.home.join("data"))
        .env("XDG_STATE_HOME", w.home.join("state"))
        .env("XDG_CACHE_HOME", w.home.join("cache"))
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &w.path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn run(w: &World, extra: &[&str]) -> Output {
    let repo = w.repo.root().to_str().unwrap().to_string();
    let mut args = vec!["run", "--repo", &repo, "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--delivery", "merge-local"];
    args.extend_from_slice(extra);
    orq(w, &args)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn store(w: &World) -> RunStore {
    RunStore::new(&w.home.join("state/orq"))
}

fn only_run(w: &World) -> String {
    let ids = store(w).run_ids().unwrap();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids[0].clone()
}

fn events(w: &World, id: &str) -> Vec<orq_orchestrator::Event> {
    store(w).events(id).unwrap()
}

#[test]
fn a_conflict_before_the_gate_is_resolved_by_a_session_and_the_run_goes_on_to_the_gate_and_merges() {
    let w = world("before-gate");
    harness(&w, &remote_main_takes_feature(&w), RESOLVE);

    let out = run(&w, &[]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));
    let id = only_run(&w);
    let events = events(&w, &id);
    let conflict = events.iter().find(|e| e.kind == "rebase.conflict").expect("a rebase.conflict event");
    assert_eq!(conflict.payload["where"], "before_gate");
    assert_eq!(conflict.payload["files"], serde_json::json!(["feature.txt"]));
    let remote_main = git(&w.remote, &["rev-parse", "main"]);
    assert_eq!(conflict.payload["onto"], remote_main.as_str());
    let at = |pred: &dyn Fn(&orq_orchestrator::Event) -> bool| events.iter().position(|e| pred(e)).unwrap_or(usize::MAX);
    let conflict_at = at(&|e| e.kind == "rebase.conflict");
    let fix_at = at(&|e| e.kind == "stage.started" && e.stage.as_deref() == Some("rebase_fix"));
    let resolved_at = at(&|e| e.kind == "rebase.completed" && e.payload["resolved_by"] == "session");
    let gate_at = at(&|e| e.kind == "stage.started" && e.stage.as_deref() == Some("verify"));
    assert!(conflict_at < fix_at && fix_at < resolved_at && resolved_at < gate_at, "{:?}", events.iter().map(|e| (&e.kind, &e.stage)).collect::<Vec<_>>());
    let main = git(w.repo.root(), &["rev-parse", "refs/heads/main"]);
    assert_eq!(git(w.repo.root(), &["show", &format!("{main}:feature.txt")]), "merged", "main has the resolved work");
    assert_eq!(git(w.repo.root(), &["rev-parse", &format!("{main}~1")]), remote_main, "the run's work sits on the moved base");
}

/// Makes the gate, once, commit feature.txt with other contents on the repository's main, so the
/// merge finds main moved and the rebase onto it conflicts; every gate run is counted in `gates`.
fn gate_moves_local_main(w: &World) -> PathBuf {
    let gates = w.home.join("gates");
    let done = w.home.join("local-moved");
    let root = w.repo.root().display().to_string();
    let script = format!(
        "echo run >> '{gates}'; [ -f '{done}' ] && exit 0; touch '{done}'; printf 'theirs\\n' > '{root}/feature.txt' && git -C '{root}' add feature.txt && git -C '{root}' -c user.name=Someone -c user.email=someone@example.invalid commit -q -m 'local: feature.txt'",
        gates = gates.display(),
        done = done.display(),
    );
    let toml = format!("gate = [\"sh\", \"-c\", {}]\n", toml_string(&script));
    std::fs::write(w.repo.root().join(".orq/run.toml"), toml).unwrap();
    gates
}

fn toml_string(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

#[test]
fn a_conflict_at_the_merge_is_resolved_by_a_session_and_the_gate_runs_again_before_main_is_fast_forwarded() {
    let w = world("at-merge");
    harness(&w, "", RESOLVE);
    let gates = gate_moves_local_main(&w);

    let out = run(&w, &[]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));
    let id = only_run(&w);
    let events = events(&w, &id);
    let conflict = events.iter().find(|e| e.kind == "rebase.conflict").expect("a rebase.conflict event");
    assert_eq!(conflict.payload["where"], "at_merge");
    assert!(events.iter().any(|e| e.kind == "rebase.completed" && e.payload["resolved_by"] == "session"));
    assert_eq!(std::fs::read_to_string(&gates).unwrap().lines().count(), 2, "the gate checks the resolved branch again");
    let main = git(w.repo.root(), &["rev-parse", "refs/heads/main"]);
    assert_eq!(git(w.repo.root(), &["show", &format!("{main}:feature.txt")]), "merged");
    assert_eq!(git(w.repo.root(), &["log", "-1", "--format=%s", &format!("{main}~1")]), "local: feature.txt", "the run's work sits on the moved main");
}

#[test]
fn a_session_that_aborts_the_rebase_leaves_the_run_blocked_before_the_gate_with_the_task_it_was_given() {
    let w = world("aborted");
    harness(&w, &remote_main_takes_feature(&w), "git rebase --abort");
    std::fs::write(
        w.repo.root().join(".orq/run.toml"),
        "gate = [\"true\"]\n[rebase]\ngenerated = [\"web/app.js\"]\nrebuild = [\"node\", \"build.mjs\"]\n",
    )
    .unwrap();

    let out = run(&w, &[]);

    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("ended the rebase onto main without completing it"), "{}", text(&out.stderr));
    let id = only_run(&w);
    let events = events(&w, &id);
    assert!(!events.iter().any(|e| e.kind == "stage.started" && e.stage.as_deref() == Some("verify")), "no gate on an unresolved branch");
    let task = std::fs::read_to_string(w.home.join("fix-task.txt")).expect("the session was started");
    assert!(task.contains("feature.txt"), "{task}");
    assert!(task.contains("other: feature.txt"), "main's commit: {task}");
    assert!(task.contains("feat: add the feature"), "the run's commit: {task}");
    assert!(task.contains("`web/app.js`") && task.contains("`node build.mjs`"), "the [rebase] settings: {task}");
}

#[test]
fn a_run_with_rebasing_off_never_starts_a_conflict_session() {
    let w = world("rebase-off");
    harness(&w, &remote_main_takes_feature(&w), RESOLVE);

    let out = run(&w, &["--no-rebase"]);

    assert!(!w.home.join("fix-task.txt").exists(), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let id = only_run(&w);
    assert!(!events(&w, &id).iter().any(|e| e.kind == "rebase.conflict"));
}
