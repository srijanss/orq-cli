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

#[test]
fn a_failure_after_a_flood_of_output_still_names_its_own_last_line() {
    // Several megabytes of output, then the line that says what went wrong.
    let setup = "    i=0; while [ $i -lt 40000 ]; do echo \"downloading chunk $i of a rather long and noisy installation log line\"; i=$((i+1)); done\n    echo 'Error: disk full' >&2\n    exit 1";
    let w = world("flood", setup);

    let report = run_mcp_setup(&request(&w, &servers()));

    assert!(fs::metadata(&w.log).unwrap().len() > 2_000_000, "the log holds the flood");
    assert!(report.notes.iter().any(|n| n.ends_with("Error: disk full") || n.contains("Error: disk full")), "{:?}", report.notes);
}

#[test]
fn a_failures_note_never_carries_credentials_tokens_or_the_runs_own_values() {
    let setup = "    echo 'Error: GET https://bob:hunter2pass@registry.example/pkg?access_token=tok-abc123 failed, password=pw-zz9 api_key: key-77q, db is run-db-s3cret' >&2\n    exit 1";
    let w = world("secret", setup);
    let env = vec![("DB_PASSWORD".to_string(), "run-db-s3cret".to_string())];

    let report = run_mcp_setup(&McpSetupRequest { env: &env, ..request(&w, &servers()) });

    let notes = report.notes.join("\n");
    assert!(notes.contains("Error: GET https://"), "the line is still there: {notes}");
    assert!(notes.contains("registry.example"), "{notes}");
    for secret in ["hunter2pass", "bob:", "tok-abc123", "pw-zz9", "key-77q", "run-db-s3cret"] {
        assert!(!notes.contains(secret), "{secret} leaked: {notes}");
    }
}

/// The notes of a setup whose last line is `line`, run with `env` as the run's own environment.
fn notes_of_failing_line(tag: &str, line: &str, env: &[(String, String)]) -> String {
    let w = world(tag, &format!("    echo '{line}' >&2\n    exit 1"));
    run_mcp_setup(&McpSetupRequest { env, ..request(&w, &servers()) }).notes.join("\n")
}

#[test]
fn a_run_value_crossing_the_lines_length_limit_is_redacted_whole_not_shown_in_part() {
    // The value starts at character 295, so a cut at 300 leaves its first five characters.
    let env = vec![("DB_PASSWORD".to_string(), "run-db-s3cret-value-long".to_string())];
    let line = format!("{} run-db-s3cret-value-long", "x".repeat(294));

    let notes = notes_of_failing_line("cut-env", &line, &env);

    assert!(!notes.contains("run-d"), "{notes}");
}

#[test]
fn a_url_password_crossing_the_lines_length_limit_is_redacted_whole_not_shown_in_part() {
    // The cut at 300 falls inside the password, before the `@` that marks it as one.
    let line = format!("{} https://bob:hunter2passw0rd@registry.example/pkg", "y".repeat(282));

    let notes = notes_of_failing_line("cut-url", &line, &[]);

    assert!(!notes.contains("bob"), "{notes}");
    assert!(!notes.contains("hunt"), "{notes}");
}

#[test]
fn a_tail_that_starts_inside_its_only_line_shows_none_of_that_line() {
    // One line longer than the part of the log that is read, so its start is never seen.
    let w = world("tail-mid-line", "    head -c 70000 /dev/zero | tr '\\0' z >&2\n    echo >&2\n    exit 1");

    let report = run_mcp_setup(&request(&w, &servers()));

    assert!(!report.notes.is_empty(), "the failure itself is still named");
    assert!(!report.notes.join("\n").contains("zzzz"), "{:?}", report.notes);
}

