//! What an Ask orq chat may do by itself, and how the person allows more. Each chat has its own Claude settings
//! file, rewritten before each turn; a refused call comes back as a `refused` item with the narrowest rule that
//! would allow it, which the person can allow once or for the chat. The agent here is a fake `claude` that records
//! its argv and a copy of the settings file it was given, and prints canned stream-json.

mod web_deps;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use orq_core::remote_inbox::{handle_with_confirm, RemoteConfig, Request, StepUp};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    base: PathBuf,
    scan: PathBuf,
    roots: StorageRoots,
    provider: StoreProjects,
}

const AGENT: &str =
    "remote_actions = true\n\n[ask_orq_agent]\nharness = \"claude\"\nmodel = \"opus\"\n";

/// The reading tools and orq's own read commands a chat may use by itself.
const DEFAULTS: [&str; 20] = [
    "Read",
    "Grep",
    "Glob",
    "Bash(orq runs:*)",
    "Bash(orq queue list:*)",
    "Bash(orq features:*)",
    "Bash(orq review show:*)",
    "Bash(orq worktree list:*)",
    "Bash(orq --version)",
    "Bash(git status:*)",
    "Bash(git log:*)",
    "Bash(git diff:*)",
    "Bash(git show:*)",
    "Bash(ls:*)",
    "Bash(cat:*)",
    "Bash(head:*)",
    "Bash(tail:*)",
    "Bash(wc:*)",
    "Bash(grep:*)",
    "mcp__project-mcp",
];

/// A reply that only says something.
fn quiet_reply() -> String {
    lines(&[
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "ok"}]}}),
        json!({"type": "result", "subtype": "success", "is_error": false, "result": "ok"}),
    ])
}

fn lines(events: &[Value]) -> String {
    events.iter().map(|line| format!("{line}\n")).collect()
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(&format!("ask-orq-permissions-{label}"));
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), AGENT).unwrap();
    std::fs::write(base.join("reply.jsonl"), quiet_reply()).unwrap();
    let agent = base.join("fake-claude");
    // Each call records into a folder of its own, named so the calls sort in the order they started, with a copy
    // of the settings file as it was when the turn started.
    let script = format!(
        "#!/bin/sh\nd='{calls}'/$(date +%s%N)-$$\nmkdir -p \"$d\"\nprintf '%s\\0' \"$@\" > \"$d/args\"\nprev=''\nfor a in \"$@\"; do if [ \"$prev\" = '--settings' ]; then cp \"$a\" \"$d/settings.json\"; fi; prev=\"$a\"; done\ncat > /dev/null\ncat '{reply}'\n",
        calls = base.join("calls").display(),
        reply = base.join("reply.jsonl").display(),
    );
    std::fs::write(&agent, script).unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scan = base.join("code");
    let options: std::sync::Arc<orq_remote::projects::AgentOptionsSource> =
        std::sync::Arc::new(|| vec![("claude".to_string(), vec!["opus".to_string()])]);
    let provider = StoreProjects::new(roots.clone(), scan.clone())
        .with_agent_options(options)
        .with_chat_agent(agent);
    Setup {
        base,
        scan,
        roots,
        provider,
    }
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

/// How a request is signed in.
#[derive(Clone, Copy, PartialEq)]
enum Who {
    /// A passkey session; its step-up check passes only for the assertion `"yes"`.
    Passkey,
    /// The access token alone.
    Token,
}

fn send_as(s: &Setup, method: &str, path: &str, body: Value, who: Who) -> (u16, Value) {
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method,
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let check = |assertion: &Value| assertion == "yes";
    let confirm: Option<StepUp> = (who == Who::Passkey).then_some(&check as StepUp);
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
    send_as(s, method, path, body, Who::Passkey)
}

fn create_chat(s: &Setup) -> String {
    let (status, created) = send(s, "POST", "/api/projects/alpha/chats", json!({}));
    assert_eq!(status, 200, "{created}");
    created["chat"]["id"].as_str().unwrap().to_string()
}

