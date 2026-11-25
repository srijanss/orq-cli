//! "Ask orq": a chat with Claude Code about one project, run by the server. Each message is one `claude -p`
//! turn in the project's main checkout; the replies come back as items. The agent here is a fake `claude`
//! that records its argv, cwd and stdin and prints canned stream-json.

mod web_deps;
mod web_push_session;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use orq_core::remote_inbox::{handle_with_confirm, RemoteConfig, Request, StepUp};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    base: PathBuf,
    scan: PathBuf,
    roots: StorageRoots,
    provider: StoreProjects,
}

const AGENT: &str = "remote_actions = true\n\n[ask_orq_agent]\nharness = \"claude\"\nmodel = \"opus\"\neffort = \"high\"\n";

/// The canned reply: a text with a bearer token in it, a tool call and its result, and the result line.
fn canned_reply() -> String {
    [
        json!({"type": "system", "subtype": "init", "session_id": "s"}),
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "It failed: Bearer abcdefgh12345678 was refused"}]}}),
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/runs/r1/events.jsonl"}}]}}),
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": "line one", "is_error": false}]}}),
        json!({"type": "result", "subtype": "success", "is_error": false, "duration_ms": 1234, "total_cost_usd": 0.05, "result": "done"}),
    ]
    .iter()
    .map(|line| format!("{line}\n"))
    .collect()
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(&format!("ask-orq-{label}"));
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), AGENT).unwrap();
    std::fs::write(base.join("reply.jsonl"), canned_reply()).unwrap();
    let agent = base.join("fake-claude");
    // Each call records into a folder of its own, named so the calls sort in the order they started.
    let script = format!(
        "#!/bin/sh\nd='{calls}'/$(date +%s%N)-$$\nmkdir -p \"$d\"\nprintf '%s\\0' \"$@\" > \"$d/args\"\npwd > \"$d/cwd\"\nwhile [ -e '{deaf}' ]; do sleep 0.05; done\ncat > \"$d/stdin\"\nif [ -e '{crash}' ]; then echo 'not logged in' >&2; exit 3; fi\ncat '{reply}'\nwhile [ -e '{hold}' ]; do sleep 0.05; done\nif [ -e '{more}' ]; then cat '{more}'; fi\n",
        calls = base.join("calls").display(),
        reply = base.join("reply.jsonl").display(),
        hold = base.join("hold").display(),
        more = base.join("more.jsonl").display(),
        deaf = base.join("deaf").display(),
        crash = base.join("crash").display(),
    );
    std::fs::write(&agent, script).unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scan = base.join("code");
    let provider = provider(&roots, &scan, &agent);
    Setup {
        base,
        scan,
        roots,
        provider,
    }
}

fn provider(roots: &StorageRoots, scan: &Path, agent: &Path) -> StoreProjects {
    let options: std::sync::Arc<orq_remote::projects::AgentOptionsSource> =
        std::sync::Arc::new(|| {
            vec![
                (
                    "claude".to_string(),
                    vec!["opus".to_string(), "sonnet".to_string()],
                ),
                ("pi".to_string(), vec!["openai/gpt-6.1-sol".to_string()]),
            ]
        });
    StoreProjects::new(roots.clone(), scan.to_path_buf())
        .with_agent_options(options)
        .with_chat_agent(agent.to_path_buf())
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&path)
        .status()
        .unwrap()
        .success());
    path
}

/// A request from a passkey session (`passkey`) or from the token alone.
fn send_as(s: &Setup, method: &str, path: &str, body: Value, passkey: bool) -> (u16, Value) {
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method,
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let yes = |_: &Value| true;
    let confirm: Option<StepUp> = passkey.then_some(&yes as StepUp);
    let body = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    let r = handle_with_confirm(
        &s.roots,
        &config,
        "server-project",
        "server",
        &request,
        &body,
        &s.provider,
        confirm,
    );
    (r.status, r.body)
}

fn send(s: &Setup, method: &str, path: &str, body: Value) -> (u16, Value) {
    send_as(s, method, path, body, true)
}

fn create_chat(s: &Setup, project: &str, body: Value) -> String {
    let (status, created) = send(s, "POST", &format!("/api/projects/{project}/chats"), body);
    assert_eq!(status, 200, "{created}");
    created["chat"]["id"].as_str().unwrap().to_string()
}

