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