fn wait_for(what: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !what() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_chat(s: &Setup, chat: &str) -> Value {
    let (status, body) = send(
        s,
        "GET",
        &format!("/api/projects/alpha/chats/{chat}"),
        Value::Null,
    );
    assert_eq!(status, 200, "{body}");
    body
}

/// Sends `text` and waits for its turn to end.
fn say(s: &Setup, chat: &str, text: &str) {
    let (status, body) = send(
        s,
        "POST",
        &format!("/api/projects/alpha/chats/{chat}/messages"),
        json!({"text": text}),
    );
    assert_eq!(status, 202, "{body}");
    wait_for(|| read_chat(s, chat)["running"] == false);
}

/// Each call of the fake agent so far, oldest first: its argv and the settings file it was given.
fn calls(s: &Setup) -> Vec<(Vec<String>, Value)> {
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
            let settings = std::fs::read_to_string(dir.join("settings.json"))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(Value::Null);
            (args, settings)
        })
        .collect()
}

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("a list: {value}"))
        .iter()
        .map(|rule| rule.as_str().unwrap().to_string())
        .collect()
}

/// The rules always denied, whatever is allowed later.
fn denies(s: &Setup) -> Vec<String> {
    let token = RemoteConfig::token_path(&s.roots);
    let passkeys = orq_core::passkey_store::PasskeyStore::path(&s.roots);
    [
        "Bash(git push:*)".to_string(),
        "Bash(cargo publish:*)".to_string(),
        "Bash(npm publish:*)".to_string(),
        "Bash(rm -rf:*)".to_string(),
        "Bash(orq serve:*)".to_string(),
        "Bash(sudo:*)".to_string(),
        format!("Read(/{})", token.display()),
        format!("Read(/{})", passkeys.display()),
        "Read(~/.claude/.credentials.json)".to_string(),
        "Read(**/.env)".to_string(),
        "Read(**/.env.*)".to_string(),
    ]
    .to_vec()
}

const REFUSAL: &str =
    "Permission to use Bash has been denied because Claude Code is running in don't ask mode.";

/// A reply whose calls were all refused: one tool call (`id`, `name`, `input`) at a time.
fn refused_reply(calls: &[(&str, &str, Value)]) -> String {
    let mut events = Vec::new();
    for (id, name, input) in calls {
        events.push(json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}}));
        events.push(json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": REFUSAL, "is_error": true}]}}));
    }
    let denials: Vec<Value> = calls
        .iter()
        .map(|(id, name, input)| json!({"tool_name": name, "tool_use_id": id, "tool_input": input}))
        .collect();
    events.push(json!({"type": "result", "subtype": "success", "is_error": false, "result": "I was not allowed", "permission_denials": denials}));
    lines(&events)
}

fn reply_with(s: &Setup, reply: String) {
    std::fs::write(s.base.join("reply.jsonl"), reply).unwrap();
}