fn wait_for(what: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !what() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_chat(s: &Setup, project: &str, chat: &str) -> Value {
    let (status, body) = send(
        s,
        "GET",
        &format!("/api/projects/{project}/chats/{chat}"),
        Value::Null,
    );
    assert_eq!(status, 200, "{body}");
    body
}

/// Sends `text` and waits for its turn to end.
fn say(s: &Setup, project: &str, chat: &str, text: &str) {
    let (status, body) = send(
        s,
        "POST",
        &format!("/api/projects/{project}/chats/{chat}/messages"),
        json!({"text": text}),
    );
    assert_eq!(status, 202, "{body}");
    wait_for(|| read_chat(s, project, chat)["running"] == false);
}

/// Each call of the fake agent so far, oldest first: its argv, cwd and stdin.
fn calls(s: &Setup) -> Vec<(Vec<String>, String, String)> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(s.base.join("calls"))
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default();
    dirs.sort();
    dirs.iter()
        .map(|dir| {
            let args = std::fs::read(dir.join("args")).unwrap_or_default();
            let args = args
                .split(|b| *b == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect();
            (
                args,
                std::fs::read_to_string(dir.join("cwd"))
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                std::fs::read_to_string(dir.join("stdin")).unwrap_or_default(),
            )
        })
        .collect()
}

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

#[test]
fn a_chat_is_created_and_each_message_is_a_claude_turn_in_the_main_checkout_whose_reply_comes_back_as_redacted_items(
) {
    let s = setup("create-send-read");
    let alpha = git_repo(&s.scan, "alpha");
    let chat = create_chat(&s, "alpha", json!({}));

    say(&s, "alpha", &chat, "why did it fail?");
    say(&s, "alpha", &chat, "and now?");

    let calls = calls(&s);
    assert_eq!(calls.len(), 2);
    let (first, cwd, stdin) = &calls[0];
    assert!(first.contains(&"-p".to_string()), "{first:?}");
    assert_eq!(value_after(first, "--output-format"), Some("stream-json"));
    assert!(first.contains(&"--verbose".to_string()));
    let session = value_after(first, "--session-id")
        .expect("the first turn names its session")
        .to_string();
    assert_eq!(session.len(), 36, "a UUID: {session}");
    assert!(
        !first.iter().any(|arg| arg.contains("why did it fail")),
        "the text is never an argument"
    );
    assert!(stdin.contains("why did it fail?"), "{stdin}");
    assert_eq!(
        Path::new(cwd).canonicalize().unwrap(),
        alpha.canonicalize().unwrap()
    );
    let (second, _, stdin) = &calls[1];
    assert_eq!(
        value_after(second, "--resume"),
        Some(session.as_str()),
        "{second:?}"
    );
    assert!(!second.contains(&"--session-id".to_string()));
    assert_eq!(stdin.trim(), "and now?");

    let read = read_chat(&s, "alpha", &chat);
    assert_eq!(read["chat"]["title"], "why did it fail?");
    let items = read["items"].as_array().unwrap();
    let kinds: Vec<&str> = items
        .iter()
        .map(|item| item["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "user",
            "assistant",
            "tool",
            "result",
            "user",
            "assistant",
            "tool",
            "result"
        ]
    );
    assert_eq!(items[0]["text"], "why did it fail?");
    assert_eq!(items[1]["text"], "It failed: Bearer [REDACTED] was refused");
    assert_eq!(items[2]["name"], "Read");
    assert_eq!(items[2]["input"], "/runs/r1/events.jsonl");
    assert_eq!(items[2]["status"], "ok");
    assert_eq!(items[2]["result"], "line one");
    assert_eq!(items[3]["cost"], 0.05);
    assert_eq!(items[3]["duration_ms"], 1234);
    assert!(items[3]["error"].is_null());

    let (status, listed) = send(&s, "GET", "/api/projects/alpha/chats", Value::Null);
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["chats"][0]["id"], chat.as_str());
}

#[test]
fn a_turn_has_only_the_reading_tools_whatever_settings_and_servers_it_would_inherit() {
    let s = setup("only-reading-tools");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s, "alpha", json!({}));

    say(&s, "alpha", &chat, "what is in the queue?");

    let calls = calls(&s);
    let (args, _, _) = &calls[0];
    // A user's or project's settings allowing Bash or Write cannot widen what a chat may use.
    assert_eq!(value_after(args, "--tools"), Some("Read,Grep,Glob"), "{args:?}");
    // Nor can an MCP server from the project's or user's configuration.
    assert!(args.contains(&"--strict-mcp-config".to_string()), "{args:?}");
}

fn project_id(repo: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(repo).unwrap())
}

