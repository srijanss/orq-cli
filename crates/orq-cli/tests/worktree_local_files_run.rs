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

/// The implementer has nobody to answer a permission prompt: the worktree's session settings accept
/// its edits, allow the local commands and MCP tools a run needs plus those the project adds, never
/// publishing to a remote, and let it write its exit marker into the run's logs.
#[test]
fn the_worktrees_session_settings_let_the_implementer_work_unattended() {
    let w = world("unattended-permissions");
    let gate_seen = w.base.join("settings-seen");
    settings(&w, &format!("allowed_tools = [\"Bash(make lint:*)\"]\ngate = [\"sh\", \"-c\", \"cat .claude/settings.local.json > {}\"]\n", gate_seen.display()));
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &w.base.join("agent-seen"));

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let seen: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&gate_seen).unwrap()).unwrap();
    let permissions = &seen["permissions"];
    assert_eq!(permissions["defaultMode"], "acceptEdits", "{seen}");
    let allow: Vec<String> = serde_json::from_value(permissions["allow"].clone()).unwrap();
    for wanted in ["Bash(git add:*)", "Bash(git commit:*)", "Bash(cargo test:*)", "Bash(npm ci)", "mcp__outside-in-tdd", "mcp__project-mcp", "Bash(make lint:*)"] {
        assert!(allow.iter().any(|rule| rule == wanted), "{wanted} in {allow:?}");
    }
    assert!(!allow.iter().any(|rule| rule.contains("push") || rule.contains("publish")), "{allow:?}");
    let deny: Vec<String> = serde_json::from_value(permissions["deny"].clone()).unwrap();
    assert!(deny.iter().any(|rule| rule == "Bash(git push:*)"), "{deny:?}");
    let dirs: Vec<std::path::PathBuf> = serde_json::from_value(permissions["additionalDirectories"].clone()).unwrap();
    let logs = w.state.join("orq").join("logs");
    assert!(dirs.iter().any(|dir| dir.parent() == Some(logs.as_path())), "the run's logs directory, under {}: {dirs:?}", logs.display());
}

/// A session nobody watches is told which command forms its settings allow, so it does not reach
/// for one that is refused.
#[test]
fn the_unattended_implement_prompt_lists_the_command_forms_the_run_allows() {
    let w = world("prompt-lists-commands");
    settings(&w, "allowed_tools = [\"Bash(make lint:*)\"]\ngate = [\"true\"]\n");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let out = run(&w, &agent, &w.base.join("agent-seen"));

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let prompt = agent.first_run().expect("the implement session ran").args.last().cloned().unwrap();
    assert!(prompt.contains("Run a command in exactly one of these forms"), "{prompt}");
    assert!(prompt.contains("`make lint ...`"), "{prompt}");
    assert!(prompt.contains("`git commit ...`"), "{prompt}");
    assert!(!prompt.contains("git push"), "{prompt}");
}

/// The gate's fix session is told the command forms too, as its settings allow them when it starts.
#[test]
fn the_gate_fix_prompt_lists_the_command_forms_the_run_allows() {
    let w = world("fix-prompt-lists-commands");
    settings(&w, "allowed_tools = [\"Bash(make lint:*)\"]\ngate = [\"false\"]\n");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();

    let prompt = fix_prompt(&w, &agent, &github(), &["--no-ci-check"]);

    assert!(prompt.contains("Run a command in exactly one of these forms"), "{prompt}");
    assert!(prompt.contains("`make lint ...`"), "{prompt}");
}

/// Runs the pipeline with a fix session that leaves its prompt behind and calls the failure
/// unrelated, ending the run; returns that prompt.
fn fix_prompt(w: &World, agent: &FakeAgent, provider: &FakeProvider, flags: &[&str]) -> String {
    let fix_seen = w.base.join("fix-seen");
    let bin = w.base.join("fix-bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\nif [ -n \"$ORQ_UNRELATED_FAILURE_FILE\" ]; then for a; do last=$a; done; printf '%s' \"$last\" > '{}'; echo 'flaky' > \"$ORQ_UNRELATED_FAILURE_FILE\"; exit 0; fi\nexec '{}' \"$@\"\n",
            fix_seen.display(),
            agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));

    let out = Command::new(env!("CARGO_BIN_EXE_orq"))
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
        .unwrap();
    std::fs::read_to_string(&fix_seen).unwrap_or_else(|_| panic!("no fix session ran: {}", String::from_utf8_lossy(&out.stderr)))
}

/// A checked-in `build.mjs` the implement session may run, which that session then changes.
fn world_with_build_script(tag: &str, gate: &str) -> (World, FakeAgent) {
    let w = world(tag);
    std::fs::write(w.repo.root().join("build.mjs"), "console.log('built')\n").unwrap();
    git(w.repo.root(), &["add", "build.mjs"]);
    git(w.repo.root(), &["-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-qm", "add the build script"]);
    settings(&w, &format!("gate = [\"{gate}\"]\n"));
    let agent = FakeAgent::builder().write_file("build.mjs", "require('child_process')\n").commit("feat: change the build").build();
    (w, agent)
}

fn implement_prompt(agent: &FakeAgent) -> String {
    agent.first_run().expect("the implement session ran").args.last().cloned().unwrap()
}

/// The rules are written again before each fix session: a script the run changed after the
/// implement session started is no longer listed to the gate's fix session.
#[test]
fn the_gate_fix_prompt_leaves_out_a_script_the_run_changed() {
    let (w, agent) = world_with_build_script("fix-prompt-changed-script", "false");

    let prompt = fix_prompt(&w, &agent, &github(), &["--no-ci-check"]);

    assert!(implement_prompt(&agent).contains("`node build.mjs`"), "{}", implement_prompt(&agent));
    assert!(prompt.contains("Run a command in exactly one of these forms"), "{prompt}");
    assert!(!prompt.contains("node build.mjs"), "{prompt}");
}

/// The same holds for a fix session started because CI failed.
#[test]
fn the_ci_fix_prompt_leaves_out_a_script_the_run_changed() {
    let (w, agent) = world_with_build_script("ci-fix-prompt-changed-script", "true");
    let red = "{\"total_count\":1,\"check_runs\":[{\"name\":\"deploy\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .reply(&["api"], red)
        .build();

    let prompt = fix_prompt(&w, &agent, &provider, &[]);

    assert!(implement_prompt(&agent).contains("`node build.mjs`"), "{}", implement_prompt(&agent));
    assert!(prompt.starts_with("CI failed") || prompt.contains("CI failed"), "{prompt}");
    assert!(prompt.contains("Run a command in exactly one of these forms"), "{prompt}");
    assert!(!prompt.contains("node build.mjs"), "{prompt}");
}