/// The refused tool items of the chat, in order.
fn refused_items(s: &Setup, chat: &str) -> Vec<Value> {
    read_chat(s, chat)["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "tool" && item["status"] == "refused")
        .cloned()
        .collect()
}

fn allow(s: &Setup, chat: &str, body: Value, who: Who) -> (u16, Value) {
    send_as(
        s,
        "POST",
        &format!("/api/projects/alpha/chats/{chat}/allow"),
        body,
        who,
    )
}

/// The rules allowed beyond the defaults in each turn so far.
fn allowed_per_turn(s: &Setup) -> Vec<Vec<String>> {
    calls(s)
        .iter()
        .map(|(_, settings)| {
            strings(&settings["permissions"]["allow"])
                .into_iter()
                .filter(|rule| !DEFAULTS.contains(&rule.as_str()))
                .collect()
        })
        .collect()
}

#[test]
fn a_refused_call_is_a_refused_item_with_the_narrowest_rule_allowed_once_for_the_next_turn_or_for_the_chat(
) {
    let s = setup("refused");
    let alpha = git_repo(&s.scan, "alpha");
    let chat = create_chat(&s);
    let main = alpha.join("src/main.rs");
    reply_with(
        &s,
        refused_reply(&[
            (
                "t1",
                "Bash",
                json!({"command": "orq queue add feat-x --approve"}),
            ),
            (
                "t2",
                "Edit",
                json!({"file_path": main.display().to_string(), "old_string": "a", "new_string": "b"}),
            ),
        ]),
    );

    say(&s, &chat, "queue feat-x and fix main");

    let refused = refused_items(&s, &chat);
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert_eq!(refused[0]["name"], "Bash");
    assert_eq!(refused[0]["input"], "orq queue add feat-x --approve");
    assert_eq!(refused[0]["rule"], "Bash(orq queue add:*)");
    let edit_rule = format!("Edit(/{})", main.display());
    assert_eq!(refused[1]["rule"], edit_rule.as_str());

    reply_with(&s, quiet_reply());
    let (status, body) = allow(
        &s,
        &chat,
        json!({"rule": "Bash(orq queue add:*)", "scope": "once", "assertion": "yes"}),
        Who::Passkey,
    );
    assert_eq!(status, 200, "{body}");
    say(&s, &chat, "Allowed: Bash(orq queue add:*). Try again.");
    say(&s, &chat, "and again");

    let (status, body) = allow(
        &s,
        &chat,
        json!({"rule": edit_rule, "scope": "chat", "assertion": "yes"}),
        Who::Passkey,
    );
    assert_eq!(status, 200, "{body}");
    say(&s, &chat, "Allowed. Try again.");
    say(&s, &chat, "and again");

    let once = "Bash(orq queue add:*)".to_string();
    assert_eq!(
        allowed_per_turn(&s),
        [
            vec![],
            vec![once],
            vec![],
            vec![edit_rule.clone()],
            vec![edit_rule.clone()],
        ]
    );
    // The chat's allowed rules are listed with it; a rule allowed once is gone once its turn started.
    assert_eq!(
        read_chat(&s, &chat)["rules"],
        json!([{"rule": edit_rule, "scope": "chat"}])
    );
}

#[test]
fn a_deny_a_wider_rule_or_any_command_is_refused_and_secrets_reads_stay_denied_after_any_allow() {
    let s = setup("deny-wider");
    let alpha = git_repo(&s.scan, "alpha");
    let chat = create_chat(&s);
    let token = RemoteConfig::token_path(&s.roots);
    let env = alpha.join("web/.env");
    reply_with(
        &s,
        refused_reply(&[
            ("t1", "Bash", json!({"command": "git push origin main"})),
            (
                "t2",
                "Read",
                json!({"file_path": token.display().to_string()}),
            ),
            ("t3", "Read", json!({"file_path": "web/.env"})),
            ("t4", "Bash", json!({"command": "orq queue add feat-x"})),
        ]),
    );
    say(&s, &chat, "push it");
    let refused = refused_items(&s, &chat);
    assert_eq!(refused[0]["rule"], "Bash(git push origin:*)");
    assert_eq!(refused[2]["rule"], format!("Read(/{})", env.display()));

    for rule in [
        "Bash(git push origin:*)".to_string(),
        format!("Read(/{})", token.display()),
        format!("Read(/{})", env.display()),
    ] {
        let (status, body) = allow(
            &s,
            &chat,
            json!({"rule": rule, "scope": "chat", "assertion": "yes"}),
            Who::Passkey,
        );
        assert_eq!(status, 422, "{rule}: {body}");
        assert!(
            body["error"].as_str().unwrap().contains("always denied"),
            "{body}"
        );
    }
    for rule in ["Bash(orq queue:*)", "Bash(*)", "Bash", "Edit"] {
        let (status, body) = allow(
            &s,
            &chat,
            json!({"rule": rule, "scope": "once", "assertion": "yes"}),
            Who::Passkey,
        );
        assert_eq!(status, 422, "{rule}: {body}");
        assert!(body["error"].as_str().unwrap().contains("wider"), "{body}");
    }

    // What may be allowed: the refused call's own rule, and the ledger's edits for the chat in one go.
    let ledger = format!("Edit(/{})", alpha.join(".tdd-features.json").display());
    for rule in ["Bash(orq queue add:*)", ledger.as_str()] {
        let (status, body) = allow(
            &s,
            &chat,
            json!({"rule": rule, "scope": "chat", "assertion": "yes"}),
            Who::Passkey,
        );
        assert_eq!(status, 200, "{rule}: {body}");
    }
    reply_with(&s, quiet_reply());
    say(&s, &chat, "Allowed. Try again.");

    let calls = calls(&s);
    let (_, settings) = calls.last().unwrap();
    assert_eq!(strings(&settings["permissions"]["deny"]), denies(&s));
    assert_eq!(
        allowed_per_turn(&s).last().unwrap(),
        &["Bash(orq queue add:*)".to_string(), ledger]
    );
}

fn audit_lines(s: &Setup) -> Vec<Value> {
    std::fs::read_to_string(s.roots.state.join("remote-actions.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn allowing_needs_the_passkey_confirmation_and_every_allow_and_removal_is_audited() {
    let s = setup("confirm-audit");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s);
    reply_with(
        &s,
        refused_reply(&[("t1", "Bash", json!({"command": "orq queue add feat-x"}))]),
    );
    say(&s, &chat, "queue it");
    let rule = "Bash(orq queue add:*)";

    let (status, body) = allow(
        &s,
        &chat,
        json!({"rule": rule, "scope": "chat"}),
        Who::Passkey,
    );
    assert_eq!(status, 401, "no confirmation: {body}");
    let (status, body) = allow(
        &s,
        &chat,
        json!({"rule": rule, "scope": "chat", "assertion": "no"}),
        Who::Passkey,
    );
    assert_eq!(status, 401, "a confirmation that does not pass: {body}");
    let (status, body) = allow(
        &s,
        &chat,
        json!({"rule": rule, "scope": "chat", "assertion": "yes"}),
        Who::Token,
    );
    assert_eq!(status, 403, "the token alone: {body}");
    assert_eq!(read_chat(&s, &chat)["rules"], json!([]));

    let (status, body) = allow(
        &s,
        &chat,
        json!({"rule": rule, "scope": "chat", "assertion": "yes"}),
        Who::Passkey,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        read_chat(&s, &chat)["rules"],
        json!([{"rule": rule, "scope": "chat"}])
    );
    let (status, body) = send(
        &s,
        "DELETE",
        &format!("/api/projects/alpha/chats/{chat}/allow"),
        json!({"rule": rule}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(read_chat(&s, &chat)["rules"], json!([]));

    let audited: Vec<(String, String, String)> = audit_lines(&s)
        .iter()
        .filter(|line| {
            matches!(
                line["action"].as_str(),
                Some("chat.allow" | "chat.disallow")
            )
        })
        .map(|line| {
            (
                line["action"].as_str().unwrap().to_string(),
                line["outcome"].as_str().unwrap().to_string(),
                line["reason"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(audited.len(), 5, "{audited:?}");
    assert!(audited[..3]
        .iter()
        .all(|(action, outcome, reason)| action == "chat.allow"
            && outcome == "refused"
            && reason.contains(rule)));
    assert_eq!(
        audited[3],
        (
            "chat.allow".to_string(),
            "allowed".to_string(),
            format!("{rule} (chat)")
        )
    );
    assert_eq!(
        audited[4],
        (
            "chat.disallow".to_string(),
            "allowed".to_string(),
            rule.to_string()
        )
    );
}

#[test]
fn a_denied_command_behind_options_a_path_quotes_or_a_wrapper_cannot_be_allowed() {
    let s = setup("deny-forms");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s);
    let commands = [
        "git -C /srv/repo push origin main",
        "/usr/bin/git push origin main",
        "git 'push' origin main",
        "env sudo ls",
        "nice -n 5 sudo ls",
        "rm -r -f build",
        "rm -fr build",
        "rm --recursive build",
        "cargo --locked publish",
        "npm --yes publish",
        "orq --verbose serve",
    ];
    let calls: Vec<(String, &str, Value)> = commands
        .iter()
        .enumerate()
        .map(|(n, command)| (format!("t{n}"), "Bash", json!({ "command": command })))
        .collect();
    let calls: Vec<(&str, &str, Value)> = calls
        .iter()
        .map(|(id, name, input)| (id.as_str(), *name, input.clone()))
        .collect();
    reply_with(&s, refused_reply(&calls));
    say(&s, &chat, "publish it");

    let refused = refused_items(&s, &chat);
    assert_eq!(refused.len(), commands.len(), "{refused:?}");
    for item in &refused {
        let rule = item["rule"].as_str().unwrap_or_else(|| panic!("a rule: {item}"));
        let (status, body) = allow(
            &s,
            &chat,
            json!({"rule": rule, "scope": "once", "assertion": "yes"}),
            Who::Passkey,
        );
        assert_eq!(status, 422, "{rule}: {body}");
        assert!(
            body["error"].as_str().unwrap().contains("always denied"),
            "{rule}: {body}"
        );
    }
    assert_eq!(read_chat(&s, &chat)["rules"], json!([]));

    // A command that only mentions a denied word as data, before the command it names, is not refused for it.
    reply_with(
        &s,
        refused_reply(&[("u1", "Bash", json!({"command": "orq queue add push-docs"}))]),
    );
    say(&s, &chat, "queue push-docs");
    let (status, body) = allow(
        &s,
        &chat,
        json!({"rule": "Bash(orq queue add:*)", "scope": "once", "assertion": "yes"}),
        Who::Passkey,
    );
    assert_eq!(status, 200, "{body}");
}

#[test]
fn a_rule_allowed_once_is_kept_for_the_next_turn_that_starts_when_a_reply_could_not_be_started() {
    let s = setup("once-kept");
    git_repo(&s.scan, "alpha");
    let chat = create_chat(&s);
    reply_with(
        &s,
        refused_reply(&[("t1", "Bash", json!({"command": "orq queue add feat-x"}))]),
    );
    say(&s, &chat, "queue feat-x");
    let rule = "Bash(orq queue add:*)";
    let (status, body) = allow(
        &s,
        &chat,
        json!({"rule": rule, "scope": "once", "assertion": "yes"}),
        Who::Passkey,
    );
    assert_eq!(status, 200, "{body}");

    // The agent can't be started: the reply fails, and no turn used the rule.
    let options: std::sync::Arc<orq_remote::projects::AgentOptionsSource> =
        std::sync::Arc::new(|| vec![("claude".to_string(), vec!["opus".to_string()])]);
    let broken = Setup {
        base: s.base.clone(),
        scan: s.scan.clone(),
        roots: s.roots.clone(),
        provider: StoreProjects::new(s.roots.clone(), s.scan.clone())
            .with_agent_options(options)
            .with_chat_agent(s.base.join("no-such-claude")),
    };
    let (status, body) = send(
        &broken,
        "POST",
        &format!("/api/projects/alpha/chats/{chat}/messages"),
        json!({"text": "Allowed. Try again."}),
    );
    assert_eq!(status, 500, "{body}");
    assert_eq!(
        read_chat(&s, &chat)["rules"],
        json!([{"rule": rule, "scope": "once"}])
    );

    reply_with(&s, quiet_reply());
    say(&s, &chat, "Allowed. Try again.");
    say(&s, &chat, "and again");
    assert_eq!(
        allowed_per_turn(&s),
        [vec![], vec![rule.to_string()], vec![]]
    );
    assert_eq!(read_chat(&s, &chat)["rules"], json!([]));
}

#[test]
fn the_server_confirms_an_allow_with_a_passkey_check_of_its_own_purpose() {
    use orq_core::remote_inbox::step_up_purpose;
    // A challenge the page asks for to allow a rule cannot turn remote actions on, nor the other way round.
    assert!(orq_core::passkey_confirm::PURPOSES.contains(&"chat.allow"));
    assert_eq!(
        step_up_purpose("/api/projects/alpha/chats/chat-1/allow"),
        "chat.allow"
    );
    assert_eq!(
        step_up_purpose("/api/settings/remote-actions"),
        "remote_actions.on"
    );
    assert_eq!(
        step_up_purpose("/api/projects/alpha/chats/chat-1/messages"),
        "remote_actions.on"
    );
}

#[test]
fn the_settings_file_holds_exactly_the_defaults_and_denies_in_dont_ask_mode_and_is_passed_with_settings(
) {
    let s = setup("defaults");
    let alpha = git_repo(&s.scan, "alpha");
    let chat = create_chat(&s);

    say(&s, &chat, "what is in the queue?");

    let calls = calls(&s);
    let (args, settings) = &calls[0];
    assert_eq!(
        value_after(args, "--settings").map(PathBuf::from),
        Some(
            s.roots
                .state
                .join("chats")
                .join(&chat)
                .join("settings.json")
        ),
        "{args:?}"
    );
    assert_eq!(value_after(args, "--permission-mode"), Some("dontAsk"));
    assert_eq!(
        settings["permissions"]["defaultMode"], "dontAsk",
        "{settings}"
    );
    assert_eq!(strings(&settings["permissions"]["allow"]), DEFAULTS);
    assert_eq!(strings(&settings["permissions"]["deny"]), denies(&s));
    // The tools a chat has at all: reading, the shell and edits, each only as its rules allow.
    assert_eq!(
        value_after(args, "--tools"),
        Some("Read,Grep,Glob,Bash,Edit,Write"),
        "{args:?}"
    );
    // The project's own settings are not changed.
    assert!(!alpha.join(".claude").exists());
}
