//! Stopping a run ends what the run started. Gate steps and sessions run in process groups of their
//! own, so ending only the process that owns the run used to leave them running.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orq_orchestrator::RunStore;
use orq_test_support::{write_ledger, FakeAgent, GitFixture, LedgerEntry};

const STARTS_A_CHILD: &str = "sleep 300 & echo $! > '{pidfile}'; wait";

struct World {
    repo: GitFixture,
    _remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    bin: PathBuf,
    pidfile: PathBuf,
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

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-stop-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = root.join("ssh");
    executable(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n");
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let pidfile = root.join("child.pid");
    World { repo, _remote: remote, state: root.join("state"), config: root.join("config"), ssh, bin: root.join("bin"), pidfile }
}

fn orq(w: &World) -> Command {
    let mut dirs = vec![w.bin.clone()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH");
    command
}

fn start_run(w: &World) -> Child {
    orq(w)
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// The process the gate step or the session wrote down, once it has.
fn started_child(w: &World, run: &mut Child) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(pid) = std::fs::read_to_string(&w.pidfile).ok().and_then(|text| text.trim().parse().ok()) {
            return pid;
        }
        if let Some(status) = run.try_wait().unwrap() {
            panic!("the run ended ({status}) before it started the child");
        }
        assert!(Instant::now() < deadline, "the run never started the child");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Executing: there is such a process and it is not a zombie nothing has reaped yet.
fn alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(stat) => {
            let state = stat.rfind(')').and_then(|at| stat[at + 1..].split_whitespace().next().map(str::to_string));
            !matches!(state.as_deref(), Some("Z" | "X" | "x"))
        }
    }
}

fn gone_soon(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Stops the only run of the world and says whether the child went with it.
fn stop_and_check(w: &World, mut run: Child) {
    let child = started_child(w, &mut run);
    assert!(alive(child), "the child is running before the stop");
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().pop().expect("the run is recorded");

    let stopped = orq(w).args(["run", "stop", &run_id]).output().unwrap();

    let gone = gone_soon(child);
    // Never leave it behind for the rest of the test run. Safety: plain system call.
    if !gone {
        unsafe { libc::kill(child, libc::SIGKILL) };
    }
    let _ = run.kill();
    let _ = run.wait();
    assert!(stopped.status.success(), "{}", String::from_utf8_lossy(&stopped.stderr));
    assert!(gone, "process {child} outlived the stop");
    let events = store.events(&run_id).unwrap();
    let failed = events.iter().find(|event| event.kind == "run.failed").expect("the stop is recorded");
    assert_eq!(failed.payload["reason"], "stopped by the user");
}

#[test]
fn stopping_a_run_ends_what_its_gate_step_started() {
    let w = world("gate");
    let script = STARTS_A_CHILD.replace("{pidfile}", &w.pidfile.display().to_string());
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"sh\", \"-c\", \"{script}\"]\n")).unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    executable(&w.bin.join("claude"), &format!("#!/bin/sh\nexec '{}' \"$@\"\n", agent.path().display()));

    stop_and_check(&w, start_run(&w));
}

#[test]
fn stopping_a_run_ends_what_its_session_started() {
    let w = world("session");
    let script = STARTS_A_CHILD.replace("{pidfile}", &w.pidfile.display().to_string());
    executable(&w.bin.join("claude"), &format!("#!/bin/sh\n{script}\n"));

    stop_and_check(&w, start_run(&w));
}
