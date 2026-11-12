//! The hook an attended session calls when it needs its person: `orq run needs-input` records the
//! need on the run and tells the person what is asked, without ever holding the session up.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::GitFixture;

struct World {
    repo: GitFixture,
    state: PathBuf,
    store: RunStore,
    sent: PathBuf,
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-needs-input-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let state = root.join("state");
    let store = RunStore::new(&state.join("orq"));
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-1".into(),
            worktree: root.join("w"),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { repo: GitFixture::new(), state, store, sent: root.join("sent") }
}

/// Calls the hook as Claude does, with `input` on stdin, and a notifier that appends its message.
fn hook(w: &World, run: &str, input: &str, notifier_prefix: &str) -> Output {
    let script = format!("{notifier_prefix}printf '%s\\n' \"$NOTIFY_MESSAGE\" >> '{}'", w.sent.display());
    let notifier = serde_json::json!(["sh", "-c", script]).to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "needs-input", run, "--repo"])
        .arg(w.repo.root())
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_NOTIFY_CMD", notifier)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn question(text: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions": [{"question": text, "header": "Color", "options": [], "multiSelect": false}]},
    })
    .to_string()
}

fn permission() -> String {
    serde_json::json!({
        "hook_event_name": "Notification",
        "message": "Claude needs your permission to use Bash",
        "notification_type": "permission_prompt",
    })
    .to_string()
}

fn needs(store: &RunStore) -> Vec<Event> {
    store.events("run-1").unwrap().into_iter().filter(|e| e.kind == "run.needs_input").collect()
}

/// The lines the notifier has written once there are `count` of them.
fn sent_lines(path: &Path, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let lines: Vec<String> = std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect();
        if lines.len() >= count || Instant::now() > deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_question_is_recorded_on_the_run_and_its_text_is_sent_to_the_person() {
    let w = world("question");
    let out = hook(&w, "run-1", &question("Pick red or blue?"), "");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty(), "stdout would be read as the hook's decision");

    let events = needs(&w.store);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["kind"], "question");
    assert_eq!(events[0].payload["text"], "Pick red or blue?");
    assert_eq!(sent_lines(&w.sent, 1), ["orq: COM-01 needs your answer: Pick red or blue?"]);
}

#[test]
fn a_slow_notifier_never_holds_the_session_up() {
    let w = world("slow");
    let started = Instant::now();
    let out = hook(&w, "run-1", &question("Still there?"), "sleep 6; ");
    assert_eq!(out.status.code(), Some(0));
    assert!(started.elapsed() < Duration::from_secs(4), "took {:?}", started.elapsed());
    assert_eq!(needs(&w.store).len(), 1, "the need is on the run before the hook returns");
}

#[test]
fn a_permission_prompt_is_recorded_and_sent() {
    let w = world("permission");
    assert_eq!(hook(&w, "run-1", &permission(), "").status.code(), Some(0));
    let events = needs(&w.store);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["kind"], "permission");
    assert_eq!(sent_lines(&w.sent, 1), ["orq: COM-01 needs your permission: Claude needs your permission to use Bash"]);
}

#[test]
fn the_permission_prompt_of_a_question_just_asked_does_not_notify_twice() {
    let w = world("once");
    assert_eq!(hook(&w, "run-1", &question("Pick one?"), "").status.code(), Some(0));
    assert_eq!(hook(&w, "run-1", &permission(), "").status.code(), Some(0));
    assert_eq!(needs(&w.store).len(), 1);
    assert_eq!(sent_lines(&w.sent, 1).len(), 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sent_lines(&w.sent, 1).len(), 1);
}

#[test]
fn a_permission_prompt_long_after_a_question_is_its_own_need() {
    let w = world("later");
    let run = w.store.load("run-1").unwrap();
    w.store
        .append_event(&Event {
            schema_version: 1,
            event_id: "old".into(),
            run_id: run.run_id,
            project_id: run.project_id,
            feature_id: run.feature_id,
            kind: "run.needs_input".into(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-01-01T00:00:00Z".into(),
            payload: serde_json::json!({"kind": "question", "text": "Earlier?"}),
        })
        .unwrap();
    assert_eq!(hook(&w, "run-1", &permission(), "").status.code(), Some(0));
    assert_eq!(needs(&w.store).len(), 2);
    assert_eq!(sent_lines(&w.sent, 1).len(), 1);
}

#[test]
fn every_question_of_one_call_is_sent_on_one_redacted_line() {
    let w = world("redact");
    let input = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions": [{"question": "Use token=abc123\nfor this?"}, {"question": "And then?"}]},
    })
    .to_string();
    assert_eq!(hook(&w, "run-1", &input, "").status.code(), Some(0));

    let lines = sent_lines(&w.sent, 1);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("And then?"), "{lines:?}");
    assert!(!lines[0].contains("abc123"), "{lines:?}");
    let recorded = needs(&w.store)[0].payload["text"].as_str().unwrap().to_string();
    assert!(recorded.contains("And then?"), "{recorded}");
    assert!(!recorded.contains("abc123") && !recorded.contains('\n'), "{recorded}");
}