/// Records run `run` of the git project `project` with events of `kinds` (each with its payload).
fn add_run(s: &Setup, run: &str, project: &str, feature: &str, events: &[(&str, Value)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord {
            run_id: run.into(),
            project_id: project.into(),
            feature_id: feature.into(),
            remote: String::new(),
            provider: String::new(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: format!("orq/{run}"),
            worktree: s.scan.join("alpha/.claude/worktrees").join(run),
            repository: String::new(),
            profiles: Default::default(),
            delivery: Default::default(),
        })
        .unwrap();
    for (n, (kind, payload)) in events.iter().enumerate() {
        let stage = kind.starts_with("stage.").then(|| "gate".to_string());
        let event = Event {
            schema_version: 1,
            event_id: format!("{run}-{n}"),
            run_id: run.into(),
            project_id: project.into(),
            feature_id: feature.into(),
            kind: (*kind).into(),
            stage,
            attempt: 1,
            occurred_at: format!("2026-11-25T10:00:0{n}Z"),
            payload: payload.clone(),
        };
        store.append_event(&event).unwrap();
    }
}

#[test]
fn a_run_chat_starts_its_first_message_with_what_the_run_is_and_every_turn_can_read_the_run_folders(
) {
    let s = setup("run-context");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(
        &s,
        "r-ctx",
        &project_id(&alpha),
        "login-page",
        &[
            ("run.started", json!({})),
            ("stage.started", json!({})),
            (
                "run.completed",
                json!({"ending": "failed", "reason": "the gate failed: 3 tests"}),
            ),
        ],
    );
    let chat = create_chat(
        &s,
        "alpha",
        json!({"context": {"kind": "run", "target": "r-ctx"}}),
    );

    say(&s, "alpha", &chat, "what happened?");
    say(&s, "alpha", &chat, "and then?");

    let calls = calls(&s);
    let (args, _, stdin) = &calls[0];
    let run_dir = s.roots.state.join("runs").join("r-ctx");
    for needed in [
        "r-ctx",
        "login-page",
        "failed",
        "the gate failed: 3 tests",
        &run_dir.join("events.jsonl").display().to_string(),
        &s.roots
            .state
            .join("logs")
            .join("r-ctx")
            .display()
            .to_string(),
        &alpha.join(".claude/worktrees/r-ctx").display().to_string(),
    ] {
        assert!(
            stdin.contains(needed),
            "the context names {needed}: {stdin}"
        );
    }
    assert!(
        stdin.trim_end().ends_with("what happened?"),
        "the person's text comes after the context: {stdin}"
    );
    assert_eq!(
        calls[1].2.trim(),
        "and then?",
        "only the first message carries the context"
    );

    let dirs: Vec<&str> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "--add-dir")
        .map(|(at, _)| args[at + 1].as_str())
        .collect();
    assert_eq!(
        dirs,
        [
            s.roots.state.join("runs").display().to_string(),
            s.roots.state.join("logs").display().to_string(),
            alpha.join(".claude/worktrees").display().to_string()
        ]
    );

    let read = read_chat(&s, "alpha", &chat);
    assert_eq!(
        read["items"][0]["text"], "what happened?",
        "the context is not shown as typed by the person"
    );
    assert_eq!(
        read["chat"]["context"],
        json!({"kind": "run", "target": "r-ctx"})
    );
}

/// The first stdin of a new chat opened on `context`, after one message.
fn first_stdin(s: &Setup, context: Value) -> String {
    let before = calls(s).len();
    let chat = create_chat(s, "alpha", json!({ "context": context }));
    say(s, "alpha", &chat, "tell me");
    calls(s)[before].2.clone()
}

#[test]
fn a_feature_queue_or_project_chat_starts_with_what_that_page_shows() {
    let s = setup("other-contexts");
    let alpha = git_repo(&s.scan, "alpha");
    std::fs::write(alpha.join(".tdd-features.json"), json!([{"featureName": "login-page", "status": "pending"}, {"featureName": "signup", "status": "pending"}]).to_string()).unwrap();
    add_run(
        &s,
        "r-live",
        &project_id(&alpha),
        "login-page",
        &[("run.started", json!({}))],
    );
    // Paused first, so the queue never tries to start what it holds.
    let (status, body) = send(&s, "POST", "/api/projects/alpha/queue/pause", Value::Null);
    assert_eq!(status, 200, "{body}");
    let (status, body) = send(
        &s,
        "POST",
        "/api/projects/alpha/queue",
        json!({"features": ["signup"]}),
    );
    assert_eq!(status, 200, "{body}");

    let feature = first_stdin(&s, json!({"kind": "feature", "target": "login-page"}));
    for needed in ["login-page", "pending", "r-live", "running"] {
        assert!(
            feature.contains(needed),
            "a feature chat names {needed}: {feature}"
        );
    }
    let queue = first_stdin(&s, json!({"kind": "queue"}));
    for needed in ["signup", "paused"] {
        assert!(
            queue.contains(needed),
            "a queue chat names {needed}: {queue}"
        );
    }
    let project = first_stdin(&s, json!({"kind": "project"}));
    for needed in [
        alpha.canonicalize().unwrap().display().to_string(),
        "r-live".to_string(),
    ] {
        assert!(
            project.contains(&needed),
            "a project chat names {needed}: {project}"
        );
    }
    assert!(project.trim_end().ends_with("tell me"), "{project}");
}

