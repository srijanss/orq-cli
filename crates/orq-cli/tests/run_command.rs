use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

/// Exit codes of `orq run`, one per outcome.
const EXIT_INELIGIBLE: i32 = 20;
const EXIT_MODEL_SELECTION: i32 = 21;
const EXIT_BUSY: i32 = 22;
const EXIT_BLOCKED: i32 = 30;
const EXIT_FAILED: i32 = 31;

struct Env {
    state: PathBuf,
    config: PathBuf,
}

fn env(tag: &str) -> Env {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    Env { state: root.join("state"), config: root.join("config") }
}

fn run(repo: &Path, env: &Env, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(repo)
        .args(["--remote", "origin", "--target", "main"])
        .args(extra)
        .env("XDG_CONFIG_HOME", &env.config)
        .env("XDG_STATE_HOME", &env.state)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn state_is_empty(env: &Env) -> bool {
    std::fs::read_dir(&env.state).map(|mut entries| entries.next().is_none()).unwrap_or(true)
}

#[test]
fn a_feature_that_may_not_start_is_refused_before_anything_is_created() {
    let cases: [(&str, Vec<LedgerEntry>, &str, &str); 3] = [
        ("missing", vec![LedgerEntry::new("other", "pending")], "COM-01", "not found"),
        ("done", vec![LedgerEntry::new("COM-01", "completed")], "COM-01", "completed"),
        (
            "unmet",
            vec![LedgerEntry::new("base", "pending"), LedgerEntry::new("COM-01", "pending").depends_on(&["base"])],
            "COM-01",
            "base",
        ),
    ];
    for (tag, entries, feature, expected) in cases {
        let repo = GitFixture::new();
        write_ledger(repo.root(), &entries);
        let env = env(tag);

        let output = run(repo.root(), &env, &["--feature", feature]);

        assert_eq!(output.status.code(), Some(EXIT_INELIGIBLE), "{tag}: {}", stderr(&output));
        assert!(stderr(&output).contains(expected), "{tag}: {}", stderr(&output));
        assert!(output.stdout.is_empty(), "{tag}: nothing is printed on stdout");
        assert!(state_is_empty(&env), "{tag}: no run was recorded");
        let worktrees = Command::new("git").arg("-C").arg(repo.root()).args(["worktree", "list"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&worktrees.stdout).lines().count(), 1, "{tag}: no worktree was made");
    }
}

#[test]
fn a_profile_that_is_not_defined_is_refused_before_anything_is_created() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("profile");

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--implementer", "nope"]);

    assert_eq!(output.status.code(), Some(EXIT_MODEL_SELECTION), "{}", stderr(&output));
    assert!(stderr(&output).contains("nope"), "{}", stderr(&output));
    assert!(state_is_empty(&env));
}

#[test]
fn a_bad_settings_file_is_refused_by_name_before_a_run_is_recorded() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "gate = \"make check\"\n").unwrap();
    let env = env("badsettings");

    let output = run(repo.root(), &env, &["--feature", "COM-01"]);

    assert!(!output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains(".orq/run.toml") && stderr(&output).contains("gate"), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    assert!(state_is_empty(&env), "no run was recorded");
}

#[test]
fn a_bad_settings_file_stops_a_detached_launch_too() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "ci_timeout_secs = 0\n").unwrap();
    let env = env("baddetach");

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--detach"]);

    assert!(!output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("ci_timeout_secs"), "{}", stderr(&output));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("state: launched"));
    assert!(state_is_empty(&env), "no log directory or runner was created");
}

#[test]
fn a_configured_gate_runs_and_a_failing_one_stops_the_run_before_the_push() {
    let w = world("cfggate");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "gate = [\"false\"]\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_ne!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(!stdout.contains("state: review_ready"), "{stdout}");
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "nothing was pushed");
}

#[test]
fn auto_exit_false_in_the_settings_file_asks_for_no_marker_and_keeps_the_sessions_own_exit_status() {
    let w = world("noautoexit");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "auto_exit = false\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let args = agent.first_run().expect("the agent ran").args;
    assert!(!args.last().unwrap().contains("implement.done"), "{args:?}");
    assert_ne!(output.status.code(), Some(0), "the session's failing exit status stops the run: {}", stderr(&output));
}

#[test]
fn extra_context_reaches_the_agent_inside_the_one_prompt_argument() {
    let w = world("ctx");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let _ = run_with(&w, &agent, &github(), &["--no-ci-check", "--context", "keep the API stable --no-gate"]);

    let args = agent.first_run().expect("the agent ran").args;
    let prompt = args.last().unwrap();
    assert!(prompt.starts_with("/implement-and-review COM-01"), "{prompt}");
    assert!(prompt.contains("Extra context:\nkeep the API stable --no-gate"), "{prompt}");
    assert!(prompt.contains("implement.done"), "the session is asked for its exit marker: {prompt}");
}

#[test]
fn bad_extra_context_is_refused_before_a_run_is_recorded() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("badctx");
    let long = "x".repeat(4001);

    for bad in ["bell\u{7}", long.as_str()] {
        let output = run(repo.root(), &env, &["--feature", "COM-01", "--context", bad]);
        assert!(!output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("context"), "{}", stderr(&output));
        assert!(state_is_empty(&env), "no run was recorded");
    }
}

#[test]
fn an_issue_id_closes_the_issue_from_the_review_description() {
    let w = world("issue");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();

    let _ = run_with(&w, &agent, &provider, &["--no-ci-check", "--issue", "#42"]);

    let create = provider.calls().into_iter().find(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])).expect("a review was created");
    assert!(create.args.iter().any(|a| a.ends_with("\n\nCloses #42")), "{:?}", create.args);
}

