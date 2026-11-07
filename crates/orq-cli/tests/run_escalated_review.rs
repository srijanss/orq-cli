//! A run whose implement session stopped because its review loop escalated has not failed: it
//! waits for a human to review again, accept or discard, with its worktree and feature as they are.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

struct World {
    repo: GitFixture,
    _remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    bin: PathBuf,
    prompts: PathBuf,
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

fn status_of(repo: &Path, name: &str) -> String {
    let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(repo.join(".tdd-features.json")).unwrap()).unwrap();
    let entry = ledger.as_array().unwrap().iter().find(|e| e["featureName"] == name).unwrap();
    entry["status"].as_str().unwrap().to_string()
}

/// A repository with a pushed `main` and a pending COM-01, and a `claude` that runs `agent`, then
/// leaves a review ledger whose one review reached round `round` with a high finding open, and
/// reports a problem in its finish marker, as a session does when its review loop ends early.
fn world(tag: &str, agent: &FakeAgent, round: Option<u32>) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-escalated-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "explain_failures = false\n").unwrap();
    let ssh = root.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let ledger = match round {
        Some(round) => format!(
            concat!(
                "base=$(git rev-parse HEAD~1); head=$(git rev-parse HEAD)\n",
                "printf '{{\"review:%s\":[{{\"start\":\"%s\",\"head\":\"%s\",\"reviewer\":\"pi\",\"model\":\"m\",\"round\":{round},",
                "\"recordedAt\":\"2026-11-07T12:00:00+00:00\",\"findings\":[{{\"id\":\"leak\",\"severity\":\"high\",\"status\":\"open\",",
                "\"claim\":\"the token reaches the log\"}}]}}]}}' \"$base\" \"$base\" \"$head\" > .tdd-review-rounds.json\n",
                "printf '{{\"review:%s\":[{{\"id\":\"leak\",\"severity\":\"high\",\"status\":\"open\"}}]}}' \"$base\" > .tdd-review-findings.json\n",
            ),
            round = round
        ),
        None => String::new(),
    };
    let claude = root.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            concat!(
                "#!/bin/sh\n",
                "for a; do last=$a; done\n",
                "printf '%s\\n--\\n' \"$last\" >> '{prompts}'\n",
                "sed -i 's/\"pending\"/\"in_progress\"/' '{features}'\n",
                "'{agent}' \"$@\" || exit $?\n",
                "for a; do last=$a; done\n",
                "marker=$(printf '%s' \"$last\" | grep -o '/[^ `]*implement\\.done' | head -n 1)\n",
                "{ledger}",
                "printf 'the review escalated after 3 rounds' > \"$marker\"\n",
            ),
            features = repo.root().join(".tdd-features.json").display(),
            prompts = root.join("prompts.log").display(),
            agent = agent.path().display(),
            ledger = ledger,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    World { repo, _remote: remote, state: root.join("state"), config: root.join("config"), ssh, bin: root.join("bin"), prompts: root.join("prompts.log") }
}

fn run(w: &World) -> Output {
    orq(w, &["run", "--repo", w.repo.root().to_str().unwrap(), "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
}

fn orq(w: &World, args: &[&str]) -> Output {
    let provider = FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    let mut dirs = vec![w.bin.clone(), provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

fn worktrees(repo: &Path) -> usize {
    git(repo, &["worktree", "list", "--porcelain"]).lines().filter(|l| l.starts_with("worktree ")).count()
}

#[test]
fn a_session_whose_review_escalated_leaves_the_run_waiting_for_a_review_decision() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("escalated", &agent, Some(3));

    let output = run(&w);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: needs_review"), "{stdout}\n{stderr}");
    assert!(stderr.contains("the review escalated after 3 rounds with 1 finding open"), "{stderr}");
    assert_ne!(output.status.code(), Some(0), "a run waiting on a human is not a success");
    assert_eq!(status_of(w.repo.root(), "COM-01"), "in_progress", "the feature stays with the run");
    assert_eq!(worktrees(w.repo.root()), 2, "the worktree is kept for the decision");
}

#[test]
fn a_session_that_reported_a_problem_without_an_escalated_review_still_fails() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("not-escalated", &agent, Some(1));

    let output = run(&w);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(stdout.contains("state: failed"), "{stdout}\n{stderr}");
    assert!(stderr.contains("the implement session reported a problem"), "{stderr}");
}

/// The commit the run's worktree is at.
fn worktree_head(repo: &Path) -> String {
    let list = git(repo, &["worktree", "list", "--porcelain"]);
    let linked = list.split("\n\n").nth(1).expect("the run's worktree");
    linked.lines().find_map(|l| l.strip_prefix("HEAD ")).unwrap().to_string()
}

#[test]
fn resuming_a_run_waiting_for_a_review_decision_reviews_again_from_the_escalated_head() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("review-again", &agent, Some(3));
    let first = run(&w);
    assert!(text(&first.stdout).contains("state: needs_review"), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let reviewed = worktree_head(w.repo.root());
    let run_id = std::fs::read_dir(w.state.join("orq/runs")).unwrap().next().unwrap().unwrap().file_name().into_string().unwrap();

    let resumed = orq(&w, &["run", "resume", &run_id, "--yes", "--no-ci-check", "--repo", w.repo.root().to_str().unwrap()]);

    let prompts = std::fs::read_to_string(&w.prompts).unwrap();
    let second = prompts.split("\n--\n").filter(|p| !p.is_empty()).nth(1).unwrap_or_else(|| panic!("no second session\n{}\n{}", text(&resumed.stdout), text(&resumed.stderr)));
    assert!(second.starts_with(&format!("/review-and-fix {reviewed}..HEAD --reviewer ")), "{second}");
    assert!(second.contains("leak"), "{second}");
    assert!(second.contains("the token reaches the log"), "{second}");
}
