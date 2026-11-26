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

#[cfg(target_os = "linux")]
#[test]
fn output_from_a_descendant_that_escaped_the_session_is_not_written_after_the_stage_ends() {
    let s = setup("escaped");
    // A descendant in a session of its own outlives the session's group and keeps its stdout.
    let script = format!(
        "{}setsid sh -c 'sleep 4; echo late-output' &\n",
        print(&[READ_TOOL, RESULT])
    );

    let began = Instant::now();
    let at_end = run(&s, &script);
    assert!(began.elapsed() < Duration::from_secs(4), "the stage waited for the escaped descendant");
    std::thread::sleep(Duration::from_secs(5));

    assert!(at_end.contains(" tool Read "), "{at_end}");
    let later = fs::read_to_string(&s.log).unwrap();
    assert_eq!(later, at_end, "the log was written after the stage ended");
}

/// A call of `tool` with id `id`, and its result with `content` (a JSON value) and `is_error`.
fn call_and_result(tool: &str, id: &str, content: &str, is_error: Option<bool>) -> [String; 2] {
    let error = is_error
        .map(|e| format!(r#""is_error":{e},"#))
        .unwrap_or_default();
    [
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"{tool}","input":{{}}}}]}}}}"#
        ),
        format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}",{error}"content":{content}}}]}}}}"#
        ),
    ]
}

#[test]
fn a_run_tests_result_shows_its_counts_and_duration_or_its_first_line() {
    let s = setup("run-tests");
    let events: Vec<String> = [
        // Namespaced, the counts wrapped in testResult and given as text parts; a failed test is an error.
        call_and_result(
            "mcp__outside-in-tdd__run_tests",
            "t1",
            r#"[{"type":"text","text":"{\"testResult\":{\"passed\":5,\"failed\":1,\"durationMs\":2500}}"}]"#,
            Some(false),
        ),
        // Plain name, bare counts, no duration and no is_error.
        call_and_result("run_tests", "t2", r#""{\"passed\":3,\"failed\":0}""#, None),
        // Counts that cannot be read: its first line, ok since the tool did not fail.
        call_and_result("mcp__outside-in-tdd__run_tests", "t3", r#""No active feature.\nmore""#, None),
        // Any other tool's successful result is not shown.
        call_and_result("Read", "t4", r#""{\"passed\":9,\"failed\":9}""#, None),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut all: Vec<&str> = events.iter().map(String::as_str).collect();
    all.push(RESULT);

    let text = run(&s, &print(&all));

    let results: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_once(" result ").map(|(_, rest)| rest))
        .collect();
    assert_eq!(
        results,
        [
            "error passed 5, failed 1 in 2.5s",
            "ok passed 3, failed 0",
            "ok No active feature."
        ],
        "{text}"
    );
}

#[test]
fn a_tool_argument_under_a_secret_name_is_not_shown() {
    let s = setup("secret-key");
    let call = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_3","name":"mcp__vault__login","input":{"api_key":"ordinary-credential-value"}}]}}"#;

    let text = run(&s, &print(&[call, RESULT]));

    assert!(
        text.lines().any(|l| l.contains(" tool mcp__vault__login")),
        "{text}"
    );
    assert!(!text.contains("ordinary-credential-value"), "{text}");
}