#[test]
fn a_bad_issue_id_is_refused_before_a_run_is_recorded() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("badissue");

    for flags in [&["--issue", "abc"][..], &["--issue", "4 2", "--detach"][..]] {
        let mut args = vec!["--feature", "COM-01"];
        args.extend_from_slice(flags);
        let output = run(repo.root(), &env, &args);
        assert!(!output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("issue"), "{}", stderr(&output));
        assert!(state_is_empty(&env), "no run was recorded");
    }
}

#[test]
fn an_assignee_and_a_reviewer_are_requested_on_the_new_review() {
    let w = world("people");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();

    let _ = run_with(&w, &agent, &provider, &["--no-ci-check", "--mr-assignee", "alice", "--mr-reviewer", "bob"]);

    let create = provider.calls().into_iter().find(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])).expect("a review was created");
    let after = |flag: &str| create.args.windows(2).find(|p| p[0] == flag).map(|p| p[1].clone());
    assert_eq!((after("--assignee").as_deref(), after("--reviewer").as_deref()), (Some("alice"), Some("bob")));
}

#[test]
fn a_flag_replaces_only_its_own_configured_person_and_a_blank_one_clears_it() {
    let create_args = |tag: &str, extra: &[&str]| {
        let w = world(tag);
        std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
        std::fs::write(w.repo.root().join(".orq/run.toml"), "mr_assignee = \"alice\"\nmr_reviewer = \"bob\"\n").unwrap();
        let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
        let provider = github();
        let mut flags = vec!["--no-ci-check"];
        flags.extend_from_slice(extra);
        let _ = run_with(&w, &agent, &provider, &flags);
        let create = provider.calls().into_iter().find(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])).expect("a review was created");
        let after = |flag: &str| create.args.windows(2).find(|p| p[0] == flag).map(|p| p[1].clone());
        (after("--assignee"), after("--reviewer"))
    };

    assert_eq!(create_args("people-cfg", &[]), (Some("alice".to_string()), Some("bob".to_string())));
    assert_eq!(create_args("people-one", &["--mr-reviewer", "carol"]), (Some("alice".to_string()), Some("carol".to_string())));
    assert_eq!(create_args("people-blank", &["--mr-assignee", ""]), (None, Some("bob".to_string())));
}

#[test]
fn a_bad_assignee_or_reviewer_is_refused_before_a_run_is_recorded() {
    let repo = GitFixture::new();
    repo.add_bare_remote();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("badpeople");

    for flags in [&["--mr-assignee", "-x"][..], &["--mr-reviewer", "a b", "--detach"][..]] {
        let mut args = vec!["--feature", "COM-01"];
        args.extend_from_slice(flags);
        let output = run(repo.root(), &env, &args);
        assert!(!output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("mr-"), "{}", stderr(&output));
        assert!(state_is_empty(&env), "no run was recorded");
    }
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

/// A directory holding a `claude` that runs the fake agent, next to the fake `gh`.
fn path_with(agent: &FakeAgent, provider: &FakeProvider, tag: &str) -> std::ffi::OsString {
    let bin = orq_test_support::process_temp_dir().join(format!("orq-run-bin-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

struct World {
    repo: GitFixture,
    remote: PathBuf,
    env: Env,
    ssh: PathBuf,
    tag: String,
}

/// A repository with a pushed `main`, a pending COM-01, and a remote the review stage can name.
fn world(tag: &str) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    // An scp-style URL the review stage can name, reached through a shim that runs the
    // remote command locally instead of over ssh.
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = orq_test_support::process_temp_dir().join(format!("orq-run-ssh-{}-{tag}", std::process::id()));
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    World { repo, remote, env: env(tag), ssh, tag: tag.to_string() }
}

const PR_URL: &str = "https://github.example/o/r/pull/7";

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

fn run_with(w: &World, agent: &FakeAgent, provider: &FakeProvider, extra: &[&str]) -> Output {
    run_selecting(w, agent, provider, &["--feature", "COM-01"], extra)
}

/// As `run_with`, but the feature is chosen by `selection` (`--feature COM-01`, `--group COM-01`, ...).
fn run_selecting(w: &World, agent: &FakeAgent, provider: &FakeProvider, selection: &[&str], extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(selection)
        .args(["--remote", "origin", "--target", "main"])
        .args(extra)
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", path_with(agent, provider, &w.tag))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

#[test]
fn a_run_that_succeeds_ends_at_review_ready_and_prints_the_run_id_state_and_url() {
    let w = world("happy");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("run: "), "{stdout}");
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    assert!(stdout.contains(&format!("review: {PR_URL}")), "{stdout}");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed");
    assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 1, "the worktree was cleaned up");
}

#[test]
fn a_foreground_run_records_its_owner_so_a_killed_one_can_be_recovered() {
    let w = world("foreground-owner");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let launched = store.events(&run_id).unwrap().into_iter().find(|e| e.kind == "run.launched");
    let launched = launched.expect("a foreground run records run.launched too");
    assert!(launched.payload.get("pid").and_then(|p| p.as_u64()).is_some(), "{:?}", launched.payload);
    assert!(launched.payload.get("log").is_none(), "a foreground run has no runner log to name: {:?}", launched.payload);
}

/// Every agent invocation (the first and the latest) is the PR-description drafter, never an implement session.
fn assert_only_drafting(agent: &FakeAgent, what: &str) {
    for run in [agent.first_run(), agent.last_run()].into_iter().flatten() {
        assert!(run.args.iter().any(|a| a.starts_with("Write a pull request title")), "{what}: {:?}", run.args);
    }
}

#[test]
fn a_foreground_run_killed_mid_flight_is_recognised_as_interrupted_and_resumes() {
    let w = world("foreground-killed");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-run-killed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch {}\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let launched = || {
        store.run_ids().unwrap_or_default().into_iter().find_map(|id| {
            let events = store.events(&id).ok()?;
            events.into_iter().find(|e| e.kind == "run.launched").map(|e| (id, e))
        })
    };
    wait_until("the foreground run to record its owner", || launched().is_some());
    wait_until("the implement agent to be running", || ready.exists());
    let (run_id, owner) = launched().unwrap();
    assert_eq!(owner.payload["pid"].as_u64(), Some(u64::from(runner.id())), "the owner is the runner itself");
    let state = |id: &str| orq_orchestrator::summarize_run(&store.events(id).unwrap()).phase;
    assert_eq!(state(&run_id), orq_orchestrator::RunPhase::Running, "alive and running");

    runner.kill().unwrap();
    runner.wait().unwrap();

    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check", "--resume"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    let events = store.events(&run_id).unwrap();
    let failed = events.iter().find(|e| e.kind == "run.failed").expect("the killed run was marked interrupted");
    assert!(failed.payload["reason"].as_str().unwrap().contains("interrupted"), "{:?}", failed.payload);
}

#[test]
fn the_runs_listing_shows_a_killed_run_as_failed_not_running() {
    let w = world("listing-killed");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-list-killed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch {}\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the implement agent to be running", || ready.exists());
    runner.kill().unwrap();
    runner.wait().unwrap();

    let listing = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["runs", "--repo"])
        .arg(w.repo.root())
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&listing.stdout).into_owned();
    assert_eq!(listing.status.code(), Some(0), "{}", stderr(&listing));
    assert!(stdout.contains("failed"), "{stdout}");
    assert!(!stdout.contains("running"), "{stdout}");
}

