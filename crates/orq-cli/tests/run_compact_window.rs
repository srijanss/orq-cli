use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";
const VARIABLE: &str = "CLAUDE_CODE_AUTO_COMPACT_WINDOW";

struct World {
    repo: GitFixture,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
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

fn world(tag: &str) -> World {
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let base = orq_test_support::process_temp_dir().join(format!("orq-compact-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    World { repo, state: base.join("state"), config: base.join("config"), ssh, base }
}

fn settings(w: &World, text: &str) {
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"true\"]\n{text}")).unwrap();
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

/// Runs `orq run`; every session the `claude` shim starts appends the window variable it sees to the returned file.
fn run(w: &World) -> (Output, PathBuf) {
    run_with_fix(w, "exit 0")
}

/// As `run`, but a session asked to repair a red gate runs `fix` in the worktree.
fn run_with_fix(w: &World, fix: &str) -> (Output, PathBuf) {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let seen = w.base.join("seen");
    let bin = w.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, format!(
            "#!/bin/sh\necho \"[${VARIABLE}]\" >> '{}'\ncase \"$*\" in *'local gate failed'*) {fix};; esac\nexec '{}' \"$@\"\n",
            seen.display(),
            agent.path().display()
        )).unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let out = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("ORQ_COMPACT_WINDOW")
        .env_remove(VARIABLE)
        .output()
        .unwrap();
    (out, seen)
}

fn lines(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file).map(|text| text.lines().map(str::to_string).collect()).unwrap_or_default()
}

#[test]
fn the_implement_session_is_told_the_configured_compact_window() {
    let w = world("setting");
    settings(&w, "compact_window = 150000\n");

    let (out, seen) = run(&w);

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let seen = lines(&seen);
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|line| line == "[150000]"), "every session got the window: {seen:?}");
}

#[test]
fn without_a_setting_sessions_are_left_to_their_own_default() {
    let w = world("default");
    settings(&w, "");

    let (out, seen) = run(&w);

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let seen = lines(&seen);
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|line| line == "[]"), "{seen:?}");
}

#[test]
fn an_unusable_window_is_refused_before_anything_starts() {
    let w = world("bad");
    settings(&w, "compact_window = 0\n");

    let (out, seen) = run(&w);

    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("compact_window"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(lines(&seen).is_empty(), "no session was started");
}

#[test]
fn the_repair_session_for_a_red_gate_is_told_the_window_too() {
    let w = world("repair");
    std::fs::write(w.repo.root().join(".orq/run.toml"), "gate = [\"test\", \"-f\", \"fixed\"]\ncompact_window = 150000\n").unwrap();
    let repair = "touch fixed; git add fixed; git -c user.name=t -c user.email=t@example.com commit -q -m 'fix: repair the gate'; exit 0";

    let (out, seen) = run_with_fix(&w, repair);

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let seen = lines(&seen);
    assert_eq!(seen.len(), 3, "the implement session, the repair session and the draft: {seen:?}");
    assert!(seen.iter().all(|line| line == "[150000]"), "{seen:?}");
}
