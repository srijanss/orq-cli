//! Every project with MCP servers gets them set up in the run's worktree the same way: from the main
//! checkout's lock when it has one, else by `mcpctl setup`, and within the pins when setup refuses.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{run_mcp_setup, McpSetupRequest, McpTimeouts};
use orq_test_support::GitFixture;

const LOCK: &str = "version = 1\n\n[[mcp]]\nname = \"tdd\"\nversion = \"0.2.3\"\n\n[[mcp]]\nname = \"project\"\nversion = \"1.2.1\"\n";

struct World {
    repo: GitFixture,
    worktree: PathBuf,
    calls: PathBuf,
    tool: PathBuf,
    log: PathBuf,
}

/// A shim `mcpctl` that records `<dir>|<args>` for each call and acts like the real one: `setup` and
/// `sync` write a lock, `init` writes a scaffold. `setup_body` replaces what `setup` does.
fn world(tag: &str, setup_body: &str) -> World {
    world_with(tag, setup_body, "", "")
}

/// [`world`], with `sync_body` and `init_body` run first in `sync` and `init`.
fn world_with(tag: &str, setup_body: &str, sync_body: &str, init_body: &str) -> World {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree(tag);
    let dir = repo.root().parent().unwrap().join(format!("mcp-setup-{tag}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let calls = dir.join("calls");
    let tool = dir.join("mcpctl");
    let script = format!(
        r#"#!/bin/sh
echo "$PWD|$*" >> '{calls}'
case "$1" in
  setup)
{setup_body}
    printf 'version = 1\n\n[[mcp]]\nname = "tdd"\nversion = "0.4.2"\n\n[[mcp]]\nname = "project"\nversion = "1.3.1"\n' > .mcpctl.lock
    echo "locked tdd 0.4.2"
    ;;
  sync)
{sync_body}
    if [ "$2" != "--locked" ]; then
      printf 'version = 1\n\n[[mcp]]\nname = "tdd"\nversion = "0.2.9"\n\n[[mcp]]\nname = "project"\nversion = "1.2.1"\n' > .mcpctl.lock
    fi
    echo "installed"
    ;;
  init)
    mkdir -p .agents
    echo "scaffold" > .agents/guide.md
{init_body}
    [ -e .tdd-config.json ] || echo '{{"adapter":"pytest-adapter"}}' > .tdd-config.json
    printf '{{"version":1,"files":{{".agents/guide.md":{{"mcp":"tdd"}},".tdd-config.json":{{"mcp":"tdd"}},"README.md":{{"mcp":"tdd"}}}}}}' > .mcpctl.scaffold.json
    echo "scaffolded .agents/guide.md"
    ;;
esac
exit 0
"#,
        calls = calls.display(),
    );
    fs::write(&tool, script).unwrap();
    fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let log = dir.join("mcp-setup.log");
    World { repo, worktree, calls, tool, log }
}

fn calls(w: &World) -> Vec<(String, String)> {
    fs::read_to_string(&w.calls)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let (dir, args) = line.split_once('|').unwrap();
            (dir.to_string(), args.to_string())
        })
        .collect()
}

fn servers() -> Vec<String> {
    vec!["tdd".to_string(), "project".to_string()]
}

fn request<'a>(w: &'a World, servers: &'a [String]) -> McpSetupRequest<'a> {
    McpSetupRequest {
        main_checkout: w.repo.root(),
        worktree: &w.worktree,
        servers,
        env: &[],
        mcpctl: Some(&w.tool),
        log: &w.log,
        timeouts: McpTimeouts::default(),
    }
}

fn ignored(dir: &Path, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "-q", "--", name])
        .status()
        .unwrap()
        .success()
}

fn args_only(w: &World) -> Vec<String> {
    let worktree = fs::canonicalize(&w.worktree).unwrap();
    calls(w)
        .into_iter()
        .map(|(dir, args)| {
            assert_eq!(fs::canonicalize(&dir).unwrap(), worktree, "mcpctl ran outside the worktree: {dir}");
            args
        })
        .collect()
}

