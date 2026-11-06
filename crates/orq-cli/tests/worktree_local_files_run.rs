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
    let base = orq_test_support::process_temp_dir().join(format!("orq-worktree-local-files-{}-{tag}", std::process::id()));
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

/// A `claude` that appends the TDD config path it was started with to `seen` (the first line is the
/// implement session), then behaves as `agent`.
fn path_with(w: &World, agent: &FakeAgent, provider: &FakeProvider, seen: &Path) -> std::ffi::OsString {
    let bin = w.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        format!("#!/bin/sh\nprintf '%s\\n' \"$TDD_CONFIG_PATH\" >> '{}'\nexec '{}' \"$@\"\n", seen.display(), agent.path().display()),
    )
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

fn settings(w: &World, text: &str) {
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), text).unwrap();
}

#[test]
fn the_gate_sees_the_dotenv_file_and_the_agent_is_pointed_at_the_main_checkouts_tdd_config() {
    let w = world("default");
    std::fs::write(w.repo.root().join(".env"), "DB_NAME=shop\n").unwrap();
    std::fs::write(w.repo.root().join(".tdd-config.json"), "{}").unwrap();
    let (gate_seen, agent_seen) = (w.base.join("gate-seen"), w.base.join("agent-seen"));
    settings(&w, &format!("gate = [\"sh\", \"-c\", \"cat .env > {}\"]\n", gate_seen.display()));
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &agent_seen);

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{}\n{stdout}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&gate_seen).unwrap(), "DB_NAME=shop\n", "the gate");
    let config = w.repo.root().join(".tdd-config.json").display().to_string();
    assert_eq!(std::fs::read_to_string(&agent_seen).unwrap().lines().next(), Some(config.as_str()), "the agent");
}

#[test]
fn the_list_of_carried_files_comes_from_the_repositorys_run_settings() {
    let w = world("configured");
    std::fs::write(w.repo.root().join("secrets.json"), "{}").unwrap();
    std::fs::write(w.repo.root().join(".env"), "DB_NAME=shop\n").unwrap();
    let gate_seen = w.base.join("gate-seen");
    settings(&w, &format!("local_files = [\"secrets.json\"]\ngate = [\"sh\", \"-c\", \"ls -A > {}\"]\n", gate_seen.display()));
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &w.base.join("agent-seen"));

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let listing = std::fs::read_to_string(&gate_seen).unwrap();
    assert!(listing.lines().any(|f| f == "secrets.json"), "{listing}");
    assert!(!listing.lines().any(|f| f == ".env"), "{listing}");
}

#[test]
fn the_worktrees_session_settings_carry_the_run_environment_and_approve_the_configured_mcp_servers() {
    let w = world("session-settings");
    std::fs::write(w.repo.root().join(".tdd-config.json"), "{}").unwrap();
    std::fs::write(w.repo.root().join(".mcp.json"), r#"{"mcpServers":{"tdd":{"command":"c"},"docs":{"command":"d"}}}"#).unwrap();
    let gate_seen = w.base.join("settings-seen");
    settings(&w, &format!("mcp_servers = [\"tdd\"]\ngate = [\"sh\", \"-c\", \"cat .claude/settings.local.json > {}\"]\n", gate_seen.display()));
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &w.base.join("agent-seen"));

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let seen: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&gate_seen).unwrap()).unwrap();
    let config = w.repo.root().join(".tdd-config.json").display().to_string();
    assert_eq!(seen["env"]["TDD_CONFIG_PATH"], config.as_str());
    assert_eq!(seen["enabledMcpjsonServers"], serde_json::json!(["tdd"]));
    let status = Command::new("git").arg("-C").arg(w.repo.root()).args(["status", "--porcelain"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&status.stdout).contains(".claude"), "the worktrees directory must not show up");
}

/// A fake `mcpctl` on the run's PATH: logs its arguments to `log`, and generates the `.mcp.json` of
/// the selected servers, as the real setup does.
fn fake_mcpctl(w: &World, log: &Path) {
    let bin = w.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join("mcpctl");
    std::fs::write(
        &tool,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncat > .mcp.json <<'JSON'\n{{\"mcpServers\":{{\"tdd\":{{\"command\":\"mcpctl\",\"args\":[\"run\",\"tdd-mcp\"]}},\"project\":{{\"command\":\"mcpctl\",\"args\":[\"run\",\"project-mcp\"]}}}}}}\nJSON\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

#[test]
fn with_no_servers_configured_the_run_locks_the_servers_of_the_carried_mcpctl_toml_and_approves_what_setup_generates() {
    let w = world("mcpctl-toml");
    std::fs::write(
        w.repo.root().join(".mcpctl.toml"),
        "[project]\nname = \"p\"\n\n[[mcp]]\nname = \"tdd-mcp\"\nversion = \"^0.2.0\"\n\n[[mcp]]\nname = \"project-mcp\"\nversion = \"^1.2.0\"\n",
    )
    .unwrap();
    let (mcpctl_seen, gate_seen) = (w.base.join("mcpctl-seen"), w.base.join("settings-seen"));
    fake_mcpctl(&w, &mcpctl_seen);
    settings(&w, &format!("gate = [\"sh\", \"-c\", \"cat .claude/settings.local.json > {}\"]\n", gate_seen.display()));
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &w.base.join("agent-seen"));

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&mcpctl_seen).unwrap(), "setup --mcp tdd-mcp --mcp project-mcp\n");
    let seen: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&gate_seen).unwrap()).unwrap();
    let mut approved: Vec<String> = serde_json::from_value(seen["enabledMcpjsonServers"].clone()).unwrap();
    approved.sort();
    assert_eq!(approved, ["project", "tdd"], "both generated servers are approved");
}
