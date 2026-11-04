use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const EXIT_FAILED: i32 = 31;
const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    tag: String,
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

fn world(tag: &str) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let base = orq_test_support::process_temp_dir().join(format!("orq-resume-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    World { repo, remote, state: base.join("state"), config: base.join("config"), ssh, tag: tag.to_string() }
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

fn path_with(agent: &FakeAgent, provider: &FakeProvider, tag: &str) -> std::ffi::OsString {
    let bin = orq_test_support::process_temp_dir().join(format!("orq-resume-bin-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    // A session asked to repair a red gate changes nothing: these tests are about the implement session.
    std::fs::write(&claude, format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

fn run_with(w: &World, agent: &FakeAgent, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .args(extra)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", path_with(agent, &github(), &w.tag))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn set_gate(w: &World, command: &str) {
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"{command}\"]\nexplain_failures = false\n")).unwrap();
}

#[test]
fn a_run_that_stopped_after_the_agent_finished_is_continued_in_its_worktree_without_asking_the_agent_again() {
    let w = world("gate");
    set_gate(&w, "false");
    let first_agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let first = run_with(&w, &first_agent, &[]);
    assert_eq!(first.status.code(), Some(EXIT_FAILED), "{}", String::from_utf8_lossy(&first.stderr));
    let worktrees_before = git(w.repo.root(), &["worktree", "list"]);
    assert_eq!(worktrees_before.lines().count(), 2, "the stopped run keeps its worktree");

    // The gate is fixed; an agent asked a second time would leave a mark on the branch.
    set_gate(&w, "true");
    let second_agent = FakeAgent::builder().write_file("again.txt", "second\n").commit("feat: second session").build();
    let second = run_with(&w, &second_agent, &["--resume"]);

    let stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    assert_eq!(second.status.code(), Some(0), "{}\n{stdout}", String::from_utf8_lossy(&second.stderr));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    assert!(stdout.contains(&format!("review: {PR_URL}")), "{stdout}");
    let pushed = git(&w.remote, &["ls-tree", "-r", "--name-only", "fix--COM-01"]);
    assert!(pushed.contains("feature.txt"), "{pushed}");
    assert!(!pushed.contains("again.txt"), "the implement stage was not run again: {pushed}");
    assert_eq!(git(w.repo.root(), &["branch", "--list", "fix--COM-*"]).lines().count(), 1, "no second branch was made");
}

#[test]
fn a_run_blocked_on_the_agent_result_asks_the_agent_again_in_the_same_worktree() {
    let w = world("blocked");
    let lazy = FakeAgent::builder().write_file("notes.txt", "wip\n").build();
    let first = run_with(&w, &lazy, &[]);
    assert_eq!(first.status.code(), Some(30), "{}", String::from_utf8_lossy(&first.stderr));

    let diligent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let second = run_with(&w, &diligent, &["--resume"]);

    let stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    assert_eq!(second.status.code(), Some(0), "{}\n{stdout}", String::from_utf8_lossy(&second.stderr));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    assert_eq!(git(w.repo.root(), &["branch", "--list", "fix--COM-*"]).lines().count(), 1, "no second branch was made");
    let pushed = git(&w.remote, &["ls-tree", "-r", "--name-only", "fix--COM-01"]);
    assert!(pushed.contains("feature.txt"), "the agent ran again: {pushed}");
}

#[test]
fn resuming_is_refused_with_a_reason_when_there_is_nothing_to_pick_up() {
    let w = world("nothing");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let none = run_with(&w, &agent, &["--resume"]);
    assert_ne!(none.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&none.stderr).contains("no earlier run"), "{}", String::from_utf8_lossy(&none.stderr));

    let first = run_with(&w, &agent, &[]);
    assert_eq!(first.status.code(), Some(0), "{}", String::from_utf8_lossy(&first.stderr));
    let done = run_with(&w, &agent, &["--resume"]);
    assert_ne!(done.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&done.stderr).contains("nothing to resume"), "{}", String::from_utf8_lossy(&done.stderr));

    let both = run_with(&w, &agent, &["--resume", "--publish"]);
    assert_ne!(both.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&both.stderr).contains("cannot be combined"), "{}", String::from_utf8_lossy(&both.stderr));
}

#[test]
fn a_run_resumed_twice_still_remembers_that_the_agent_finished() {
    let w = world("twice");
    set_gate(&w, "false");
    let first_agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    assert_eq!(run_with(&w, &first_agent, &[]).status.code(), Some(EXIT_FAILED));
    let second_agent = FakeAgent::builder().write_file("again.txt", "second\n").commit("feat: second session").build();
    assert_eq!(run_with(&w, &second_agent, &["--resume"]).status.code(), Some(EXIT_FAILED), "the gate still fails");

    set_gate(&w, "true");
    let third = run_with(&w, &second_agent, &["--resume"]);

    assert_eq!(third.status.code(), Some(0), "{}", String::from_utf8_lossy(&third.stderr));
    let pushed = git(&w.remote, &["ls-tree", "-r", "--name-only", "fix--COM-01"]);
    assert!(!pushed.contains("again.txt"), "no agent session was started on either resume: {pushed}");
}

#[test]
fn resuming_does_not_need_a_free_branch_name() {
    let w = world("names");
    set_gate(&w, "false");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    assert_eq!(run_with(&w, &agent, &[]).status.code(), Some(EXIT_FAILED));
    // Every name a new run could take is used up; the stopped run's own branch is the one reused.
    for n in 2..=100 {
        git(w.repo.root(), &["branch", &format!("fix--COM-01-{n}"), "main"]);
    }

    set_gate(&w, "true");
    let second = run_with(&w, &agent, &["--resume"]);

    assert_eq!(second.status.code(), Some(0), "{}", String::from_utf8_lossy(&second.stderr));
}

#[test]
fn a_resume_that_fails_in_setup_still_remembers_that_the_agent_finished() {
    let w = world("setup-fails");
    set_gate(&w, "false");
    let first_agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    assert_eq!(run_with(&w, &first_agent, &[]).status.code(), Some(EXIT_FAILED));

    // The resumed run stops in setup, before it reaches any stage.
    std::fs::write(w.repo.root().join(".orq/run.toml"), "gate = [\"true\"]\nsetup = [[\"false\"]]\nexplain_failures = false\n").unwrap();
    let second_agent = FakeAgent::builder().write_file("again.txt", "second\n").commit("feat: second session").build();
    assert_eq!(run_with(&w, &second_agent, &["--resume"]).status.code(), Some(EXIT_FAILED), "setup fails");

    set_gate(&w, "true");
    let third = run_with(&w, &second_agent, &["--resume"]);

    assert_eq!(third.status.code(), Some(0), "{}", String::from_utf8_lossy(&third.stderr));
    let pushed = git(&w.remote, &["ls-tree", "-r", "--name-only", "fix--COM-01"]);
    assert!(!pushed.contains("again.txt"), "the agent was not asked again after a failed resume: {pushed}");
}