#[test]
fn with_a_lock_in_the_main_checkout_the_worktree_runs_exactly_the_locked_versions() {
    let w = world("locked", "");
    fs::write(w.repo.root().join(".mcpctl.lock"), LOCK).unwrap();

    let report = run_mcp_setup(&request(&w, &servers()));

    assert_eq!(fs::read_to_string(w.worktree.join(".mcpctl.lock")).unwrap(), LOCK, "the lock is copied in");
    assert!(ignored(&w.worktree, ".mcpctl.lock"), "the copied lock stays out of git status");
    assert_eq!(args_only(&w), ["sync --locked", "init"], "locked install, then scaffold; never setup");
    let versions: Vec<(&str, Option<&str>, &str)> = report
        .versions
        .iter()
        .map(|v| (v.name.as_str(), v.version.as_deref(), v.source.as_str()))
        .collect();
    assert_eq!(versions, [("tdd", Some("0.2.3"), "locked"), ("project", Some("1.2.1"), "locked")]);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
}

fn versions(report: &orq_orchestrator::McpSetupReport) -> Vec<(String, Option<String>, String)> {
    report.versions.iter().map(|v| (v.name.clone(), v.version.clone(), v.source.clone())).collect()
}

fn version(name: &str, version: &str, source: &str) -> (String, Option<String>, String) {
    (name.to_string(), Some(version.to_string()), source.to_string())
}

#[test]
fn without_a_lock_the_servers_are_set_up_at_their_newest_versions_in_one_call() {
    let w = world("newest", "");

    let report = run_mcp_setup(&request(&w, &servers()));

    assert_eq!(args_only(&w), ["setup --mcp tdd --mcp project"]);
    assert_eq!(versions(&report), [version("tdd", "0.4.2", "newest"), version("project", "1.3.1", "newest")]);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
}

const REFUSED: &str = "    echo \"Error: mcp 'tdd' source version 0.4.2 does not satisfy constraint '^0.2.3'\" >&2\n    exit 1\n";

#[test]
fn a_setup_refused_by_the_pins_falls_back_to_the_versions_within_them_and_says_why() {
    let w = world("pinned", REFUSED);

    let report = run_mcp_setup(&request(&w, &servers()));

    assert_eq!(args_only(&w), ["setup --mcp tdd --mcp project", "sync", "init"]);
    assert_eq!(versions(&report), [version("tdd", "0.2.9", "within pins"), version("project", "1.2.1", "within pins")]);
    assert!(w.worktree.join(".agents/guide.md").exists(), "the worktree is still scaffolded");
    let note = report.notes.join("\n");
    assert!(note.contains("does not satisfy constraint '^0.2.3'"), "the reason: {note}");
    assert!(note.contains("tdd 0.2.9") && note.contains("project 1.2.1"), "the versions used: {note}");
}

#[test]
fn a_missing_or_hanging_mcpctl_is_a_note_and_the_servers_run_at_the_machines_active_versions() {
    let active = vec![
        ("tdd".to_string(), None, "machine active".to_string()),
        ("project".to_string(), None, "machine active".to_string()),
    ];

    let w = world("missing", "");
    let missing = run_mcp_setup(&McpSetupRequest { mcpctl: None, ..request(&w, &servers()) });
    assert!(missing.notes.iter().any(|n| n.contains("mcpctl is not on PATH")), "{:?}", missing.notes);
    assert_eq!(versions(&missing), active);

    let w = world("hanging", "    sleep 30\n");
    let started = std::time::Instant::now();
    let short = McpTimeouts { install: std::time::Duration::from_secs(1), init: std::time::Duration::from_secs(1) };
    let hung = run_mcp_setup(&McpSetupRequest { timeouts: short, ..request(&w, &servers()) });
    assert!(started.elapsed() < std::time::Duration::from_secs(15), "stopped at its time limit");
    assert!(hung.notes.iter().any(|n| n.contains("mcpctl setup") && n.contains("did not finish within 1s")), "{:?}", hung.notes);
    assert_eq!(versions(&hung), active);
}

#[test]
fn what_mcpctl_prints_lands_in_the_setup_log() {
    let w = world("logged", REFUSED);

    run_mcp_setup(&request(&w, &servers()));

    let log = fs::read_to_string(&w.log).unwrap();
    assert!(log.contains("does not satisfy constraint"), "{log}");
    assert!(log.contains("scaffolded .agents/guide.md"), "{log}");
    assert!(log.contains("$ mcpctl setup --mcp tdd --mcp project") && log.contains("$ mcpctl init"), "each call is headed: {log}");
}

