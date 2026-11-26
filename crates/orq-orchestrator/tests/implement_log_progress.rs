//! An unattended session prints stream-json events; orq turns each step into one readable, redacted line
//! of its log as it happens, and ends the log with the session's final answer.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orq_orchestrator::{run_implement_stage, ImplementSession, RoleProfile, RunRecord, RunStore};

fn root(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!(
        "orq-implement-progress-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(run_id: &str) -> RunRecord {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "implementer".to_string(),
        RoleProfile {
            harness: "claude".to_string(),
            model: "opus".to_string(),
            thinking: None,
        },
    );
    RunRecord {
        delivery: Default::default(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "git@github.com:acme/app.git".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        branch: "fix--COM-02".to_string(),
        worktree: PathBuf::from(".claude/worktrees/fix--COM-02"),
        repository: String::new(),
        profiles,
    }
}

struct Setup {
    store: RunStore,
    cwd: PathBuf,
    log: PathBuf,
}

fn setup(tag: &str) -> Setup {
    let root = root(tag);
    let store = RunStore::new(&root.join("state"));
    store.create(&record("run-1")).unwrap();
    let cwd = root.join("worktree");
    fs::create_dir_all(&cwd).unwrap();
    Setup {
        store,
        cwd,
        log: root.join("implement.log"),
    }
}

/// Runs `script` with `sh -c` as an unattended session and returns the log's contents.
fn run(s: &Setup, script: &str) -> String {
    let args = vec!["-c".to_string(), script.to_string()];
    let session = ImplementSession {
        program: "sh",
        args: &args,
        env: &[],
        cwd: &s.cwd,
        session_id: Some("sess-42"),
        log: &s.log,
        terminal: false,
        exit_marker: None,
    };
    let outcome = run_implement_stage(&s.store, "run-1", &session).unwrap();
    assert!(outcome.success, "{outcome:?}");
    fs::read_to_string(&s.log).unwrap()
}

/// `printf` lines for each event, for a fake harness script.
fn print(events: &[&str]) -> String {
    events
        .iter()
        .map(|event| format!("printf '%s\\n' '{event}'\n"))
        .collect()
}

const READ_TOOL: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"crates/orq-orchestrator/src/implement.rs"}}]}}"#;
const RESULT: &str = r#"{"type":"result","subtype":"success","result":"All done, committed.","usage":{"input_tokens":3,"output_tokens":4}}"#;

fn wait_for(path: &Path, what: &str, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(path) {
            if text.contains(what) {
                return Some(text);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn the_log_has_the_tool_line_before_the_session_ends() {
    let s = setup("live");
    let go = s.cwd.join("go");
    // The fake session waits for the test to have seen its tool line before it ends.
    let script = format!(
        "{}while [ ! -f go ]; do sleep 0.05; done\n{}",
        print(&[
            r#"{"type":"system","subtype":"init","session_id":"abc"}"#,
            READ_TOOL
        ]),
        print(&[RESULT])
    );
    let log = s.log.clone();
    let watcher = std::thread::spawn(move || {
        let seen = wait_for(
            &log,
            "tool Read crates/orq-orchestrator/src/implement.rs",
            Duration::from_secs(20),
        );
        fs::write(&go, "").unwrap();
        seen
    });

    let text = run(&s, &script);

    let seen = watcher.join().unwrap();
    assert!(
        seen.is_some(),
        "the tool line was not in the log while the session ran: {text}"
    );
    let line = text
        .lines()
        .find(|line| line.contains(" tool Read "))
        .unwrap();
    let (clock, rest) = line.split_once(' ').unwrap();
    assert_eq!(
        rest, "tool Read crates/orq-orchestrator/src/implement.rs",
        "{text}"
    );
    let parts: Vec<&str> = clock.split(':').collect();
    assert_eq!(parts.len(), 3, "{line}");
    assert!(
        parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_digit())),
        "{line}"
    );
    assert!(!text.contains(r#""subtype":"init""#), "{text}");
}

#[test]
fn a_bash_tool_line_shows_its_description_not_its_command() {
    let s = setup("bash");
    let bash = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Running the build now.\nMore detail here."},{"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"curl -H \"Authorization: Bearer sk-ant-api03-SECRETSECRETSECRETSECRET\" https://example.com","description":"Fetch the release notes"}}]}}"#;
    let error = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","is_error":true,"content":"curl: (6) could not resolve host\nsecond line"}]}}"#;

    let text = run(&s, &print(&[bash, error, RESULT]));

    assert!(
        text.lines()
            .any(|l| l.ends_with(" tool Bash Fetch the release notes")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.ends_with(" said Running the build now.")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.ends_with(" result error curl: (6) could not resolve host")),
        "{text}"
    );
    assert!(!text.contains("SECRETSECRET"), "{text}");
    assert!(!text.contains("More detail here"), "{text}");
}

#[test]
fn the_final_result_text_is_the_last_line() {
    let s = setup("final");

    let text = run(&s, &print(&[READ_TOOL, RESULT]));

    assert_eq!(text.lines().last(), Some("All done, committed."), "{text}");
}

#[test]
fn a_line_that_is_not_json_is_kept_as_is() {
    let s = setup("plain");

    let text = run(
        &s,
        &format!(
            "echo 'warming up {{not json'\n{}",
            print(&[READ_TOOL, RESULT])
        ),
    );

    assert!(text.lines().any(|l| l == "warming up {not json"), "{text}");
    assert!(text.contains(" tool Read "), "{text}");
}
