use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{latest_checkpoint, record_checkpoint, RunStore};
use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";
const OTHER_URL: &str = "https://github.example/o/r/pull/99";

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

fn world(tag: &str, agent: &FakeAgent) -> World {
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    world_with(tag, agent, provider)
}

fn world_with(tag: &str, agent: &FakeAgent, provider: FakeProvider) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let base = orq_test_support::process_temp_dir().join(format!("orq-resume-fx-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);

    let bin = base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
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
        .env_remove("HERDR_SOCKET_PATH")
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

fn start_paused(w: &World) -> (String, RunStore) {
    let count = pausing_gate(w);
    let first = orq(w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert_eq!(first.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr));
    let stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    let pause_log = std::fs::read_to_string(w.base.join("pause.log")).unwrap_or_default();
    assert!(stdout.contains("state: paused"), "{stdout}\npause: {pause_log}");
    assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 1);
    let store = RunStore::new(&w.state.join("orq"));
    let ids = store.run_ids().unwrap();
    assert_eq!(ids.len(), 1);
    (ids[0].clone(), store)
}

fn resume(w: &World, run_id: &str) -> Output {
    orq(w, &["run", "resume", run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()])
}

fn agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build()
}

fn count_calls(w: &World, prefix: &[&str]) -> usize {
    w.provider.calls().iter().filter(|c| c.args.len() >= prefix.len() && c.args.iter().zip(prefix).all(|(a, b)| a == b)).count()
}

/// The target branch gets a new commit during setup, and the first gate asks for a pause, so the
/// run stops with its rebase and gate done. Every gate leaves its `GATE_BASE` as a line in
/// `gate-bases`. Returns the new commit of the target.
fn target_moves_and_first_gate_pauses(w: &World) -> String {
    let other = w.base.join("other");
    git(w.repo.root(), &["clone", "--quiet", "--branch", "main", &w.remote.display().to_string(), &other.display().to_string()]);
    git(&other, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "--quiet", "--allow-empty", "-m", "moved"]);
    let seen = w.base.join("gate-bases");
    let script = w.base.join("gate.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ -f '{seen}' ] || XDG_STATE_HOME='{state}' '{orq}' run pause \"$(ls '{state}/orq/runs')\" >/dev/null 2>&1\necho \"$GATE_BASE\" >> '{seen}'\n",
            seen = seen.display(),
            state = w.state.display(),
            orq = env!("CARGO_BIN_EXE_orq"),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(
        w.repo.root().join(".orq/run.toml"),
        format!("gate = [\"{}\"]\nexplain_failures = false\nsetup = [[\"git\", \"-C\", \"{}\", \"push\", \"--quiet\", \"origin\", \"main\"]]\n", script.display(), other.display()),
    )
    .unwrap();
    git(&other, &["rev-parse", "HEAD"]).trim().to_string()
}