#[test]
fn a_chat_takes_only_this_projects_runs_and_features_and_the_ask_orq_agents_claude_models() {
    let s = setup("context-and-agent");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    std::fs::write(
        alpha.join(".tdd-features.json"),
        json!([{"featureName": "login-page", "status": "pending"}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        beta.join(".tdd-features.json"),
        json!([{"featureName": "beta-only", "status": "pending"}]).to_string(),
    )
    .unwrap();
    add_run(
        &s,
        "r-beta",
        &project_id(&beta),
        "beta-only",
        &[("run.started", json!({}))],
    );
    for context in [
        json!({"kind": "run", "target": "r-beta"}),
        json!({"kind": "feature", "target": "beta-only"}),
        json!({"kind": "run", "target": "no-such-run"}),
    ] {
        let (status, body) = send(
            &s,
            "POST",
            "/api/projects/alpha/chats",
            json!({ "context": context }),
        );
        assert_eq!(
            status, 422,
            "{context} is no run or feature of alpha: {body}"
        );
    }
    create_chat(
        &s,
        "alpha",
        json!({"context": {"kind": "feature", "target": "login-page"}}),
    );

    let global = create_chat(&s, "alpha", json!({}));
    say(&s, "alpha", &global, "hi");
    let picked = create_chat(&s, "alpha", json!({"model": "sonnet"}));
    say(&s, "alpha", &picked, "hi");
    let calls = calls(&s);
    assert_eq!(
        (
            value_after(&calls[0].0, "--model"),
            value_after(&calls[0].0, "--effort")
        ),
        (Some("opus"), Some("high")),
        "{:?}",
        calls[0].0
    );
    assert_eq!(
        value_after(&calls[1].0, "--model"),
        Some("sonnet"),
        "{:?}",
        calls[1].0
    );
    assert_eq!(read_chat(&s, "alpha", &picked)["chat"]["model"], "sonnet");

    for model in ["openai/gpt-6.1-sol", "no-such-model"] {
        let (status, body) = send(
            &s,
            "POST",
            "/api/projects/alpha/chats",
            json!({ "model": model }),
        );
        assert_eq!(
            status, 422,
            "{model} is not a Claude model on offer: {body}"
        );
    }

    std::fs::write(s.roots.config_file(), "remote_actions = true\n").unwrap();
    let (status, body) = send(&s, "POST", "/api/projects/alpha/chats", json!({}));
    assert_eq!(
        (status, body["error"].as_str()),
        (409, Some("set the Ask orq agent in Settings")),
        "{body}"
    );
}

fn settings_version(s: &Setup) -> String {
    let (status, body) = send(s, "GET", "/api/settings", Value::Null);
    assert_eq!(status, 200, "{body}");
    body["settings"]["version"].as_str().unwrap().to_string()
}

fn audit_lines(s: &Setup) -> Vec<Value> {
    std::fs::read_to_string(s.roots.state.join("remote-actions.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_ask_orq_agent_is_set_through_the_settings_with_a_passkey_and_remote_actions_and_only_to_claude(
) {
    let s = setup("agent-setting");
    let change = |agent: Value, passkey: bool| {
        send_as(
            &s,
            "POST",
            "/api/settings/agents",
            json!({"ask_orq_agent": agent, "version": settings_version(&s)}),
            passkey,
        )
    };

    let (status, body) = change(json!({"harness": "claude", "model": "sonnet"}), false);
    assert_eq!(status, 403, "the token alone cannot change it: {body}");
    let (status, body) = change(
        json!({"harness": "pi", "model": "openai/gpt-6.1-sol"}),
        true,
    );
    assert_eq!(status, 422, "only Claude: {body}");
    let (status, body) = change(json!({"harness": "claude", "model": "sonnet"}), true);
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["settings"]["ask_orq_agent"]["model"], "sonnet",
        "{body}"
    );
    assert_eq!(
        body["settings"]["ask_orq_agent"]["harness"], "claude",
        "{body}"
    );

    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    let (status, body) = change(json!({"harness": "claude", "model": "opus"}), true);
    assert_eq!(status, 404, "remote actions off: {body}");
    assert!(!std::fs::read_to_string(s.roots.config_file())
        .unwrap()
        .contains("ask_orq_agent"));

    let outcomes: Vec<Value> = audit_lines(&s)
        .into_iter()
        .filter(|line| line["action"] == "settings.agents")
        .map(|line| line["outcome"].clone())
        .collect();
    assert_eq!(outcomes, ["refused", "refused", "allowed", "refused"]);
}

fn message(s: &Setup, chat: &str, text: &str) -> (u16, Value) {
    send(
        s,
        "POST",
        &format!("/api/projects/alpha/chats/{chat}/messages"),
        json!({ "text": text }),
    )
}

#[test]
fn a_chat_writes_one_reply_at_a_time_and_the_machine_at_most_two() {
    let s = setup("turn-limits");
    git_repo(&s.scan, "alpha");
    let (a, b, c) = (
        create_chat(&s, "alpha", json!({})),
        create_chat(&s, "alpha", json!({})),
        create_chat(&s, "alpha", json!({})),
    );
    std::fs::write(s.base.join("hold"), "").unwrap();

    assert_eq!(message(&s, &a, "first").0, 202);
    let (status, body) = message(&s, &a, "second");
    assert_eq!(
        (status, body["error"].as_str()),
        (409, Some("a reply is still being written")),
        "{body}"
    );
    assert_eq!(message(&s, &b, "first").0, 202);
    let (status, body) = message(&s, &c, "first");
    assert_eq!(status, 409, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains('2'),
        "the refusal gives the count: {body}"
    );
    assert_eq!(
        read_chat(&s, "alpha", &c)["items"],
        json!([]),
        "a refused message leaves no turn"
    );

    std::fs::remove_file(s.base.join("hold")).unwrap();
    wait_for(|| {
        read_chat(&s, "alpha", &a)["running"] == false
            && read_chat(&s, "alpha", &b)["running"] == false
    });
    say(&s, "alpha", &c, "now");
    assert_eq!(calls(&s).len(), 3, "the refused messages started nothing");
}

/// The pid of the fake agent's `n`th call (from its calls folder's name), once it has started.
fn call_pid(s: &Setup, n: usize) -> u32 {
    wait_for(|| calls(s).len() > n);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(s.base.join("calls"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    dirs.sort();
    dirs[n]
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .rsplit('-')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn stop_kills_the_reply_being_written_and_records_that_it_was_stopped() {
    let s = setup("stop");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s, "alpha", json!({}));
    let (status, body) = send(
        &s,
        "POST",
        &format!("/api/projects/alpha/chats/{chat}/stop"),
        Value::Null,
    );
    assert_eq!(status, 409, "nothing to stop: {body}");

    std::fs::write(s.base.join("hold"), "").unwrap();
    assert_eq!(message(&s, &chat, "a long one").0, 202);
    let pid = call_pid(&s, 0);
    let (status, body) = send(
        &s,
        "POST",
        &format!("/api/projects/alpha/chats/{chat}/stop"),
        Value::Null,
    );
    assert_eq!(status, 200, "{body}");

    wait_for(|| !Path::new(&format!("/proc/{pid}")).exists());
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the fake was killed while still held"
    );
    let read = read_chat(&s, "alpha", &chat);
    assert_eq!(read["running"], false);
    assert_eq!(
        (
            read["items"][0]["state"].as_str(),
            read["items"][0]["reason"].as_str()
        ),
        (Some("stopped"), Some("stopped from the web")),
        "{read}"
    );
    assert_eq!(
        message(&s, &chat, "next").0,
        202,
        "a stopped chat takes the next message"
    );
    std::fs::remove_file(s.base.join("hold")).unwrap();
}

#[test]
fn a_reply_over_the_time_limit_is_stopped_and_one_left_running_at_startup_is_marked_interrupted() {
    let s = setup("time-limit");
    let s = Setup {
        provider: provider(&s.roots, &s.scan, &s.base.join("fake-claude"))
            .with_chat_time_limit(Duration::from_millis(300)),
        ..s
    };
    git_repo(&s.scan, "alpha");
    std::fs::write(s.base.join("hold"), "").unwrap();
    let slow = create_chat(&s, "alpha", json!({}));
    assert_eq!(message(&s, &slow, "take your time").0, 202);
    let pid = call_pid(&s, 0);
    wait_for(|| read_chat(&s, "alpha", &slow)["running"] == false);
    let item = read_chat(&s, "alpha", &slow)["items"][0].clone();
    assert_eq!(
        (item["state"].as_str(), item["reason"].as_str()),
        (Some("stopped"), Some("the reply ran out of time")),
        "{item}"
    );
    wait_for(|| !Path::new(&format!("/proc/{pid}")).exists());
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the held fake was killed"
    );

    let s = Setup {
        provider: provider(&s.roots, &s.scan, &s.base.join("fake-claude")),
        ..s
    };
    let left = create_chat(&s, "alpha", json!({}));
    assert_eq!(message(&s, &left, "still going").0, 202);
    // The server starts again: what it finds running was cut off.
    let s = Setup {
        provider: provider(&s.roots, &s.scan, &s.base.join("fake-claude")),
        ..s
    };
    let read = read_chat(&s, "alpha", &left);
    assert_eq!(read["running"], false, "{read}");
    assert_eq!(read["items"][0]["state"], "interrupted", "{read}");
    std::fs::remove_file(s.base.join("hold")).unwrap();
}

#[test]
fn a_reply_that_never_reads_its_message_is_still_stopped_at_the_time_limit() {
    let s = setup("deaf");
    let s = Setup {
        provider: provider(&s.roots, &s.scan, &s.base.join("fake-claude"))
            .with_chat_time_limit(Duration::from_millis(300)),
        ..s
    };
    git_repo(&s.scan, "alpha");
    // The fake reads nothing while this is there, and the longest message is more than a pipe holds.
    std::fs::write(s.base.join("deaf"), "").unwrap();
    let chat = create_chat(&s, "alpha", json!({}));
    let long: String = "𝄞".repeat(20_000);
    assert_eq!(message(&s, &chat, &long).0, 202);
    let pid = call_pid(&s, 0);

    wait_for(|| read_chat(&s, "alpha", &chat)["running"] == false);
    let item = read_chat(&s, "alpha", &chat)["items"][0].clone();
    let state = (item["state"].clone(), item["reason"].clone());
    wait_for(|| !Path::new(&format!("/proc/{pid}")).exists());
    let gone = !Path::new(&format!("/proc/{pid}")).exists();
    std::fs::remove_file(s.base.join("deaf")).unwrap();
    assert_eq!(state, (json!("stopped"), json!("the reply ran out of time")), "{item}");
    assert!(gone, "the fake that would not read was killed");
}

#[test]
fn a_reply_whose_agent_fails_or_cannot_start_is_recorded_as_failed_not_done() {
    let s = setup("agent-fails");
    git_repo(&s.scan, "alpha");
    std::fs::write(s.base.join("crash"), "").unwrap();
    let chat = create_chat(&s, "alpha", json!({}));
    say(&s, "alpha", &chat, "why did it fail?");
    let read = read_chat(&s, "alpha", &chat);
    let item = &read["items"][0];
    assert_eq!(item["state"], "failed", "an agent that exits with an error wrote no reply: {read}");
    assert_eq!(item["reason"], "the reply ended with an error (exit status 3)", "{read}");
    assert_eq!(read["items"].as_array().unwrap().len(), 1, "{read}");

    let s = Setup {
        provider: provider(&s.roots, &s.scan, &s.base.join("no-such-claude")),
        ..s
    };
    let chat = create_chat(&s, "alpha", json!({}));
    let (status, body) = message(&s, &chat, "hello?");
    assert_eq!(status, 500, "{body}");
    let read = read_chat(&s, "alpha", &chat);
    assert_eq!(read["running"], false, "{read}");
    assert_eq!(
        (read["items"][0]["state"].as_str(), read["items"][0]["reason"].as_str()),
        (Some("failed"), Some("the reply could not be started")),
        "{read}"
    );
}

#[test]
fn a_chat_with_no_reply_being_written_is_deleted_with_its_turn_files() {
    let s = setup("delete");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s, "alpha", json!({}));
    let path = format!("/api/projects/alpha/chats/{chat}");
    std::fs::write(s.base.join("hold"), "").unwrap();
    assert_eq!(message(&s, &chat, "hello").0, 202);
    let (status, body) = send(&s, "DELETE", &path, Value::Null);
    assert_eq!(
        (status, body["error"].as_str()),
        (409, Some("a reply is still being written")),
        "{body}"
    );

    std::fs::remove_file(s.base.join("hold")).unwrap();
    wait_for(|| read_chat(&s, "alpha", &chat)["running"] == false);
    let folder = s.roots.state.join("chats").join(&chat);
    assert!(folder.join("turn-1.jsonl").exists());
    let (status, body) = send(&s, "DELETE", &path, Value::Null);
    assert_eq!(status, 200, "{body}");
    assert!(!folder.exists(), "the turn files go with the chat");
    assert_eq!(send(&s, "GET", &path, Value::Null).0, 404);
    assert_eq!(send(&s, "DELETE", &path, Value::Null).0, 404);
    assert_eq!(
        send(&s, "GET", "/api/projects/alpha/chats", Value::Null).1["chats"],
        json!([])
    );
}

#[test]
fn chats_need_a_passkey_session_writes_need_remote_actions_and_each_write_is_audited() {
    let s = setup("access");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s, "alpha", json!({}));
    let base = format!("/api/projects/alpha/chats/{chat}");
    let every = [
        ("GET", "/api/projects/alpha/chats".to_string(), Value::Null),
        ("POST", "/api/projects/alpha/chats".to_string(), json!({})),
        ("GET", base.clone(), Value::Null),
        ("GET", format!("{base}/stream"), Value::Null),
        ("POST", format!("{base}/messages"), json!({"text": "hi"})),
        ("POST", format!("{base}/stop"), Value::Null),
        ("DELETE", base.clone(), Value::Null),
    ];
    for (method, path, body) in &every {
        let (status, reply) = send_as(&s, method, path, body.clone(), false);
        assert_eq!(status, 403, "{method} {path} with the token alone: {reply}");
    }

    std::fs::write(
        s.roots.config_file(),
        AGENT.replace("remote_actions = true", "remote_actions = false"),
    )
    .unwrap();
    for (method, path, body) in every.iter().filter(|(method, _, _)| *method != "GET") {
        let (status, reply) = send(&s, method, path, body.clone());
        assert_eq!(
            status, 403,
            "{method} {path} with remote actions off: {reply}"
        );
    }
    assert_eq!(
        send(&s, "GET", &base, Value::Null).0,
        200,
        "reading needs no remote actions"
    );
    std::fs::write(s.roots.config_file(), AGENT).unwrap();

    let long = format!("why did {} fail?", "x".repeat(100));
    std::fs::write(s.base.join("hold"), "").unwrap();
    assert_eq!(message(&s, &chat, &long).0, 202);
    assert_eq!(
        send(&s, "POST", &format!("{base}/stop"), Value::Null).0,
        200
    );
    std::fs::remove_file(s.base.join("hold")).unwrap();
    wait_for(|| read_chat(&s, "alpha", &chat)["running"] == false);
    assert_eq!(send(&s, "DELETE", &base, Value::Null).0, 200);

    let allowed: Vec<(String, String, String, String)> = audit_lines(&s)
        .into_iter()
        .filter(|line| {
            line["action"].as_str().unwrap().starts_with("chat.") && line["outcome"] == "allowed"
        })
        .map(|line| {
            [
                &line["action"],
                &line["project"],
                &line["run"],
                &line["reason"],
            ]
            .map(|field| field.as_str().unwrap_or_default().to_string())
            .into()
        })
        .collect();
    let first_80: String = long.chars().take(80).collect();
    assert_eq!(
        allowed,
        [
            (
                "chat.message".to_string(),
                "alpha".to_string(),
                chat.clone(),
                first_80.clone()
            ),
            (
                "chat.stop".to_string(),
                "alpha".to_string(),
                chat.clone(),
                first_80.clone()
            ),
            (
                "chat.delete".to_string(),
                "alpha".to_string(),
                chat.clone(),
                first_80
            ),
        ]
    );
    assert!(
        audit_lines(&s)
            .iter()
            .any(|line| line["action"] == "chat.message" && line["outcome"] == "refused"),
        "refusals are audited too"
    );
}

/// Reads from an open stream for `limit`; everything it said.
fn read_for(stream: &mut std::net::TcpStream, limit: Duration) -> String {
    use std::io::Read;
    let start = Instant::now();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    while start.elapsed() < limit {
        if let Ok(n) = stream.read(&mut buf) {
            got.extend(&buf[..n]);
        }
    }
    String::from_utf8_lossy(&got).into_owned()
}

#[test]
fn the_stream_sends_a_chats_items_as_the_reply_is_written_and_only_to_a_passkey_session() {
    use std::io::Write;
    let s = setup("stream");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s, "alpha", json!({}));
    let more = json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "and the second part"}]}});
    std::fs::write(s.base.join("more.jsonl"), format!("{more}\n")).unwrap();
    std::fs::write(s.base.join("hold"), "").unwrap();

    let state = std::sync::Arc::new(orq_remote::ServerState {
        roots: s.roots.clone(),
        config: RemoteConfig {
            token: "tok".into(),
        },
        project_id: "server-project".into(),
        project_name: "server".into(),
    });
    let host = orq_core::passkey_register::PasskeyHost::from_setting(web_push_session::RP).unwrap();
    let auth = std::sync::Arc::new(
        orq_core::passkey_routes::PasskeyAuth::new(host).with_loopback_token(false),
    );
    let server = orq_remote::Server::start_with_auth(
        std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        state,
        std::sync::Arc::new(provider(&s.roots, &s.scan, &s.base.join("fake-claude"))),
        auth,
    )
    .unwrap();
    let session = web_push_session::sign_in(server.addr(), "tok");
    // A stream is a read: it needs the session's cookie, not its CSRF token.
    let _ = &session.csrf;
    let path = format!("/api/projects/alpha/chats/{chat}/stream");

    let (status, _, _) = web_push_session::raw(
        server.addr(),
        "GET",
        &path,
        &[("Authorization", "Bearer tok")],
        b"",
    );
    assert_ne!(status, 200, "the token alone gets no stream");

    assert_eq!(message(&s, &chat, "why did it fail?").0, 202);
    wait_for(|| calls(&s).len() == 1);
    let mut stream = std::net::TcpStream::connect(server.addr()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: x\r\nCookie: {}\r\n\r\n",
        session.cookie
    )
    .unwrap();
    let first = read_for(&mut stream, Duration::from_millis(2500));
    assert!(
        first.contains(" 200 ") && first.contains("text/event-stream"),
        "{first}"
    );
    assert!(
        first.contains("event: item")
            && first.contains("why did it fail?")
            && first.contains("It failed: Bearer [REDACTED] was refused"),
        "{first}"
    );
    assert!(
        !first.contains("and the second part"),
        "not printed yet: {first}"
    );

    std::fs::remove_file(s.base.join("hold")).unwrap();
    let rest = read_for(&mut stream, Duration::from_millis(2500));
    assert!(
        rest.contains("and the second part"),
        "the item printed later is sent: {rest}"
    );
    assert!(
        rest.contains("event: state") && rest.contains("\"running\":false"),
        "the end of the reply is sent: {rest}"
    );
}