#[test]
fn input_it_cannot_use_leaves_the_session_alone() {
    let w = world("bad");
    for input in ["not json", "{}", r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{}}"#, r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"waiting"}"#] {
        let out = hook(&w, "run-1", input, "");
        assert_eq!(out.status.code(), Some(0), "{input}");
        assert!(out.stdout.is_empty(), "{input}");
    }
    assert!(needs(&w.store).is_empty());

    // A run that is gone still never denies the tool: exit code 2 would.
    let out = hook(&w, "no-such-run", &question("Anyone?"), "");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let out = hook(&w, "../escape", &question("Anyone?"), "");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!w.sent.exists());
}

#[test]
fn a_permission_prompt_after_the_question_is_answered_is_its_own_need() {
    let w = world("answered");
    assert_eq!(hook(&w, "run-1", &question("Pick one?"), "").status.code(), Some(0));
    let answered = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions": [{"question": "Pick one?"}]},
        "tool_response": {"answers": {"Pick one?": "the secret answer"}},
    })
    .to_string();
    let out = hook(&w, "run-1", &answered, "");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty());
    assert_eq!(hook(&w, "run-1", &permission(), "").status.code(), Some(0));

    let kinds: Vec<String> = needs(&w.store).iter().map(|e| e.payload["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, ["question", "permission"]);
    let given: Vec<Event> = w.store.events("run-1").unwrap().into_iter().filter(|e| e.kind == "run.input_given").collect();
    assert_eq!(given.len(), 1);
    assert!(!given[0].payload.to_string().contains("secret"), "what was answered stays out of the record");
    assert_eq!(sent_lines(&w.sent, 2).len(), 2, "the answer itself tells nobody");
}

#[test]
fn a_notifier_that_fails_leaves_the_session_alone_and_the_need_on_the_run() {
    let w = world("notifier-fails");
    let failing = format!("printf 'ran\\n' >> '{}'; exit 7; ", w.sent.display());
    let started = Instant::now();
    let out = hook(&w, "run-1", &question("Anyone?"), &failing);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty());
    assert!(started.elapsed() < Duration::from_secs(4), "took {:?}", started.elapsed());
    assert_eq!(needs(&w.store).len(), 1);
    assert_eq!(sent_lines(&w.sent, 1), ["ran"], "the notifier did run, and failed");
}

#[test]
fn run_settings_it_cannot_read_leave_the_session_alone_and_the_need_on_the_run() {
    let w = world("bad-settings");
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "not = [valid").unwrap();
    let out = hook(&w, "run-1", &question("Anyone?"), "");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty());
    assert_eq!(needs(&w.store).len(), 1);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!w.sent.exists(), "settings that cannot be read send nothing");
}

#[test]
fn a_permission_prompt_after_a_question_was_left_unanswered_is_told_about() {
    let w = world("rejected");
    // A rejected question fires no hook, and a tool's prompt reads the same as a question's: only
    // the few seconds a question's own prompt takes tell them apart.
    let asked = Command::new("date").args(["-u", "-d", "12 seconds ago", "+%Y-%m-%dT%H:%M:%SZ"]).output().unwrap();
    let run = w.store.load("run-1").unwrap();
    w.store
        .append_event(&Event {
            schema_version: 1,
            event_id: "asked".into(),
            run_id: run.run_id,
            project_id: run.project_id,
            feature_id: run.feature_id,
            kind: "run.needs_input".into(),
            stage: None,
            attempt: 1,
            occurred_at: String::from_utf8(asked.stdout).unwrap().trim().to_string(),
            payload: serde_json::json!({"kind": "question", "text": "Pick one?"}),
        })
        .unwrap();
    assert_eq!(hook(&w, "run-1", &permission(), "").status.code(), Some(0));
    assert_eq!(needs(&w.store).len(), 2);
    assert_eq!(sent_lines(&w.sent, 1).len(), 1);
}