#[test]
fn a_gate_rerun_after_a_resume_is_still_told_the_commit_the_branch_was_rebased_onto() {
    let agent = agent();
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();
    let w = world_with("rebasedbase", &agent, provider);
    let moved = target_moves_and_first_gate_pauses(&w);
    let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main"]);
    let stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    assert!(stdout.contains("state: paused"), "{stdout}\n{}", String::from_utf8_lossy(&first.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);

    let second = orq(&w, &["run", "resume", &run_id, "--yes", "--repo", w.repo.root().to_str().unwrap()]);

    let seen = std::fs::read_to_string(w.base.join("gate-bases")).unwrap();
    let bases: Vec<&str> = seen.lines().collect();
    assert!(bases.len() >= 2, "the gate ran again after the CI fix: {bases:?}\n{}", String::from_utf8_lossy(&second.stderr));
    assert!(bases.iter().all(|base| *base == moved), "expected {moved}: {bases:?}");
}

/// The events of the paused run turn unreadable while the next run reads its base, after the resume
/// has checked them. The rebase then cannot be carried, so the resume stops before it does anything,
/// and because a resume continues a feature's newest run, it leaves no run behind to take the
/// paused one's place.
#[test]
fn a_resume_that_cannot_carry_the_rebase_stops_and_the_paused_run_can_be_resumed_again() {
    let agent = agent();
    let w = world("carryfails", &agent);
    let moved = target_moves_and_first_gate_pauses(&w);
    let first = orq(&w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main"]);
    let stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    assert!(stdout.contains("state: paused"), "{stdout}\n{}", String::from_utf8_lossy(&first.stderr));
    let store = RunStore::new(&w.state.join("orq"));
    let run_id = store.run_ids().unwrap().remove(0);
    let events = w.state.join("orq/runs").join(&run_id).join("events.jsonl");
    let intact = std::fs::read_to_string(&events).unwrap();
    let armed = w.base.join("break-events");
    std::fs::write(&armed, "").unwrap();
    // The next run reads its base from the local target branch; that read breaks the events.
    let real_git = String::from_utf8_lossy(&Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout).trim().to_string();
    let wrapper = w.base.join("bin/git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \"$*\" in *'refs/heads/main^{{commit}}'*) if [ -f '{armed}' ]; then rm '{armed}'; echo broken >> '{events}'; fi;; esac\nexec '{real_git}' \"$@\"\n",
            armed = armed.display(),
            events = events.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let aborted = resume(&w, &run_id);

    let stderr = String::from_utf8_lossy(&aborted.stderr).into_owned();
    assert_ne!(aborted.status.code(), Some(0), "{}\n{stderr}", String::from_utf8_lossy(&aborted.stdout));
    assert!(stderr.contains("cannot carry the rebase"), "{stderr}");
    assert!(!armed.exists(), "reading the resume's base broke the events");
    assert_eq!(store.run_ids().unwrap(), vec![run_id.clone()], "the aborted resume leaves no run behind");
    assert_eq!(count_calls(&w, &["pr", "create"]), 0);
    // Apart from the broken line, the paused run was not touched.
    assert_eq!(std::fs::read_to_string(&events).unwrap(), format!("{intact}broken\n"));
    std::fs::write(&events, &intact).unwrap();
    assert_eq!(orq_orchestrator::summarize_run(&store.events(&run_id).unwrap()).phase, orq_orchestrator::RunPhase::Paused);

    let again = resume(&w, &run_id);

    assert_eq!(again.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&again.stdout), String::from_utf8_lossy(&again.stderr));
    let resumed = store.run_ids().unwrap().into_iter().find(|id| *id != run_id).expect("the resume started a run");
    assert_eq!(orq_orchestrator::rebased_onto(&store, &resumed).unwrap().as_deref(), Some(moved.as_str()));
}

#[test]
fn a_resumed_run_does_not_redo_the_stages_the_checkpoint_says_are_done() {
    let agent = agent();
    let w = world("stages", &agent);
    let (run_id, store) = start_paused(&w);
    assert!(git(&w.remote, &["branch", "--list", "fix--COM-01"]).trim().is_empty(), "paused before the push");

    let second = resume(&w, &run_id);

    let stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    assert_eq!(second.status.code(), Some(0), "{stdout}\n{}", String::from_utf8_lossy(&second.stderr));
    assert!(stdout.contains(&format!("review: {PR_URL}")), "{stdout}");
    // The gate had passed before the pause: it is not run again, and the push and review happen once.
    assert_eq!(std::fs::read_to_string(w.base.join("gate-runs")).unwrap().lines().count(), 1);
    assert!(git(&w.remote, &["ls-tree", "-r", "--name-only", "fix--COM-01"]).contains("feature.txt"));
    assert_eq!(count_calls(&w, &["pr", "create"]), 1);
    assert_eq!(store.run_ids().unwrap().len(), 2);
}

#[test]
fn a_checkpoint_that_records_the_push_and_the_review_request_prevents_both_from_happening_again() {
    let agent = agent();
    let w = world("effects", &agent);
    let (run_id, store) = start_paused(&w);
    // As if the run had paused after opening the review request: the push and the request are
    // recorded as done, so neither may be repeated (the remote has no branch and the forge no calls).
    let mut checkpoint = latest_checkpoint(&store, &run_id).unwrap().unwrap();
    checkpoint.completed_stages.extend(["push".to_string(), "review_request".to_string()]);
    checkpoint.next_stage = Some("ci".to_string());
    checkpoint.push_ref = Some(checkpoint.branch.clone());
    checkpoint.review_request_ref = Some(OTHER_URL.to_string());
    record_checkpoint(&store, &run_id, &checkpoint).unwrap();

    let second = resume(&w, &run_id);

    let stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    assert_eq!(second.status.code(), Some(0), "{stdout}\n{}", String::from_utf8_lossy(&second.stderr));
    assert!(stdout.contains(&format!("review: {OTHER_URL}")), "{stdout}");
    assert!(git(&w.remote, &["branch", "--list", "fix--COM-01"]).trim().is_empty(), "no second push");
    assert_eq!(count_calls(&w, &["pr", "create"]), 0);
    assert_eq!(count_calls(&w, &["pr", "list"]), 0);
}

#[test]
fn a_detached_resume_returns_at_once_and_the_resumed_run_still_finishes() {
    let agent = agent();
    let w = world("detached", &agent);
    let (run_id, store) = start_paused(&w);

    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--detach", "--repo", w.repo.root().to_str().unwrap()]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while count_calls(&w, &["pr", "create"]) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(count_calls(&w, &["pr", "create"]), 1);
    assert_eq!(std::fs::read_to_string(w.base.join("gate-runs")).unwrap().lines().count(), 1);
    assert_eq!(store.run_ids().unwrap().len(), 2);
}

/// The web launcher runs exactly `orq run resume <run> --yes --detach --repo <repo>` and reads its output:
/// a refusal must start `orq: cannot resume <run>:` and a started resume must print `run: <id>`.
#[test]
fn the_launchers_resume_command_refuses_in_its_own_words_and_names_the_run_it_started() {
    let agent = agent();
    let w = world("launcher-contract", &agent);
    let repo = w.repo.root().to_str().unwrap().to_string();

    let unknown = orq(&w, &["run", "resume", "no-such-run", "--yes", "--detach", "--repo", &repo]);
    let stderr = String::from_utf8_lossy(&unknown.stderr).into_owned();
    assert_eq!(unknown.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("orq: cannot resume no-such-run"), "{stderr}");

    let (run_id, _store) = start_paused(&w);
    let started = orq(&w, &["run", "resume", &run_id, "--yes", "--detach", "--repo", &repo]);
    let stdout = String::from_utf8_lossy(&started.stdout).into_owned();
    assert_eq!(started.status.code(), Some(0), "{stdout}\n{}", String::from_utf8_lossy(&started.stderr));
    assert!(stdout.lines().any(|l| l.starts_with("run: ") && l.len() > 5), "{stdout}");
}

/// A run whose events cannot be read is the operator's problem, not a reason the run may not resume:
/// the launcher takes only a message that starts `cannot resume` for a refusal.
#[test]
fn a_run_whose_events_cannot_be_read_does_not_claim_it_cannot_resume() {
    let agent = agent();
    let w = world("launcher-unreadable", &agent);
    let repo = w.repo.root().to_str().unwrap().to_string();
    let (run_id, _store) = start_paused(&w);
    std::fs::write(w.state.join("orq/runs").join(&run_id).join("events.jsonl"), "{ not json\n").unwrap();

    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--detach", "--repo", &repo]);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("orq: "), "{stderr}");
    assert!(!stderr.starts_with("orq: cannot resume"), "{stderr}");
}

/// The web always asks for a pane; without Herdr running, the resume is detached as before.
#[test]
fn a_resume_in_herdr_without_herdr_running_still_resumes_detached() {
    let agent = agent();
    let w = world("no-herdr", &agent);
    let (run_id, store) = start_paused(&w);

    let out = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--detach", "--in-herdr", "--repo", w.repo.root().to_str().unwrap()]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while count_calls(&w, &["pr", "create"]) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(count_calls(&w, &["pr", "create"]), 1);
    assert_eq!(store.run_ids().unwrap().len(), 2);
}

/// A stand-in for `herdr`: logs each call, names a new tab's pane, and keeps the command a pane is
/// asked to run without running it.
fn fake_herdr(dir: &Path) -> PathBuf {
    let path = dir.join("herdr");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {calls}\ncase \"$1 $2\" in\n\"tab create\") echo '{{\"result\":{{\"root_pane\":{{\"workspace_id\":\"w1\",\"tab_id\":\"t1\",\"pane_id\":\"p1\"}}}}}}';;\n\"pane run\") printf '%s' \"$4\" > {command};;\nesac\n",
        calls = dir.join("herdr-calls.log").display(),
        command = dir.join("herdr-command.txt").display(),
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

/// The web resumes a run as it starts one: while Herdr is running, the resumed run goes in the
/// foreground of a Herdr pane, where its session can be watched and answered, not unseen in the background.
#[test]
fn a_resume_for_the_web_while_herdr_is_running_runs_in_the_foreground_of_a_herdr_pane() {
    let agent = agent();
    let w = world("herdr-pane", &agent);
    let (run_id, store) = start_paused(&w);
    let herdr = fake_herdr(&w.base);

    let out = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "resume", &run_id, "--yes", "--detach", "--in-herdr", "--repo", w.repo.root().to_str().unwrap()])
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &w.path)
        .env("HERDR_BIN_PATH", &herdr)
        .env("HERDR_SOCKET_PATH", w.base.join("herdr.sock"))
        .env("HERDR_WORKSPACE_ID", "w1")
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let resumed = stdout.lines().find_map(|line| line.strip_prefix("run: ")).unwrap_or_else(|| panic!("no run id in {stdout}")).to_string();
    let calls = std::fs::read_to_string(w.base.join("herdr-calls.log")).unwrap_or_default();
    assert!(calls.lines().next().is_some_and(|create| create.starts_with("tab create")), "{calls}");
    let command = std::fs::read_to_string(w.base.join("herdr-command.txt")).unwrap();
    let quoted = command.strip_prefix("exec sh -c ").unwrap_or_else(|| panic!("{command}"));
    let command = String::from_utf8_lossy(&Command::new("sh").args(["-c", &format!("printf %s {quoted}")]).output().unwrap().stdout).into_owned();
    assert!(command.contains("'--resume'") && command.contains("'--feature' 'COM-01'"), "{command}");
    assert!(command.contains(&format!("'--run-id' '{resumed}'")), "the pane runs the resume the web was told of: {command}");
    assert!(!command.contains("--detach") && !command.contains("--in-herdr"), "the pane runs it in the foreground: {command}");
    assert_eq!(count_calls(&w, &["pr", "create"]), 0, "nothing runs detached");
    assert_eq!(store.run_ids().unwrap(), vec![run_id], "the resume is the pane's to record");
}
