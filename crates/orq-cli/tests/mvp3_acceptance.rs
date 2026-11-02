//! MVP3 acceptance: two repositories share one state directory, each starts a detached run of the
//! same feature at the same time, and each sees only its own run. The launchers have exited by then
//! (as when a popup closes), and both runs still finish.

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

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

struct Project {
    #[allow(dead_code)] // keeps the fixture (and its cleanup) alive
    repo: GitFixture,
    /// The repository, in a directory named for the project (the project's name is the directory's).
    root: PathBuf,
    remote: PathBuf,
}

fn project(name: &str) -> Project {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let root = repo.root().with_file_name(name);
    std::fs::rename(repo.root(), &root).unwrap();
    Project { repo, root, remote }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !done() {
        assert!(started.elapsed().as_secs() < 60, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn two_projects_run_the_same_feature_at_once_and_each_sees_only_its_own_run() {
    let base = std::env::temp_dir().join(format!("orq-mvp3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    std::fs::create_dir_all(base.join("bin")).unwrap();
    let (state, config) = (base.join("state"), base.join("config"));
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    // The agent takes a moment, so both runs are going at once when the launchers return.
    let claude = base.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -p ]; then printf 'Add the feature\\n\\nThe feature.\\n'; exit 0; fi\nsleep 2\nexec '{}' \"$@\"\n",
            agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![base.join("bin"), provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();

    let projects = [project("alpha"), project("beta")];
    let launch = |p: &Project| {
        let output = Command::new(env!("CARGO_BIN_EXE_orq"))
            .args(["run", "--repo"])
            .arg(&p.root)
            .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--detach"])
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("ORQ_FORGE", "gh")
            .env("GIT_SSH_COMMAND", &ssh)
            .env("PATH", &path)
            .env_remove("HERDR_ACTIVE_PANE_CWD")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", String::from_utf8_lossy(&output.stderr));
        stdout.lines().find_map(|l| l.strip_prefix("run: ")).expect(&stdout).to_string()
    };
    let first = launch(&projects[0]);
    let second = launch(&projects[1]);
    assert_ne!(first, second);

    // Each project lists exactly its own run.
    let listed = |p: &Project| -> Vec<String> {
        let output = Command::new(env!("CARGO_BIN_EXE_orq"))
            .args(["runs", "--repo"])
            .arg(&p.root)
            .arg("--json")
            .env("XDG_STATE_HOME", &state)
            .env_remove("HERDR_ACTIVE_PANE_CWD")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        value["runs"].as_array().unwrap().iter().map(|r| r["run_id"].as_str().unwrap().to_string()).collect()
    };
    for (p, run_id) in [(&projects[0], &first), (&projects[1], &second)] {
        wait_until("the run to be listed", || !listed(p).is_empty());
        assert_eq!(listed(p), vec![run_id.clone()]);
    }

    // Both finish on their own, though both launchers have exited.
    let store = orq_orchestrator::RunStore::new(&state.join("orq"));
    for run_id in [&first, &second] {
        wait_until("a detached run to complete", || {
            store.events(run_id).map(|e| e.iter().any(|e| e.kind == "worktree.removed")).unwrap_or(false)
        });
        let kinds: Vec<String> = store.events(run_id).unwrap().into_iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&"review.created".to_string()), "{run_id}: {kinds:?}");
    }
    for p in &projects {
        assert!(git(&p.remote, &["branch", "--list"]).contains("fix--COM-01"), "the run branch was pushed to its own remote");
    }
}
