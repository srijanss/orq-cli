use orq_orchestrator::RunStore;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

#[allow(dead_code)]
struct World {
    repo: GitFixture,
    remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
    provider: FakeProvider,
    path: std::ffi::OsString,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn world(tag: &str, agent: &FakeAgent, pi: &FakeAgent) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let base = orq_test_support::process_temp_dir().join(format!("orq-claude-to-pi-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);

    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let pi_stub = bin.join("pi");
    std::fs::write(&pi_stub, format!("#!/bin/sh\nexec '{}' \"$@\"\n", pi.path().display())).unwrap();
    std::fs::set_permissions(&pi_stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    World { repo, remote, state: base.join("state"), config: base.join("config"), ssh, base, provider, path }
}

fn orq(w: &World, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &w.path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

/// A gate that counts its runs and asks the run it belongs to to pause at its next boundary.
fn pausing_gate(w: &World) -> PathBuf {
    let count = w.base.join("gate-runs");
    let script = w.base.join("gate.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho run >> '{count}'\nXDG_STATE_HOME='{state}' '{orq}' run pause \"$(ls '{state}/orq/runs')\" >'{pause_log}' 2>&1\nexit 0\n",
            count = count.display(),
            orq = env!("CARGO_BIN_EXE_orq"),
            state = w.state.display(),
            pause_log = w.base.join("pause.log").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"{}\"]\nexplain_failures = false\n", script.display())).unwrap();
    count
}

fn agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build()
}



const PROFILES: &str = "[models.fast-claude]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[models.slow-claude]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.team-pi]\nharness = \"pi\"\nmodel = \"team/model\"\n";

fn resumed_run(store: &RunStore, first: &str) -> String {
    store.run_ids().unwrap().into_iter().find(|id| id != first).expect("a resumed run")
}

fn request(w: &World, run_id: &str, role: &str, profile: &str) {
    let out = orq(w, &["run", "switch", run_id, "--role", role, "--profile", profile, "--repo", w.repo.root().to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn after_a_switch_from_claude_to_pi_the_pi_session_continues_in_the_same_worktree_from_the_handoff() {
    let agent = agent();
    let pi = FakeAgent::builder().write_file("pi.txt", "from pi\n").commit("feat: pi continues the work").build();
    let w = world("claude-to-pi", &agent, &pi);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
    pausing_gate(&w);
    let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let paused = store.load(&run_id).unwrap();
    assert!(pi.last_run().is_none(), "pi must not run before the switch");

    request(&w, &run_id, "implementer", "team-pi");
    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    let run = pi.first_run().expect("a pi implement session ran after the switch");
    assert!(run.cwd.ends_with(paused.worktree.file_name().unwrap()), "same worktree: {} vs {}", run.cwd.display(), paused.worktree.display());
    let prompt = run.args.join(" ");
    assert!(prompt.contains("--model team/model"), "{prompt}");
    assert!(prompt.contains("Handoff from the previous session:"), "{prompt}");
    assert!(prompt.contains("COM-01"), "{prompt}");
    let log = git(w.repo.root(), &["log", "--format=%s", &paused.branch]);
    assert!(log.contains("feat: pi continues the work"), "the pi commit lands on the run's branch: {log}");
    let new_id = resumed_run(&store, &run_id);
    assert_eq!(store.load(&new_id).unwrap().branch, paused.branch);
}

#[test]
fn a_switch_to_another_profile_of_the_same_harness_keeps_the_finished_implement_session() {
    let agent = agent();
    let pi = FakeAgent::builder().write_file("pi.txt", "from pi\n").commit("feat: pi continues the work").build();
    let w = world("same-harness", &agent, &pi);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
    pausing_gate(&w);
    let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}", String::from_utf8_lossy(&first.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let before = agent.first_run().expect("the first implement session ran");

    request(&w, &run_id, "implementer", "fast-claude");
    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    // The finished session is not run again: the resumed run only inherits it.
    let new_id = resumed_run(&store, &run_id);
    let events = store.events(&new_id).unwrap();
    assert!(!events.iter().any(|e| e.kind == "stage.started" && e.stage.as_deref() == Some("implement_and_review")), "implement ran again: {:?}", events.iter().map(|e| format!("{} {:?}", e.kind, e.stage)).collect::<Vec<_>>());
    assert!(events.iter().any(|e| e.kind == "stage.completed" && e.stage.as_deref() == Some("implement_and_review")));
    assert!(before.args.join(" ").contains("/implement-and-review"));
    assert!(pi.last_run().is_none());
}

/// Like `pausing_gate`, but only the first run asks for a pause, so a resumed run is not paused again.
fn pausing_once_gate(w: &World) -> PathBuf {
    let count = pausing_gate(w);
    let script = w.base.join("gate.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho run >> '{count}'\n[ \"$(wc -l < '{count}')\" -eq 1 ] && XDG_STATE_HOME='{state}' '{orq}' run pause \"$(ls '{state}/orq/runs')\" >'{pause_log}' 2>&1\nexit 0\n",
            count = count.display(),
            orq = env!("CARGO_BIN_EXE_orq"),
            state = w.state.display(),
            pause_log = w.base.join("pause.log").display()
        ),
    )
    .unwrap();
    count
}

#[test]
fn after_a_switch_to_a_new_harness_the_new_work_is_gated_again_and_pushed() {
    let agent = agent();
    let pi = FakeAgent::builder().write_file("pi.txt", "from pi\n").commit("feat: pi continues the work").build();
    let w = world("gate-again", &agent, &pi);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
    let gate_runs = pausing_once_gate(&w);
    let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let paused = store.load(&run_id).unwrap();
    assert_eq!(std::fs::read_to_string(&gate_runs).unwrap().lines().count(), 1);

    request(&w, &run_id, "implementer", "team-pi");
    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    assert_eq!(std::fs::read_to_string(&gate_runs).unwrap().lines().count(), 2, "the pi work must pass the gate");
    let local = git(w.repo.root(), &["rev-parse", &paused.branch]);
    let remote = git(&w.remote, &["rev-parse", &paused.branch]);
    assert_eq!(remote.trim(), local.trim(), "the remote branch holds the pi commit");
    assert!(git(&w.remote, &["log", "--format=%s", &paused.branch]).contains("feat: pi continues the work"));
}

#[test]
fn a_switch_whose_new_work_fails_the_gate_is_not_pushed() {
    let agent = agent();
    let pi = FakeAgent::builder().write_file("pi.txt", "from pi\n").commit("feat: pi continues the work").build();
    let w = world("gate-fails", &agent, &pi);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
    let gate_runs = pausing_once_gate(&w);
    // The gate passes (and pauses) on its first run and fails on every later one.
    let script = w.base.join("gate.sh");
    let text = std::fs::read_to_string(&script).unwrap().replace("exit 0\n", "[ \"$(wc -l < '__COUNT__')\" -eq 1 ] || { echo 'gate: pi work rejected' >&2; exit 1; }\nexit 0\n").replace("__COUNT__", &gate_runs.display().to_string());
    std::fs::write(&script, text).unwrap();
    let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let paused = store.load(&run_id).unwrap();

    request(&w, &run_id, "implementer", "team-pi");
    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()]);

    assert_ne!(out.status.code(), Some(0), "a failing gate must fail the resumed run: {}", String::from_utf8_lossy(&out.stdout));
    // A failing gate triggers fix attempts, so it runs more than once after the switch.
    assert!(std::fs::read_to_string(&gate_runs).unwrap().lines().count() >= 2, "the pi work reached the gate");
    let on_remote = Command::new("git").arg("-C").arg(&w.remote).args(["log", "--format=%s", &paused.branch]).output().unwrap();
    assert!(!String::from_utf8_lossy(&on_remote.stdout).contains("feat: pi continues the work"), "rejected work must not be pushed");
}

#[test]
fn a_switch_after_the_push_gates_the_new_work_again_and_pushes_it_on_top() {
    let agent = agent();
    let pi = FakeAgent::builder().write_file("pi.txt", "from pi\n").commit("feat: pi continues the work").build();
    let w = world("switch-after-push", &agent, &pi);
    std::fs::write(w.repo.root().join(".orq.toml"), PROFILES).unwrap();
    // A gate that only counts its runs; the pause is asked for by the remote while it takes the first push.
    let count = w.base.join("gate-runs");
    let script = w.base.join("gate.sh");
    std::fs::write(&script, format!("#!/bin/sh\necho run >> '{}'\nexit 0\n", count.display())).unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"{}\"]\nexplain_failures = false\n", script.display())).unwrap();
    let hook = w.remote.join("hooks/pre-receive");
    let seen = w.base.join("pushes-seen");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho push >> '{seen}'\n[ \"$(wc -l < '{seen}')\" -eq 1 ] && XDG_STATE_HOME='{state}' '{orq}' run pause \"$(ls '{state}/orq/runs')\" >'{log}' 2>&1\nexit 0\n",
            seen = seen.display(),
            orq = env!("CARGO_BIN_EXE_orq"),
            state = w.state.display(),
            log = w.base.join("pause.log").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let paused = store.load(&run_id).unwrap();
    assert_eq!(std::fs::read_to_string(&seen).unwrap().lines().count(), 1, "the first run pushed once, then paused");
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
    assert!(!git(&w.remote, &["log", "--format=%s", &paused.branch]).contains("pi continues"), "precondition: pi has not run");

    request(&w, &run_id, "implementer", "team-pi");
    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2, "the pi work must pass the gate again");
    assert_eq!(std::fs::read_to_string(&seen).unwrap().lines().count(), 2, "and be pushed again");
    let local = git(w.repo.root(), &["rev-parse", &paused.branch]);
    assert_eq!(git(&w.remote, &["rev-parse", &paused.branch]).trim(), local.trim());
    assert!(git(&w.remote, &["log", "--format=%s", &paused.branch]).contains("feat: pi continues the work"));
}
