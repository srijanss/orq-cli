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
    let base = orq_test_support::process_temp_dir().join(format!("orq-worktree-db-env-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    World { repo, state: base.join("state"), config: base.join("config"), ssh, base }
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

/// A `claude` that appends the database name it was started with to `seen` (the first line is the
/// implement session), then behaves as `agent`.
fn path_with(w: &World, agent: &FakeAgent, provider: &FakeProvider, seen: &Path) -> std::ffi::OsString {
    let bin = w.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\nprintf '%s\\n' \"$DB_NAME\" >> '{}'\nexec '{}' \"$@\"\n", seen.display(), agent.path().display()))
        .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

fn run(w: &World, agent: &FakeAgent, seen: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", path_with(w, agent, &github(), seen))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

#[test]
fn setup_the_agent_and_the_gate_all_see_the_databases_named_for_the_runs_branch() {
    let w = world("all");
    std::fs::write(w.repo.root().join(".env"), "DB_NAME=shop\nSECRET=hunter2\n").unwrap();
    let (setup_seen, agent_seen, gate_seen) = (w.base.join("setup-seen"), w.base.join("agent-seen"), w.base.join("gate-seen"));
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(
        w.repo.root().join(".orq/run.toml"),
        format!(
            "setup = [[\"sh\", \"-c\", \"printf %s \\\"$DB_NAME\\\" > {}\"]]\ngate = [\"sh\", \"-c\", \"printf %s \\\"$DB_NAME\\\" > {}\"]\n",
            setup_seen.display(),
            gate_seen.display()
        ),
    )
    .unwrap();
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &agent_seen);

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{}\n{stdout}", String::from_utf8_lossy(&out.stderr));
    for (what, file) in [("setup", &setup_seen), ("the agent", &agent_seen), ("the gate", &gate_seen)] {
        let seen = std::fs::read_to_string(file).unwrap_or_default();
        assert_eq!(seen.lines().next(), Some("shop_fix_com_01"), "{what}");
    }
    assert!(!stdout.contains("hunter2"), "no other value of the dotenv file is shown: {stdout}");
}

/// A `claude` for a world whose gate is red until `fixed` exists. A fix session (asked about the
/// failed gate) must be headless (`-p`); it records the database it saw, creates `fixed`, and commits
/// it only if `commit`. Any other session behaves as `agent`.
fn fixing_path(w: &World, agent: &FakeAgent, provider: &FakeProvider, fix_seen: &Path, commit: bool) -> std::ffi::OsString {
    let bin = w.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let commit = if commit { "git add fixed; git -c user.name=t -c user.email=t@example.invalid commit -q -m 'fix: repair the gate'; " } else { "" };
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\ncase \"$*\" in *'local gate failed'*) case \" $* \" in *' -p '*) ;; *) exit 3;; esac; printf '%s\\n' \"$DB_NAME\" >> '{}'; touch fixed; {commit}exit 0;; esac\n[ -e '{}' ] && exit 0\n: > '{}'\nexec '{}' \"$@\"\n",
            fix_seen.display(),
            w.base.join("implemented").display(),
            w.base.join("implemented").display(),
            agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

/// A world whose gate records the database it sees on every run and is red until `fixed` exists.
fn red_gate_world(tag: &str) -> (World, PathBuf, PathBuf) {
    let w = world(tag);
    std::fs::write(w.repo.root().join(".env"), "DB_NAME=shop\n").unwrap();
    let (gate_seen, fix_seen) = (w.base.join("gate-seen"), w.base.join("fix-seen"));
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(
        w.repo.root().join(".orq/run.toml"),
        format!("gate = [\"sh\", \"-c\", \"printf '%s\\\\n' \\\"$DB_NAME\\\" >> {}; test -f fixed\"]\n", gate_seen.display()),
    )
    .unwrap();
    (w, gate_seen, fix_seen)
}

fn run_with_path(w: &World, path: std::ffi::OsString, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .args(extra)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_red_gate_starts_a_headless_fix_session_and_both_it_and_the_rerun_gate_see_the_run_environment() {
    let (w, gate_seen, fix_seen) = red_gate_world("gate-fix");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = fixing_path(&w, &agent, &provider, &fix_seen, true);

    let out = run_with_path(&w, path, &[]);

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{}\n{stdout}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&gate_seen).unwrap(), "shop_fix_com_01\nshop_fix_com_01\n", "the gate ran twice");
    assert_eq!(std::fs::read_to_string(&fix_seen).unwrap(), "shop_fix_com_01\n", "one fix session");
    let branch = git_out(w.repo.root(), &["branch", "--list", "fix--COM-*"]).trim().trim_start_matches(['*', '+', ' ']).to_string();
    let tree = git_out(w.repo.root(), &["ls-tree", "-r", "--name-only", &branch]);
    assert!(tree.contains("fixed"), "the repair is part of the branch: {tree}");
}

#[test]
fn a_repair_left_uncommitted_blocks_the_run_instead_of_publishing_the_unfixed_commit() {
    let (w, _gate_seen, fix_seen) = red_gate_world("gate-fix-uncommitted");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let provider = github();
    let path = fixing_path(&w, &agent, &provider, &fix_seen, false);

    let out = run_with_path(&w, path, &[]);

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_ne!(out.status.code(), Some(0), "{stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stdout.contains("state: blocked") && stderr.contains("uncommitted"), "{stdout}\n{stderr}");
    assert!(!stdout.contains(PR_URL), "no review was opened: {stdout}");
}

#[test]
fn publishing_an_existing_branch_never_starts_a_fix_session() {
    let (w, _gate_seen, fix_seen) = red_gate_world("gate-fix-publish");
    git(w.repo.root(), &["checkout", "--quiet", "-b", "topic"]);
    std::fs::write(w.repo.root().join("topic.txt"), "work\n").unwrap();
    git(w.repo.root(), &["add", "topic.txt"]);
    git(w.repo.root(), &["-c", "user.name=Topic", "-c", "user.email=topic@example.invalid", "commit", "--quiet", "-m", "feat: the topic"]);
    let agent = FakeAgent::builder().build();
    let provider = github();
    let path = fixing_path(&w, &agent, &provider, &fix_seen, true);
    let head = git_out(w.repo.root(), &["rev-parse", "HEAD"]);

    let out = run_with_path(&w, path, &["--publish"]);

    assert_ne!(out.status.code(), Some(0), "the gate is red: {}", String::from_utf8_lossy(&out.stdout));
    assert!(!fix_seen.exists(), "no agent was asked to edit the user's checkout");
    assert_eq!(head, git_out(w.repo.root(), &["rev-parse", "HEAD"]), "the checkout is untouched");
}

/// A drop tool that records the test database name it is asked to drop, and the directory it ran in.
fn drop_tool(w: &World) -> (PathBuf, PathBuf) {
    let log = w.base.join("drops.log");
    let tool = w.base.join("drop-tool");
    std::fs::write(&tool, format!("#!/bin/sh\necho \"$PWD $*\" >> '{}'\nexit 0\n", log.display())).unwrap();
    std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    (tool, log)
}

fn drop_world(tag: &str, extra: &str) -> (World, PathBuf) {
    let w = world(tag);
    let denv = format!("{}{}", ".", "env");
    std::fs::write(w.repo.root().join(denv), "DB_NAME=shop\n").unwrap();
    let (tool, log) = drop_tool(&w);
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("test_db_drop_cmd = [\"{}\"]\n{extra}", tool.display())).unwrap();
    (w, log)
}

#[test]
fn a_run_drops_the_test_database_of_its_removed_worktree_from_the_main_checkout() {
    let (w, log) = drop_world("drop", "");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &w.base.join("agent-seen"));

    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
    let drops = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(drops.lines().count(), 1, "{drops}");
    assert!(drops.trim_end().ends_with("test_shop_fix_com_01"), "{drops}");
    assert!(drops.starts_with(&w.repo.root().canonicalize().unwrap().display().to_string()), "run from the main checkout: {drops}");
}

#[test]
fn keep_test_db_leaves_the_test_database_alone_without_failing_the_run() {
    let (w, log) = drop_world("keep", "keep_test_db = true\n");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &w.base.join("agent-seen"));

    assert_eq!(out.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
    assert!(!log.exists(), "nothing is dropped when the databases are kept: {:?}", std::fs::read_to_string(&log));
}

#[test]
fn a_retained_worktree_keeps_its_test_database() {
    let (w, log) = drop_world("retained", "");
    let agent = FakeAgent::builder()
        .write_file("feature.txt", "done\n")
        .commit("feat: add the feature")
        .write_file("stray.txt", "left uncommitted\n")
        .build();

    let out = run(&w, &agent, &w.base.join("agent-seen"));

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let worktrees = String::from_utf8_lossy(&Command::new("git").arg("-C").arg(w.repo.root()).args(["worktree", "list"]).output().unwrap().stdout).into_owned();
    assert!(worktrees.lines().count() > 1, "premise: the worktree is retained\n{worktrees}\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(!log.exists(), "a retained worktree still uses its database: {:?}", std::fs::read_to_string(&log));
}