#[test]
fn nothing_is_set_up_in_the_main_checkout_itself() {
    let w = world("main-itself", "");
    let main = w.repo.root().to_path_buf();
    let servers = servers();

    let report = run_mcp_setup(&McpSetupRequest { worktree: &main, ..request(&w, &servers) });

    assert!(calls(&w).is_empty(), "mcpctl ran in the main checkout: {:?}", calls(&w));
    assert!(report.notes.iter().any(|n| n.contains("main checkout")), "{:?}", report.notes);
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn everything_the_setup_created_stays_out_of_git_status_and_tracked_files_are_left_alone() {
    let w = world("excluded", "");
    fs::write(w.repo.root().join(".mcpctl.lock"), LOCK).unwrap();

    let report = run_mcp_setup(&request(&w, &servers()));

    assert!(report.notes.is_empty(), "{:?}", report.notes);
    let status = git_in(&w.worktree, &["status", "--porcelain", "--untracked-files=all"]);
    assert_eq!(status, "", "the lock, the scaffold record and every scaffolded file are excluded");
    let exclude = fs::read_to_string(w.repo.root().join(".git/info/exclude")).unwrap_or_default();
    assert!(!exclude.contains("README.md"), "a tracked file is the branch's own: {exclude}");
}

#[test]
fn a_locked_install_that_reports_a_pin_refusal_is_a_failure_and_never_installs_unlocked() {
    let w = world_with("locked-refused", "", "    echo 'Error: tdd 0.2.3 does not satisfy constraint ^0.3' >&2\n    exit 1", "");
    fs::write(w.repo.root().join(".mcpctl.lock"), LOCK).unwrap();

    let report = run_mcp_setup(&request(&w, &servers()));

    assert_eq!(args_only(&w), ["sync --locked"], "no unlocked sync, no init");
    assert_eq!(fs::read_to_string(w.worktree.join(".mcpctl.lock")).unwrap(), LOCK, "the lock is not replaced");
    assert!(report.versions.iter().all(|v| v.source == "machine active"), "{:?}", report.versions);
    assert!(report.notes.iter().any(|n| n.contains("does not satisfy constraint")), "{:?}", report.notes);
}

#[test]
fn an_init_that_fails_after_a_locked_install_is_a_note_and_what_it_left_behind_is_kept_out_of_git_status() {
    // init writes its scaffold record and one file, then fails.
    let init = "    printf '{\"version\":1,\"files\":{\".agents/guide.md\":{\"mcp\":\"tdd\"}}}' > .mcpctl.scaffold.json\n    echo 'Error: template missing' >&2\n    exit 2";
    let w = world_with("init-fails", "", "", init);
    fs::write(w.repo.root().join(".mcpctl.lock"), LOCK).unwrap();

    let report = run_mcp_setup(&request(&w, &servers()));

    assert_eq!(args_only(&w), ["sync --locked", "init"], "nothing runs after the failed init");
    assert!(report.notes.iter().any(|n| n.contains("mcpctl init") && n.contains("Error: template missing")), "{:?}", report.notes);
    assert!(report.versions.iter().all(|v| v.source == "machine active" && v.version.is_none()), "{:?}", report.versions);
    for left in [".agents/guide.md", ".mcpctl.scaffold.json", ".mcpctl.lock"] {
        assert!(ignored(&w.worktree, left), "{left} is kept out of git status");
    }
}

#[test]
fn an_init_that_hangs_after_a_locked_install_is_stopped_at_its_own_limit() {
    let w = world_with("init-hangs", "", "", "    sleep 30");
    fs::write(w.repo.root().join(".mcpctl.lock"), LOCK).unwrap();
    let started = std::time::Instant::now();
    let short = McpTimeouts { install: std::time::Duration::from_secs(10), init: std::time::Duration::from_secs(1) };

    let report = run_mcp_setup(&McpSetupRequest { timeouts: short, ..request(&w, &servers()) });

    assert!(started.elapsed() < std::time::Duration::from_secs(8), "stopped at init's limit, not install's");
    assert_eq!(args_only(&w), ["sync --locked", "init"]);
    assert!(report.notes.iter().any(|n| n.contains("mcpctl init did not finish within 1s")), "{:?}", report.notes);
    assert!(report.versions.iter().all(|v| v.source == "machine active"), "{:?}", report.versions);
    assert!(ignored(&w.worktree, ".mcpctl.lock"), "the carried lock is still kept out of git status");
}