/// A server for `s` with passkeys, and a signed-in session's cookie.
fn serve(s: &Setup) -> (orq_remote::Server, String) {
    let state = std::sync::Arc::new(orq_remote::ServerState {
        roots: s.roots.clone(),
        config: RemoteConfig { token: "tok".into() },
        project_id: "server-project".into(),
        project_name: "server".into(),
    });
    let host = orq_core::passkey_register::PasskeyHost::from_setting(web_push_session::RP).unwrap();
    let auth = std::sync::Arc::new(orq_core::passkey_routes::PasskeyAuth::new(host).with_loopback_token(false));
    let server = orq_remote::Server::start_with_auth(
        std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        state,
        std::sync::Arc::new(provider(&s.roots, &s.scan, &s.base.join("fake-claude"))),
        auth,
    )
    .unwrap();
    let session = web_push_session::sign_in(server.addr(), "tok");
    (server, session.cookie)
}

#[test]
fn a_stream_taken_up_again_sends_every_item_again_since_any_may_have_changed_meanwhile() {
    use std::io::Write;
    let s = setup("stream-again");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s, "alpha", json!({}));
    say(&s, "alpha", &chat, "why did it fail?");
    let (server, cookie) = serve(&s);

    // The page had every item, but the tool's (index 2) only while it was still running.
    let mut stream = std::net::TcpStream::connect(server.addr()).unwrap();
    stream.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    write!(
        stream,
        "GET /api/projects/alpha/chats/{chat}/stream HTTP/1.1\r\nHost: x\r\nCookie: {cookie}\r\nLast-Event-ID: 3\r\n\r\n"
    )
    .unwrap();
    let got = read_for(&mut stream, Duration::from_millis(1500));
    assert!(got.contains(" 200 "), "{got}");
    assert!(
        got.contains("id: 2\nevent: item") && got.contains("\"status\":\"ok\"") && got.contains("line one"),
        "the tool's finished state reaches the page: {got}"
    );
}