#[test]
fn quoted_json_spaced_and_header_secrets_never_reach_a_note() {
    let cases = [
        ("quoted-value", r#"Error: login failed password="hunter2 q" for registry"#, "hunter2"),
        ("single-quoted-value", "Error: login failed password='pw-single q' again", "pw-single"),
        ("escaped-quoted-value", r#"Error: login failed password=\"pw-escaped q\" again"#, "pw-escaped"),
        ("json", r#"Error: bad response {"token":"abc123xyz","user":"me"}"#, "abc123xyz"),
        ("json-spaced", r#"Error: bad config {"api_key" : "k-998877"}"#, "k-998877"),
        ("spaced-equals", "Error: client_secret = cs-445566 rejected", "cs-445566"),
        ("bearer", "Error: 401 with Authorization: Bearer tok-bearer-1234", "tok-bearer-1234"),
        ("basic", "Error: 401 with Authorization: Basic dXNlcjpwYXNzd29yZA==", "dXNlcjpwYXNzd29yZA"),
        ("token-only-url", "Error: GET https://ghs-tok-55aa@registry.example/pkg failed", "ghs-tok-55aa"),
    ];
    for (tag, line, secret) in cases {
        let notes = notes_of_failing_line(tag, &line.replace('\'', "'\\''"), &[]);

        assert!(notes.contains("Error:"), "{tag}: the line is still there: {notes}");
        assert!(!notes.contains(secret), "{tag}: {secret} leaked: {notes}");
    }
}

#[test]
fn pwd_and_auth_values_never_reach_a_note_bare_quoted_or_in_json() {
    let cases = [
        ("pwd-bare", "Error: login pwd=hunter2pw rejected", "hunter2pw"),
        ("auth-bare", "Error: login auth=opaque-login-value rejected", "opaque-login-value"),
        ("pwd-quoted", r#"Error: login pwd="pw quoted-77" rejected"#, "quoted-77"),
        ("auth-spaced", "Error: login auth : opaque-spaced-1 rejected", "opaque-spaced-1"),
        ("pwd-json", r#"Error: bad config {"pwd":"pw-json-55","user":"me"}"#, "pw-json-55"),
        ("auth-json", r#"Error: bad config {"auth" : "auth-json-66"}"#, "auth-json-66"),
        ("auth-scheme", "Error: auth=Basic dXNlcjpwYXNzd29yZA== rejected", "dXNlcjpwYXNzd29yZA"),
        ("authorization-quoted", r#"Error: {"Authorization":"Bearer two words-88"}"#, "words-88"),
    ];
    for (tag, line, secret) in cases {
        let notes = notes_of_failing_line(tag, &line.replace('\'', "'\\''"), &[]);

        assert!(notes.contains("Error:"), "{tag}: the line is still there: {notes}");
        assert!(!notes.contains(secret), "{tag}: {secret} leaked: {notes}");
    }
}

#[test]
fn an_authorization_header_with_tabs_for_spaces_never_reaches_a_note() {
    let cases = [
        ("tab-after-colon", "Error: 401 with Authorization:\tBasic dXNlcjpwYXNzd29yZA==", "dXNlcjpwYXNzd29yZA"),
        ("tab-after-scheme", "Error: 401 with Authorization: Basic\tdXNlcjpwYXNzd29yZA==", "dXNlcjpwYXNzd29yZA"),
        ("tabs-throughout", "Error: 401 with Authorization\t:\tBearer\ttok-tabbed-4321", "tok-tabbed-4321"),
    ];
    for (tag, line, secret) in cases {
        let notes = notes_of_failing_line(tag, line, &[]);

        assert!(notes.contains("Error:"), "{tag}: the line is still there: {notes}");
        assert!(!notes.contains(secret), "{tag}: {secret} leaked: {notes}");
    }
}

#[test]
fn an_auth_value_is_hidden_whole_and_only_a_known_scheme_name_stays() {
    let cases = [
        ("auth-word", "Error: auth=hunter rejected", "auth=[REDACTED] rejected", "hunter"),
        ("auth-quoted-words", r#"Error: login auth="private phrase" failed"#, r#"auth="[REDACTED]" failed"#, "private"),
        ("auth-json-words", r#"Error: {"auth":"secret words-12"}"#, r#"{"auth":"[REDACTED]"}"#, "secret"),
        ("auth-known-scheme", "Error: auth=Basic dXNlcjpwYXNz rejected", "auth=Basic [REDACTED] rejected", "dXNlcjpwYXNz"),
        ("authorization-token", "Error: Authorization: token ghp-opaque-1 denied", "Authorization: token [REDACTED] denied", "ghp-opaque-1"),
    ];
    for (tag, line, shown, secret) in cases {
        let notes = notes_of_failing_line(tag, &line.replace('\'', "'\\''"), &[]);

        assert!(notes.contains(shown), "{tag}: expected {shown} in: {notes}");
        assert!(!notes.contains(secret), "{tag}: {secret} leaked: {notes}");
    }
}