#[test]
fn the_runs_listing_leaves_a_live_run_alone_and_records_a_dead_pause_requested_run_failed_once() {
    let w = world("listing-edges");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-list-edges-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the implement agent to be running", || ready.exists());
    let list = |extra: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_orq"))
            .args(["runs", "--repo"])
            .arg(w.repo.root())
            .args(extra)
            .env("XDG_CONFIG_HOME", &w.env.config)
            .env("XDG_STATE_HOME", &w.env.state)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let run_id = store.run_ids().unwrap().into_iter().next().expect("the run was recorded");

    let alive = list(&[]);
    assert!(alive.contains("running") && !alive.contains("failed"), "a live run is left alone: {alive}");

    orq_orchestrator::request_pause(&store, &run_id).unwrap();
    runner.kill().unwrap();
    runner.wait().unwrap();

    let json = list(&["--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["runs"][0]["phase"], "failed", "{json}");
    let _ = list(&[]);
    let failures = store.events(&run_id).unwrap().iter().filter(|e| e.kind == "run.failed").count();
    assert_eq!(failures, 1, "listing again does not record the failure twice");
}

#[test]
fn the_runs_listing_keeps_a_paused_run_paused_even_though_its_process_is_gone() {
    let w = world("listing-paused");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-list-paused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the implement agent to be running", || ready.exists());
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let run_id = store.run_ids().unwrap().into_iter().next().expect("the run was recorded");
    orq_orchestrator::request_pause(&store, &run_id).unwrap();
    let requested = store.events(&run_id).unwrap().into_iter().find(|e| e.kind == "run.pause_requested").unwrap();
    store
        .append_event(&orq_orchestrator::Event {
            event_id: format!("{}-paused", requested.event_id),
            kind: "run.paused".to_string(),
            payload: serde_json::json!({}),
            ..requested
        })
        .unwrap();
    runner.kill().unwrap();
    runner.wait().unwrap();

    for extra in [&[][..], &["--json"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_orq"))
            .args(["runs", "--repo"])
            .arg(w.repo.root())
            .args(extra)
            .env("XDG_CONFIG_HOME", &w.env.config)
            .env("XDG_STATE_HOME", &w.env.state)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        assert!(stdout.contains("paused") && !stdout.contains("failed"), "{stdout}");
    }
    assert!(store.events(&run_id).unwrap().iter().all(|e| e.kind != "run.failed"), "no failure was recorded");
}

#[test]
fn the_runs_listing_says_so_when_a_dead_run_cannot_be_recorded_as_failed() {
    let w = world("listing-unwritable");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-list-unwritable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    let ready = slow_bin.join("agent-started");
    std::fs::write(&claude, format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", ready.display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut runner = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the implement agent to be running", || ready.exists());
    runner.kill().unwrap();
    runner.wait().unwrap();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    let run_id = store.run_ids().unwrap().into_iter().next().expect("the run was recorded");
    let log = w.env.state.join("orq").join("runs").join(&run_id).join("events.jsonl");
    std::fs::set_permissions(&log, std::os::unix::fs::PermissionsExt::from_mode(0o444)).unwrap();
    if std::fs::OpenOptions::new().append(true).open(&log).is_ok() {
        // A privileged user ignores the file mode, so the write cannot be made to fail here.
        eprintln!("skipped: the event log is still writable (running with write privileges)");
        return;
    }

    let listing = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["runs", "--repo"])
        .arg(w.repo.root())
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .output()
        .unwrap();
    assert_eq!(listing.status.code(), Some(0), "{}", stderr(&listing));
    let warning = stderr(&listing);
    assert!(warning.contains(&run_id), "the warning names the run: {warning}");
}

#[test]
fn a_run_that_failed_after_opening_the_review_resumes_without_repeating_the_agent_or_the_review() {
    let w = world("no-replay");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let first = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();
    let output = run_with(&w, &agent, &first, &[]);
    assert_eq!(output.status.code(), Some(EXIT_FAILED), "{}", stderr(&output));
    assert!(first.calls().iter().any(|c| c.args.starts_with(&["pr".into(), "create".into()])), "the first run opened the review");
    let pushed = git(&w.remote, &["rev-parse", "fix--COM-01"]);

    // The review now exists on the forge; a second agent and provider must show nothing is redone.
    let second_agent = FakeAgent::builder().write_file("again.txt", "x\n").commit("feat: again").build();
    let second = FakeProvider::builder()
        .reply(&["pr", "list"], &format!("[{{\"number\":7,\"url\":\"{PR_URL}\",\"state\":\"OPEN\",\"isCrossRepository\":false}}]\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let output = run_with(&w, &second_agent, &second, &["--no-ci-check", "--resume"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert_only_drafting(&second_agent, "the finished implement session is not started again");
    assert!(!w.repo.root().join("again.txt").exists(), "no new work was committed");
    assert!(!second.calls().iter().any(|c| c.args.starts_with(&["pr".into(), "create".into()])), "no second review: {:?}", second.calls());
    assert_eq!(git(&w.remote, &["rev-parse", "fix--COM-01"]), pushed, "the pushed branch is untouched");
    assert!(stdout.contains(PR_URL), "{stdout}");
}

#[test]
fn a_run_that_failed_between_the_push_and_the_review_resumes_to_open_it_once_without_redoing_the_work() {
    let w = world("push-then-crash");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let broken = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .fail(&["pr", "create"], 1, "forge unavailable")
        .build();
    let output = run_with(&w, &agent, &broken, &["--no-ci-check"]);
    assert_ne!(output.status.code(), Some(0), "the review could not be opened");
    let pushed = git(&w.remote, &["rev-parse", "fix--COM-01"]);

    let second_agent = FakeAgent::builder().write_file("again.txt", "x\n").commit("feat: again").build();
    let second = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let output = run_with(&w, &second_agent, &second, &["--no-ci-check", "--resume"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert_only_drafting(&second_agent, "the implement session is not started again");
    let creates = second.calls().iter().filter(|c| c.args.starts_with(&["pr".into(), "create".into()])).count();
    assert_eq!(creates, 1, "the review is opened exactly once: {:?}", second.calls());
    assert_eq!(git(&w.remote, &["rev-parse", "fix--COM-01"]), pushed, "the branch was not rewritten");
    assert!(stdout.contains(PR_URL), "{stdout}");
}

#[test]
fn a_run_stopped_by_its_gate_before_the_push_resumes_to_push_and_open_the_review_without_the_agent() {
    let w = world("gate-then-resume");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    let toml = w.repo.root().join(".orq/run.toml");
    std::fs::write(&toml, "gate = [\"false\"]\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "nothing was pushed");

    std::fs::write(&toml, "gate = [\"true\"]\n").unwrap();
    let second_agent = FakeAgent::builder().write_file("again.txt", "x\n").commit("feat: again").build();
    let provider = github();
    let output = run_with(&w, &second_agent, &provider, &["--no-ci-check", "--resume"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert_only_drafting(&second_agent, "the implement session is not started again");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "pushed on resume");
    let creates = provider.calls().iter().filter(|c| c.args.starts_with(&["pr".into(), "create".into()])).count();
    assert_eq!(creates, 1, "{:?}", provider.calls());
}

#[test]
fn an_agent_that_leaves_no_commit_blocks_the_run_and_nothing_is_pushed() {
    let w = world("nocommit");
    let agent = FakeAgent::builder().write_file("notes.txt", "wip\n").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: blocked"), "{stdout}");
    assert!(!stdout.contains("review:"), "{stdout}");
    assert!(!git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "nothing was pushed");
    assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 2, "the worktree is kept");
}

#[test]
fn a_red_ci_fails_the_run_but_still_reports_the_review_request() {
    let w = world("redci");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();

    let output = run_with(&w, &agent, &provider, &[]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_FAILED), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: failed"), "{stdout}");
    assert!(stdout.contains(&format!("review: {PR_URL}")), "{stdout}");
    assert!(stderr(&output).contains("test"), "the failing check is named: {}", stderr(&output));
    assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 2, "the worktree is kept");
}

#[test]
fn a_feature_with_a_run_in_progress_is_refused_with_its_own_exit_code() {
    let w = world("busy");
    let slow_bin = orq_test_support::process_temp_dir().join(format!("orq-run-slow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&slow_bin);
    std::fs::create_dir_all(&slow_bin).unwrap();
    let claude = slow_bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![slow_bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut first = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !w.env.state.join("orq/runs").exists() && started.elapsed().as_secs() < 10 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let second = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    let _ = first.kill();
    let _ = first.wait();

    assert_eq!(second.status.code(), Some(EXIT_BUSY), "{}", stderr(&second));
    assert!(stderr(&second).contains("run-"), "the active run is named: {}", stderr(&second));
    assert!(second.stdout.is_empty());
}

#[test]
fn a_harness_found_through_a_relative_path_entry_still_runs_inside_the_worktree() {
    let w = world("relpath");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let absolute = path_with(&agent, &provider, &w.tag);
    let mut entries: Vec<PathBuf> = std::env::split_paths(&absolute).collect();
    // The harness directory is named relative to the directory orq is started from.
    let bin = entries[0].clone();
    entries[0] = PathBuf::from(bin.file_name().unwrap());

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .current_dir(bin.parent().unwrap())
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(entries).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
}

#[test]
fn a_lock_that_cannot_be_released_fails_the_run_but_keeps_the_review_url() {
    let w = world("lockrelease");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    // The harness hands the feature's lock to another run before doing its work, so the
    // release at the end of this run is refused.
    let claude = std::env::split_paths(&path).next().unwrap().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nfor f in '{}'/orq/locks/*/COM-01.lock; do echo run-other > \"$f\"; done\nexec '{}' \"$@\"\n",
            w.env.state.display(),
            agent.path().display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_FAILED), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: failed"), "{stdout}");
    assert!(stdout.contains(&format!("review: {PR_URL}")), "the review is still named: {stdout}");
    assert!(stderr(&output).contains("lock"), "the release failure is reported: {}", stderr(&output));
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !done() {
        assert!(started.elapsed().as_secs() < 60, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn a_detached_run_returns_at_once_while_the_run_goes_on_and_finishes_after_the_launcher_exited() {
    let w = world("detach");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    // The agent takes a moment, so the run is certainly still going when the launcher returns.
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    let claude = std::env::split_paths(&path).next().unwrap().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -p ]; then printf 'Add the feature\\n\\nThe feature.\\n'; exit 0; fi\nsleep 2\nexec '{}' \"$@\"\n",
            agent.path().display()
        ),
    )
    .unwrap();

    let started = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(started.elapsed().as_secs() < 2, "the launcher did not wait for the run: {:?}", started.elapsed());
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let log = stdout.lines().find_map(|l| l.strip_prefix("log: ")).expect(&stdout).to_string();
    assert!(stdout.contains("state: launched"), "{stdout}");

    // The run finishes on its own: its worktree is cleaned up and the review request exists.
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
    let events = store.events(&run_id).unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"run.launched"), "{kinds:?}");
    assert!(kinds.contains(&"review.created"), "{kinds:?}");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed");
    assert!(std::path::Path::new(&log).is_file(), "the run log exists at {log}");
}

#[test]
fn a_group_of_one_runs_the_feature_to_review_ready_like_feature_does_attached_or_detached() {
    let w = world("groupone");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_selecting(&w, &agent, &github(), &["--group", "COM-01"], &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed");

    // Detached, the launcher forwards --group to the runner it starts and the run finishes.
    let w = world("groupone-detach");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    let claude = std::env::split_paths(&path).next().unwrap().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -p ]; then printf 'Add the feature\\n\\nThe feature.\\n'; exit 0; fi\nexec '{}' \"$@\"\n",
            agent.path().display()
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--group", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached group run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
    let kinds: Vec<String> = store.events(&run_id).unwrap().into_iter().map(|e| e.kind).collect();
    assert!(kinds.iter().any(|k| k == "review.created"), "{kinds:?}");
    assert!(git(&w.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed");
}

#[test]
fn a_group_of_several_features_runs_once_on_the_first_features_branch_with_one_review_request() {
    let w = world("groupmany");
    write_ledger(
        w.repo.root(),
        &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending").depends_on(&["COM-01"])],
    );
    let agent = FakeAgent::builder()
        .write_file("one.txt", "1\n")
        .commit("feat: COM-01")
        .write_file("two.txt", "2\n")
        .commit("feat: COM-02")
        .build();
    let provider = github();

    // Given out of order: the dependency decides.
    let output = run_selecting(&w, &agent, &provider, &["--group", "COM-02,COM-01"], &["--no-ci-check"]);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    let branches = git(&w.remote, &["branch", "--list"]);
    assert!(branches.contains("fix--COM-01"), "{branches}");
    assert!(!branches.contains("COM-02"), "one branch for the group: {branches}");
    let prompt = agent.first_run().expect("the agent ran").args.last().cloned().unwrap();
    assert!(prompt.starts_with("/implement-and-review COM-01 COM-02 "), "{prompt}");
    let creates: Vec<_> = provider.calls().into_iter().filter(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])).collect();
    assert_eq!(creates.len(), 1, "one review request");
    let title = creates[0].args.iter().skip_while(|a| *a != "--title").nth(1).cloned().expect("a title");
    assert!(title.contains("COM-01") && title.contains("COM-02"), "{title}");
}

#[test]
fn a_detached_run_that_may_not_start_is_refused_synchronously_and_launches_nothing() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "completed")]);
    let env = env("detach-refused");

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--detach"]);

    assert_eq!(output.status.code(), Some(EXIT_INELIGIBLE), "{}", stderr(&output));
    assert!(state_is_empty(&env));
}

#[test]
fn a_detached_launch_of_a_feature_that_is_already_running_fails_with_the_reason_and_records_no_run() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("detach-busy");
    let inspected = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["project", "inspect", "--cwd"])
        .arg(repo.root())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    let project: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let store = orq_orchestrator::RunStore::new(&env.state.join("orq"));
    store
        .start_run(&orq_orchestrator::RunRecord {
            run_id: "run-active".into(),
            project_id: project["id"].as_str().unwrap().to_string(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-active".into(),
            worktree: PathBuf::from("/tmp/w"),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--detach"]);

    assert_eq!(output.status.code(), Some(EXIT_BUSY), "{}", stderr(&output));
    assert!(stderr(&output).contains("already has an active run: run-active"), "{}", stderr(&output));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("state: launched"));
    assert_eq!(store.run_ids().unwrap(), ["run-active"], "no ghost run was recorded");
    assert!(!env.state.join("orq/logs").exists(), "no log directory was created");
}

#[test]
fn a_detached_group_is_refused_when_any_member_not_only_the_first_is_already_running() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending")]);
    let env = env("detach-group-busy");
    let inspected = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["project", "inspect", "--cwd"])
        .arg(repo.root())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    let project: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let store = orq_orchestrator::RunStore::new(&env.state.join("orq"));
    store
        .start_run(&orq_orchestrator::RunRecord {
            run_id: "run-active".into(),
            project_id: project["id"].as_str().unwrap().to_string(),
            feature_id: "COM-02".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-active".into(),
            worktree: PathBuf::from("/tmp/w"),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();

    let output = run(repo.root(), &env, &["--group", "COM-01,COM-02", "--detach"]);

    assert_eq!(output.status.code(), Some(EXIT_BUSY), "{}", stderr(&output));
    assert!(stderr(&output).contains("COM-02") && stderr(&output).contains("run-active"), "{}", stderr(&output));
    assert_eq!(store.run_ids().unwrap(), ["run-active"], "no ghost run was recorded");
}

// Reads /proc, so it can only run where procfs exists.
#[cfg(target_os = "linux")]
#[test]
fn the_detached_runner_and_its_agent_live_in_a_session_of_their_own() {
    let w = world("detach-session");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    // The harness stand-in notes the session it runs in (field 6 of /proc/<pid>/stat).
    let note = w.env.state.join("agent-session");
    let claude = std::env::split_paths(&path).next().unwrap().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -p ]; then printf 'Add the feature\\n\\nThe feature.\\n'; exit 0; fi\nawk '{{print $6}}' /proc/$$/stat > '{}'\nexec '{}' \"$@\"\n",
            note.display(),
            agent.path().display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
    let events = store.events(&run_id).unwrap();
    let pid = events.iter().find(|e| e.kind == "run.launched").unwrap().payload["pid"].as_u64().unwrap();
    let agent_session: u64 = std::fs::read_to_string(&note).unwrap().trim().parse().unwrap();
    let own_session: u64 =
        std::fs::read_to_string("/proc/self/stat").unwrap().rsplit(')').next().unwrap().split_whitespace().nth(3).unwrap().parse().unwrap();
    assert_eq!(agent_session, pid, "the agent runs in the session the runner leads");
    assert_ne!(agent_session, own_session, "and not in the launcher's session");
}

#[test]
fn the_runner_log_and_its_directory_are_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("detach-private");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let log = PathBuf::from(stdout.lines().find_map(|l| l.strip_prefix("log: ")).expect(&stdout));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&log), 0o600, "the log is readable by its owner only");
    assert_eq!(mode(log.parent().unwrap()), 0o700, "the log directory is closed to others");
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
}

#[test]
fn detaching_keeps_an_option_value_that_is_spelled_like_the_flag() {
    let w = world("detach-value");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = path_with(&agent, &provider, &w.tag);
    // The repository is reached through a directory literally named `--detach`.
    let odd = orq_test_support::process_temp_dir().join(format!("orq-odd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&odd);
    std::fs::create_dir_all(&odd).unwrap();
    std::os::unix::fs::symlink(w.repo.root(), odd.join("--detach")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .current_dir(&odd)
        .args(["run", "--repo", "--detach"])
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
        .env("XDG_CONFIG_HOME", &w.env.config)
        .env("XDG_STATE_HOME", &w.env.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string();
    let store = orq_orchestrator::RunStore::new(&w.env.state.join("orq"));
    wait_until("the detached run to complete", || {
        store.events(&run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
    });
}

fn completed_events(w: &World, stdout: &str) -> Vec<(String, Option<String>)> {
    let run_id = stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect("run id printed").trim().to_string();
    let World { env: scratch, .. } = w;
    let store = orq_orchestrator::RunStore::new(&scratch.state.join("orq"));
    store
        .events(&run_id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "run.completed")
        .map(|e| {
            (
                e.payload["ending"].as_str().unwrap().to_string(),
                e.payload.get("reason").and_then(|r| r.as_str()).map(str::to_string),
            )
        })
        .collect()
}

#[test]
fn every_run_records_exactly_one_run_completed_event_with_how_it_ended() {
    let w = world("completed-happy");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", stderr(&output));
    assert_eq!(completed_events(&w, &stdout), [("review_ready".to_string(), None)]);

    let w = world("completed-blocked");
    let agent = FakeAgent::builder().write_file("notes.txt", "wip\n").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{}\n{stdout}", stderr(&output));
    let done = completed_events(&w, &stdout);
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0].0, "blocked");
    assert!(done[0].1.is_some(), "the reason is kept: {done:?}");
}

#[test]
fn a_run_whose_worktree_cannot_be_created_still_records_that_it_failed() {
    let w = world("completed-no-worktree");
    // A file where git keeps its worktree metadata makes `git worktree add` fail.
    std::fs::write(w.repo.root().join(".git").join("worktrees"), "in the way").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).contains("worktree"), "{}", stderr(&output));
    let World { env: scratch, .. } = &w;
    let runs = scratch.state.join("orq").join("runs");
    let ids: Vec<String> = std::fs::read_dir(&runs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?} {}", stderr(&output));
    let events = orq_orchestrator::RunStore::new(&scratch.state.join("orq")).events(&ids[0]).unwrap();
    let done: Vec<_> = events.iter().filter(|e| e.kind == "run.completed").collect();
    assert_eq!(done.len(), 1, "{events:?}");
    assert_eq!(done[0].payload["ending"].as_str(), Some("failed"));
    assert!(done[0].payload["reason"].as_str().is_some_and(|r| r.contains("worktree")), "{:?}", done[0].payload);
}

#[test]
fn a_configured_notification_command_hears_how_the_run_ended_and_its_failure_changes_nothing() {
    let w = world("notify");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let heard = w.repo.root().join("heard.txt");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    let toml = format!("notify_cmd = [\"sh\", \"-c\", \"printf %s \\\"$NOTIFY_MESSAGE\\\" > '{}'\"]\n", heard.display());
    std::fs::write(w.repo.root().join(".orq/run.toml"), toml).unwrap();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let message = std::fs::read_to_string(&heard).expect("the notifier ran");
    assert!(message.starts_with("orq: COM-01 review_ready"), "{message}");

    let w = world("notifyfails");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "notify_cmd = [\"no-such-notifier\"]\n").unwrap();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("notification"), "{}", stderr(&output));
}

fn notify_world(tag: &str, mode: Option<&str>) -> (World, PathBuf) {
    let w = world(tag);
    let heard = w.repo.root().join("heard.txt");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    let mut toml = format!("notify_cmd = [\"sh\", \"-c\", \"printf '%s\\\\n' \\\"$NOTIFY_MESSAGE\\\" >> '{}'\"]\n", heard.display());
    if let Some(mode) = mode {
        toml.push_str(&format!("notify_on = \"{mode}\"\n"));
    }
    std::fs::write(w.repo.root().join(".orq/run.toml"), toml).unwrap();
    (w, heard)
}

#[test]
fn a_run_that_cannot_create_its_worktree_is_still_announced() {
    let (w, heard) = notify_world("notifywt", None);
    // A file where the worktrees' parent directory must go.
    std::fs::write(w.repo.root().join(".claude"), "in the way").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert!(!output.status.success(), "{}", stderr(&output));
    let heard = std::fs::read_to_string(&heard).expect("the notifier ran");
    assert!(heard.starts_with("orq: COM-01 failed"), "{heard}");
}

#[test]
fn notify_on_failures_stays_quiet_for_a_clean_run_and_speaks_for_one_that_stops() {
    let (w, heard) = notify_world("notifyquiet", Some("failures"));
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!heard.exists(), "a clean run is not announced under notify_on = failures");

    // An agent that changes nothing leaves no work to review: the run stops, and that is announced.
    let (w, heard) = notify_world("notifystops", Some("failures"));
    let idle = FakeAgent::builder().build();
    let _ = run_with(&w, &idle, &github(), &["--no-ci-check"]);
    let heard = std::fs::read_to_string(&heard).expect("a run that stops is announced");
    assert!(heard.starts_with("orq: COM-01 blocked") || heard.starts_with("orq: COM-01 failed"), "{heard}");
}

#[test]
fn publish_pushes_the_checked_out_branch_and_opens_the_review_without_a_worktree_or_an_implement_session() {
    let w = world("publish");
    git(w.repo.root(), &["checkout", "--quiet", "-b", "topic"]);
    std::fs::write(w.repo.root().join("topic.txt"), "work\n").unwrap();
    git(w.repo.root(), &["add", "topic.txt"]);
    git(w.repo.root(), &["-c", "user.name=Topic", "-c", "user.email=topic@example.invalid", "commit", "--quiet", "-m", "feat: the topic"]);
    // The agent is only the description drafter here: it must not be asked to implement anything.
    let agent = FakeAgent::builder().build();
    let provider = github();

    let output = run_with(&w, &agent, &provider, &["--publish", "--no-ci-check"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("review: {PR_URL}")), "{}", String::from_utf8_lossy(&output.stdout));
    let pushed = Command::new("git").arg("-C").arg(&w.remote).args(["rev-parse", "--verify", "refs/heads/topic"]).output().unwrap();
    assert!(pushed.status.success(), "the branch was pushed");
    assert!(provider.calls().iter().any(|c| c.args.starts_with(&["pr".to_string(), "create".to_string()])));
    assert!(!w.repo.root().join(".claude/worktrees").exists(), "no worktree was created");
    let head = Command::new("git").arg("-C").arg(w.repo.root()).args(["symbolic-ref", "--short", "HEAD"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "topic");
    // The only agent session is the description drafter, run in the repository itself.
    let first = agent.first_run().expect("the drafter ran");
    assert_eq!(first.args, agent.last_run().unwrap().args, "exactly one agent session");
}

#[test]
fn publish_refuses_a_checkout_it_cannot_publish_and_leaves_everything_alone() {
    // (tag, how to prepare the repo, what the refusal must say)
    let cases: [(&str, fn(&Path), &str); 4] = [
        ("pubtarget", |_| {}, "is the target branch"),
        (
            "pubdetached",
            |r| {
                git(r, &["checkout", "--quiet", "--detach"]);
            },
            "detached"),
        (
            "pubdirty",
            |r| {
                git(r, &["checkout", "--quiet", "-b", "topic"]);
                std::fs::write(r.join("topic.txt"), "work\n").unwrap();
                git(r, &["add", "topic.txt"]);
                git(r, &["-c", "user.name=Topic", "-c", "user.email=topic@example.invalid", "commit", "--quiet", "-m", "topic"]);
                std::fs::write(r.join("topic.txt"), "changed\n").unwrap();
            },
            "uncommitted changes",
        ),
        (
            "pubempty",
            |r| {
                git(r, &["checkout", "--quiet", "-b", "topic"]);
            },
            "no commits beyond"),
    ];
    for (tag, prepare, message) in cases {
        let w = world(tag);
        prepare(w.repo.root());
        let agent = FakeAgent::builder().build();
        let provider = github();
        let output = run_with(&w, &agent, &provider, &["--publish", "--no-ci-check"]);
        assert!(!output.status.success(), "{tag}: {}", String::from_utf8_lossy(&output.stdout));
        assert!(stderr(&output).contains(message), "{tag}: {}", stderr(&output));
        assert!(provider.calls().is_empty(), "{tag}: no review was asked for");
        assert!(agent.last_run().is_none(), "{tag}: no agent session ran");
        assert!(!w.repo.root().join(".claude/worktrees").exists(), "{tag}: no worktree");
    }
}

#[test]
fn the_implement_session_is_reachable_from_the_app_and_a_failed_run_starts_an_explainer_session() {
    let w = world("explainer");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    let first = agent.first_run().expect("the implement session ran").args;
    let at = first.iter().position(|a| a == "--remote-control").expect("remote control is on by default");
    assert!(first[at + 1].starts_with("orq/") || !first[at + 1].is_empty(), "{first:?}");
    wait_until("the explainer session", || agent.last_run().is_some_and(|run| run.args != first));
    let explain = agent.last_run().unwrap().args;
    assert!(explain.iter().any(|a| a == "--remote-control"), "{explain:?}");
    assert!(explain.last().unwrap().contains("the implement session failed"), "{explain:?}");
}

#[test]
fn both_switches_off_in_the_settings_file_start_no_remote_control_and_no_explainer() {
    let w = world("noexplainer");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "remote_control = false\nexplain_failures = false\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    let first = agent.first_run().expect("the implement session ran").args;
    assert!(!first.iter().any(|a| a == "--remote-control"), "{first:?}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(agent.last_run().unwrap().args, first, "no second session was started");
}

#[test]
fn remote_control_off_with_the_explainer_on_starts_an_explainer_that_is_not_reachable_from_the_app() {
    let w = world("explainer-local");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "remote_control = false\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_ne!(output.status.code(), Some(0), "{}", stderr(&output));
    let first = agent.first_run().expect("the implement session ran").args;
    wait_until("the explainer session", || agent.last_run().is_some_and(|run| run.args != first));
    let explain = agent.last_run().unwrap().args;
    assert!(!explain.iter().any(|a| a == "--remote-control"), "{explain:?}");
}

fn push_session(agent: &FakeAgent) -> Option<Vec<String>> {
    agent.last_run().map(|run| run.args).filter(|args| args.iter().any(|a| a == "PushNotification"))
}

#[test]
fn claude_push_sends_the_run_end_to_the_claude_app_through_a_print_session() {
    let w = world("claudepush");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "claude_push = true\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let output = run_with(&w, &agent, &github(), &["--no-ci-check"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    wait_until("the push session", || push_session(&agent).is_some());
    let args = push_session(&agent).unwrap();
    assert!(args.iter().any(|a| a == "-p"), "{args:?}");
    assert!(args.last().unwrap().contains("COM-01 review_ready"), "{args:?}");

    // The session starts in a directory made for it alone: not the shared temp dir (where anyone could plant
    // a CLAUDE.md), not the repository, and closed to other users.
    use std::os::unix::fs::PermissionsExt;
    let cwd = agent.last_run().unwrap().cwd;
    let shared = std::env::temp_dir();
    assert_ne!(cwd.canonicalize().unwrap(), shared.canonicalize().unwrap(), "{cwd:?}");
    assert!(cwd.canonicalize().unwrap().starts_with(shared.canonicalize().unwrap()), "{cwd:?}");
    assert!(!cwd.starts_with(w.repo.root()), "{cwd:?}");
    assert_eq!(std::fs::metadata(&cwd).unwrap().permissions().mode() & 0o777, 0o700, "{cwd:?}");
}

#[test]
fn claude_push_stays_off_unless_asked_for_and_follows_notify_on() {
    let off = world("claudepushoff");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    assert_eq!(run_with(&off, &agent, &github(), &["--no-ci-check"]).status.code(), Some(0));
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(push_session(&agent).is_none(), "off by default");

    let quiet = world("claudepushquiet");
    std::fs::create_dir_all(quiet.repo.root().join(".orq")).unwrap();
    std::fs::write(quiet.repo.root().join(".orq/run.toml"), "claude_push = true\nnotify_on = \"failures\"\n").unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    assert_eq!(run_with(&quiet, &agent, &github(), &["--no-ci-check"]).status.code(), Some(0));
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(push_session(&agent).is_none(), "a clean run is not announced under notify_on = failures");
}

#[test]
fn a_group_run_is_announced_once_naming_every_feature_of_the_group() {
    let (w, heard) = notify_world("notifygroup", None);
    write_ledger(
        w.repo.root(),
        &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending").depends_on(&["COM-01"])],
    );
    let agent = FakeAgent::builder().write_file("one.txt", "1\n").commit("feat: COM-01").write_file("two.txt", "2\n").commit("feat: COM-02").build();

    let output = run_selecting(&w, &agent, &github(), &["--group", "COM-02,COM-01"], &["--no-ci-check"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let heard = std::fs::read_to_string(&heard).expect("the notifier ran");
    let lines: Vec<_> = heard.lines().collect();
    assert_eq!(lines.len(), 1, "{heard}");
    assert!(lines[0].starts_with("orq: COM-01,COM-02 review_ready"), "{heard}");
}
