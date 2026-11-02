//! MVP2 acceptance: `orq run` end to end against a fixture repository, a bare remote, a fake
//! agent and a fake `gh`/`glab`. A run never merges, whatever the forge, the target or the ending.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const EXIT_BLOCKED: i32 = 30;
const EXIT_FAILED: i32 = 31;

const PR_URL: &str = "https://github.example/o/r/pull/7";
const MR_URL: &str = "https://gitlab.example/o/r/-/merge_requests/3";
const RUN_BRANCH: &str = "fix--COM-01";

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

struct World {
    repo: GitFixture,
    remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    bin: PathBuf,
}

/// A repository with `main` and `release` pushed, a pending COM-01, and a remote the review
/// stage can name.
fn world(tag: &str) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["branch", "release"]);
    git(repo.root(), &["push", "--quiet", "origin", "main", "release"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let base = std::env::temp_dir().join(format!("orq-accept-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    std::fs::create_dir_all(base.join("bin")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    make_executable(&ssh);
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    World { repo, remote, state: base.join("state"), config: base.join("config"), ssh, bin: base.join("bin") }
}

fn make_executable(path: &Path) {
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

fn gitlab() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["mr", "list"], "[]\n")
        .reply(&["mr", "create"], &format!("{MR_URL}\n"))
        .reply(&["mr", "view"], &format!("{{\"iid\":3,\"web_url\":\"{MR_URL}\"}}\n"))
        .build()
}

/// `orq run` for COM-01 with `claude` standing for the fake agent and the provider's CLIs first
/// on PATH; `extra_bin` holds any further fake programs (a failing `cargo`, say).
fn run(w: &World, agent: &FakeAgent, provider: &FakeProvider, forge: &str, target: &str, extra: &[&str]) -> Output {
    let claude = w.bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\nexec '{}' \"$@\"\n", agent.path().display())).unwrap();
    make_executable(&claude);
    let mut dirs = vec![w.bin.clone(), provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", target])
        .args(extra)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", forge)
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn feature_agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build()
}

/// The remote's branches, so a test can see what a run left there.
fn remote_branches(w: &World) -> Vec<String> {
    git(&w.remote, &["for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .lines()
        .map(str::to_string)
        .collect()
}

fn remote_sha(w: &World, branch: &str) -> String {
    git(&w.remote, &["rev-parse", branch]).trim().to_string()
}

/// A run never merges: no provider call is a merge, and the target branches on the remote are
/// exactly where they were.
fn assert_nothing_merged(provider: &FakeProvider, w: &World, before: &(String, String)) {
    for call in provider.calls() {
        assert!(
            !call.args.iter().any(|a| a == "merge" || a == "--auto" || a.contains("merge_when")),
            "a merge was invoked: {call:?}"
        );
    }
    assert_eq!(&remote_sha(w, "main"), &before.0, "main moved");
    assert_eq!(&remote_sha(w, "release"), &before.1, "release moved");
}

fn targets(w: &World) -> (String, String) {
    (remote_sha(w, "main"), remote_sha(w, "release"))
}

/// The value following `flag` in the one provider call whose first two words are `verb`.
fn flag_of(provider: &FakeProvider, verb: [&str; 2], flag: &str) -> String {
    let call = provider
        .calls()
        .into_iter()
        .find(|c| c.args.len() > 1 && c.args[0] == verb[0] && c.args[1] == verb[1])
        .unwrap_or_else(|| panic!("no {verb:?} call"));
    let at = call.args.iter().position(|a| a == flag).unwrap_or_else(|| panic!("no {flag} in {call:?}"));
    call.args[at + 1].clone()
}

#[test]
fn a_github_run_against_main_and_against_another_branch_opens_a_pull_request_into_that_branch() {
    for target in ["main", "release"] {
        let w = world(&format!("gh-{target}"));
        let before = targets(&w);
        let provider = github();

        let output = run(&w, &feature_agent(), &provider, "gh", target, &["--no-ci-check"]);

        assert_eq!(output.status.code(), Some(0), "{}\n{}", stderr(&output), stdout(&output));
        assert!(stdout(&output).contains("state: review_ready"), "{}", stdout(&output));
        assert!(stdout(&output).contains(&format!("review: {PR_URL}")), "{}", stdout(&output));
        assert_eq!(flag_of(&provider, ["pr", "create"], "--base"), target);
        assert_eq!(flag_of(&provider, ["pr", "create"], "--head"), RUN_BRANCH);
        assert!(remote_branches(&w).contains(&RUN_BRANCH.to_string()), "the run branch was pushed");
        assert_nothing_merged(&provider, &w, &before);
    }
}

#[test]
fn a_gitlab_run_against_main_and_against_another_branch_opens_a_merge_request_into_that_branch() {
    for target in ["main", "release"] {
        let w = world(&format!("gl-{target}"));
        let before = targets(&w);
        let provider = gitlab();

        let output = run(&w, &feature_agent(), &provider, "glab", target, &["--no-ci-check"]);

        assert_eq!(output.status.code(), Some(0), "{}\n{}", stderr(&output), stdout(&output));
        assert!(stdout(&output).contains("state: review_ready"), "{}", stdout(&output));
        assert!(stdout(&output).contains(&format!("review: {MR_URL}")), "{}", stdout(&output));
        assert_eq!(flag_of(&provider, ["mr", "create"], "--target-branch"), target);
        assert_eq!(flag_of(&provider, ["mr", "create"], "--source-branch"), RUN_BRANCH);
        assert!(remote_branches(&w).contains(&RUN_BRANCH.to_string()), "the run branch was pushed");
        assert_nothing_merged(&provider, &w, &before);
    }
}

#[test]
fn a_green_ci_on_github_still_ends_at_review_ready_without_a_merge() {
    let w = world("gh-green");
    let before = targets(&w);
    let green = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"success\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply_once(&["api"], green)
        .reply(&["api"], "{\"state\":\"success\",\"total_count\":0,\"statuses\":[]}\n")
        .build();

    let output = run(&w, &feature_agent(), &provider, "gh", "main", &[]);

    assert_eq!(output.status.code(), Some(0), "{}\n{}", stderr(&output), stdout(&output));
    assert!(stdout(&output).contains("state: review_ready"), "{}", stdout(&output));
    assert_nothing_merged(&provider, &w, &before);
}

/// A stopped run leaves a review nowhere: nothing but the base is on the remote, no pull request
/// was asked for, and nothing merged.
fn assert_stopped_before_any_review(provider: &FakeProvider, w: &World, before: &(String, String)) {
    assert!(!remote_branches(w).contains(&RUN_BRANCH.to_string()), "nothing was pushed");
    assert!(
        !provider.calls().iter().any(|c| c.args.first().is_some_and(|a| a == "pr" || a == "mr")),
        "no review was requested: {:?}",
        provider.calls()
    );
    assert_nothing_merged(provider, w, before);
}

#[test]
fn an_agent_that_commits_nothing_stops_the_run_for_both_forges() {
    for (forge, provider) in [("gh", github()), ("glab", gitlab())] {
        let w = world(&format!("nocommit-{forge}"));
        let before = targets(&w);
        let agent = FakeAgent::builder().write_file("notes.txt", "wip\n").build();

        let output = run(&w, &agent, &provider, forge, "main", &["--no-ci-check"]);

        assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{}\n{}", stderr(&output), stdout(&output));
        assert!(stdout(&output).contains("state: blocked"), "{}", stdout(&output));
        assert_stopped_before_any_review(&provider, &w, &before);
    }
}

#[test]
fn an_agent_that_leaves_the_worktree_dirty_stops_the_run_for_both_forges() {
    for (forge, provider) in [("gh", github()), ("glab", gitlab())] {
        let w = world(&format!("dirty-{forge}"));
        let before = targets(&w);
        let agent = FakeAgent::builder()
            .write_file("feature.txt", "done\n")
            .commit("feat: add the feature")
            .write_file("scratch.txt", "left behind\n")
            .build();

        let output = run(&w, &agent, &provider, forge, "main", &["--no-ci-check"]);

        assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{}\n{}", stderr(&output), stdout(&output));
        assert!(stdout(&output).contains("state: blocked"), "{}", stdout(&output));
        assert_stopped_before_any_review(&provider, &w, &before);
        assert_eq!(git(w.repo.root(), &["worktree", "list"]).lines().count(), 2, "the worktree is kept");
    }
}

#[test]
fn a_red_gate_stops_the_run_before_anything_is_pushed_for_both_forges() {
    for (forge, provider) in [("gh", github()), ("glab", gitlab())] {
        let w = world(&format!("redgate-{forge}"));
        // A Rust project is gated by `cargo test`, and this `cargo` fails.
        std::fs::write(w.repo.root().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        git(w.repo.root(), &["add", "Cargo.toml"]);
        git(w.repo.root(), &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "add a manifest"]);
        git(w.repo.root(), &["push", "--quiet", &w.remote.display().to_string(), "main"]);
        let before = targets(&w);
        let cargo = w.bin.join("cargo");
        std::fs::write(&cargo, "#!/bin/sh\necho 'test result: FAILED' >&2\nexit 101\n").unwrap();
        make_executable(&cargo);

        let output = run(&w, &feature_agent(), &provider, forge, "main", &["--no-ci-check"]);

        assert_eq!(output.status.code(), Some(EXIT_FAILED), "{}\n{}", stderr(&output), stdout(&output));
        assert!(stdout(&output).contains("state: failed"), "{}", stdout(&output));
        assert!(stderr(&output).contains("gate"), "{}", stderr(&output));
        assert_stopped_before_any_review(&provider, &w, &before);
    }
}

#[test]
fn a_red_ci_fails_the_run_but_reports_the_review_and_merges_nothing_on_github() {
    let w = world("redci");
    let before = targets(&w);
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();

    let output = run(&w, &feature_agent(), &provider, "gh", "main", &[]);

    assert_eq!(output.status.code(), Some(EXIT_FAILED), "{}\n{}", stderr(&output), stdout(&output));
    assert!(stdout(&output).contains("state: failed"), "{}", stdout(&output));
    assert!(stdout(&output).contains(&format!("review: {PR_URL}")), "{}", stdout(&output));
    assert_nothing_merged(&provider, &w, &before);
}
