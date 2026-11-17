//! A fix session reads the run's logs from copies inside its own worktree, `.claude/orq-run/`, so it
//! needs nothing outside the worktree and no `--add-dir`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
    /// Where the fake fix session leaves what it saw, one numbered file per attempt.
    seen: PathBuf,
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
    let base = orq_test_support::process_temp_dir().join(format!("orq-fix-logs-in-worktree-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    std::fs::create_dir_all(base.join("seen")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    World { repo, state: base.join("state"), config: base.join("config"), ssh, seen: base.join("seen"), base }
}

fn settings(w: &World, text: &str) {
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), text).unwrap();
}

fn github() -> FakeProvider {
    github_with_checks(None)
}

/// A forge whose checks on the pushed branch answer `checks`, when given.
fn github_with_checks(checks: Option<&str>) -> FakeProvider {
    let builder = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"));
    match checks {
        Some(checks) => builder.reply(&["api"], checks).build(),
        None => builder.build(),
    }
}

const RED_CHECKS: &str = "{\"total_count\":1,\"check_runs\":[{\"name\":\"deploy\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";

/// A `claude` that runs the implement session as `agent`, as the failure explainer records what it
/// was given and could read, and as a fix session (told by
/// `ORQ_UNRELATED_FAILURE_FILE`) records what it was given and could read, then runs `then`.
fn fake_claude(w: &World, agent: &FakeAgent, then: &str) -> PathBuf {
    let bin = w.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let seen = w.seen.display();
    let script = format!(
        "#!/bin/sh\n\
         case \"$*\" in *'orq run of this worktree has failed'*)\n\
           printf '%s\\n' \"$@\" > '{seen}'/explain-argv\n\
           ls -A .claude/orq-run > '{seen}'/explain-listing 2>/dev/null\n\
           git status --porcelain > '{seen}'/explain-status.tmp\n\
           mv '{seen}'/explain-status.tmp '{seen}'/explain-status\n\
           exit 0;;\n\
         esac\n\
         if [ -z \"$ORQ_UNRELATED_FAILURE_FILE\" ]; then exec '{agent}' \"$@\"; fi\n\
         n=$(ls '{seen}' | grep -c '^argv-')\n\
         n=$((n + 1))\n\
         printf '%s\\n' \"$@\" > '{seen}'/argv-$n\n\
         pwd > '{seen}'/cwd-$n\n\
         printf '%s\\n' \"$ORQ_UNRELATED_FAILURE_FILE\" > '{seen}'/report-path-$n\n\
         ls -A .claude/orq-run > '{seen}'/listing-$n 2>/dev/null\n\
         cp .claude/orq-run/gate.log '{seen}'/gate-log-$n 2>/dev/null\n\
         cat .claude/orq-run/ci-failure-*.log > '{seen}'/ci-log-$n 2>/dev/null\n\
         git status --porcelain > '{seen}'/status-$n\n\
         {then}\n",
        agent = agent.path().display(),
    );
    // pi is the same session under its own name.
    for name in ["claude", "pi"] {
        std::fs::write(bin.join(name), &script).unwrap();
        std::fs::set_permissions(bin.join(name), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    bin
}

fn run(w: &World, agent: &FakeAgent, then: &str, flags: &[&str]) -> Output {
    run_with(w, agent, &github(), then, flags)
}

fn run_with(w: &World, agent: &FakeAgent, provider: &FakeProvider, then: &str, flags: &[&str]) -> Output {
    let mut dirs = vec![fake_claude(w, agent, then), provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main"])
        .args(flags)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn seen(w: &World, name: &str) -> String {
    std::fs::read_to_string(w.seen.join(name)).unwrap_or_else(|_| panic!("the fix session left no {name}"))
}

fn implemented() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build()
}

/// A gate that prints which run of it this is and fails.
fn failing_gate(w: &World) {
    settings(w, "gate = [\"sh\", \"-c\", \"echo the gate said boom; false\"]\n");
}

const UNRELATED: &str = "echo 'billing clock test is flaky' > \"$ORQ_UNRELATED_FAILURE_FILE\"";

#[test]
fn a_gate_fix_session_reads_the_gate_log_from_its_worktree_and_gets_no_add_dir() {
    let w = world("gate-log-copy");
    failing_gate(&w);

    let out = run(&w, &implemented(), UNRELATED, &["--no-ci-check"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    let argv = seen(&w, "argv-1");
    assert!(argv.contains(".claude/orq-run/gate.log"), "the task names the copy: {argv}\n{stderr}");
    assert!(!argv.lines().any(|a| a == "--add-dir"), "{argv}");
    assert!(seen(&w, "gate-log-1").contains("the gate said boom"), "the copy holds the gate's log");
}

#[test]
fn the_report_a_gate_fix_session_leaves_in_its_worktree_is_honoured_and_the_copies_go_with_the_session() {
    let w = world("gate-report");
    failing_gate(&w);

    let out = run(&w, &implemented(), UNRELATED, &["--no-ci-check"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    let report = seen(&w, "report-path-1");
    assert!(report.trim_end().ends_with(".claude/orq-run/unrelated-failure.txt"), "{report}");
    assert!(report.starts_with(seen(&w, "cwd-1").trim_end()), "the report is in the session's worktree: {report}");
    assert!(stderr.contains("billing clock test is flaky"), "the report is honoured: {stderr}");
    assert_eq!(seen(&w, "status-1"), "", "git sees no copies");
    let worktree = seen(&w, "cwd-1");
    assert!(!Path::new(worktree.trim_end()).join(".claude/orq-run").exists(), "the copies are removed after the session");
    let run_logs = std::fs::read_dir(w.state.join("orq/logs")).unwrap().next().unwrap().unwrap().path();
    assert!(std::fs::read_to_string(run_logs.join("gate.log")).unwrap().contains("the gate said boom"), "the original stays");
    assert_eq!(std::fs::read_to_string(run_logs.join("unrelated-failure.txt")).unwrap(), "billing clock test is flaky\n", "a copy of the report is kept");
}

#[test]
fn a_ci_fix_session_reads_the_ci_failure_from_its_worktree() {
    let w = world("ci-log-copy");
    settings(&w, "gate = [\"true\"]\n");

    let out = run_with(&w, &implemented(), &github_with_checks(Some(RED_CHECKS)), UNRELATED, &[]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    let argv = seen(&w, "argv-1");
    assert!(argv.contains(".claude/orq-run"), "the task names the copy: {argv}\n{stderr}");
    assert!(!argv.lines().any(|a| a == "--add-dir"), "{argv}");
    assert!(seen(&w, "listing-1").starts_with("ci-failure-"), "{}", seen(&w, "listing-1"));
    assert!(seen(&w, "ci-log-1").contains("deploy"), "the copy names the failing check");
    assert!(seen(&w, "report-path-1").trim_end().ends_with(".claude/orq-run/unrelated-failure.txt"));
    assert!(stderr.contains("billing clock test is flaky"), "the report is honoured: {stderr}");
    assert_eq!(seen(&w, "status-1"), "");
    assert!(!Path::new(seen(&w, "cwd-1").trim_end()).join(".claude/orq-run").exists());
}

#[test]
fn the_failure_explainer_reads_the_runs_logs_from_its_worktree() {
    let w = world("explain");
    let failing = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").exit_code(3).build();

    let out = run(&w, &failing, "", &["--no-ci-check"]);

    assert_ne!(out.status.code(), Some(0));
    let started = std::time::Instant::now();
    while !w.seen.join("explain-status").exists() {
        assert!(started.elapsed().as_secs() < 60, "no explainer: {}", String::from_utf8_lossy(&out.stderr));
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let argv = seen(&w, "explain-argv");
    assert!(argv.contains(".claude/orq-run"), "the prompt names the copies: {argv}");
    assert!(!argv.lines().any(|a| a == "--add-dir"), "{argv}");
    let listing = seen(&w, "explain-listing");
    assert!(listing.lines().any(|name| name.ends_with(".log")), "the run's logs are copied: {listing}");
    assert!(!listing.lines().any(|name| name == "explain.log"), "the explainer's own log is not: {listing}");
    assert_eq!(seen(&w, "explain-status"), "", "git sees no copies");
}

#[test]
fn a_second_attempt_gets_a_fresh_copy_of_the_gate_log_and_a_tracked_claude_folder_is_left_as_it_was() {
    let w = world("second-attempt");
    // The project tracks a file in `.claude/` and already keeps the copies' folder out of git.
    std::fs::create_dir_all(w.repo.root().join(".claude")).unwrap();
    std::fs::write(w.repo.root().join(".claude/settings.json"), "{}\n").unwrap();
    git(w.repo.root(), &["add", ".claude/settings.json"]);
    git(w.repo.root(), &["-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-qm", "claude settings"]);
    let ssh = format!("core.sshCommand={}", w.ssh.display());
    git(w.repo.root(), &["-c", &ssh, "push", "--quiet", "origin", "main"]);
    let exclude = w.repo.root().join(".git/info/exclude");
    std::fs::write(&exclude, "/.claude/orq-run\n").unwrap();
    // Each run of the gate says which it is.
    let count = w.base.join("gate-runs");
    settings(&w, &format!("gate = [\"sh\", \"-c\", \"echo run >> '{0}'; echo gate run $(wc -l < '{0}'); false\"]\n", count.display()));
    // The first session leaves a file among its copies and fixes nothing.
    let then = "touch .claude/orq-run/left-over.txt";

    let out = run(&w, &implemented(), then, &["--no-ci-check"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    let second = seen(&w, "gate-log-2");
    assert!(second.contains("gate run 2"), "the second session gets a fresh copy of the gate log: {second}\n{stderr}");
    assert!(!seen(&w, "gate-log-1").contains("gate run 2"));
    assert!(!seen(&w, "listing-2").contains("left-over.txt"), "an earlier copy is replaced");
    assert_eq!(seen(&w, "status-1"), "", "git sees no copies");
    assert_eq!(seen(&w, "status-2"), "");
    let worktree = seen(&w, "cwd-1");
    assert_eq!(std::fs::read_to_string(Path::new(worktree.trim_end()).join(".claude/settings.json")).unwrap(), "{}\n");
    assert_eq!(std::fs::read_to_string(&exclude).unwrap().matches("orq-run").count(), 1, "the exclude line is not repeated");
}

#[test]
fn a_pi_fix_session_reads_the_same_copies_from_its_worktree() {
    let w = world("pi");
    failing_gate(&w);
    std::fs::write(w.repo.root().join(".orq.toml"), "[models.team-pi]\nharness = \"pi\"\nmodel = \"team/model\"\n").unwrap();

    let out = run(&w, &implemented(), UNRELATED, &["--no-ci-check", "--implementer", "team-pi"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    let argv = seen(&w, "argv-1");
    assert!(argv.contains(".claude/orq-run/gate.log"), "the task names the copy: {argv}\n{stderr}");
    assert!(!argv.lines().any(|a| a == "--add-dir"), "{argv}");
    assert!(seen(&w, "gate-log-1").contains("the gate said boom"));
    assert!(seen(&w, "report-path-1").trim_end().ends_with(".claude/orq-run/unrelated-failure.txt"));
    assert!(stderr.contains("billing clock test is flaky"), "the report is honoured: {stderr}");
    assert_eq!(seen(&w, "status-1"), "");
}
