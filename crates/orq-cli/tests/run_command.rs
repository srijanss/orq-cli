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
    let root = std::env::temp_dir().join(format!("orq-run-{}-{tag}", std::process::id()));
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
    let bin = std::env::temp_dir().join(format!("orq-run-bin-{}-{tag}", std::process::id()));
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
    let ssh = std::env::temp_dir().join(format!("orq-run-ssh-{}-{tag}", std::process::id()));
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
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main"])
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
    let slow_bin = std::env::temp_dir().join(format!("orq-run-slow-{}", std::process::id()));
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
